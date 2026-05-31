//! Generic Linux hwmon (sysfs) tech — no device specifics. Two roles:
//! - **read** labelled temperatures from any chip by `name` (e.g. `coretemp`, `gigabyte_wmi`), and
//! - **control** PWM fan channels on a chip (e.g. the ITE `it8689` Super-I/O): read/write `pwmN`,
//!   `pwmN_enable`, and read `fanN_input` tachometers, with the 0–100% ↔ 0–255 duty scaling.
//!
//! No std-external dependencies. Errors on reads are folded into `None`/empty (callers treat
//! "no reading" as their fail-safe trigger); PWM writes surface `io::Error` so a control module can
//! declare a fault and restore.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const HWMON_ROOT: &str = "/sys/class/hwmon";

/// `pwmN_enable` value for **manual** PWM control (the module commands the duty).
const PWM_ENABLE_MANUAL: u8 = 1;
/// `pwmN_enable` value for **automatic** firmware/SmartFan control — the fail-safe restore target.
const PWM_ENABLE_AUTO: u8 = 2;

// ---------------------------------------------------------------------------------------------
// Temperature reading
// ---------------------------------------------------------------------------------------------

/// Read every `tempN_input` (°C) from all `/sys/class/hwmon` chips whose `name` equals `chip`,
/// labelled by `tempN_label` where present (else `<chip>.tempN`). Returns empty if the chip is
/// absent or hwmon is unreadable (callers treat "no temps" as their fail-safe trigger).
///
/// Note: when several chips share `name` (e.g. dual-socket `k10temp`), their temps are concatenated;
/// labels collide only where the chips expose no `tempN_label`. Use [`read_chip_temps`] when you
/// need each instance disambiguated.
pub fn read_temps(chip: &str) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    for dir in chip_dirs(chip) {
        for (n, label, c) in temps_in_dir(&dir) {
            out.push((label.unwrap_or_else(|| format!("{chip}.temp{n}")), c));
        }
    }
    out
}

/// One chip instance's temperatures, with a stable per-instance discriminator so multiple chips
/// sharing a `name` (e.g. four `spd5118` DIMMs, two `r8169` NICs) don't collide in labels.
pub struct ChipTemps {
    /// The hwmon `name` (e.g. `spd5118`).
    pub chip: String,
    /// Per-instance discriminator: the `device` symlink target basename (e.g. `11-0050`, a stable
    /// i2c/PCI address) when available, else the `hwmonN` directory name.
    pub instance: String,
    /// `(sensor label, °C)` for each `tempN_input` on this chip; label is `tempN_label` else `tempN`.
    pub temps: Vec<(String, i32)>,
}

/// Read every chip whose `name` **matches** one of `chips`, each returned as a [`ChipTemps`]
/// carrying an instance discriminator. Matching is by NAME PREFIX (`name == q` or
/// `name.starts_with(q)`) so a family token like `r8169` also catches PCI-suffixed instances
/// (`r8169_0_600:00`, `r8169_0_700:00`); the per-instance discriminator keeps their labels distinct.
/// Chips/sensors that are absent or unreadable are simply omitted (never an error). Order follows
/// `chips`, then sysfs enumeration order within a match.
pub fn read_chip_temps(chips: &[String]) -> Vec<ChipTemps> {
    let mut out = Vec::new();
    for chip in chips {
        for dir in chip_dirs_prefix(chip) {
            let temps: Vec<(String, i32)> = temps_in_dir(&dir)
                .into_iter()
                .map(|(n, label, c)| (label.unwrap_or_else(|| format!("temp{n}")), c))
                .collect();
            if temps.is_empty() {
                continue;
            }
            out.push(ChipTemps {
                chip: chip.clone(),
                instance: instance_id(&dir),
                temps,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// PWM fan control
// ---------------------------------------------------------------------------------------------

/// The sysfs directory of the first `/sys/class/hwmon` chip whose `name` equals `chip`, or `None`
/// if no such chip is present. A control module resolves this once at `open` and addresses
/// `pwmN`/`fanN_input` underneath it.
pub fn chip_path(chip: &str) -> Option<PathBuf> {
    chip_dirs(chip).into_iter().next()
}

/// Read a fan tachometer (`fanN_input`, RPM) under `dir`; `channel` is 1-based. `None` if absent or
/// unreadable (a channel with no fan or no tach wire reads `0`, which is returned as `Some(0)`).
pub fn read_fan_rpm(dir: &Path, channel: u8) -> Option<i32> {
    read_sysfs_i64(&dir.join(format!("fan{channel}_input"))).map(|v| v as i32)
}

/// Read a channel's current `pwmN_enable` mode under `dir`; `channel` is 1-based.
pub fn read_pwm_enable(dir: &Path, channel: u8) -> Option<u8> {
    read_sysfs_i64(&dir.join(format!("pwm{channel}_enable"))).map(|v| v as u8)
}

/// Read a channel's current raw `pwmN` value (0–255) under `dir`; `channel` is 1-based.
pub fn read_pwm_raw(dir: &Path, channel: u8) -> Option<u8> {
    read_sysfs_i64(&dir.join(format!("pwm{channel}"))).map(|v| v.clamp(0, 255) as u8)
}

/// Put a channel under MANUAL control and command its duty `pct` (0–100%): write `pwmN_enable=1`
/// then `pwmN=<scaled 0–255>`. `channel` is 1-based. Re-asserting `enable=1` on every call defends
/// against a board EC that periodically reclaims SmartFan. Returns the raw value written.
pub fn set_pwm_duty(dir: &Path, channel: u8, pct: u32) -> io::Result<u8> {
    write_sysfs(&dir.join(format!("pwm{channel}_enable")), PWM_ENABLE_MANUAL)?;
    let raw = pct_to_raw(pct);
    write_sysfs(&dir.join(format!("pwm{channel}")), raw)?;
    Ok(raw)
}

/// Restore a channel to AUTOMATIC firmware/SmartFan control (`pwmN_enable=2`). `channel` is 1-based.
/// Idempotent — the fail-safe restore target (a frozen manual duty is never the resting state).
pub fn set_pwm_auto(dir: &Path, channel: u8) -> io::Result<()> {
    write_sysfs(&dir.join(format!("pwm{channel}_enable")), PWM_ENABLE_AUTO).map(|_| ())
}

/// Convert a duty percent (0–100, clamped) to a raw hwmon PWM value (0–255), rounded to nearest.
pub fn pct_to_raw(pct: u32) -> u8 {
    let pct = pct.min(100);
    ((pct * 255 + 50) / 100) as u8
}

/// Convert a raw hwmon PWM value (0–255) to a duty percent (0–100), rounded to nearest.
pub fn raw_to_pct(raw: u8) -> u32 {
    (raw as u32 * 100 + 127) / 255
}

// ---------------------------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------------------------

/// All `/sys/class/hwmon/hwmonN` directories whose `name` equals `chip` (exact — used by control
/// and the flat [`read_temps`], where the caller names a specific chip like `it8689`/`coretemp`).
fn chip_dirs(chip: &str) -> Vec<PathBuf> {
    chip_dirs_where(|name| name == chip)
}

/// All chip dirs whose `name` equals `query` OR starts with it (prefix — used by monitoring, so a
/// family token like `r8169` catches `r8169_0_600:00`/`r8169_0_700:00`).
fn chip_dirs_prefix(query: &str) -> Vec<PathBuf> {
    chip_dirs_where(|name| name == query || name.starts_with(query))
}

/// All `/sys/class/hwmon/hwmonN` directories whose `name` satisfies `pred`, sorted for stable order.
fn chip_dirs_where(pred: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let Ok(root) = fs::read_dir(HWMON_ROOT) else {
        return dirs;
    };
    for entry in root.flatten() {
        let dir = entry.path();
        if fs::read_to_string(dir.join("name"))
            .map(|n| pred(n.trim()))
            .unwrap_or(false)
        {
            dirs.push(dir);
        }
    }
    dirs.sort();
    dirs
}

/// `(channel, tempN_label or None, °C)` for every `tempN_input` directly under `dir`.
fn temps_in_dir(dir: &Path) -> Vec<(String, Option<String>, i32)> {
    let mut out = Vec::new();
    let Ok(files) = fs::read_dir(dir) else {
        return out;
    };
    for f in files.flatten() {
        let fname = f.file_name().to_string_lossy().into_owned();
        let Some(n) = fname
            .strip_prefix("temp")
            .and_then(|s| s.strip_suffix("_input"))
        else {
            continue;
        };
        if let Some(milli) = read_sysfs_i64(&f.path()) {
            let label = fs::read_to_string(dir.join(format!("temp{n}_label")))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            out.push((n.to_string(), label, (milli / 1000) as i32));
        }
    }
    out
}

/// A stable per-instance discriminator for a chip dir: the `device` symlink target basename (e.g.
/// `11-0050` for an i2c device, `0000:07:00.0` for PCI), falling back to the `hwmonN` dir name.
fn instance_id(dir: &Path) -> String {
    fs::read_link(dir.join("device"))
        .ok()
        .and_then(|t| t.file_name().map(|s| s.to_string_lossy().into_owned()))
        .or_else(|| dir.file_name().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

/// Parse a sysfs integer file (trimmed). `None` on any read/parse error.
fn read_sysfs_i64(path: &Path) -> Option<i64> {
    fs::read_to_string(path).ok()?.trim().parse::<i64>().ok()
}

/// Write an integer to a sysfs attribute.
fn write_sysfs(path: &Path, value: u8) -> io::Result<u8> {
    fs::write(path, value.to_string())?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct_to_raw_scales_and_rounds() {
        assert_eq!(pct_to_raw(0), 0);
        assert_eq!(pct_to_raw(100), 255);
        assert_eq!(pct_to_raw(35), 89); // 35% of 255 = 89.25 -> 89
        assert_eq!(pct_to_raw(50), 128); // 127.5 -> 128
        assert_eq!(pct_to_raw(200), 255, "over-100% clamps to full");
    }

    #[test]
    fn raw_to_pct_is_the_inverse_within_rounding() {
        assert_eq!(raw_to_pct(0), 0);
        assert_eq!(raw_to_pct(255), 100);
        assert_eq!(raw_to_pct(89), 35);
        assert_eq!(raw_to_pct(128), 50);
    }

    #[test]
    fn duty_round_trip_is_stable_at_the_floor_and_ceiling() {
        for pct in [0, 35, 50, 75, 100] {
            assert_eq!(raw_to_pct(pct_to_raw(pct)), pct, "round-trip {pct}%");
        }
    }
}
