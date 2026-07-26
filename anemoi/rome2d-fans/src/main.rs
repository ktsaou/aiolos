//! rome2d-fans anemos — ASRockRack ROME2D16-2T board fan control via inband IPMI (label-driven
//! signal model, SOW-0018).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI/signals/logging/curve+EMA/
//! protocol/restore); `ipmi` is the IPMI transport; `board` is the board's OEM fan commands; `hwmon`
//! reads CPU temps.
//!
//! It reports the **motherboard unit** (`id` = `board`, shared with `ipmi-temps` so the BMC temps and
//! these fans merge into one unit): `fan1..fan8` components (an `rpm` producer + a `duty` sink that
//! carries its own per-zone `driving` record), and per-socket `cpu1`/`cpu2` components from k10temp
//! (read per instance, so they merge with `ipmi-temps`' CPU1/CPU2 package sensors).
//!
//! The control path reads routed/local temperatures, computes the established baseline plus optional
//! source-matched case overlays, drives all 8 fans, detects faults, and releases to BMC auto on exit.

mod board;
mod fault;
mod zones;

use anemos::{
    Anemos, Component, Control, Controller, Device, Driving, ExtraCmd, Inputs, ModuleInfo,
    OpenMode, PolicyOutcome, Provenance, Report, Role, Signal, SignalCurvePolicy, SinkState, Unit,
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
            case_policy: None,
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
    case_policy: Option<SignalCurvePolicy>,
    faults: FanFaultTracker,
}

impl Device for Rome2dFansDevice {
    fn collect(&mut self, _inputs: Option<&Inputs>) -> Report {
        // Read-only snapshot for `rome2d-fans info`: local CPU sensors plus BMC duty/RPM readbacks.
        // Never claims, sets, or releases the board, so it is safe while another controller owns it.
        let (mut components, mut signals) = cpu_components();

        let (duty_readback, fan_rpms) = self.board.read_fan_status();
        let (mut fc, mut fs) =
            fan_components(&fan_rpms, duty_readback.as_deref(), None, &[false; 8]);
        components.append(&mut fc);
        signals.append(&mut fs);
        Report::ok(vec![board_unit()], components, signals)
    }

    fn apply(&mut self, inputs: Option<&Inputs>, ctrl: &mut Controller) -> Report {
        // --- control path -----------------------------------------------------------------------
        let routed_temp_maxes = input_temp_maxes_by_module(inputs);
        let cpu_temps = hwmon::read_temps("k10temp");
        let (cpu_components_snapshot, local_cpu_signals) = cpu_components();
        let cpu_max = cpu_temps.iter().map(|(_, value)| *value).max();
        let input_max = input_temps(inputs).into_iter().max();
        let raw_driving = [input_max, cpu_max].into_iter().flatten().max();
        let zones = self
            .zones
            .get_or_insert_with(|| ZoneControllers::for_main_path(ctrl.path()));
        let zone_mode = zones.both_present();
        let confirmed = self.faults.confirmed();

        let (baseline, base) = if zone_mode {
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
            tracing::info!(
                cpu_raw,
                case_raw,
                cpu_smoothed = cpu_duty.smoothed,
                case_smoothed = case_duty.smoothed,
                cpu_pct,
                case_pct,
                "baseline decision: board fan zones"
            );
            (
                BaselineOutcome::Zone {
                    cpu_raw,
                    case_raw,
                    cpu_smoothed: cpu_duty.smoothed,
                    case_smoothed: case_duty.smoothed,
                    case_pct,
                },
                zones::per_fan_duties(cpu_pct, case_pct),
            )
        } else {
            let Some(raw) = raw_driving else {
                return release_or_error(&mut self.board, "indeterminable temp");
            };
            let duty = ctrl.duty(raw);
            let Some(pct) = duty.pct else {
                return release_or_error(&mut self.board, "no usable curve");
            };
            tracing::info!(routed_temp_maxes = ?routed_temp_maxes, cpu_max = ?cpu_max,
                raw_driving = raw, smoothed_driving = duty.smoothed,
                commanded_pct = pct, "baseline decision: all board fans (uniform)");
            (
                BaselineOutcome::Uniform {
                    raw,
                    smoothed: duty.smoothed,
                    pct,
                },
                [pct; 8],
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

        let requested = apply_case_overlay(base, case_policy.overlay_pct());
        let commanded = fault::compensate(requested, &confirmed);
        tracing::info!(
            baseline_case_pct = baseline.case_pct(),
            overlay_pct = ?case_policy.overlay_pct(),
            ?commanded,
            ?confirmed,
            "decision: set board fans (baseline plus case overlay)"
        );
        let set = if commanded.iter().all(|pct| *pct == commanded[0]) {
            self.board.set_all_fans(commanded[0] as i32)
        } else {
            self.board
                .set_fans_per_fan(&commanded.map(|pct| pct as i32))
        };
        if let Err(error) = set {
            return Report::error(format!("set fans: {error}"));
        }
        let outcome = ApplyOutcome {
            commanded,
            baseline,
            case_overlay: case_policy,
        };
        // --- end control path -------------------------------------------------------------------

        // Report stage: REAL CPU temps (per socket) + the 8 fans, each carrying its own per-zone
        // `driving` record. No driving-* producer signals — driving lives on the sinks.
        let (mut components, mut signals) = (cpu_components_snapshot, local_cpu_signals);

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
        let drives = fan_drives(&outcome, &routed_temp_maxes, cpu_max);
        let (mut fc, mut fs) = fan_components(
            &fan_rpms,
            duty_readback.as_deref(),
            Some(&drives),
            &now_faulted,
        );
        components.append(&mut fc);
        signals.append(&mut fs);
        match outcome.warning() {
            Some(warning) => Report::ok_warn(vec![board_unit()], components, signals, warning),
            None => Report::ok(vec![board_unit()], components, signals),
        }
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

/// Build per-socket `cpu0`/`cpu1` components from k10temp, read PER INSTANCE so a dual-socket board's
/// two k10temp chips (which report identical Tctl/Tccd labels) land in DISTINCT components with unique
/// signal ids — and so they merge with `ipmi-temps`'s `cpu0`/`cpu1` (CPU1/CPU2) instead of forming an
/// awkward second "cpu" group with duplicate ids.
fn cpu_components() -> (Vec<Component>, Vec<Signal>) {
    let mut chips = hwmon::read_chip_temps(&["k10temp".to_string()]);
    chips.sort_by(|a, b| a.instance.cmp(&b.instance)); // stable socket order
    let (mut components, mut signals) = (Vec::new(), Vec::new());
    for (sock, chip) in chips.iter().enumerate() {
        if chip.temps.is_empty() {
            continue;
        }
        // 1-indexed to match the BMC's CPU1/CPU2 (k10temp instance 0 = socket 0 = CPU1), so the
        // k10temp cores merge into the same `cpu1`/`cpu2` group as `ipmi-temps`' package sensor.
        let n = sock + 1;
        let cid = format!("{BOARD_ID}:cpu{n}");
        components.push(
            Component::new(&cid, BOARD_ID)
                .name(format!("cpu{n}"))
                .typed("cpu"),
        );
        for (label, t) in &chip.temps {
            signals.push(
                Signal::producer(format!("{cid}:{}", slug(label)), &cid, "temperature")
                    .value(json!(t))
                    .uom("C")
                    .name(label.clone()),
            );
        }
    }
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
    routed_temp_maxes: &[SourceTempMax],
    cpu_max: Option<i32>,
) -> [FanDrive; 8] {
    let cpu = prov("cpu (max)", cpu_max);
    let routed: Vec<Provenance> = routed_temp_maxes
        .iter()
        .map(|source| {
            Provenance::new(format!("{} (max)", source.module))
                .value(json!(source.value))
                .uom("C")
        })
        .collect();
    std::array::from_fn(|i| {
        let baseline = match &outcome.baseline {
            BaselineOutcome::Uniform { raw, smoothed, .. } => FanDrive {
                driven_by: routed.iter().cloned().chain(cpu.clone()).collect(),
                driving: Driving::new()
                    .kind("temperature")
                    .raw(*raw as f64)
                    .input(*smoothed as f64)
                    .uom("C")
                    .output(outcome.commanded[i] as f64)
                    .how("uniform: max(routed,cpu)→curve"),
            },
            BaselineOutcome::Zone {
                cpu_raw,
                cpu_smoothed,
                ..
            } if i < 2 => FanDrive {
                driven_by: [cpu.clone()].into_iter().flatten().collect(),
                driving: Driving::new()
                    .kind("temperature")
                    .raw(*cpu_raw as f64)
                    .input(*cpu_smoothed as f64)
                    .uom("C")
                    .output(outcome.commanded[i] as f64)
                    .how("zone:cpu"),
            },
            BaselineOutcome::Zone {
                case_raw,
                case_smoothed,
                ..
            } => FanDrive {
                driven_by: routed.clone(),
                driving: Driving::new()
                    .kind("temperature")
                    .raw(*case_raw as f64)
                    .input(*case_smoothed as f64)
                    .uom("C")
                    .output(outcome.commanded[i] as f64)
                    .how("zone:case"),
            },
        };

        if i >= 2
            && outcome
                .case_overlay
                .overlay_pct()
                .is_some_and(|overlay| overlay >= outcome.baseline.case_pct())
        {
            if let Some(mut driving) = outcome.case_overlay.driving() {
                driving.output = Some(outcome.commanded[i] as f64);
                return FanDrive {
                    driven_by: outcome.case_overlay.driven_by(),
                    driving,
                };
            }
        }
        baseline
    })
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

/// Existing fan calculation before optional case-only overlays.
enum BaselineOutcome {
    Uniform {
        raw: i32,
        smoothed: i32,
        pct: u32,
    },
    Zone {
        cpu_raw: i32,
        case_raw: i32,
        cpu_smoothed: i32,
        case_smoothed: i32,
        case_pct: u32,
    },
}

impl BaselineOutcome {
    fn case_pct(&self) -> u32 {
        match self {
            BaselineOutcome::Uniform { pct, .. } => *pct,
            BaselineOutcome::Zone { case_pct, .. } => *case_pct,
        }
    }
}

/// What the apply tick decided, carried to the report stage.
struct ApplyOutcome {
    commanded: [u32; 8],
    baseline: BaselineOutcome,
    case_overlay: PolicyOutcome,
}

impl ApplyOutcome {
    #[cfg(test)]
    fn uniform(commanded: [u32; 8], raw: i32, smoothed: i32) -> Self {
        ApplyOutcome {
            commanded,
            baseline: BaselineOutcome::Uniform {
                raw,
                smoothed,
                pct: commanded[0],
            },
            case_overlay: PolicyOutcome::Inactive,
        }
    }
    #[cfg(test)]
    fn zone(
        commanded: [u32; 8],
        cpu_raw: i32,
        case_raw: i32,
        cpu_smoothed: i32,
        case_smoothed: i32,
    ) -> Self {
        ApplyOutcome {
            commanded,
            baseline: BaselineOutcome::Zone {
                cpu_raw,
                case_raw,
                cpu_smoothed,
                case_smoothed,
                case_pct: commanded[2],
            },
            case_overlay: PolicyOutcome::Inactive,
        }
    }
    fn commanded(&self) -> [u32; 8] {
        self.commanded
    }
    fn warning(&self) -> Option<&str> {
        self.case_overlay.warning()
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

fn apply_case_overlay(mut baseline: [u32; 8], overlay_pct: Option<u32>) -> [u32; 8] {
    if let Some(overlay_pct) = overlay_pct {
        for pct in &mut baseline[2..] {
            *pct = (*pct).max(overlay_pct);
        }
    }
    baseline
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

fn case_policy_path(main_curve_path: &str) -> String {
    match main_curve_path.strip_suffix(".curve.json") {
        Some(stem) => format!("{stem}.case.policy.json"),
        None => format!("{main_curve_path}.case.policy.json"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceTempMax {
    module: String,
    value: i32,
}

/// Extract every temperature value from all routed peer inputs (legacy control reduction).
fn input_temps(inputs: Option<&Inputs>) -> Vec<i32> {
    let mut values = Vec::new();
    if let Some(inputs) = inputs {
        for signals in inputs.values() {
            push_temps(signals, &mut values);
        }
    }
    values
}

fn push_temps(signals: &[Signal], out: &mut Vec<i32>) {
    for signal in signals {
        if signal.kind() == Some("temperature") {
            if let Some(value) = signal.value_i64() {
                out.push(value as i32);
            }
        }
    }
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
        for signal in signals {
            if signal.role != Role::Producer || signal.kind() != Some("temperature") {
                continue;
            }
            let Some(value) = signal.value_i64() else {
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
    fn input_temp_maxes_extract_all_sources_and_ignore_non_temperature_sinks() {
        let mut inputs: Inputs = HashMap::new();
        inputs.insert("nvidia:GPU-2".into(), vec![temp_signal("gpu", 71)]);
        inputs.insert(
            "nvme:SER-A".into(),
            vec![
                temp_signal("ssd", 40),
                temp_signal("ssd", 44),
                Signal::sink("board:fan3:duty", "board:fan3", "fan-duty").value(json!(100)),
                temp_signal("ssd", i64::from(i32::MAX) + 1),
            ],
        );

        assert_eq!(
            input_temp_maxes_by_module(Some(&inputs)),
            vec![
                SourceTempMax {
                    module: "nvidia".into(),
                    value: 71,
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
    fn uniform_fan_drives_carry_one_curve_decision_on_every_fan() {
        let o = ApplyOutcome::uniform([60; 8], 70, 68);
        let sources = vec![
            SourceTempMax {
                module: "nvidia".into(),
                value: 64,
            },
            SourceTempMax {
                module: "nvme".into(),
                value: 50,
            },
        ];
        let d = fan_drives(&o, &sources, Some(45));
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
        let sources = vec![
            SourceTempMax {
                module: "nvidia".into(),
                value: 64,
            },
            SourceTempMax {
                module: "nvme".into(),
                value: 50,
            },
        ];
        let d = fan_drives(&o, &sources, Some(55));
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
        let sources = vec![
            SourceTempMax {
                module: "nvidia".into(),
                value: 60,
            },
            SourceTempMax {
                module: "nvme".into(),
                value: 40,
            },
        ];
        let d = fan_drives(&o, &sources, Some(50));
        assert_eq!(d[2].driving.output, Some(100.0), "case fans boosted");
        assert_eq!(d[0].driving.output, Some(30.0), "CPU zone unaffected");
    }

    #[test]
    fn every_claimed_board_sink_satisfies_the_driving_contract() {
        // The fan sinks aiolos commands must all carry a complete driving record (CI contract).
        let o = ApplyOutcome::uniform([55; 8], 66, 64);
        let sources = vec![
            SourceTempMax {
                module: "nvidia".into(),
                value: 64,
            },
            SourceTempMax {
                module: "nvme".into(),
                value: 50,
            },
        ];
        let drives = fan_drives(&o, &sources, Some(45));
        let rpms: Vec<(String, Option<i32>)> =
            (1..=8).map(|n| (format!("FAN{n}"), Some(1200))).collect();
        let (components, signals) = fan_components(&rpms, None, Some(&drives), &[false; 8]);
        let report = Report::ok(vec![board_unit()], components, signals);
        assert!(report.sink_contract_violations().is_empty());
    }

    #[test]
    fn policy_fail_high_changes_case_fans_but_keeps_cpu_fans_on_baseline() {
        let outcome = ApplyOutcome {
            commanded: [45, 45, 100, 100, 100, 100, 100, 100],
            baseline: BaselineOutcome::Zone {
                cpu_raw: 68,
                case_raw: 60,
                cpu_smoothed: 66,
                case_smoothed: 58,
                case_pct: 40,
            },
            case_overlay: PolicyOutcome::FailHigh {
                warning: "required nvme telemetry missing".into(),
            },
        };
        let drives = fan_drives(&outcome, &[], Some(68));
        assert_eq!(drives[0].driving.output, Some(45.0));
        assert_eq!(drives[0].driving.how.as_deref(), Some("zone:cpu"));
        assert_eq!(drives[2].driving.output, Some(100.0));
        assert_eq!(
            drives[2].driving.how.as_deref(),
            Some("case-policy:fail-high")
        );
        assert_eq!(drives[2].driven_by.len(), 1);
        assert_eq!(outcome.warning(), Some("required nvme telemetry missing"));

        let rpms: Vec<(String, Option<i32>)> =
            (1..=8).map(|n| (format!("FAN{n}"), Some(1200))).collect();
        let (components, signals) = fan_components(&rpms, None, Some(&drives), &[false; 8]);
        assert!(Report::ok(vec![board_unit()], components, signals)
            .sink_contract_violations()
            .is_empty());
    }

    #[test]
    fn case_overlay_only_raises_all_six_case_fans() {
        assert_eq!(
            apply_case_overlay([40, 45, 30, 50, 65, 70, 80, 90], Some(65)),
            [40, 45, 65, 65, 65, 70, 80, 90]
        );
        assert_eq!(
            apply_case_overlay([40, 45, 70, 70, 70, 70, 70, 70], None),
            [40, 45, 70, 70, 70, 70, 70, 70]
        );
    }

    #[test]
    fn case_policy_path_tracks_the_resolved_main_curve_directory() {
        assert_eq!(
            case_policy_path("/tmp/etc/rome2d-fans.curve.json"),
            "/tmp/etc/rome2d-fans.case.policy.json"
        );
    }
}
