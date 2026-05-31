//! Operator config for the `hwmon-temps` anemos: which hwmon chips to report.
//!
//! `hwmon-temps.conf` is a plain list — one hwmon chip `name` per line; blank lines and `#` comments
//! ignored. The file lives at `$AIOLOS_ETC_DIR/hwmon-temps.conf` (tests/dev) else
//! `/opt/aiolos/etc/hwmon-temps.conf`. When absent or empty, a built-in default set is used.
//!
//! The default set is the BMC-less workstation's monitorable temperatures EXCEPT the ones already
//! reported by dedicated modules: CPU (`coretemp`) is reported by `it87`, NVMe by `nvme`. `acpitz`
//! is omitted by default (ACPI zones are often duplicative/noisy) — add it here to enable it.

const DEFAULT_CONF_PATH: &str = "/opt/aiolos/etc/hwmon-temps.conf";
const CONF_FILENAME: &str = "hwmon-temps.conf";

/// Built-in default chip set when the operator config is absent or empty. Matched by name prefix,
/// so `r8169` catches the PCI-suffixed `r8169_0_600:00`/`r8169_0_700:00` NIC sensors. `it8689` is
/// deliberately omitted: on this board its temps are the SAME Super-I/O sensors `gigabyte_wmi`
/// already exposes (and the `it87` fan module owns that chip) — listing both would double-report.
const DEFAULT_CHIPS: &[&str] = &[
    "gigabyte_wmi", // board / VRM / chipset thermal sensors (vendor WMI view of the Super-I/O)
    "spd5118",      // DDR5 DIMM temperature sensors (one chip per module)
    "r8169",        // Realtek NIC temperature sensors (prefix-matched)
];

/// The chip names to report: the operator config list if it yields any, else the default set.
pub fn chips() -> Vec<String> {
    let configured = match std::fs::read_to_string(conf_path()) {
        Ok(body) => parse_conf(&body),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            eprintln!("hwmon-temps: cannot read config ({e}); using built-in default chip set");
            Vec::new()
        }
    };
    if configured.is_empty() {
        DEFAULT_CHIPS.iter().map(|s| s.to_string()).collect()
    } else {
        configured
    }
}

fn conf_path() -> String {
    match std::env::var("AIOLOS_ETC_DIR") {
        Ok(dir) => format!("{dir}/{CONF_FILENAME}"),
        Err(_) => DEFAULT_CONF_PATH.to_string(),
    }
}

/// Parse the chip list: one name per line; strip `#` comments and whitespace; drop blanks;
/// de-duplicate preserving first-seen order. Pure (testable).
pub fn parse_conf(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in body.lines() {
        let content = line.split('#').next().unwrap_or("").trim();
        if content.is_empty() {
            continue;
        }
        let name = content.to_string();
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_stripping_comments_and_blanks() {
        let body = "\
# chips to monitor
gigabyte_wmi   # VRM
spd5118

acpitz         # enable ACPI zones too
";
        assert_eq!(
            parse_conf(body),
            vec![
                "gigabyte_wmi".to_string(),
                "spd5118".to_string(),
                "acpitz".to_string()
            ]
        );
    }

    #[test]
    fn deduplicates_preserving_order() {
        assert_eq!(
            parse_conf("a\nb\na\n"),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn empty_or_comment_only_yields_nothing() {
        assert!(parse_conf("").is_empty());
        assert!(parse_conf("# only a comment\n\n   \n").is_empty());
    }
}
