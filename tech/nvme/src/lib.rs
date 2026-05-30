//! Level-1 tech: NVMe drive enumeration + per-drive temperature, read from sysfs.
//!
//! Each NVMe controller appears under `/sys/class/nvme/nvmeN/` with cached identity attributes
//! (`serial`, `model`) and a single `hwmonM/` node exposing `tempK_input` (milli-°C) labelled by
//! `tempK_label` (e.g. "Composite", "Sensor 1"). The controller **serial** is the stable id (it
//! survives reboots; the `nvmeN`/`hwmonM` numbering is probe-order dependent). This crate is
//! read-only — it controls no device. The temp read goes through the NVMe driver (a SMART-log
//! admin command), so it can block on a wedged controller; isolating it in its own process (the
//! `nvme` anemos) is deliberate.

use std::fs;
use std::path::{Path, PathBuf};

/// Default sysfs class dir for NVMe controllers. Override via `AIOLOS_SYSFS_NVME` (dev/testing).
const DEFAULT_NVME_CLASS: &str = "/sys/class/nvme";

/// One detected NVMe controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvmeInfo {
    /// Stable id: the controller serial (trimmed). Survives reboots, unlike nvmeN/hwmonM numbering.
    pub serial: String,
    /// Human model string, e.g. "Samsung SSD 990 PRO 4TB" (may be empty).
    pub model: String,
    /// The controller's sysfs dir (…/nvmeN) — used to locate its hwmon node for temps.
    pub path: PathBuf,
}

fn nvme_class_dir() -> PathBuf {
    std::env::var("AIOLOS_SYSFS_NVME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_NVME_CLASS))
}

/// Enumerate NVMe controllers (those exposing a non-empty `serial`), serial-sorted for a
/// deterministic order. Empty if none or sysfs is unreadable (the caller treats "none" as a real,
/// declared result).
pub fn enumerate() -> Vec<NvmeInfo> {
    enumerate_in(&nvme_class_dir())
}

/// `enumerate` against an explicit class dir (the production path or a test fixture root).
fn enumerate_in(class_dir: &Path) -> Vec<NvmeInfo> {
    let mut out = Vec::new();
    let Ok(dir) = fs::read_dir(class_dir) else {
        return out;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        // Only real controller dirs carry a `serial`; this also skips non-controller entries.
        let Some(serial) = read_trim(&path.join("serial")).filter(|s| !s.is_empty()) else {
            continue;
        };
        let model = read_trim(&path.join("model")).unwrap_or_default();
        out.push(NvmeInfo {
            serial,
            model,
            path,
        });
    }
    out.sort_by(|a, b| a.serial.cmp(&b.serial));
    out
}

/// Read every temperature (°C) from the controller's hwmon node, labelled by `tempK_label` (else
/// `tempK`), label-sorted. `controller_path` is an [`NvmeInfo::path`]. Empty if the drive has no
/// hwmon node or it is unreadable (the caller treats "no temps" as a skip/fail-safe signal).
pub fn read_temps(controller_path: &Path) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(controller_path) else {
        return out;
    };
    // The controller dir contains a single `hwmonM` subdir carrying the temp sensors.
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with("hwmon") {
            out.extend(read_hwmon_dir(&e.path()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Read all `tempK_input` (milli-°C → °C) from one hwmon dir, labelled by `tempK_label`.
fn read_hwmon_dir(dir: &Path) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    let Ok(files) = fs::read_dir(dir) else {
        return out;
    };
    for f in files.flatten() {
        let fname = f.file_name().to_string_lossy().into_owned();
        let Some(k) = fname
            .strip_prefix("temp")
            .and_then(|s| s.strip_suffix("_input"))
        else {
            continue;
        };
        let Some(milli) = read_trim(&f.path()).and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        let label = read_trim(&dir.join(format!("temp{k}_label")))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("temp{k}"));
        out.push((label, milli / 1000));
    }
    out
}

fn read_trim(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sensor spec `(tempK, milli_celsius, label)`.
    type SensorSpec<'a> = (u32, i32, &'a str);
    /// A drive spec `(dirname, serial, hwmon_index, sensors)`.
    type DriveSpec<'a> = (&'a str, &'a str, u32, &'a [SensorSpec<'a>]);

    /// Build a fake `/sys/class/nvme` tree: per drive a `serial`/`model` and a `hwmonH` node with
    /// the given sensors. Returns the class-dir root.
    fn fixture(root: &Path, drives: &[DriveSpec]) {
        for (dirname, serial, hwmon_idx, sensors) in drives {
            let ctrl = root.join(dirname);
            fs::create_dir_all(&ctrl).unwrap();
            if !serial.is_empty() {
                fs::write(ctrl.join("serial"), serial).unwrap();
            }
            fs::write(ctrl.join("model"), "Test SSD").unwrap();
            let hwmon = ctrl.join(format!("hwmon{hwmon_idx}"));
            fs::create_dir_all(&hwmon).unwrap();
            for (k, milli, label) in *sensors {
                fs::write(hwmon.join(format!("temp{k}_input")), milli.to_string()).unwrap();
                if !label.is_empty() {
                    fs::write(hwmon.join(format!("temp{k}_label")), label).unwrap();
                }
            }
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("aiolos-nvme-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn enumerates_drives_by_serial_sorted_skipping_non_controllers() {
        let root = tmp("enum");
        fixture(
            &root,
            &[
                ("nvme1", "SERIAL-B", 1, &[(1, 30850, "Composite")]),
                ("nvme0", "SERIAL-A", 0, &[(1, 32850, "Composite")]),
                ("nvme-fabrics", "", 9, &[]), // no serial -> skipped
            ],
        );
        let drives = enumerate_in(&root);
        assert_eq!(drives.len(), 2, "only serial-bearing controllers enumerate");
        assert_eq!(drives[0].serial, "SERIAL-A", "serial-sorted");
        assert_eq!(drives[1].serial, "SERIAL-B");
        assert_eq!(drives[0].model, "Test SSD");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_per_drive_temps_with_labels_and_milli_conversion() {
        let root = tmp("temps");
        fixture(
            &root,
            &[(
                "nvme0",
                "SERIAL-A",
                0,
                &[
                    (1, 32850, "Composite"),
                    (2, 30850, "Sensor 1"),
                    (3, 42850, ""),
                ],
            )],
        );
        let drives = enumerate_in(&root);
        let temps = read_temps(&drives[0].path);
        // °C = milli/1000 (truncated); label-sorted; missing label -> "tempK".
        assert_eq!(
            temps,
            vec![
                ("Composite".to_string(), 32),
                ("Sensor 1".to_string(), 30),
                ("temp3".to_string(), 42),
            ]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_hwmon_or_dir_yields_empty() {
        let root = tmp("empty");
        // A controller with a serial but no hwmon node.
        let ctrl = root.join("nvme0");
        fs::create_dir_all(&ctrl).unwrap();
        fs::write(ctrl.join("serial"), "SERIAL-A").unwrap();
        let drives = enumerate_in(&root);
        assert_eq!(drives.len(), 1);
        assert!(
            read_temps(&drives[0].path).is_empty(),
            "no hwmon node -> no temps"
        );
        // A nonexistent path yields empty, never panics.
        assert!(read_temps(Path::new("/nonexistent/aiolos-nvme")).is_empty());
        let _ = fs::remove_dir_all(&root);
    }
}
