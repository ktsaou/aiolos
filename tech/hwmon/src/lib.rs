//! Generic Linux hwmon (sysfs) sensor reader — reusable level-1 tech, no device specifics.

use std::fs;

/// Read every `tempN_input` (°C) from all `/sys/class/hwmon` chips whose `name` equals `chip`,
/// labelled by `tempN_label` where present (else `<chip>.tempN`). Returns empty if the chip is
/// absent or hwmon is unreadable (callers treat "no temps" as their fail-safe trigger).
pub fn read_temps(chip: &str) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    let Ok(dir) = fs::read_dir("/sys/class/hwmon") else {
        return out;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        if fs::read_to_string(path.join("name"))
            .map(|n| n.trim() != chip)
            .unwrap_or(true)
        {
            continue;
        }
        let Ok(files) = fs::read_dir(&path) else {
            continue;
        };
        for f in files.flatten() {
            let fname = f.file_name().to_string_lossy().into_owned();
            let Some(n) = fname
                .strip_prefix("temp")
                .and_then(|s| s.strip_suffix("_input"))
            else {
                continue;
            };
            if let Ok(milli) = fs::read_to_string(f.path())
                .unwrap_or_default()
                .trim()
                .parse::<i32>()
            {
                let label = fs::read_to_string(path.join(format!("temp{n}_label")))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("{chip}.temp{n}"));
                out.push((label, milli / 1000));
            }
        }
    }
    out
}
