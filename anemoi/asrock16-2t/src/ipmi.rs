//! Inband IPMI over `/dev/ipmi0` via raw libc ioctls — zero extra deps.
//!
//! The Linux IPMI char interface is asynchronous: `IPMICTL_SEND_COMMAND` queues a request, then
//! `poll(POLLIN)` + `IPMICTL_RECEIVE_MSG_TRUNC` fetches the reply. The response's first data byte
//! is the IPMI completion code. Struct layouts and ioctl numbers are taken verbatim from this
//! host's `/usr/include/linux/ipmi.h` and asserted at compile time.
//!
//! Verified ASRockRack ROME2D16-2T fan control (netfn 0x3a): claim all-manual (0xd8 ×16 0x01),
//! set duty (0xd6 ×16 bytes, non-zero), release to BMC auto (0xd8 ×16 0x00), query duty (0xda).

use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::size_of;
use std::os::unix::io::AsRawFd;
use std::ptr;
use std::time::{Duration, Instant};

// ---- IPMI command set (verified) ------------------------------------------
const NETFN_OEM: u8 = 0x3a;
const CMD_FAN_MODE: u8 = 0xd8; // claim (0x01×16) / release (0x00×16)
const CMD_SET_DUTY: u8 = 0xd6;
const CMD_QUERY_DUTY: u8 = 0xda;

// ---- addressing (linux/ipmi.h) --------------------------------------------
const IPMI_SYSTEM_INTERFACE_ADDR_TYPE: libc::c_int = 0x0c;
const IPMI_BMC_CHANNEL: libc::c_short = 0x0f;
const IPMI_RESPONSE_RECV_TYPE: libc::c_int = 1;

const RESP_TIMEOUT: Duration = Duration::from_secs(2);

// ---- kernel structs (repr(C), sizes asserted) -----------------------------

#[repr(C)]
struct SystemInterfaceAddr {
    addr_type: libc::c_int,
    channel: libc::c_short,
    lun: libc::c_uchar,
}

#[repr(C)]
struct IpmiMsg {
    netfn: libc::c_uchar,
    cmd: libc::c_uchar,
    data_len: libc::c_ushort,
    data: *mut libc::c_uchar,
}

#[repr(C)]
struct IpmiReq {
    addr: *mut libc::c_uchar,
    addr_len: libc::c_uint,
    msgid: libc::c_long,
    msg: IpmiMsg,
}

#[repr(C)]
struct IpmiRecv {
    recv_type: libc::c_int,
    addr: *mut libc::c_uchar,
    addr_len: libc::c_uint,
    msgid: libc::c_long,
    msg: IpmiMsg,
}

// Layout must match the kernel exactly (sizes are baked into the ioctl numbers).
const _: () = assert!(size_of::<SystemInterfaceAddr>() == 8);
const _: () = assert!(size_of::<IpmiMsg>() == 16);
const _: () = assert!(size_of::<IpmiReq>() == 40);
const _: () = assert!(size_of::<IpmiRecv>() == 48);

// ---- ioctl numbers (computed; asserted against research-verified values) ---
const fn ioc(dir: u32, ty: u32, nr: u32, size: u32) -> u32 {
    (dir << 30) | (size << 16) | (ty << 8) | nr
}
const IOC_READ: u32 = 2;
const IOC_RW: u32 = 3;
const IPMI_IOC_MAGIC: u32 = b'i' as u32; // 0x69

const IPMICTL_SEND_COMMAND: u32 = ioc(IOC_READ, IPMI_IOC_MAGIC, 13, size_of::<IpmiReq>() as u32);
const IPMICTL_RECEIVE_MSG_TRUNC: u32 =
    ioc(IOC_RW, IPMI_IOC_MAGIC, 11, size_of::<IpmiRecv>() as u32);
const _: () = assert!(IPMICTL_SEND_COMMAND == 0x8028_690D);
const _: () = assert!(IPMICTL_RECEIVE_MSG_TRUNC == 0xC030_690B);

// ---- device handle ---------------------------------------------------------

pub struct Ipmi {
    file: File,
    msgid: libc::c_long,
    /// Whether we currently hold manual fan control (so we claim ONCE, not every tick).
    claimed: bool,
}

impl Ipmi {
    pub fn open() -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/ipmi0")
            .context("opening /dev/ipmi0 (needs root + ipmi_devintf)")?;
        Ok(Ipmi {
            file,
            msgid: 0,
            claimed: false,
        })
    }

    /// Send one raw command, wait for its reply, verify the completion code, return response data
    /// (the bytes after the completion code).
    fn raw(&mut self, netfn: u8, cmd: u8, data: &[u8]) -> Result<Vec<u8>> {
        let fd = self.file.as_raw_fd();

        let mut addr = SystemInterfaceAddr {
            addr_type: IPMI_SYSTEM_INTERFACE_ADDR_TYPE,
            channel: IPMI_BMC_CHANNEL,
            lun: 0,
        };
        self.msgid = self.msgid.wrapping_add(1);
        let msgid = self.msgid;

        let mut req_data = data.to_vec();
        let msg = IpmiMsg {
            netfn,
            cmd,
            data_len: req_data.len() as libc::c_ushort,
            data: if req_data.is_empty() {
                ptr::null_mut()
            } else {
                req_data.as_mut_ptr()
            },
        };
        let req = IpmiReq {
            addr: &mut addr as *mut _ as *mut libc::c_uchar,
            addr_len: size_of::<SystemInterfaceAddr>() as libc::c_uint,
            msgid,
            msg,
        };

        let r = unsafe {
            libc::ioctl(
                fd,
                IPMICTL_SEND_COMMAND as libc::c_ulong,
                &req as *const IpmiReq,
            )
        };
        if r < 0 {
            bail!(
                "IPMI send (cmd 0x{cmd:02x}): {}",
                io::Error::last_os_error()
            );
        }
        // Keep request buffers alive until the kernel has copied them.
        drop(req_data);

        let deadline = Instant::now() + RESP_TIMEOUT;
        loop {
            let now = Instant::now();
            if now >= deadline {
                bail!("IPMI response timeout (cmd 0x{cmd:02x})");
            }
            let remaining = deadline.saturating_duration_since(now);
            if !poll_in(fd, remaining)? {
                bail!("IPMI response timeout (cmd 0x{cmd:02x})");
            }

            let mut addr_buf = [0u8; 32];
            let mut resp = [0u8; 256];
            let mut recv = IpmiRecv {
                recv_type: 0,
                addr: addr_buf.as_mut_ptr(),
                addr_len: addr_buf.len() as libc::c_uint,
                msgid: 0,
                msg: IpmiMsg {
                    netfn: 0,
                    cmd: 0,
                    data_len: resp.len() as libc::c_ushort,
                    data: resp.as_mut_ptr(),
                },
            };
            let r = unsafe {
                libc::ioctl(
                    fd,
                    IPMICTL_RECEIVE_MSG_TRUNC as libc::c_ulong,
                    &mut recv as *mut IpmiRecv,
                )
            };
            if r < 0 {
                let e = io::Error::last_os_error();
                match e.raw_os_error() {
                    Some(libc::EAGAIN) | Some(libc::EINTR) => continue, // not ready yet / interrupted
                    _ => bail!("IPMI recv (cmd 0x{cmd:02x}): {e}"),
                }
            }

            // Ignore async events / replies that aren't ours.
            if recv.recv_type != IPMI_RESPONSE_RECV_TYPE || recv.msgid != msgid {
                continue;
            }
            let n = recv.msg.data_len as usize;
            if n == 0 {
                bail!("IPMI empty response (cmd 0x{cmd:02x})");
            }
            let cc = resp[0];
            if cc != 0x00 {
                bail!("IPMI completion code 0x{cc:02x} (netfn 0x{netfn:02x} cmd 0x{cmd:02x})");
            }
            return Ok(resp[1..n.min(resp.len())].to_vec());
        }
    }

    /// Claim all 16 fans to manual control. Idempotent on the BMC; we track it so we only send it
    /// when needed (not every tick — re-claiming every tick is needless IPMI traffic).
    pub fn claim_manual(&mut self) -> Result<()> {
        self.raw(NETFN_OEM, CMD_FAN_MODE, &claim_payload())?;
        self.claimed = true;
        Ok(())
    }

    /// Release all fans back to BMC auto control (the fail-safe).
    pub fn release_auto(&mut self) -> Result<()> {
        self.raw(NETFN_OEM, CMD_FAN_MODE, &release_payload())?;
        self.claimed = false;
        Ok(())
    }

    /// Claim (if needed) and set every fan's duty to `pct`. On a persistent set failure this
    /// RELEASES to BMC auto rather than leaving the board claimed-manual without a fresh duty
    /// (which would freeze the fans with no regulation). Policy lives in `regulate` (unit-tested).
    pub fn set_all_fans(&mut self, pct: i32) -> Result<()> {
        regulate(self, pct)
    }

    fn write_duty(&mut self, pct: i32) -> Result<()> {
        self.raw(NETFN_OEM, CMD_SET_DUTY, &duty_payload(pct))
            .map(|_| ())
    }

    /// Query the current per-fan duty (0xda). Returns the raw 16 bytes (percent each).
    pub fn query_duty(&mut self) -> Result<Vec<u8>> {
        self.raw(NETFN_OEM, CMD_QUERY_DUTY, &[])
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

impl FanBus for Ipmi {
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
/// **release to BMC auto** so the board is never left claimed-manual without a fresh duty (which
/// would freeze the fans with no thermal regulation).
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

fn poll_in(fd: libc::c_int, timeout: Duration) -> io::Result<bool> {
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    loop {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let r = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        return Ok(r > 0);
    }
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

    #[test]
    fn ioctl_numbers_match_verified_values() {
        assert_eq!(IPMICTL_SEND_COMMAND, 0x8028_690D);
        assert_eq!(IPMICTL_RECEIVE_MSG_TRUNC, 0xC030_690B);
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
        // The R2 safety guarantee: a persistent set failure must NOT leave the board claimed-manual
        // (fans frozen) — it must release to BMC auto.
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
