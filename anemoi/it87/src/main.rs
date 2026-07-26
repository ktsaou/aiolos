//! it87 anemos — consumer-board fan control via the Linux `it87` hwmon driver (sysfs PWM).
//! Label-driven signal model (SOW-0018).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI/signals/logging/curve+EMA/
//! protocol/restore); `hwmon` is the sysfs PWM + temperature tech.
//!
//! It reports the **board** unit (`id` = `board`, shared with `hwmon-temps` on that host so they
//! merge): a `cpu` component (coretemp), a `control` component (driving decision), and `fan{ch}`
//! components — managed channels carry an `rpm` producer + a `duty` sink; unmanaged-but-spinning
//! headers (e.g. a BIOS-driven CPU fan) carry read-only `rpm` + `duty` producers.
//!
//! Case fans follow CPU plus routed temperature inputs; CPU-zone headers follow CPU only.

mod config;

use anemos::{
    Anemos, Component, Control, Controller, CurveCache, Device, Driving, Inputs, ModuleInfo,
    OpenMode, PolicyOutcome, Provenance, Report, Role, Signal, SignalCurvePolicy, SinkState, Unit,
};
use config::It87Config;
use serde_json::json;
use std::path::{Path, PathBuf};

const BOARD_ID: &str = "board";

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "it87",
            curve_default_path: Some("/opt/aiolos/etc/it87.curve.json"),
            curve_env_filename: Some("it87.curve.json"),
        },
        It87,
    )
}

struct It87;

impl Anemos for It87 {
    fn detect(&mut self) -> Report {
        let cfg = config::load();
        match hwmon::chip_path(&cfg.chip) {
            Some(_) => {
                let (mut components, mut signals) = (Vec::new(), Vec::new());
                for ch in cfg.managed_channels() {
                    let fc = format!("{BOARD_ID}:fan{ch}");
                    components.push(
                        Component::new(&fc, BOARD_ID)
                            .name(format!("fan{ch}"))
                            .typed("fan"),
                    );
                    signals.push(
                        Signal::sink(format!("{fc}:duty"), &fc, "fan-duty")
                            .uom("%")
                            .range(0.0, 100.0)
                            .name("duty")
                            .control(Control {
                                needs_claim: true,
                                safe: Some(json!("auto")),
                                direction: Some("up=more-cooling".into()),
                                readback: Some(format!("{fc}:rpm")),
                                ..Default::default()
                            }),
                    );
                }
                Report::ok(vec![board_unit(&cfg)], components, signals)
            }
            // The chip isn't present: a real "nothing to manage" result (empty, NOT an error).
            None => Report::ok(Vec::new(), Vec::new(), Vec::new()),
        }
    }

    fn open(&mut self, _id: &str, mode: OpenMode) -> anyhow::Result<Box<dyn Device>> {
        let cfg = config::load();
        let dir = hwmon::chip_path(&cfg.chip)
            .ok_or_else(|| anyhow::anyhow!("hwmon chip '{}' not present", cfg.chip))?;
        Ok(Box::new(It87Device {
            dir,
            cfg,
            zones: None,
            case_policy: None,
            restore_armed: mode == OpenMode::Control,
        }))
    }

    fn restore_all(&mut self) {
        let cfg = config::load();
        let Some(dir) = hwmon::chip_path(&cfg.chip) else {
            return;
        };
        let mut failed = false;
        for ch in cfg.managed_channels() {
            if let Err(e) = hwmon::set_pwm_auto(&dir, ch) {
                eprintln!("restore FAILED (pwm{ch}): {e}");
                failed = true;
            }
        }
        if failed {
            std::process::exit(2);
        }
        tracing::info!("all managed channels restored to firmware/automatic");
    }
}

struct Zones {
    cpu: Controller,
    case: Controller,
    cpu_path: String,
    case_path: String,
}

impl Zones {
    fn for_main_path(main_curve_path: &str) -> Self {
        let cpu_path = zone_path(main_curve_path, "cpu");
        let case_path = zone_path(main_curve_path, "case");
        Zones {
            cpu: Controller::new(cpu_path.clone()),
            case: Controller::new(case_path.clone()),
            cpu_path,
            case_path,
        }
    }
    fn both_present(&self) -> bool {
        !CurveCache::new(self.cpu_path.as_str()).curve().is_empty()
            && !CurveCache::new(self.case_path.as_str()).curve().is_empty()
    }
}

fn zone_path(main_curve_path: &str, zone: &str) -> String {
    match main_curve_path.strip_suffix(".curve.json") {
        Some(stem) => format!("{stem}.{zone}.curve.json"),
        None => format!("{main_curve_path}.{zone}"),
    }
}

struct It87Device {
    dir: PathBuf,
    cfg: It87Config,
    zones: Option<Zones>,
    case_policy: Option<SignalCurvePolicy>,
    restore_armed: bool,
}

impl Device for It87Device {
    fn collect(&mut self, _inputs: Option<&Inputs>) -> Report {
        let cpu_temps = hwmon::read_temps("coretemp");
        let (mut components, mut signals) = cpu_component(&cpu_temps);
        for ch in self.cfg.managed_channels() {
            let fc = format!("{BOARD_ID}:fan{ch}");
            components.push(
                Component::new(&fc, BOARD_ID)
                    .name(format!("fan{ch}"))
                    .typed("fan"),
            );
            if let Some(rpm) = hwmon::read_fan_rpm(&self.dir, ch) {
                signals.push(
                    Signal::producer(format!("{fc}:rpm"), &fc, "fan-rpm")
                        .value(json!(rpm))
                        .uom("rpm")
                        .name("rpm"),
                );
            }
            let state = match hwmon::read_pwm_enable(&self.dir, ch) {
                Some(2) => SinkState::Released,
                Some(1) if self.restore_armed => SinkState::Claimed,
                Some(_) | None => SinkState::Unknown,
            };
            let mut sink = Signal::sink(format!("{fc}:duty"), &fc, "fan-duty")
                .uom("%")
                .range(0.0, 100.0)
                .name("duty");
            if let Some(pct) = hwmon::read_pwm_raw(&self.dir, ch).map(hwmon::raw_to_pct) {
                sink = sink.value(json!(pct));
            }
            signals.push(sink.control(Control {
                needs_claim: true,
                state,
                safe: Some(json!("auto")),
                direction: Some("up=more-cooling".into()),
                readback: Some(format!("{fc}:rpm")),
                ..Default::default()
            }));
        }
        let (mut uc, mut us) = unmanaged_fan_components(&self.dir, &self.cfg);
        components.append(&mut uc);
        signals.append(&mut us);
        Report::ok(vec![board_unit(&self.cfg)], components, signals)
    }

    fn apply(&mut self, inputs: Option<&Inputs>, ctrl: &mut Controller) -> Report {
        // --- control path -----------------------------------------------------------------------
        let routed_temp_maxes = input_temp_maxes_by_module(inputs);
        let cpu_temps = hwmon::read_temps("coretemp");
        let cpu_max = cpu_temps.iter().map(|(_, t)| *t).max();
        let routed_max = routed_temp_maxes.iter().map(|s| s.value).max();
        let case_raw_opt = [routed_max, cpu_max].into_iter().flatten().max();
        let (_, local_cpu_signals) = cpu_component(&cpu_temps);

        let zones = self
            .zones
            .get_or_insert_with(|| Zones::for_main_path(ctrl.path()));
        let zone_mode = zones.both_present();

        let (mut commanded, baseline) = if zone_mode {
            let (Some(cpu_raw), Some(case_raw)) = (cpu_max, case_raw_opt) else {
                zones.cpu.reset();
                zones.case.reset();
                return self.release_or_error("zone mode: a zone temp is indeterminable");
            };
            let cpu_duty = zones.cpu.duty(cpu_raw);
            let case_duty = zones.case.duty(case_raw);
            let (Some(cpu_pct), Some(case_pct)) = (cpu_duty.pct, case_duty.pct) else {
                zones.cpu.reset();
                zones.case.reset();
                return self.release_or_error("zone mode: no usable curve");
            };
            tracing::info!(
                cpu_raw,
                case_raw,
                routed_max,
                cpu_pct,
                case_pct,
                cpu_smoothed = cpu_duty.smoothed,
                case_smoothed = case_duty.smoothed,
                "decision: set board fans (zone mode)"
            );
            let commanded = self.commanded_zone(cpu_pct, case_pct);
            (
                commanded,
                BaselineDecision::Zone {
                    cpu_raw,
                    cpu_smoothed: cpu_duty.smoothed,
                    case_raw,
                    case_smoothed: case_duty.smoothed,
                    case_pct,
                },
            )
        } else {
            let Some(raw) = case_raw_opt else {
                return self.release_or_error("indeterminable temp");
            };
            let duty = ctrl.duty(raw);
            let Some(pct) = duty.pct else {
                return self.release_or_error("no usable curve");
            };
            tracing::info!(routed_max = ?routed_max, cpu_max = ?cpu_max, raw_driving = raw,
                smoothed = duty.smoothed, commanded_pct = pct, "decision: set all board fans (uniform)");
            let commanded: Vec<(u8, u32)> = self
                .cfg
                .managed_channels()
                .into_iter()
                .map(|ch| (ch, pct))
                .collect();
            (
                commanded,
                BaselineDecision::Uniform {
                    raw,
                    smoothed: duty.smoothed,
                    pct,
                },
            )
        };

        let case_policy = self
            .case_policy
            .get_or_insert_with(|| SignalCurvePolicy::new(case_policy_path(ctrl.path())))
            .evaluate(inputs, &local_cpu_signals);
        if let Some(warning) = case_policy.warning() {
            tracing::warn!(
                path = %self.case_policy.as_ref().expect("policy initialized").path().display(),
                %warning,
                "case-fan overlay failed high"
            );
        }
        apply_case_overlay(
            &mut commanded,
            &self.cfg.case_channels,
            case_policy.overlay_pct(),
        );
        let decision = Decision {
            baseline,
            case_overlay: case_policy,
        };
        tracing::info!(
            baseline_case_pct = decision.baseline.case_pct(),
            overlay_pct = ?decision.case_overlay.overlay_pct(),
            ?commanded,
            "decision: set board fans (baseline plus case overlay)"
        );

        for &(ch, pct) in &commanded {
            if let Err(e) = hwmon::set_pwm_duty(&self.dir, ch, pct) {
                let _ = self.restore_to_auto();
                return Report::error(format!("set pwm{ch}: {e}"));
            }
        }
        // --- end control path -------------------------------------------------------------------

        // Real CPU temps + managed fans, each carrying its own per-zone `driving` record on the
        // sink. Routed board/NVMe/etc. temperatures remain owned by their publisher modules and are
        // referenced only as case-fan provenance.
        let (mut components, mut signals) = cpu_component(&cpu_temps);
        for &(ch, pct) in &commanded {
            let fc = format!("{BOARD_ID}:fan{ch}");
            components.push(
                Component::new(&fc, BOARD_ID)
                    .name(format!("fan{ch}"))
                    .typed("fan"),
            );
            if let Some(rpm) = hwmon::read_fan_rpm(&self.dir, ch) {
                signals.push(
                    Signal::producer(format!("{fc}:rpm"), &fc, "fan-rpm")
                        .value(json!(rpm))
                        .uom("rpm")
                        .name("rpm"),
                );
            }
            let cpu_zone = self.cfg.cpu_channels.contains(&ch);
            signals.push(
                Signal::sink(format!("{fc}:duty"), &fc, "fan-duty")
                    .value(json!(pct))
                    .uom("%")
                    .range(0.0, 100.0)
                    .name("duty")
                    .control(Control {
                        needs_claim: true,
                        state: SinkState::Claimed,
                        safe: Some(json!("auto")),
                        direction: Some("up=more-cooling".into()),
                        readback: Some(format!("{fc}:rpm")),
                        driven_by: driven_by_for_channel(
                            ch,
                            &self.cfg,
                            &routed_temp_maxes,
                            cpu_max,
                            zone_mode,
                            &decision,
                        ),
                        driving: Some(decision.driving_for(cpu_zone, pct)),
                    }),
            );
        }
        let (mut uc, mut us) = unmanaged_fan_components(&self.dir, &self.cfg);
        components.append(&mut uc);
        signals.append(&mut us);
        match decision.warning() {
            Some(warning) => {
                Report::ok_warn(vec![board_unit(&self.cfg)], components, signals, warning)
            }
            None => Report::ok(vec![board_unit(&self.cfg)], components, signals),
        }
    }

    fn restore(&mut self) {
        if !self.restore_armed {
            return;
        }
        match self.restore_to_auto() {
            Ok(()) => {
                tracing::info!("managed channels restored to firmware/automatic");
                self.restore_armed = false;
            }
            Err(e) => eprintln!("WARNING: pwm restore failed (will retry on drop): {e}"),
        }
    }
}

impl It87Device {
    fn commanded_zone(&self, cpu_pct: u32, case_pct: u32) -> Vec<(u8, u32)> {
        self.cfg
            .managed_channels()
            .into_iter()
            .map(|ch| {
                let pct = if self.cfg.cpu_channels.contains(&ch) {
                    cpu_pct
                } else {
                    case_pct
                };
                (ch, pct)
            })
            .collect()
    }

    fn restore_to_auto(&self) -> anyhow::Result<()> {
        let mut first_err = None;
        for ch in self.cfg.managed_channels() {
            if let Err(e) = hwmon::set_pwm_auto(&self.dir, ch) {
                first_err.get_or_insert(anyhow::anyhow!("pwm{ch}: {e}"));
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn release_or_error(&self, why: &str) -> Report {
        match self.restore_to_auto() {
            Ok(()) => Report::error(format!("{why} — released to firmware/automatic")),
            Err(e) => Report::error(format!("{why}; release failed: {e}")),
        }
    }
}

impl Drop for It87Device {
    fn drop(&mut self) {
        if self.restore_armed {
            let _ = self.restore_to_auto();
        }
    }
}

fn board_unit(cfg: &It87Config) -> Unit {
    Unit::new(BOARD_ID)
        .name("board")
        .description(format!("{} board", cfg.chip))
        .typed("board")
}

/// The `cpu` component + its coretemp producer signals (the sensors that drive the CPU fans).
fn cpu_component(cpu_temps: &[(String, i32)]) -> (Vec<Component>, Vec<Signal>) {
    if cpu_temps.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let cid = format!("{BOARD_ID}:cpu");
    let components = vec![Component::new(&cid, BOARD_ID).name("cpu").typed("cpu")];
    let signals = cpu_temps
        .iter()
        .map(|(label, t)| {
            Signal::producer(format!("{cid}:{}", slug(label)), &cid, "temperature")
                .value(json!(t))
                .uom("C")
                .name(label.clone())
        })
        .collect();
    (components, signals)
}

/// Existing fan calculation before optional case-only overlays.
enum BaselineDecision {
    Uniform {
        raw: i32,
        smoothed: i32,
        pct: u32,
    },
    Zone {
        cpu_raw: i32,
        cpu_smoothed: i32,
        case_raw: i32,
        case_smoothed: i32,
        case_pct: u32,
    },
}

impl BaselineDecision {
    /// The generic `driving` record for one managed channel: uniform -> max(cpu,routed temps);
    /// zone -> CPU temp for a CPU-zone channel, else max(cpu,routed temps) for a case channel.
    /// `output` is the commanded duty.
    fn driving_for(&self, cpu_zone: bool, output: u32) -> Driving {
        match self {
            BaselineDecision::Uniform { raw, smoothed, .. } => Driving::new()
                .kind("temperature")
                .raw(*raw as f64)
                .input(*smoothed as f64)
                .uom("C")
                .output(output as f64)
                .how("uniform: max(cpu,routed)→curve"),
            BaselineDecision::Zone {
                cpu_raw,
                cpu_smoothed,
                case_raw,
                case_smoothed,
                ..
            } => {
                let (raw, smoothed, how) = if cpu_zone {
                    (*cpu_raw, *cpu_smoothed, "zone:cpu")
                } else {
                    (*case_raw, *case_smoothed, "zone:case")
                };
                Driving::new()
                    .kind("temperature")
                    .raw(raw as f64)
                    .input(smoothed as f64)
                    .uom("C")
                    .output(output as f64)
                    .how(how)
            }
        }
    }

    fn case_pct(&self) -> u32 {
        match self {
            BaselineDecision::Uniform { pct, .. } => *pct,
            BaselineDecision::Zone { case_pct, .. } => *case_pct,
        }
    }
}

/// The control decision carried to the report stage.
struct Decision {
    baseline: BaselineDecision,
    case_overlay: PolicyOutcome,
}

impl Decision {
    fn overlay_drives(&self, cpu_zone: bool) -> bool {
        !cpu_zone
            && self
                .case_overlay
                .overlay_pct()
                .is_some_and(|overlay| overlay >= self.baseline.case_pct())
    }

    fn driving_for(&self, cpu_zone: bool, output: u32) -> Driving {
        if self.overlay_drives(cpu_zone) {
            if let Some(mut driving) = self.case_overlay.driving() {
                driving.output = Some(output as f64);
                return driving;
            }
        }
        self.baseline.driving_for(cpu_zone, output)
    }

    fn warning(&self) -> Option<&str> {
        self.case_overlay.warning()
    }
}

fn case_policy_path(main_curve_path: &str) -> String {
    match main_curve_path.strip_suffix(".curve.json") {
        Some(stem) => format!("{stem}.case.policy.json"),
        None => format!("{main_curve_path}.case.policy.json"),
    }
}

fn apply_case_overlay(commanded: &mut [(u8, u32)], case_channels: &[u8], overlay_pct: Option<u32>) {
    let Some(overlay_pct) = overlay_pct else {
        return;
    };
    for (channel, pct) in commanded {
        if case_channels.contains(channel) {
            *pct = (*pct).max(overlay_pct);
        }
    }
}

fn slug(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

/// Unmanaged fan headers worth REPORTING: those currently spinning (`rpm > 0`). Pure (unit-tested).
fn unmanaged_spinning(all: &[(u8, Option<i32>)], managed: &[u8]) -> Vec<u8> {
    all.iter()
        .filter(|(ch, _)| !managed.contains(ch))
        .filter(|(_, rpm)| rpm.is_some_and(|r| r > 0))
        .map(|(ch, _)| *ch)
        .collect()
}

/// Read-only `fan{ch}` components for the chip's unmanaged-but-spinning headers (rpm + duty
/// producers, no sink — aiolos does not control these).
fn unmanaged_fan_components(dir: &Path, cfg: &It87Config) -> (Vec<Component>, Vec<Signal>) {
    let managed = cfg.managed_channels();
    let all: Vec<(u8, Option<i32>)> = hwmon::fan_channels(dir)
        .into_iter()
        .map(|ch| (ch, hwmon::read_fan_rpm(dir, ch)))
        .collect();
    let mut components = Vec::new();
    let mut signals = Vec::new();
    for ch in unmanaged_spinning(&all, &managed) {
        let fc = format!("{BOARD_ID}:fan{ch}");
        components.push(
            Component::new(&fc, BOARD_ID)
                .name(format!("fan{ch}"))
                .typed("fan"),
        );
        if let Some(pct) = hwmon::read_pwm_raw(dir, ch).map(hwmon::raw_to_pct) {
            signals.push(
                Signal::producer(format!("{fc}:duty"), &fc, "fan-duty")
                    .value(json!(pct))
                    .uom("%")
                    .range(0.0, 100.0)
                    .name("duty"),
            );
        }
        if let Some(rpm) = hwmon::read_fan_rpm(dir, ch) {
            signals.push(
                Signal::producer(format!("{fc}:rpm"), &fc, "fan-rpm")
                    .value(json!(rpm))
                    .uom("rpm")
                    .name("rpm"),
            );
        }
    }
    (components, signals)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceTempMax {
    module: String,
    value: i32,
}

fn driven_by_for_channel(
    ch: u8,
    cfg: &It87Config,
    routed_temp_maxes: &[SourceTempMax],
    cpu_max: Option<i32>,
    zone_mode: bool,
    decision: &Decision,
) -> Vec<Provenance> {
    let mut out = Vec::new();
    let cpu_zone = cfg.cpu_channels.contains(&ch);
    if let Some(v) = cpu_max {
        out.push(Provenance::new("self:cpu").value(json!(v)).uom("C"));
    }
    if decision.overlay_drives(cpu_zone) {
        return decision.case_overlay.driven_by();
    }
    if !zone_mode || !cpu_zone {
        out.extend(routed_temp_maxes.iter().map(|s| {
            Provenance::new(format!("{} (max)", s.module))
                .value(json!(s.value))
                .uom("C")
        }));
    }
    out
}

/// Extract one max temperature per routed source module (keys are `module:id`).
fn input_temp_maxes_by_module(inputs: Option<&Inputs>) -> Vec<SourceTempMax> {
    let mut by_module = std::collections::BTreeMap::<String, i32>::new();
    let Some(inputs) = inputs else {
        return Vec::new();
    };
    for (key, signals) in inputs {
        let module = key
            .split_once(':')
            .map(|(module, _)| module)
            .unwrap_or(key.as_str());
        for s in signals {
            if s.role != Role::Producer || s.kind() != Some("temperature") {
                continue;
            }
            let Some(value) = s.value_i64() else {
                continue;
            };
            let Ok(value) = i32::try_from(value) else {
                continue;
            };
            by_module
                .entry(module.to_string())
                .and_modify(|max| *max = (*max).max(value))
                .or_insert(value);
        }
    }
    by_module
        .into_iter()
        .map(|(module, value)| SourceTempMax { module, value })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_signal(value: i64) -> Signal {
        Signal::producer("g:t:temp", "g:t", "temperature")
            .value(json!(value))
            .uom("C")
    }

    #[test]
    fn zone_path_inserts_before_suffix() {
        assert_eq!(
            zone_path("/opt/aiolos/etc/it87.curve.json", "cpu"),
            "/opt/aiolos/etc/it87.cpu.curve.json"
        );
        assert_eq!(zone_path("/weird/path", "cpu"), "/weird/path.cpu");
    }

    #[test]
    fn commanded_zone_splits_cpu_and_case() {
        let dev = It87Device {
            dir: PathBuf::from("/nonexistent"),
            cfg: It87Config {
                chip: "it8689".into(),
                cpu_channels: vec![1],
                case_channels: vec![3, 4],
            },
            zones: None,
            case_policy: None,
            restore_armed: true,
        };
        assert_eq!(dev.commanded_zone(30, 75), vec![(1, 30), (3, 75), (4, 75)]);
    }

    #[test]
    fn unmanaged_spinning_reports_only_spinning_unmanaged_headers() {
        let all = vec![
            (1u8, Some(1293)),
            (2, Some(0)),
            (3, Some(1254)),
            (4, Some(1259)),
            (5, Some(0)),
        ];
        assert_eq!(unmanaged_spinning(&all, &[3, 4]), vec![1]);
    }

    #[test]
    fn unmanaged_spinning_excludes_managed_and_hides_dead_headers() {
        let all = vec![(1u8, Some(0)), (2, None), (3, Some(900)), (4, Some(800))];
        assert_eq!(unmanaged_spinning(&all, &[3]), vec![4]);
        assert_eq!(unmanaged_spinning(&all, &[]), vec![3, 4]);
    }

    #[test]
    fn input_temp_maxes_by_module_extracts_temperature_producer_maxes() {
        use std::collections::HashMap;
        let mut inputs: Inputs = HashMap::new();
        inputs.insert("nvidia:GPU-1".into(), vec![temp_signal(63)]);
        inputs.insert(
            "hwmon-temps:board".into(),
            vec![
                temp_signal(70),
                temp_signal(66),
                Signal::producer("board:fan3:rpm", "board:fan3", "fan-rpm").value(json!(1400)),
                Signal::sink("board:fan3:duty", "board:fan3", "fan-duty").value(json!(70)),
                temp_signal(i64::from(i32::MAX) + 1),
            ],
        );
        inputs.insert("nvme:SER-A".into(), vec![temp_signal(44)]);

        assert_eq!(
            input_temp_maxes_by_module(Some(&inputs)),
            vec![
                SourceTempMax {
                    module: "hwmon-temps".into(),
                    value: 70,
                },
                SourceTempMax {
                    module: "nvidia".into(),
                    value: 63,
                },
                SourceTempMax {
                    module: "nvme".into(),
                    value: 44,
                },
            ]
        );
        assert!(input_temp_maxes_by_module(None).is_empty());
    }

    #[test]
    fn driven_by_for_channel_keeps_cpu_zone_cpu_only_and_case_zone_all_sources() {
        let cfg = It87Config {
            chip: "it8689".into(),
            cpu_channels: vec![1],
            case_channels: vec![3, 4],
        };
        let routed = vec![
            SourceTempMax {
                module: "hwmon-temps".into(),
                value: 71,
            },
            SourceTempMax {
                module: "nvidia".into(),
                value: 52,
            },
        ];
        let decision = Decision {
            baseline: BaselineDecision::Zone {
                cpu_raw: 68,
                cpu_smoothed: 66,
                case_raw: 71,
                case_smoothed: 69,
                case_pct: 70,
            },
            case_overlay: PolicyOutcome::Inactive,
        };

        let cpu = driven_by_for_channel(1, &cfg, &routed, Some(68), true, &decision);
        assert_eq!(cpu.len(), 1);
        assert_eq!(cpu[0].name, "self:cpu");

        let case = driven_by_for_channel(3, &cfg, &routed, Some(68), true, &decision);
        assert_eq!(case.len(), 3);
        assert_eq!(case[0].name, "self:cpu");
        assert_eq!(case[1].name, "hwmon-temps (max)");
        assert_eq!(case[2].name, "nvidia (max)");
    }

    #[test]
    fn claimed_it87_sink_satisfies_the_driving_contract() {
        let dec = Decision {
            baseline: BaselineDecision::Zone {
                cpu_raw: 70,
                cpu_smoothed: 68,
                case_raw: 60,
                case_smoothed: 58,
                case_pct: 75,
            },
            case_overlay: PolicyOutcome::Inactive,
        };
        let cpu = dec.driving_for(true, 40);
        assert_eq!(cpu.input, Some(68.0));
        assert_eq!(cpu.output, Some(40.0));
        assert_eq!(cpu.how.as_deref(), Some("zone:cpu"));
        let case = dec.driving_for(false, 75);
        assert_eq!(case.input, Some(58.0));
        assert_eq!(case.how.as_deref(), Some("zone:case"));
        let sink = Signal::sink("board:fan1:duty", "board:fan1", "fan-duty")
            .value(json!(40))
            .control(Control {
                state: SinkState::Claimed,
                driving: Some(cpu),
                ..Default::default()
            });
        assert!(Report::ok(vec![], vec![], vec![sink])
            .sink_contract_violations()
            .is_empty());
    }

    #[test]
    fn policy_fail_high_changes_case_channels_but_keeps_cpu_decision() {
        let decision = Decision {
            baseline: BaselineDecision::Zone {
                cpu_raw: 68,
                cpu_smoothed: 66,
                case_raw: 60,
                case_smoothed: 58,
                case_pct: 40,
            },
            case_overlay: PolicyOutcome::FailHigh {
                warning: "required nvme telemetry missing".into(),
            },
        };
        let cpu = decision.driving_for(true, 45);
        assert_eq!(cpu.output, Some(45.0));
        assert_eq!(cpu.how.as_deref(), Some("zone:cpu"));

        let case = decision.driving_for(false, 100);
        assert_eq!(case.input, Some(1.0));
        assert_eq!(case.output, Some(100.0));
        assert_eq!(case.how.as_deref(), Some("case-policy:fail-high"));
        assert_eq!(decision.warning(), Some("required nvme telemetry missing"));
    }

    #[test]
    fn case_overlay_only_raises_configured_case_channels() {
        let mut commanded = vec![(1, 40), (3, 30), (4, 80), (5, 50)];
        apply_case_overlay(&mut commanded, &[3, 4, 5], Some(65));
        assert_eq!(commanded, vec![(1, 40), (3, 65), (4, 80), (5, 65)]);

        let mut unchanged = vec![(1, 40), (3, 30), (4, 80), (5, 50)];
        apply_case_overlay(&mut unchanged, &[3, 4, 5], None);
        assert_eq!(unchanged, vec![(1, 40), (3, 30), (4, 80), (5, 50)]);
    }

    #[test]
    fn case_policy_path_tracks_the_resolved_main_curve_directory() {
        assert_eq!(
            case_policy_path("/tmp/etc/it87.curve.json"),
            "/tmp/etc/it87.case.policy.json"
        );
    }
}
