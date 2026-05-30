//! Per-zone board-fan control (SOW-0010): split the 8 headers into two independently-curved zones.
//!
//! - **CPU zone** — FAN1/FAN2, the Noctua CPU coolers, driven by CPU (k10temp) temperature.
//! - **Case zone** — FAN3..FAN8, the 120 mm case fans, driven by `max(all routed inputs)` (GPU,
//!   NVMe, and any future routed source); CPU temp is deliberately NOT in the case max, so a
//!   CPU-only spike does not blast the case fans.
//!
//! Each zone has its own `anemos::Controller` (own curve file → own EMA/deadband/sensitivity), so
//! the safety-critical curve+floor logic stays in the SDK (we do not reimplement it). The zone
//! curve files sit next to the main curve, derived by suffix from the SDK controller's resolved
//! path so the `$AIOLOS_ETC_DIR` override is honoured automatically:
//!
//! | role | file (when the main curve is `…/asrock16-2t.curve.json`) |
//! |------|----------------------------------------------------------|
//! | CPU  | `…/asrock16-2t.cpu.curve.json`                           |
//! | case | `…/asrock16-2t.case.curve.json`                          |
//!
//! Zone mode activates **only when BOTH** zone files load a non-empty curve; otherwise the uniform
//! SDK controller drives all 8 fans (fully back-compatible — the shipped single curve is unchanged).

use anemos::{Controller, CurveCache};

/// The 0-based fan indices of the CPU-cooler zone (FAN1, FAN2).
pub const CPU_ZONE: [usize; 2] = [0, 1];
/// The 0-based fan indices of the case-fan zone (FAN3..FAN8).
pub const CASE_ZONE: [usize; 6] = [2, 3, 4, 5, 6, 7];

/// The two per-zone controllers plus their file paths (kept so `both_present` can re-probe the
/// config cheaply each tick without perturbing the controllers' EMA).
pub struct ZoneControllers {
    pub cpu: Controller,
    pub case: Controller,
    cpu_path: String,
    case_path: String,
}

impl ZoneControllers {
    /// Build the zone controllers from the main curve path (e.g. `…/asrock16-2t.curve.json`),
    /// deriving the per-zone file paths by suffix so they sit next to it (and inherit the env dir).
    pub fn for_main_path(main_curve_path: &str) -> Self {
        let cpu_path = zone_path(main_curve_path, "cpu");
        let case_path = zone_path(main_curve_path, "case");
        ZoneControllers {
            cpu: Controller::new(cpu_path.clone()),
            case: Controller::new(case_path.clone()),
            cpu_path,
            case_path,
        }
    }

    /// True iff BOTH zone curve files currently load a non-empty curve (the live mode switch). This
    /// is a pure config read (a throwaway `CurveCache` per zone) — it never touches the persistent
    /// controllers' EMA, so probing the mode every tick has no control side effects.
    pub fn both_present(&self) -> bool {
        !CurveCache::new(self.cpu_path.as_str()).curve().is_empty()
            && !CurveCache::new(self.case_path.as_str()).curve().is_empty()
    }
}

/// Derive a zone curve path from the main curve path by inserting `.<zone>` before the
/// `.curve.json` suffix. Falls back to appending `.<zone>` if the expected suffix is absent
/// (defensive — the shipped path always ends in `.curve.json`).
fn zone_path(main_curve_path: &str, zone: &str) -> String {
    match main_curve_path.strip_suffix(".curve.json") {
        Some(stem) => format!("{stem}.{zone}.curve.json"),
        None => format!("{main_curve_path}.{zone}"),
    }
}

/// Compose the 8 per-fan base duties from the two zone percentages: FAN1/2 = `cpu_pct`,
/// FAN3–8 = `case_pct`.
pub fn per_fan_duties(cpu_pct: u32, case_pct: u32) -> [u32; 8] {
    let mut d = [case_pct; 8];
    for &i in &CPU_ZONE {
        d[i] = cpu_pct;
    }
    d
}

/// The zone an absolute fan index belongs to (for sibling-boost compensation). FAN1/2 → CPU,
/// everything else → case.
pub fn zone_of(fan_index: usize) -> Zone {
    if CPU_ZONE.contains(&fan_index) {
        Zone::Cpu
    } else {
        Zone::Case
    }
}

/// The two fan zones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Cpu,
    Case,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_path_inserts_before_suffix() {
        assert_eq!(
            zone_path("/opt/aiolos/etc/asrock16-2t.curve.json", "cpu"),
            "/opt/aiolos/etc/asrock16-2t.cpu.curve.json"
        );
        assert_eq!(
            zone_path("/opt/aiolos/etc/asrock16-2t.curve.json", "case"),
            "/opt/aiolos/etc/asrock16-2t.case.curve.json"
        );
        // Env-dir override path is handled the same way (suffix is what matters).
        assert_eq!(
            zone_path("/tmp/etc/asrock16-2t.curve.json", "case"),
            "/tmp/etc/asrock16-2t.case.curve.json"
        );
    }

    #[test]
    fn zone_path_falls_back_when_suffix_absent() {
        assert_eq!(zone_path("/weird/path", "cpu"), "/weird/path.cpu");
    }

    #[test]
    fn per_fan_duties_splits_cpu_and_case() {
        // FAN1/2 take the CPU duty; FAN3-8 take the case duty.
        assert_eq!(per_fan_duties(30, 75), [30, 30, 75, 75, 75, 75, 75, 75]);
        assert_eq!(per_fan_duties(100, 40), [100, 100, 40, 40, 40, 40, 40, 40]);
    }

    #[test]
    fn zone_of_partitions_the_eight_fans() {
        assert_eq!(zone_of(0), Zone::Cpu);
        assert_eq!(zone_of(1), Zone::Cpu);
        for i in 2..8 {
            assert_eq!(zone_of(i), Zone::Case, "FAN{} should be a case fan", i + 1);
        }
    }
}
