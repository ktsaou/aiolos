//! rome2d-fans anemos — ASRockRack ROME2D16-2T board fan control via inband IPMI (label-driven
//! signal model, SOW-0018).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI/signals/logging/curve+EMA/
//! protocol/restore); `ipmi` is the IPMI transport; `board` is the board's OEM fan commands; `hwmon`
//! reads CPU temps.
//!
//! It reports the **motherboard unit** (`id` = `board`, shared with `ipmi-temps` so the BMC temps and
//! these fans merge into one unit): `fan1..fan8` components (an `rpm` producer + a `duty` sink each),
//! a `cpu` component (the k10temp CPU sensors that drive the CPU fans), and a `control` component
//! carrying the driving-decision signals.
//!
//! The control path (read routed temps, compute duties via curve+EMA, drive the 8 fans, fault
//! detection, release-to-auto fail-safe) is UNCHANGED from v1 — this is a reporting refactor.

mod board;
mod fault;
mod zones;

use anemos::{
    Anemos, Component, Control, Controller, Device, Driving, ExtraCmd, Inputs, ModuleInfo,
    OpenMode, Provenance, Report, Signal, SinkState, Unit,
};
use board::Board;
use fault::FanFaultTracker;
use serde_json::json;
use std::collections::HashMap;
use zones::ZoneControllers;

/// The stable id of the physical motherboard unit (shared with `ipmi-temps` so they merge).
const BOARD_ID: &str = "board";
const BOARD_DESC: &str = "ASRockRack ROME2D16-2T";

fn main() -> ! {
    let mut extra: HashMap<&'static str, ExtraCmd> = HashMap::new();
    extra.insert("query", Box::new(|_args| query_mode()));
    anemos::run_with(
        ModuleInfo {
            name: "rome2d-fans",
            curve_default_path: Some("/opt/aiolos/etc/rome2d-fans.curve.json"),
            curve_env_filename: Some("rome2d-fans.curve.json"),
        },
        Rome2dFans,
        extra,
    )
}

struct Rome2dFans;

impl Anemos for Rome2dFans {
    fn detect(&mut self) -> Report {
        let (mut components, mut signals) = (Vec::new(), Vec::new());
        for i in 1..=8 {
            let fc = format!("{BOARD_ID}:fan{i}");
            components.push(
                Component::new(&fc, BOARD_ID)
                    .name(format!("fan{i}"))
                    .typed("fan"),
            );
            signals.push(
                Signal::producer(format!("{fc}:rpm"), &fc, "fan-rpm")
                    .uom("rpm")
                    .name("rpm"),
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
        Report::ok(vec![board_unit()], components, signals)
    }

    fn open(&mut self, _id: &str, mode: OpenMode) -> anyhow::Result<Box<dyn Device>> {
        let mut board = Board::open()?;
        board.prefetch_fan_factors();
        Ok(Box::new(Rome2dFansDevice {
            board,
            restore_armed: mode == OpenMode::Control,
            zones: None,
            faults: FanFaultTracker::new(),
        }))
    }

    fn restore_all(&mut self) {
        match (|| -> anyhow::Result<()> { Board::open()?.release_auto() })() {
            Ok(()) => tracing::info!("fans released to BMC auto"),
            Err(e) => {
                eprintln!("restore FAILED: {e}");
                std::process::exit(2);
            }
        }
    }
}

struct Rome2dFansDevice {
    board: Board,
    restore_armed: bool,
    zones: Option<ZoneControllers>,
    faults: FanFaultTracker,
}

impl Device for Rome2dFansDevice {
    fn collect(&mut self, _inputs: Option<&Inputs>) -> Report {
        // Read-only snapshot for `rome2d-fans info`: local CPU sensors plus BMC duty/RPM readbacks.
        // Never claims, sets, or releases the board, so it is safe while another controller owns it.
        let cpu_temps = hwmon::read_temps("k10temp");
        let (mut components, mut signals) = cpu_components(&cpu_temps);

        let (duty_readback, fan_rpms) = self.board.read_fan_status();
        let (mut fc, mut fs) =
            fan_components(&fan_rpms, duty_readback.as_deref(), None, &[false; 8]);
        components.append(&mut fc);
        signals.append(&mut fs);
        Report::ok(vec![board_unit()], components, signals)
    }

    fn apply(&mut self, inputs: Option<&Inputs>, ctrl: &mut Controller) -> Report {
        // --- control path (UNCHANGED from v1) ---------------------------------------------------
        let gpu_temps = input_temps_from(inputs, "nvidia");
        let nvme_temps = input_temps_from(inputs, "nvme");
        let cpu_temps = hwmon::read_temps("k10temp");
        let gpu_max = gpu_temps.iter().copied().max();
        let nvme_max = nvme_temps.iter().copied().max();
        let cpu_max = cpu_temps.iter().map(|(_, t)| *t).max();
        let input_max = input_temps(inputs).into_iter().max();
        let raw_driving = [input_max, cpu_max].into_iter().flatten().max();

        let zones = self
            .zones
            .get_or_insert_with(|| ZoneControllers::for_main_path(ctrl.path()));
        let zone_mode = zones.both_present();
        let confirmed = self.faults.confirmed();

        let outcome = if zone_mode {
            let (Some(cpu_raw), Some(case_raw)) = (cpu_max, input_max) else {
                self.reset_zone_dampers();
                return release_or_error(
                    &mut self.board,
                    "zone mode: a zone temp is indeterminable",
                );
            };
            let cpu_duty = zones.cpu.duty(cpu_raw);
            let case_duty = zones.case.duty(case_raw);
            let (Some(cpu_pct), Some(case_pct)) = (cpu_duty.pct, case_duty.pct) else {
                self.reset_zone_dampers();
                return release_or_error(&mut self.board, "zone mode: no usable curve");
            };
            let base = zones::per_fan_duties(cpu_pct, case_pct);
            let commanded = fault::compensate(base, &confirmed);
            tracing::info!(
                cpu_raw,
                case_raw,
                cpu_smoothed = cpu_duty.smoothed,
                case_smoothed = case_duty.smoothed,
                cpu_pct,
                case_pct,
                ?commanded,
                ?confirmed,
                "decision: set board fans (zone mode)"
            );
            if let Err(e) = self.board.set_fans_per_fan(&commanded.map(|p| p as i32)) {
                return Report::error(format!("set fans: {e}"));
            }
            ApplyOutcome::zone(
                commanded,
                cpu_raw,
                case_raw,
                cpu_duty.smoothed,
                case_duty.smoothed,
            )
        } else {
            let Some(raw) = raw_driving else {
                return release_or_error(&mut self.board, "indeterminable temp");
            };
            let duty = ctrl.duty(raw);
            let Some(pct) = duty.pct else {
                return release_or_error(&mut self.board, "no usable curve");
            };
            let base = [pct; 8];
            let commanded = fault::compensate(base, &confirmed);
            tracing::info!(gpu_max = ?gpu_max, nvme_max = ?nvme_max, cpu_max = ?cpu_max,
                raw_driving = raw, smoothed_driving = duty.smoothed,
                commanded_pct = pct, ?commanded, ?confirmed,
                "decision: set all board fans (uniform)");
            let set = if commanded == base {
                self.board.set_all_fans(pct as i32)
            } else {
                self.board.set_fans_per_fan(&commanded.map(|p| p as i32))
            };
            if let Err(e) = set {
                return Report::error(format!("set fans: {e}"));
            }
            ApplyOutcome::uniform(commanded, raw, duty.smoothed)
        };
        // --- end control path -------------------------------------------------------------------

        // Report stage: REAL CPU temps (the only board temperatures) + the 8 fans, each carrying its
        // own per-zone `driving` record. No driving-* producer signals — driving lives on the sinks.
        let (mut components, mut signals) = cpu_components(&cpu_temps);

        // Observability read AFTER the control decision (short timeout): true per-fan duty + RPM.
        let (duty_readback, fan_rpms) = self.board.read_fan_status();
        let commanded = outcome.commanded();
        let rpms: [Option<i32>; 8] = std::array::from_fn(|i| fan_rpms.get(i).and_then(|(_, r)| *r));
        let now_faulted = self.faults.update(&commanded, &rpms);
        for (i, f) in now_faulted.iter().enumerate() {
            if *f {
                tracing::warn!(
                    fan = i + 1,
                    commanded = commanded[i],
                    "FAN FAULT: commanded above threshold but reads ~0 RPM (stalled/failed fan)"
                );
            }
        }
        let drives = fan_drives(&outcome, gpu_max, nvme_max, cpu_max);
        let (mut fc, mut fs) = fan_components(
            &fan_rpms,
            duty_readback.as_deref(),
            Some(&drives),
            &now_faulted,
        );
        components.append(&mut fc);
        signals.append(&mut fs);
        Report::ok(vec![board_unit()], components, signals)
    }

    fn restore(&mut self) {
        if !self.restore_armed {
            return;
        }
        let result = (|| -> anyhow::Result<()> { Board::open()?.release_auto() })();
        match &result {
            Ok(()) => tracing::info!("released BMC auto control"),
            Err(e) => eprintln!("WARNING: BMC release failed (will retry on drop): {e}"),
        }
        self.restore_armed = still_armed_after(result.is_ok());
    }
}

impl Rome2dFansDevice {
    fn reset_zone_dampers(&mut self) {
        if let Some(z) = self.zones.as_mut() {
            z.cpu.reset();
            z.case.reset();
        }
    }
}

impl Drop for Rome2dFansDevice {
    fn drop(&mut self) {
        if self.restore_armed {
            if let Ok(mut b) = Board::open() {
                let _ = b.release_auto();
            }
        }
    }
}

fn board_unit() -> Unit {
    Unit::new(BOARD_ID)
        .name("board")
        .description(BOARD_DESC)
        .typed("board")
}

/// Build the `cpu` component + its k10temp producer signals (the sensors driving the CPU fans).
fn cpu_components(cpu_temps: &[(String, i32)]) -> (Vec<Component>, Vec<Signal>) {
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

/// The per-fan control decision: what drove this fan (`driven_by`) + the generic `driving` record.
struct FanDrive {
    driven_by: Vec<Provenance>,
    driving: Driving,
}

fn prov(name: &str, v: Option<i32>) -> Option<Provenance> {
    v.map(|x| Provenance::new(name).value(json!(x)).uom("C"))
}

/// Per-fan driving from the tick's outcome: uniform → every fan driven by max(gpu,nvme,cpu); zone →
/// CPU fans (FAN1/2) by CPU temp, case fans (FAN3–8) by the routed (GPU/NVMe) max. Accurate per fan.
fn fan_drives(
    outcome: &ApplyOutcome,
    gpu_max: Option<i32>,
    nvme_max: Option<i32>,
    cpu_max: Option<i32>,
) -> [FanDrive; 8] {
    let gpu = prov("gpu (max)", gpu_max);
    let nvme = prov("nvme (max)", nvme_max);
    let cpu = prov("cpu (max)", cpu_max);
    match outcome {
        ApplyOutcome::Uniform {
            commanded,
            raw,
            smoothed,
            ..
        } => std::array::from_fn(|i| FanDrive {
            driven_by: [gpu.clone(), nvme.clone(), cpu.clone()]
                .into_iter()
                .flatten()
                .collect(),
            driving: Driving::new()
                .kind("temperature")
                .raw(*raw as f64)
                .input(*smoothed as f64)
                .uom("C")
                .output(commanded[i] as f64)
                .how("uniform: max(gpu,nvme,cpu)→curve"),
        }),
        ApplyOutcome::Zone {
            commanded,
            cpu_raw,
            case_raw,
            cpu_smoothed,
            case_smoothed,
        } => std::array::from_fn(|i| {
            if i < 2 {
                FanDrive {
                    driven_by: [cpu.clone()].into_iter().flatten().collect(),
                    driving: Driving::new()
                        .kind("temperature")
                        .raw(*cpu_raw as f64)
                        .input(*cpu_smoothed as f64)
                        .uom("C")
                        .output(commanded[i] as f64)
                        .how("zone:cpu"),
                }
            } else {
                FanDrive {
                    driven_by: [gpu.clone(), nvme.clone()].into_iter().flatten().collect(),
                    driving: Driving::new()
                        .kind("temperature")
                        .raw(*case_raw as f64)
                        .input(*case_smoothed as f64)
                        .uom("C")
                        .output(commanded[i] as f64)
                        .how("zone:case"),
                }
            }
        }),
    }
}

/// Build the `fan1..fan8` components: an `rpm` producer + a `duty` sink. `drives` present => aiolos
/// commands the fans (claimed, value = decision output, driving attached); `None` => read-only info
/// (firmware readback, no decision).
fn fan_components(
    fan_rpms: &[(String, Option<i32>)],
    duty_readback: Option<&[u8]>,
    drives: Option<&[FanDrive; 8]>,
    faulted: &[bool; 8],
) -> (Vec<Component>, Vec<Signal>) {
    let mut components = Vec::new();
    let mut signals = Vec::new();
    for (i, (_label, rpm)) in fan_rpms.iter().enumerate() {
        let n = i + 1;
        let fc = format!("{BOARD_ID}:fan{n}");
        components.push(
            Component::new(&fc, BOARD_ID)
                .name(format!("fan{n}"))
                .typed("fan"),
        );
        if let Some(r) = rpm {
            signals.push(
                Signal::producer(format!("{fc}:rpm"), &fc, "fan-rpm")
                    .value(json!(r))
                    .uom("rpm")
                    .name("rpm"),
            );
        }
        let mut sink = Signal::sink(format!("{fc}:duty"), &fc, "fan-duty")
            .uom("%")
            .range(0.0, 100.0)
            .name("duty");
        if faulted[i] {
            sink = sink.label("fault", "true");
        }
        let control = match drives {
            Some(d) => {
                let out = d[i].driving.output.unwrap_or(0.0);
                sink = sink.value(json!(out as i64));
                Control {
                    needs_claim: true,
                    state: SinkState::Claimed,
                    safe: Some(json!("auto")),
                    direction: Some("up=more-cooling".into()),
                    readback: Some(format!("{fc}:rpm")),
                    driven_by: d[i].driven_by.clone(),
                    driving: Some(d[i].driving.clone()),
                }
            }
            None => {
                if let Some(v) = duty_readback.and_then(|d| d.get(i)) {
                    sink = sink.value(json!(*v as i64));
                }
                Control {
                    needs_claim: true,
                    state: SinkState::Unknown,
                    safe: Some(json!("auto")),
                    direction: Some("up=more-cooling".into()),
                    readback: Some(format!("{fc}:rpm")),
                    driven_by: Vec::new(),
                    driving: None,
                }
            }
        };
        signals.push(sink.control(control));
    }
    (components, signals)
}

/// What the apply tick decided, carried to the report stage.
enum ApplyOutcome {
    Uniform {
        commanded: [u32; 8],
        raw: i32,
        smoothed: i32,
    },
    Zone {
        commanded: [u32; 8],
        cpu_raw: i32,
        case_raw: i32,
        cpu_smoothed: i32,
        case_smoothed: i32,
    },
}

impl ApplyOutcome {
    fn uniform(commanded: [u32; 8], raw: i32, smoothed: i32) -> Self {
        ApplyOutcome::Uniform {
            commanded,
            raw,
            smoothed,
        }
    }
    fn zone(
        commanded: [u32; 8],
        cpu_raw: i32,
        case_raw: i32,
        cpu_smoothed: i32,
        case_smoothed: i32,
    ) -> Self {
        ApplyOutcome::Zone {
            commanded,
            cpu_raw,
            case_raw,
            cpu_smoothed,
            case_smoothed,
        }
    }
    fn commanded(&self) -> [u32; 8] {
        match self {
            ApplyOutcome::Uniform { commanded, .. } | ApplyOutcome::Zone { commanded, .. } => {
                *commanded
            }
        }
    }
}

/// Release the board to BMC auto and report it as an `error` apply (the safe fallback when we cannot
/// determine a duty); used by both the no-temp and empty-curve paths.
fn release_or_error(board: &mut Board, why: &str) -> Report {
    match board.release_auto() {
        Ok(()) => Report::error(format!("{why} — released to BMC auto")),
        Err(e) => Report::error(format!("{why}; release failed: {e}")),
    }
}

fn still_armed_after(released_ok: bool) -> bool {
    !released_ok
}

/// Read-only diagnostic: send only `0xda` (query duty) and print the result. Returns an exit code.
fn query_mode() -> i32 {
    match Board::open() {
        Ok(mut b) => match b.query_duty() {
            Ok(duty) => {
                println!("0xda OK ({} bytes): {duty:?}", duty.len());
                for (i, d) in duty.iter().take(8).enumerate() {
                    println!("  FAN{} = {}%", i + 1, d);
                }
                0
            }
            Err(e) => {
                eprintln!("0xda query FAILED: {e}");
                2
            }
        },
        Err(e) => {
            eprintln!("open /dev/ipmi0 FAILED: {e}");
            3
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

/// Extract every temperature value from ALL routed peer inputs (source-agnostic; the driving max).
fn input_temps(inputs: Option<&Inputs>) -> Vec<i32> {
    let mut v = Vec::new();
    if let Some(inputs) = inputs {
        for signals in inputs.values() {
            push_temps(signals, &mut v);
        }
    }
    v
}

/// Extract temperature values only from inputs whose SOURCE MODULE is `src` (keys are `module:id`).
fn input_temps_from(inputs: Option<&Inputs>, src: &str) -> Vec<i32> {
    let mut v = Vec::new();
    if let Some(inputs) = inputs {
        let prefix = format!("{src}:");
        for (key, signals) in inputs {
            if key.starts_with(&prefix) {
                push_temps(signals, &mut v);
            }
        }
    }
    v
}

/// Append the value of every `temperature` producer signal to `out`.
fn push_temps(signals: &[Signal], out: &mut Vec<i32>) {
    for s in signals {
        if s.kind() == Some("temperature") {
            if let Some(t) = s.value_i64() {
                out.push(t as i32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_signal(unit_kind: &str, value: i64) -> Signal {
        Signal::producer(
            format!("{unit_kind}:c:temp"),
            format!("{unit_kind}:c"),
            "temperature",
        )
        .value(json!(value))
        .uom("C")
    }

    #[test]
    fn fan_restore_stays_armed_until_release_succeeds() {
        assert!(still_armed_after(false));
        assert!(!still_armed_after(true));
    }

    #[test]
    fn input_temps_extracts_all_temps_source_agnostic() {
        let mut inputs: Inputs = HashMap::new();
        inputs.insert("nvidia:GPU-1".into(), vec![temp_signal("gpu", 63)]);
        inputs.insert("nvme:SER-A".into(), vec![temp_signal("ssd", 44)]);
        let mut temps = input_temps(Some(&inputs));
        temps.sort();
        assert_eq!(temps, vec![44, 63], "driving max sees every routed source");
    }

    #[test]
    fn input_temps_from_partitions_by_source_module() {
        let mut inputs: Inputs = HashMap::new();
        inputs.insert("nvidia:GPU-1".into(), vec![temp_signal("gpu", 63)]);
        inputs.insert("nvidia:GPU-2".into(), vec![temp_signal("gpu", 71)]);
        inputs.insert(
            "nvme:SER-A".into(),
            vec![temp_signal("ssd", 40), temp_signal("ssd", 44)],
        );

        let mut gpu = input_temps_from(Some(&inputs), "nvidia");
        gpu.sort();
        assert_eq!(gpu, vec![63, 71]);
        let mut nv = input_temps_from(Some(&inputs), "nvme");
        nv.sort();
        assert_eq!(nv, vec![40, 44]);
        // A short source name must not match a longer module (the `:` guards it); unknown -> empty.
        assert!(input_temps_from(Some(&inputs), "nv").is_empty());
        assert!(input_temps_from(Some(&inputs), "other").is_empty());
        assert!(input_temps_from(None, "nvidia").is_empty());
        assert!(input_temps(None).is_empty());
    }

    #[test]
    fn uniform_fan_drives_carry_one_curve_decision_on_every_fan() {
        let o = ApplyOutcome::uniform([60; 8], 70, 68);
        let d = fan_drives(&o, Some(64), Some(50), Some(45));
        for fd in &d {
            assert_eq!(fd.driving.output, Some(60.0));
            assert_eq!(fd.driving.raw, Some(70.0));
            assert_eq!(fd.driving.input, Some(68.0));
            assert_eq!(fd.driving.kind.as_deref(), Some("temperature"));
            // every fan is driven by all three routed sources.
            assert_eq!(fd.driven_by.len(), 3);
        }
    }

    #[test]
    fn zone_fan_drives_split_cpu_and_case() {
        let commanded = zones::per_fan_duties(30, 75);
        let o = ApplyOutcome::zone(commanded, 55, 72, 54, 70);
        let d = fan_drives(&o, Some(64), Some(50), Some(55));
        // FAN1/2 = cpu zone, driven by cpu only.
        assert_eq!(d[0].driving.output, Some(30.0));
        assert_eq!(d[0].driving.input, Some(54.0));
        assert_eq!(d[0].driving.how.as_deref(), Some("zone:cpu"));
        assert_eq!(d[0].driven_by.len(), 1);
        // FAN3 = case zone, driven by gpu+nvme.
        assert_eq!(d[2].driving.output, Some(75.0));
        assert_eq!(d[2].driving.input, Some(70.0));
        assert_eq!(d[2].driving.how.as_deref(), Some("zone:case"));
        assert_eq!(d[2].driven_by.len(), 2);
    }

    #[test]
    fn zone_fan_drives_reflect_a_case_fan_boost() {
        let base = zones::per_fan_duties(30, 60);
        let mut confirmed = [false; 8];
        confirmed[4] = true;
        let commanded = fault::compensate(base, &confirmed);
        let o = ApplyOutcome::zone(commanded, 50, 65, 50, 64);
        let d = fan_drives(&o, Some(60), Some(40), Some(50));
        assert_eq!(d[2].driving.output, Some(100.0), "case fans boosted");
        assert_eq!(d[0].driving.output, Some(30.0), "CPU zone unaffected");
    }

    #[test]
    fn every_claimed_board_sink_satisfies_the_driving_contract() {
        // The fan sinks aiolos commands must all carry a complete driving record (CI contract).
        let o = ApplyOutcome::uniform([55; 8], 66, 64);
        let drives = fan_drives(&o, Some(64), Some(50), Some(45));
        let rpms: Vec<(String, Option<i32>)> =
            (1..=8).map(|n| (format!("FAN{n}"), Some(1200))).collect();
        let (components, signals) = fan_components(&rpms, None, Some(&drives), &[false; 8]);
        let report = Report::ok(vec![board_unit()], components, signals);
        assert!(report.sink_contract_violations().is_empty());
    }
}
