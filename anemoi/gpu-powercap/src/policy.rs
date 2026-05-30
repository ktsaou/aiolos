//! The power-cap policy: configuration + the pure trigger decision.
//!
//! Deliberately **conservative by default** so the module never throttles a running job on a brief
//! utility blip. The default policy is *monitor + log* while on battery, and only actually caps the
//! GPUs once the UPS's estimated runtime falls below a low threshold (battery genuinely draining) —
//! the moment where shaving GPU power buys the most extra runtime. Capping merely for "on battery"
//! is opt-in (`cap_on_battery=true`). On AC restore (or when no trigger holds) the cap is lifted
//! back to the firmware default.
//!
//! Config `gpu-powercap.conf` (`key=value`, `#` comments) at `$AIOLOS_ETC_DIR/gpu-powercap.conf`
//! else `/opt/aiolos/etc/gpu-powercap.conf`. No secrets — only thresholds.

const DEFAULT_CONF_PATH: &str = "/opt/aiolos/etc/gpu-powercap.conf";
const CONF_FILENAME: &str = "gpu-powercap.conf";

/// Power-cap policy knobs (all overridable in `gpu-powercap.conf`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Policy {
    /// Cap target as a percentage of each GPU's firmware **default** limit (clamped to the device's
    /// min on apply). E.g. 70 -> cap to 70% of default. Must be 1..=100.
    pub cap_pct: u32,
    /// Cap as soon as any monitored UPS is on battery (regardless of runtime). Default `false`
    /// (conservative: a short outage on a healthy battery should not throttle a job).
    pub cap_on_battery: bool,
    /// Cap when an on-battery UPS's estimated runtime (seconds) is at or below this floor — battery
    /// genuinely draining. `0` disables the runtime trigger. Default 300 s (5 min).
    pub runtime_floor_s: i64,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            cap_pct: 70,
            cap_on_battery: false,
            runtime_floor_s: 300,
        }
    }
}

impl Policy {
    /// Resolve the config path: `$AIOLOS_ETC_DIR/gpu-powercap.conf` if set, else the install default.
    fn conf_path() -> String {
        match std::env::var("AIOLOS_ETC_DIR") {
            Ok(dir) => format!("{dir}/{CONF_FILENAME}"),
            Err(_) => DEFAULT_CONF_PATH.to_string(),
        }
    }

    /// Load the policy from operator config, falling back to defaults for any missing/invalid key.
    /// A missing file is normal (use defaults). Pure parsing is in `parse` (testable).
    pub fn load() -> Self {
        match std::fs::read_to_string(Self::conf_path()) {
            Ok(body) => Self::parse(&body),
            Err(_) => Policy::default(),
        }
    }

    /// Parse `key=value` lines onto the defaults. Unknown keys and unparseable values are ignored
    /// (the default stands), so a typo never disables the fail-safe. `cap_pct` is clamped to 1..=100.
    pub fn parse(body: &str) -> Self {
        let mut p = Policy::default();
        for line in body.lines() {
            let content = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = content.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "cap_pct" => {
                    if let Ok(n) = v.parse::<u32>() {
                        p.cap_pct = n.clamp(1, 100);
                    }
                }
                "cap_on_battery" => {
                    if let Ok(b) = parse_bool(v) {
                        p.cap_on_battery = b;
                    }
                }
                "runtime_floor_s" => {
                    if let Ok(n) = v.parse::<i64>() {
                        p.runtime_floor_s = n.max(0);
                    }
                }
                _ => {}
            }
        }
        p
    }

    /// The cap target in mW for a GPU whose firmware default limit is `default_mw`. (The device's
    /// accepted min is enforced separately by the tech crate's `set_power_limit` clamp.)
    pub fn cap_target_mw(&self, default_mw: u32) -> u32 {
        // u64 intermediate avoids overflow for large mW values; result fits u32 (cap_pct <= 100).
        ((default_mw as u64 * self.cap_pct as u64) / 100) as u32
    }
}

fn parse_bool(v: &str) -> Result<bool, ()> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(()),
    }
}

/// The aggregate power signal across ALL monitored UPS inputs for one tick. `on_battery` is true if
/// ANY UPS is on battery; `min_runtime_s` is the smallest reported runtime among on-battery UPSes
/// (the binding constraint); `low_battery` if ANY UPS raised LB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PowerSignal {
    pub on_battery: bool,
    pub low_battery: bool,
    pub min_runtime_s: Option<i64>,
}

/// What to do with the GPU power limit this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Hold the firmware default (AC present, or no trigger met) — lift any prior cap.
    Lift,
    /// Cap the limit (a trigger fired). The reason is carried for logging/readings.
    Cap(CapReason),
}

/// Why a cap fired (for observability).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapReason {
    /// On battery and `cap_on_battery` is enabled.
    OnBattery,
    /// On battery with estimated runtime at/below `runtime_floor_s`.
    LowRuntime,
    /// On battery and the UPS raised the LB (low-battery) flag.
    LowBatteryFlag,
}

impl CapReason {
    pub fn as_str(self) -> &'static str {
        match self {
            CapReason::OnBattery => "on-battery",
            CapReason::LowRuntime => "low-runtime",
            CapReason::LowBatteryFlag => "low-battery-flag",
        }
    }
}

/// The pure trigger decision. Only ever caps while ON BATTERY — on AC we always lift, so a misread
/// runtime on mains can never throttle a job. Order of precedence (most-urgent reason first) is for
/// the logged reason only; the action (cap) is the same.
pub fn decide(policy: &Policy, sig: &PowerSignal) -> Decision {
    if !sig.on_battery {
        return Decision::Lift; // AC present -> never capped
    }
    // On battery: the LB flag is the UPS's own "critically low" signal — always honour it.
    if sig.low_battery {
        return Decision::Cap(CapReason::LowBatteryFlag);
    }
    // Runtime trigger: enabled when the floor is > 0 and a runtime is known and at/below it.
    if policy.runtime_floor_s > 0 {
        if let Some(rt) = sig.min_runtime_s {
            if rt <= policy.runtime_floor_s {
                return Decision::Cap(CapReason::LowRuntime);
            }
        }
    }
    // Otherwise cap only if the operator opted into capping for any on-battery state.
    if policy.cap_on_battery {
        return Decision::Cap(CapReason::OnBattery);
    }
    Decision::Lift // conservative default: on battery but healthy -> monitor only
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_conservative() {
        let p = Policy::default();
        assert_eq!(p.cap_pct, 70);
        assert!(
            !p.cap_on_battery,
            "default must NOT cap merely for on-battery"
        );
        assert_eq!(p.runtime_floor_s, 300);
    }

    #[test]
    fn ac_present_always_lifts() {
        let p = Policy::default();
        // Even an absurd config can't cap on AC.
        let sig = PowerSignal {
            on_battery: false,
            low_battery: true,
            min_runtime_s: Some(1),
        };
        assert_eq!(decide(&p, &sig), Decision::Lift);
    }

    #[test]
    fn on_battery_healthy_is_monitor_only_by_default() {
        let p = Policy::default();
        let sig = PowerSignal {
            on_battery: true,
            low_battery: false,
            min_runtime_s: Some(3600),
        };
        assert_eq!(decide(&p, &sig), Decision::Lift);
    }

    #[test]
    fn low_runtime_triggers_a_cap() {
        let p = Policy::default(); // floor 300
        let sig = PowerSignal {
            on_battery: true,
            low_battery: false,
            min_runtime_s: Some(250),
        };
        assert_eq!(decide(&p, &sig), Decision::Cap(CapReason::LowRuntime));
        // Exactly at the floor also caps.
        let at = PowerSignal {
            min_runtime_s: Some(300),
            ..sig
        };
        assert_eq!(decide(&p, &at), Decision::Cap(CapReason::LowRuntime));
    }

    #[test]
    fn low_battery_flag_caps_even_with_unknown_runtime() {
        let p = Policy::default();
        let sig = PowerSignal {
            on_battery: true,
            low_battery: true,
            min_runtime_s: None,
        };
        assert_eq!(decide(&p, &sig), Decision::Cap(CapReason::LowBatteryFlag));
    }

    #[test]
    fn cap_on_battery_opt_in_caps_a_healthy_battery() {
        let p = Policy {
            cap_on_battery: true,
            ..Policy::default()
        };
        let sig = PowerSignal {
            on_battery: true,
            low_battery: false,
            min_runtime_s: Some(3600),
        };
        assert_eq!(decide(&p, &sig), Decision::Cap(CapReason::OnBattery));
    }

    #[test]
    fn runtime_floor_zero_disables_the_runtime_trigger() {
        let p = Policy {
            runtime_floor_s: 0,
            ..Policy::default()
        };
        let sig = PowerSignal {
            on_battery: true,
            low_battery: false,
            min_runtime_s: Some(1),
        };
        // No runtime trigger, cap_on_battery off -> monitor only.
        assert_eq!(decide(&p, &sig), Decision::Lift);
    }

    #[test]
    fn cap_target_is_a_percentage_of_default() {
        let p = Policy {
            cap_pct: 70,
            ..Policy::default()
        };
        assert_eq!(p.cap_target_mw(600_000), 420_000);
        // 100% is the default itself (no-op cap); large values don't overflow.
        let full = Policy {
            cap_pct: 100,
            ..Policy::default()
        };
        assert_eq!(full.cap_target_mw(600_000), 600_000);
    }

    #[test]
    fn parse_applies_known_keys_and_ignores_junk() {
        let body = "\
# comment
cap_pct = 80
cap_on_battery = yes
runtime_floor_s = 600
bogus = 5      # ignored
cap_pct = 250  # clamped to 100
";
        let p = Policy::parse(body);
        assert_eq!(p.cap_pct, 100, "out-of-range cap_pct clamps to 100");
        assert!(p.cap_on_battery);
        assert_eq!(p.runtime_floor_s, 600);
    }

    #[test]
    fn parse_missing_or_invalid_keeps_defaults() {
        // Empty -> defaults; an unparseable value leaves the default standing (never disarms).
        assert_eq!(Policy::parse(""), Policy::default());
        let p = Policy::parse("cap_pct = abc\nruntime_floor_s = -5\n");
        assert_eq!(p.cap_pct, 70, "invalid value keeps default");
        assert_eq!(
            p.runtime_floor_s, 0,
            "negative floor clamps to 0 (disabled)"
        );
    }
}
