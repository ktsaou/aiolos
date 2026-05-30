//! ASRockRack ROME2D16-2T BMC analog temperature sensors over inband IPMI, built on the generic
//! `ipmi` transport: standard `Get Sensor Reading` (0x04/0x2d) + `Get Sensor Reading Factors`
//! (0x04/0x23) — the SAME mechanism the asrock fan module uses for tach RPM, here applied to the
//! board/CPU/DIMM/NIC temperature sensors. This is the board-specific layer (hardcoded sensor table,
//! like `board.rs::FAN_SENSORS`); the generic IPMI transport lives in the `ipmi` crate.
//!
//! Sensor-only: this reads and reports; it controls nothing.

use anyhow::Result;
use ipmi::{Ipmi, SensorFactors, SensorReading};
use std::time::Duration;

/// Short per-call timeout for these read-only observability reads — identical rationale to the
/// asrock fan module's `OBS_TIMEOUT`: a slow/unresponsive BMC degrades a sensor to "absent" rather
/// than blowing the orchestrator's apply deadline. With ≤ 1 IPMI call per sensor per tick, the
/// worst case over the full table stays comfortably under the default apply deadline, while a
/// healthy BMC (single-digit-ms reads) keeps wide headroom.
const OBS_TIMEOUT: Duration = Duration::from_millis(100);

/// One BMC analog temperature sensor: its stable IPMI sensor number and a stable human label.
struct TempSensor {
    /// IPMI sensor number (`Get Sensor Reading` argument).
    num: u8,
    /// Stable label reported as the reading's `label` (and consumed by routing).
    label: &'static str,
}

/// This board's analog temperature sensors (verified numbers from the host inspection / activation
/// brief). Hardcoded because the module is board-specific by name — exactly like
/// `board.rs::FAN_SENSORS`. Each is read every tick; only those whose reading is `available` are
/// reported (unpopulated DIMM slots and absent sensors report "ns"/unavailable and are skipped —
/// never emitted as a bogus 0). Note `0x2D` is intentionally absent (CARD_SIDE1 is 0x2C, LAN is
/// 0x2E).
const TEMP_SENSORS: &[TempSensor] = &[
    TempSensor {
        num: 0x28,
        label: "CPU1",
    },
    TempSensor {
        num: 0x29,
        label: "CPU2",
    },
    TempSensor {
        num: 0x2a,
        label: "MB1",
    },
    TempSensor {
        num: 0x2b,
        label: "MB2",
    },
    TempSensor {
        num: 0x2c,
        label: "CARD_SIDE",
    },
    TempSensor {
        num: 0x2e,
        label: "LAN",
    },
    TempSensor {
        num: 0x48,
        label: "DDR4_A",
    },
    TempSensor {
        num: 0x49,
        label: "DDR4_B",
    },
    TempSensor {
        num: 0x4a,
        label: "DDR4_C",
    },
    TempSensor {
        num: 0x4b,
        label: "DDR4_D",
    },
    TempSensor {
        num: 0x4c,
        label: "DDR4_E",
    },
    TempSensor {
        num: 0x4d,
        label: "DDR4_F",
    },
    TempSensor {
        num: 0x4e,
        label: "DDR4_G",
    },
    TempSensor {
        num: 0x4f,
        label: "DDR4_H",
    },
    TempSensor {
        num: 0x50,
        label: "DDR4_I",
    },
    TempSensor {
        num: 0x51,
        label: "DDR4_J",
    },
    TempSensor {
        num: 0x52,
        label: "DDR4_K",
    },
    TempSensor {
        num: 0x53,
        label: "DDR4_L",
    },
    TempSensor {
        num: 0x54,
        label: "DDR4_M",
    },
    TempSensor {
        num: 0x55,
        label: "DDR4_N",
    },
    TempSensor {
        num: 0x56,
        label: "DDR4_O",
    },
    TempSensor {
        num: 0x57,
        label: "DDR4_P",
    },
];

/// The BMC temperature reader: an IPMI handle plus the per-sensor linear conversion factors, cached
/// once known (constant for these linear sensors — same caching policy as `board.rs::fan_factors`).
/// A sensor still `None` is retried on later ticks, so a transient BMC hiccup on the first read is
/// not permanent.
pub struct Sensors {
    ipmi: Ipmi,
    factors: Vec<Option<SensorFactors>>,
}

impl Sensors {
    pub fn open() -> Result<Self> {
        Ok(Sensors {
            ipmi: Ipmi::open()?,
            factors: vec![None; TEMP_SENSORS.len()],
        })
    }

    /// Best-effort: fetch and cache every sensor's conversion factors once, at instance bind, so the
    /// first `apply` tick is no heavier than the rest (the factor reads happen here, off the tick's
    /// deadline). Failures are left `None` and retried by `read_temps`. Short-timeout, read-only.
    pub fn prefetch_factors(&mut self) {
        for (i, s) in TEMP_SENSORS.iter().enumerate() {
            if self.factors[i].is_none() {
                self.factors[i] = self.ipmi.read_sensor_factors(s.num, 0, OBS_TIMEOUT).ok();
            }
        }
    }

    /// Read every hardcoded temperature sensor and return `(label, °C)` for the ones currently
    /// readable. At most **one** IPMI call per sensor per tick: if a sensor's constant conversion
    /// factors aren't cached yet (prefetch failed), fetch them this tick and report no temperature
    /// until the next one — so even a cold cache never does factor+sensor in the same tick. A sensor
    /// that is unavailable ("ns"), errors, or has no factors yet is skipped (never a bogus reading).
    /// Short-timeout, read-only.
    pub fn read_temps(&mut self) -> Vec<(&'static str, i32)> {
        let mut out = Vec::with_capacity(TEMP_SENSORS.len());
        for (i, s) in TEMP_SENSORS.iter().enumerate() {
            let temp = match self.factors[i] {
                Some(f) => temp_celsius(Some(f), self.ipmi.read_sensor(s.num, OBS_TIMEOUT).ok()),
                None => {
                    // Factors unknown -> fetch them now (one call); the temperature comes next tick.
                    // The reading byte (0) is immaterial for a linear sensor.
                    self.factors[i] = self.ipmi.read_sensor_factors(s.num, 0, OBS_TIMEOUT).ok();
                    None
                }
            };
            if let Some(t) = temp {
                out.push((s.label, t));
            }
        }
        out
    }
}

/// Convert a temperature sensor reading to whole °C, or `None` when the factors are missing, the
/// read failed, or the sensor reports its value as unavailable ("ns"). Unlike a fan tach, a
/// temperature may legitimately be negative, so no non-negativity clamp is applied. Pure
/// (unit-testable without hardware).
fn temp_celsius(factors: Option<SensorFactors>, reading: Option<SensorReading>) -> Option<i32> {
    let (f, r) = (factors?, reading?);
    if !r.available {
        return None;
    }
    Some(f.convert(r.raw).round() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hardcoded table matches the activation brief: 22 sensors, the exact numbers, and 0x2D
    /// intentionally absent (CARD_SIDE1=0x2C, LAN=0x2E).
    #[test]
    fn sensor_table_is_the_verified_set() {
        let nums: Vec<u8> = TEMP_SENSORS.iter().map(|s| s.num).collect();
        let mut expected = vec![0x28u8, 0x29, 0x2a, 0x2b, 0x2c, 0x2e];
        expected.extend(0x48u8..=0x57); // DDR4_A..P
        assert_eq!(nums, expected);
        assert_eq!(TEMP_SENSORS.len(), 22);
        assert!(!nums.contains(&0x2d), "0x2D is intentionally not a sensor");
        // Labels are unique and stable.
        let mut labels: Vec<&str> = TEMP_SENSORS.iter().map(|s| s.label).collect();
        let n = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), n, "labels must be unique");
    }

    #[test]
    fn temp_celsius_converts_only_a_valid_reading() {
        // A linear temperature sensor with M=1, B=0, exps=0 -> °C == raw byte.
        let f = SensorFactors {
            m: 1,
            b: 0,
            b_exp: 0,
            r_exp: 0,
        };
        assert_eq!(
            temp_celsius(
                Some(f),
                Some(SensorReading {
                    raw: 47,
                    available: true
                })
            ),
            Some(47)
        );

        // Unavailable ("ns") / missing reading / missing factors -> None (never a bogus temp).
        assert_eq!(
            temp_celsius(
                Some(f),
                Some(SensorReading {
                    raw: 47,
                    available: false
                })
            ),
            None
        );
        assert_eq!(temp_celsius(Some(f), None), None);
        assert_eq!(
            temp_celsius(
                None,
                Some(SensorReading {
                    raw: 47,
                    available: true
                })
            ),
            None
        );
    }

    #[test]
    fn temp_celsius_allows_negative_and_offset_factors() {
        // Below-ambient is legitimate for a temperature (no non-negativity clamp, unlike fan RPM):
        // M=1, B=-10, B_exp=0 -> °C = raw - 10.
        let f = SensorFactors {
            m: 1,
            b: -10,
            b_exp: 0,
            r_exp: 0,
        };
        assert_eq!(
            temp_celsius(
                Some(f),
                Some(SensorReading {
                    raw: 5,
                    available: true
                })
            ),
            Some(-5)
        );
    }
}
