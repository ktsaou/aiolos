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

    /// Send one raw command with the default `RESP_TIMEOUT`. See [`Ipmi::raw_with_timeout`].
    pub fn raw(&mut self, netfn: u8, cmd: u8, data: &[u8]) -> Result<Vec<u8>> {
        self.raw_with_timeout(netfn, cmd, data, RESP_TIMEOUT)
    }

    /// Send one raw command, wait for its reply (bounded by `timeout`), verify the completion code,
    /// and return the response data (the bytes AFTER the completion code). A short `timeout` is used
    /// for best-effort observability reads; control commands use the default `RESP_TIMEOUT`.
    pub fn raw_with_timeout(
        &mut self,
        netfn: u8,
        cmd: u8,
        data: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>> {
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

        let deadline = Instant::now() + timeout;
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

    /// `Get Sensor Reading` (netfn 0x04, cmd 0x2d) for `sensor`, bounded by `timeout`: the raw analog
    /// byte plus whether the reading is currently valid. Standard IPMI.
    pub fn read_sensor(&mut self, sensor: u8, timeout: Duration) -> Result<SensorReading> {
        let r = self.raw_with_timeout(0x04, 0x2d, &[sensor], timeout)?;
        parse_sensor_reading(&r)
    }

    /// `Get Sensor Reading Factors` (netfn 0x04, cmd 0x23) for `sensor` at `reading`, bounded by
    /// `timeout`: the linear conversion factors. For a linear sensor the factors are constant, so
    /// `reading` is immaterial.
    pub fn read_sensor_factors(
        &mut self,
        sensor: u8,
        reading: u8,
        timeout: Duration,
    ) -> Result<SensorFactors> {
        let r = self.raw_with_timeout(0x04, 0x23, &[sensor, reading], timeout)?;
        parse_factors(&r)
    }
}

/// Decode a `Get Sensor Reading` reply body (bytes after the completion code): byte 0 is the raw
/// reading, byte 1 is the status. If the status byte is present we honour it; if a BMC omits it but
/// still returned a reading, we trust the reading as available (the value was returned — for
/// observability that beats silently dropping it).
fn parse_sensor_reading(r: &[u8]) -> Result<SensorReading> {
    let raw = *r.first().context("empty sensor reading")?;
    let available = match r.get(1) {
        Some(&status) => reading_available(status),
        None => true,
    };
    Ok(SensorReading { raw, available })
}

/// One `Get Sensor Reading` result: the raw analog byte and whether it is currently valid.
#[derive(Debug, Clone, Copy)]
pub struct SensorReading {
    pub raw: u8,
    pub available: bool,
}

/// Linear conversion factors from `Get Sensor Reading Factors`. The converted analog value is
/// `((m * raw) + (b * 10^b_exp)) * 10^r_exp` (IPMI v2.0 §35.5). `m`/`b` are 10-bit signed; the
/// exponents are 4-bit signed. The raw reading is treated as **unsigned** (the analog data format
/// lives in the SDR, not here; fan-tach sensors are universally unsigned).
#[derive(Debug, Clone, Copy)]
pub struct SensorFactors {
    pub m: i32,
    pub b: i32,
    pub b_exp: i32,
    pub r_exp: i32,
}

impl SensorFactors {
    /// Apply the IPMI linearisation to an unsigned raw reading.
    pub fn convert(&self, raw: u8) -> f64 {
        ((self.m as f64 * raw as f64) + (self.b as f64 * 10f64.powi(self.b_exp)))
            * 10f64.powi(self.r_exp)
    }
}

/// Whether a `Get Sensor Reading` value is currently valid, per the IPMI status byte (the byte
/// after the reading — index 1 of the post-completion-code data, IPMI v2.0 §35.14): bit 6 (0x40)
/// "sensor scanning enabled" must be SET and bit 5 (0x20) "reading/state unavailable" must be CLEAR.
/// (Matches `ipmitool`; verified live on this BMC, where working sensors report 0xC0.)
fn reading_available(status: u8) -> bool {
    (status & 0x20) == 0 && (status & 0x40) != 0
}

/// Decode the body of `Get Sensor Reading Factors` (the bytes after the completion code):
/// `[next_reading, M_lsb, (M_msb<<6|tol), B_lsb, (B_msb<<6|acc_lsb), acc/dir, (R_exp<<4|B_exp)]`.
fn parse_factors(r: &[u8]) -> Result<SensorFactors> {
    if r.len() < 7 {
        bail!("short Get Sensor Reading Factors reply ({} bytes)", r.len());
    }
    let m = sign_extend_10(((r[2] as u16 & 0xc0) << 2) | r[1] as u16);
    let b = sign_extend_10(((r[4] as u16 & 0xc0) << 2) | r[3] as u16);
    let r_exp = sign_extend_4((r[6] >> 4) & 0x0f);
    let b_exp = sign_extend_4(r[6] & 0x0f);
    Ok(SensorFactors { m, b, b_exp, r_exp })
}

/// Sign-extend a 10-bit two's-complement value to i32.
fn sign_extend_10(v: u16) -> i32 {
    if v & 0x200 != 0 {
        v as i32 - 1024
    } else {
        v as i32
    }
}

/// Sign-extend a 4-bit two's-complement value to i32.
fn sign_extend_4(v: u8) -> i32 {
    if v & 0x08 != 0 {
        v as i32 - 16
    } else {
        v as i32
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

    #[test]
    fn factors_decode_matches_verified_bmc_bytes() {
        // ROME2D16-2T fan sensor 0x60/0x62 returned `20 64 00 00 00 00 00` -> M=100,B=0,exps=0.
        let f = parse_factors(&[0x20, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!((f.m, f.b, f.b_exp, f.r_exp), (100, 0, 0, 0));
        assert_eq!(f.convert(6).round() as i32, 600, "raw 6 -> 600 RPM");
        assert_eq!(f.convert(12).round() as i32, 1200, "raw 12 -> 1200 RPM");
        assert!(parse_factors(&[0x00; 6]).is_err(), "short reply rejected");
    }

    #[test]
    fn factors_decode_matches_live_voltage_sensor() {
        // Ground-truth pin of the R_exp/B_exp nibble order against REAL hardware: on the ROME2D16-2T
        // BMC, VOLT_3VSB (sensor 0x01) read raw 0xab with factors `20 02 00 00 00 00 e0` -> M=2,
        // B=0, R_exp=-2 (upper nibble of 0xe0), B_exp=0 -> 2*171*10^-2 = 3.42 V, exactly what
        // `ipmitool` reports (3.420 V). The swapped interpretation would give 342 V — proving
        // upper-nibble = R_exp.
        let f = parse_factors(&[0x20, 0x02, 0x00, 0x00, 0x00, 0x00, 0xe0]).unwrap();
        assert_eq!((f.m, f.b, f.r_exp, f.b_exp), (2, 0, -2, 0));
        assert!(
            (f.convert(0xab) - 3.42).abs() < 1e-9,
            "got {}",
            f.convert(0xab)
        );
    }

    #[test]
    fn factors_decode_high_bits_of_m_and_b() {
        // M's MSBs live in byte-4 bits [7:6] and B's in byte-6 bits [7:6] (IPMI v2.0 §35.5) — NOT
        // the low two bits. byte4=0x40 -> M[8]=1 -> M = 0x100 | 0x05 = 261; byte6=0x40 -> B[8]=1 ->
        // B = 0x100 | 0x07 = 263. (Masking the low two bits instead would wrongly yield M=5, B=7.)
        // No sensor on the live BMC exercises a non-zero MSB byte, so this is pinned synthetically.
        let f = parse_factors(&[0x20, 0x05, 0x40, 0x07, 0x40, 0x00, 0x00]).unwrap();
        assert_eq!((f.m, f.b), (261, 263));
    }

    #[test]
    fn factors_decode_negative_b_with_b_exp() {
        // B = -512 (10-bit two's complement: msb bits 0b10 -> 0x200), B_exp = 1, M = 1.
        // convert(0) = (1*0 + (-512)*10^1) * 10^0 = -5120.
        let f = parse_factors(&[0x20, 0x01, 0x00, 0x00, 0x80, 0x00, 0x01]).unwrap();
        assert_eq!((f.m, f.b, f.r_exp, f.b_exp), (1, -512, 0, 1));
        assert!((f.convert(0) - (-5120.0)).abs() < 1e-9);
    }

    #[test]
    fn factors_sign_extend_and_exponents() {
        // M = 10-bit signed: M_lsb=0x00, M_msb bits = 0b11 -> 0x300 = 768 -> -256.
        // r_exp nibble 0xE -> -2 ; b_exp nibble 0x0 -> 0.
        let f = parse_factors(&[0x00, 0x00, 0xc0, 0x00, 0x00, 0x00, 0xe0]).unwrap();
        assert_eq!(f.m, -256);
        assert_eq!(f.r_exp, -2);
        assert_eq!(f.b_exp, 0);
        // (-256 * 4) * 10^-2 = -10.24
        assert!((f.convert(4) - (-10.24)).abs() < 1e-9);
    }

    #[test]
    fn sensor_reading_availability() {
        assert!(
            reading_available(0xc0),
            "scanning on (0x40), not unavailable -> valid"
        );
        assert!(!reading_available(0x20), "unavailable bit set -> invalid");
        assert!(!reading_available(0x00), "scanning disabled -> invalid");
        assert!(
            !reading_available(0x60),
            "unavailable bit wins even if scanning on"
        );
    }

    #[test]
    fn sensor_reading_parse_contract() {
        // Normal reply [raw, status]: status honoured (0xc0 -> available; 0x20 -> not).
        let ok = parse_sensor_reading(&[0x06, 0xc0]).unwrap();
        assert_eq!((ok.raw, ok.available), (0x06, true));
        assert!(!parse_sensor_reading(&[0x06, 0x20]).unwrap().available);
        assert!(
            !parse_sensor_reading(&[0x06, 0x00]).unwrap().available,
            "scanning disabled (0x40 clear) -> not available"
        );
        // Status byte omitted but a reading was returned -> trust it (available).
        let bare = parse_sensor_reading(&[0x06]).unwrap();
        assert_eq!((bare.raw, bare.available), (0x06, true));
        // No bytes at all -> error (never a bogus reading).
        assert!(parse_sensor_reading(&[]).is_err());
    }
}
