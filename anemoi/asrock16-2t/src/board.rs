//! ASRockRack ROME2D16-2T OEM fan control (netfn 0x3a), built on the generic `ipmi` transport:
//! claim all-manual (0xd8 ×16 0x01), set duty (0xd6 ×16 bytes, non-zero), release to BMC auto
//! (0xd8 ×16 0x00), query duty (0xda). This is the board-specific layer; the generic IPMI transport
//! lives in the `ipmi` crate.

use anyhow::{bail, Result};
use ipmi::Ipmi;

const NETFN_OEM: u8 = 0x3a;
const CMD_FAN_MODE: u8 = 0xd8; // claim (0x01×16) / release (0x00×16)
const CMD_SET_DUTY: u8 = 0xd6;
const CMD_QUERY_DUTY: u8 = 0xda;

/// The board's fan controller: an IPMI handle plus whether we currently hold manual control (so we
/// claim ONCE, not every tick).
pub struct Board {
    ipmi: Ipmi,
    claimed: bool,
}

impl Board {
    pub fn open() -> Result<Self> {
        Ok(Board {
            ipmi: Ipmi::open()?,
            claimed: false,
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

    /// Claim (if needed) and set every fan's duty to `pct`. On a persistent set failure this
    /// RELEASES to BMC auto rather than leaving the board claimed-manual without a fresh duty.
    pub fn set_all_fans(&mut self, pct: i32) -> Result<()> {
        regulate(self, pct)
    }

    fn write_duty(&mut self, pct: i32) -> Result<()> {
        self.ipmi
            .raw(NETFN_OEM, CMD_SET_DUTY, &duty_payload(pct))
            .map(|_| ())
    }

    /// Query the current per-fan duty (0xda). Returns the raw bytes (percent each). Read-only.
    pub fn query_duty(&mut self) -> Result<Vec<u8>> {
        self.ipmi.raw(NETFN_OEM, CMD_QUERY_DUTY, &[])
    }
}

// ---- fan-control policy (trait-based so it is unit-testable without hardware) ----

/// Minimal fan-bus operations the regulation policy needs.
pub trait FanBus {
    fn is_claimed(&self) -> bool;
    fn claim(&mut self) -> Result<()>;
    fn set_duty(&mut self, pct: i32) -> Result<()>;
    fn release(&mut self) -> Result<()>;
}

impl FanBus for Board {
    fn is_claimed(&self) -> bool {
        self.claimed
    }
    fn claim(&mut self) -> Result<()> {
        self.claim_manual()
    }
    fn set_duty(&mut self, pct: i32) -> Result<()> {
        self.write_duty(pct)
    }
    fn release(&mut self) -> Result<()> {
        self.release_auto()
    }
}

/// Claim-if-needed → set duty → re-claim+retry once on failure; if a duty still can't be set,
/// **release to BMC auto** so the board is never left claimed-manual without a fresh duty.
pub fn regulate<B: FanBus>(bus: &mut B, pct: i32) -> Result<()> {
    if !bus.is_claimed() {
        bus.claim()?; // claim failed -> nothing claimed -> safe to propagate
    }
    if let Err(first) = bus.set_duty(pct) {
        if let Err(reclaim) = bus.claim() {
            let _ = bus.release(); // can't regulate -> hand back to BMC auto
            bail!("set failed ({first}); re-claim failed ({reclaim}); released to BMC auto");
        }
        if let Err(second) = bus.set_duty(pct) {
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

/// 16 duty bytes. Byte value == percent (0x64 = 100). Clamped to 1..=100 so no byte is ever zero
/// (a zero byte → `0xcc invalid data field` and the claimed-but-undutied minimum trap).
fn duty_payload(pct: i32) -> [u8; 16] {
    let byte = pct.clamp(0, 100).max(1) as u8;
    [byte; 16]
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
        assert_eq!(duty_payload(0), [1u8; 16]); // 0% -> 1 (avoid 0xcc trap)
        assert_eq!(duty_payload(-5), [1u8; 16]);
        assert_eq!(duty_payload(50), [50u8; 16]);
        assert_eq!(duty_payload(100), [100u8; 16]); // 0x64
        assert_eq!(duty_payload(150), [100u8; 16]); // clamp high
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
        fn set_duty(&mut self, _pct: i32) -> Result<()> {
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
        assert!(regulate(&mut b, 50).is_ok());
        assert!(b.claimed && b.claims == 1 && b.sets == 1 && b.releases == 0);
    }

    #[test]
    fn regulate_releases_to_auto_when_duty_cannot_be_set() {
        // R2: a persistent set failure must NOT leave the board claimed-manual — release to BMC auto.
        let mut b = MockBus::new(true);
        assert!(regulate(&mut b, 50).is_err());
        assert!(
            b.releases >= 1,
            "must release to BMC auto when it cannot set a duty"
        );
        assert!(
            !b.claimed,
            "must never be left claimed-manual without a fresh duty"
        );
    }
}
