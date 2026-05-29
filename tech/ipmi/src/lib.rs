//! Level-1 tech: inband IPMI over `/dev/ipmi0` via raw libc ioctls — generic transport, zero extra
//! deps. Device-/board-specific OEM commands are built by the caller on top of `raw`.
//!
//! The Linux IPMI char interface is asynchronous: `IPMICTL_SEND_COMMAND` queues a request, then
//! `poll(POLLIN)` + `IPMICTL_RECEIVE_MSG_TRUNC` fetches the reply. The response's first data byte is
//! the IPMI completion code. Struct layouts and ioctl numbers are taken verbatim from
//! `/usr/include/linux/ipmi.h` and asserted at compile time.

use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::size_of;
use std::os::unix::io::AsRawFd;
use std::ptr;
use std::time::{Duration, Instant};

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

/// An open `/dev/ipmi0` handle. Send raw `(netfn, cmd, data)` commands via [`Ipmi::raw`].
pub struct Ipmi {
    file: File,
    msgid: libc::c_long,
}

impl Ipmi {
    pub fn open() -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/ipmi0")
            .context("opening /dev/ipmi0 (needs root + ipmi_devintf)")?;
        Ok(Ipmi { file, msgid: 0 })
    }

    /// Send one raw command, wait for its reply, verify the completion code, return the response
    /// data (the bytes AFTER the completion code). Bounded by `RESP_TIMEOUT`.
    pub fn raw(&mut self, netfn: u8, cmd: u8, data: &[u8]) -> Result<Vec<u8>> {
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
}

/// poll `fd` for `POLLIN` with a timeout, recomputing the remaining time on EINTR (a SIGTERM/SIGINT
/// without SA_RESTART can interrupt this; we keep waiting for the reply but stay bounded).
fn poll_in(fd: libc::c_int, timeout: Duration) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(false); // timed out
        }
        let ms = deadline
            .saturating_duration_since(now)
            .as_millis()
            .min(i32::MAX as u128) as i32;
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let r = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue; // recompute remaining from the deadline (no drift)
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
    fn ioctl_numbers_match_verified_values() {
        assert_eq!(IPMICTL_SEND_COMMAND, 0x8028_690D);
        assert_eq!(IPMICTL_RECEIVE_MSG_TRUNC, 0xC030_690B);
    }
}
