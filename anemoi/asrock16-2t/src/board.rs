//! ASRockRack ROME2D16-2T OEM fan control (netfn 0x3a), built on the generic `ipmi` transport:
//! claim all-manual (0xd8 ×16 0x01), set duty (0xd6 ×16 bytes, non-zero), release to BMC auto
//! (0xd8 ×16 0x00), query duty (0xda). This is the board-specific layer; the generic IPMI transport
//! lives in the `ipmi` crate.

use anyhow::{bail, Result};
use ipmi::{Ipmi, SensorFactors, SensorReading};
use std::time::Duration;

const NETFN_OEM: u8 = 0x3a;
const CMD_FAN_MODE: u8 = 0xd8; // claim (0x01×16) / release (0x00×16)
const CMD_SET_DUTY: u8 = 0xd6;
const CMD_QUERY_DUTY: u8 = 0xda;

/// Short per-call timeout for the read-only observability reads (RPM tachs + duty readback). Keeps
/// the whole observability batch well under the orchestrator's apply deadline even on a slow BMC: a
/// laggy sensor just degrades to "no reading" rather than dominating the tick. Control commands
/// (claim/set/release) keep the full default IPMI timeout — they must be reliable, not fast.
/// The observability batch is bounded by construction to ≤ 9 IPMI calls per tick (≤ 1 per fan +
/// the duty readback — see `read_fan_rpms`), so at 100 ms each the worst case is ~0.9 s, well under
/// the default 2 s apply deadline, while a healthy BMC (single-digit-ms reads) keeps ~10–20× headroom.
const OBS_TIMEOUT: Duration = Duration::from_millis(100);

/// Tachometer sensor numbers for FAN1_1..FAN8_1 (Entity 29, Fan Device) — verified on this board.
/// FAN1/FAN2 are the Noctua CPU coolers (low RPM), FAN3–FAN8 the 120 mm case fans. The `FAN*_2`
/// (`0x68..0x6F`) and `FAN_PSU*` (`0x70/0x71`) sensors report "No Reading" here and are not read.
const FAN_SENSORS: [u8; 8] = [0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67];

/// Observability snapshot: the per-fan duty readback (`0xda`, or `None` if it failed) and each
/// fan's `(label, RPM-or-None)`.
type FanStatus = (Option<Vec<u8>>, Vec<(String, Option<i32>)>);

/// The board's fan controller: an IPMI handle, whether we currently hold manual control (so we
/// claim ONCE, not every tick), and the per-fan tach conversion factors. Each fan's factors are
/// fetched lazily and cached once known (they are constant for these linear sensors); a fan still
/// `None` is retried on later ticks, so a transient BMC hiccup on the first read is not permanent.
pub struct Board {
    ipmi: Ipmi,
    claimed: bool,
    fan_factors: [Option<SensorFactors>; 8],
}

impl Board {
    pub fn open() -> Result<Self> {
        Ok(Board {
            ipmi: Ipmi::open()?,
            claimed: false,
            fan_factors: [None; 8],
        })
    }

    /// Claim all 16 fans to manual control.
    pub fn claim_manual(&mut self) -> Result<()> {
        self.ipmi.raw(NETFN_OEM, CMD_FAN_MODE, &claim_payload())?;
        self.claimed = true;
        Ok(())
    }

    /// Release all fans back to BMC auto control (the fail-safe). Clears `claimed` regardless of the
    /// result: after a release attempt we no longer intend to hold manual control, so the next
    /// control cycle must re-claim rather than trust a stale `claimed=true`. Returns the BMC error.
    pub fn release_auto(&mut self) -> Result<()> {
        let r = self.ipmi.raw(NETFN_OEM, CMD_FAN_MODE, &release_payload());
        self.claimed = false;
        r.map(|_| ())
    }

    /// Claim (if needed) and set every fan's duty to `pct` (uniform). On a persistent set failure
    /// this RELEASES to BMC auto rather than leaving the board claimed-manual without a fresh duty.
    pub fn set_all_fans(&mut self, pct: i32) -> Result<()> {
        regulate(self, &[pct; 8])
    }

    /// Claim (if needed) and set the 8 fans to per-fan duties `pcts` (FAN1..FAN8); same
    /// claim/retry/release safety as `set_all_fans`. Used by the per-zone (SOW-0010) path.
    pub fn set_fans_per_fan(&mut self, pcts: &[i32; 8]) -> Result<()> {
        regulate(self, pcts)
    }

    fn write_duty(&mut self, pcts: &[i32; 8]) -> Result<()> {
        self.ipmi
            .raw(NETFN_OEM, CMD_SET_DUTY, &duty_payload(pcts))
            .map(|_| ())
    }

    /// Query the current per-fan duty (0xda). Returns the raw bytes (percent each). Read-only.
    pub fn query_duty(&mut self) -> Result<Vec<u8>> {
        self.ipmi.raw(NETFN_OEM, CMD_QUERY_DUTY, &[])
    }

    /// Best-effort: fetch and cache every fan's conversion factors once, at instance bind, so the
    /// first `apply` tick is no heavier than the rest (8 factor reads happen here, off the tick's
    /// deadline). Failures are left `None` and retried by `read_fan_rpms`. Short-timeout, read-only.
    pub fn prefetch_fan_factors(&mut self) {
        for (i, &sensor) in FAN_SENSORS.iter().enumerate() {
            if self.fan_factors[i].is_none() {
                self.fan_factors[i] = self.ipmi.read_sensor_factors(sensor, 0, OBS_TIMEOUT).ok();
            }
        }
    }

    /// Read-only observability snapshot under the short `OBS_TIMEOUT`: the live per-fan duty (the
    /// `0xda` readback, or `None` if it failed) and each fan's tachometer RPM. Never touches fan
    /// control; a slow/unreadable sensor degrades to `None`, never blowing the caller's tick.
    pub fn read_fan_status(&mut self) -> FanStatus {
        let duty = self
            .ipmi
            .raw_with_timeout(NETFN_OEM, CMD_QUERY_DUTY, &[], OBS_TIMEOUT)
            .ok();
        (duty, self.read_fan_rpms())
    }

    /// Each fan's tachometer RPM via standard IPMI sensor reads (`FAN1..FAN8`). At most **one** IPMI
    /// call per fan per tick: if a fan's constant conversion factors aren't cached yet (prefetch
    /// failed), fetch them this tick and report no RPM until the next one — so even a cold cache
    /// never does factor+sensor in the same tick (the whole observability batch stays ≤ 9 calls).
    /// A fan whose sensor is unavailable or errors yields `None`. Short-timeout, read-only.
    fn read_fan_rpms(&mut self) -> Vec<(String, Option<i32>)> {
        FAN_SENSORS
            .iter()
            .enumerate()
            .map(|(i, &sensor)| {
                let rpm = match self.fan_factors[i] {
                    Some(f) => fan_rpm(Some(f), self.ipmi.read_sensor(sensor, OBS_TIMEOUT).ok()),
                    None => {
                        // Factors unknown -> fetch them now (one call); RPM comes next tick. The
                        // reading byte (0) is immaterial for a linear sensor.
                        self.fan_factors[i] =
                            self.ipmi.read_sensor_factors(sensor, 0, OBS_TIMEOUT).ok();
                        None
                    }
                };
                (format!("FAN{}", i + 1), rpm)
            })
            .collect()
    }
}

/// Convert a tach sensor reading to RPM, or `None` when the factors are missing, the reading
/// failed, the sensor reports its value as unavailable, or the conversion is negative (which would
/// indicate bad factors / a firmware bug — omit it rather than report a nonsensical RPM). Pure
/// (unit-testable without hardware).
fn fan_rpm(factors: Option<SensorFactors>, reading: Option<SensorReading>) -> Option<i32> {
    let (f, r) = (factors?, reading?);
    if !r.available {
        return None;
    }
    let rpm = f.convert(r.raw).round();
    if rpm < 0.0 {
        return None;
    }
    Some(rpm as i32)
}

// ---- fan-control policy (trait-based so it is unit-testable without hardware) ----

/// Minimal fan-bus operations the regulation policy needs. `set_duty` takes the 8 per-fan duties
/// (FAN1..FAN8); a uniform set just passes the same value eight times.
pub trait FanBus {
    fn is_claimed(&self) -> bool;
    fn claim(&mut self) -> Result<()>;
    fn set_duty(&mut self, pcts: &[i32; 8]) -> Result<()>;
    fn release(&mut self) -> Result<()>;
}

impl FanBus for Board {
    fn is_claimed(&self) -> bool {
        self.claimed
    }
    fn claim(&mut self) -> Result<()> {
        self.claim_manual()
    }
    fn set_duty(&mut self, pcts: &[i32; 8]) -> Result<()> {
        self.write_duty(pcts)
    }
    fn release(&mut self) -> Result<()> {
        self.release_auto()
    }
}

/// Claim-if-needed → set the 8 per-fan duties → re-claim+retry once on failure; if a duty still
/// can't be set, **release to BMC auto** so the board is never left claimed-manual without a fresh
/// duty. The duties are the 8 fan percentages (FAN1..FAN8); a uniform set passes `[pct; 8]`.
pub fn regulate<B: FanBus>(bus: &mut B, pcts: &[i32; 8]) -> Result<()> {
    if !bus.is_claimed() {
        bus.claim()?; // claim failed -> nothing claimed -> safe to propagate
    }
    if let Err(first) = bus.set_duty(pcts) {
        if let Err(reclaim) = bus.claim() {
            let _ = bus.release(); // can't regulate -> hand back to BMC auto
            bail!("set failed ({first}); re-claim failed ({reclaim}); released to BMC auto");
        }
        if let Err(second) = bus.set_duty(pcts) {
            let _ = bus.release(); // still can't set a duty -> never hold manual frozen
            bail!("set failed twice ({first}; {second}); released to BMC auto");
        }
    }
    Ok(())
}

// ---- pure payload builders (unit-testable without the device) --------------

fn claim_payload() -> [u8; 16] {
    [0x01; 16]
}

fn release_payload() -> [u8; 16] {
    [0x00; 16]
}

/// 16 duty bytes for the 8 fans. The ROME2D16-2T requires each fan's duty in BOTH halves: bytes 0–7
/// = FAN1..FAN8 and bytes 8–15 MIRROR them. A non-mirrored (or low) tail byte → `0xcc invalid data
/// field` — hardware-verified on this board: an `0x01`/`0x0a` tail is rejected, an equal-to-head tail
/// is accepted. Each `pcts[i]` is clamped to 1..=100 (a zero byte also trips the 0xcc trap).
fn duty_payload(pcts: &[i32; 8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, &pct) in pcts.iter().enumerate() {
        let v = pct.clamp(0, 100).max(1) as u8;
        out[i] = v;
        out[i + 8] = v;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_are_correct() {
        assert_eq!(claim_payload(), [0x01u8; 16]);
        assert_eq!(release_payload(), [0x00u8; 16]);
    }

    #[test]
    fn duty_is_never_zero() {
        // Uniform: BOTH halves mirror the duty; 0 -> 1 so no byte is ever zero (0xcc trap).
        assert_eq!(duty_payload(&[0; 8]), [1u8; 16]); // 0% -> 1 (avoid 0xcc trap)
        assert_eq!(duty_payload(&[-5; 8]), [1u8; 16]);
        assert_eq!(duty_payload(&[50; 8]), [50u8; 16]);
        assert_eq!(duty_payload(&[100; 8]), [100u8; 16]); // 0x64
        assert_eq!(duty_payload(&[150; 8]), [100u8; 16]); // clamp high
    }

    #[test]
    fn duty_payload_per_fan_mirrors_each_fan_into_both_halves() {
        // Per-zone duties: bytes 0-7 = FAN1..FAN8 (clamped 1..=100); bytes 8-15 MIRROR them (the board
        // rejects a non-mirrored tail with 0xcc).
        let p = duty_payload(&[30, 30, 75, 75, 75, 75, 75, 75]);
        assert_eq!(&p[0..8], &[30, 30, 75, 75, 75, 75, 75, 75]);
        assert_eq!(&p[8..16], &[30, 30, 75, 75, 75, 75, 75, 75], "tail must mirror the head");
        // A zero / negative per-fan duty is still floored to 1 (never a zero byte), in both halves.
        let z = duty_payload(&[0, -1, 100, 150, 1, 50, 50, 50]);
        assert_eq!(&z[0..8], &[1, 1, 100, 100, 1, 50, 50, 50]);
        assert_eq!(&z[8..16], &[1, 1, 100, 100, 1, 50, 50, 50]);
    }

    /// A mock fan bus for testing the `regulate` policy without /dev/ipmi0.
    struct MockBus {
        claimed: bool,
        fail_set: bool,
        claims: u32,
        sets: u32,
        releases: u32,
    }
    impl MockBus {
        fn new(fail_set: bool) -> Self {
            MockBus {
                claimed: false,
                fail_set,
                claims: 0,
                sets: 0,
                releases: 0,
            }
        }
    }
    impl FanBus for MockBus {
        fn is_claimed(&self) -> bool {
            self.claimed
        }
        fn claim(&mut self) -> Result<()> {
            self.claims += 1;
            self.claimed = true;
            Ok(())
        }
        fn set_duty(&mut self, _pcts: &[i32; 8]) -> Result<()> {
            self.sets += 1;
            if self.fail_set {
                bail!("mock set failure");
            }
            Ok(())
        }
        fn release(&mut self) -> Result<()> {
            self.releases += 1;
            self.claimed = false;
            Ok(())
        }
    }

    #[test]
    fn regulate_ok_claims_then_sets_without_release() {
        let mut b = MockBus::new(false);
        assert!(regulate(&mut b, &[50; 8]).is_ok());
        assert!(b.claimed && b.claims == 1 && b.sets == 1 && b.releases == 0);
    }

    #[test]
    fn regulate_releases_to_auto_when_duty_cannot_be_set() {
        // R2: a persistent set failure must NOT leave the board claimed-manual — release to BMC auto.
        let mut b = MockBus::new(true);
        assert!(regulate(&mut b, &[50; 8]).is_err());
        assert!(
            b.releases >= 1,
            "must release to BMC auto when it cannot set a duty"
        );
        assert!(
            !b.claimed,
            "must never be left claimed-manual without a fresh duty"
        );
    }

    #[test]
    fn fan_rpm_converts_only_a_valid_reading() {
        // Verified board factors: M=100, B=0, exps=0 -> RPM = raw*100.
        let f = SensorFactors {
            m: 100,
            b: 0,
            b_exp: 0,
            r_exp: 0,
        };
        let avail = SensorReading {
            raw: 6,
            available: true,
        };
        assert_eq!(fan_rpm(Some(f), Some(avail)), Some(600));

        // Edge raw values still convert linearly.
        assert_eq!(
            fan_rpm(
                Some(f),
                Some(SensorReading {
                    raw: 0,
                    available: true
                })
            ),
            Some(0)
        );
        assert_eq!(
            fan_rpm(
                Some(f),
                Some(SensorReading {
                    raw: 255,
                    available: true
                })
            ),
            Some(25500)
        );

        // Unavailable / missing reading / missing factors -> None (never a wrong RPM).
        let unavail = SensorReading {
            raw: 6,
            available: false,
        };
        assert_eq!(fan_rpm(Some(f), Some(unavail)), None);
        assert_eq!(fan_rpm(Some(f), None), None);
        assert_eq!(fan_rpm(None, Some(avail)), None);

        // A negative conversion (bad factors) is omitted, not reported as a nonsensical RPM.
        let neg = SensorFactors {
            m: -100,
            b: 0,
            b_exp: 0,
            r_exp: 0,
        };
        assert_eq!(
            fan_rpm(
                Some(neg),
                Some(SensorReading {
                    raw: 5,
                    available: true
                })
            ),
            None
        );
    }
}
