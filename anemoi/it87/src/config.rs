//! Operator config for the `it87` anemos: which hwmon chip and which PWM channels each zone drives.
//!
//! `it87.conf` is `key=value`, one per line; blank lines and `#` comments ignored. It maps THIS
//! board's wiring (which is host-specific, like a curve) so the module binary stays generic across
//! ITE boards. The file lives at `$AIOLOS_ETC_DIR/it87.conf` (tests/dev) else
//! `/opt/aiolos/etc/it87.conf`; when absent, built-in defaults for this host stand.
//!
//! Keys:
//! - `chip`  — hwmon `name` of the Super-I/O (default `it8689`).
//! - `cpu`   — comma-separated 1-based PWM channels driven by CPU temperature (default `1`).
//! - `case`  — comma-separated 1-based PWM channels driven by `max(GPU, CPU)` (default `3,4`).
//!
//! Unknown keys and unparseable channels are ignored (a typo never disarms control); a channel
//! listed in neither zone is simply not managed (left on firmware/auto).

const DEFAULT_CONF_PATH: &str = "/opt/aiolos/etc/it87.conf";
const CONF_FILENAME: &str = "it87.conf";

/// Resolved wiring: the chip name plus the per-zone PWM channel lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct It87Config {
    pub chip: String,
    pub cpu_channels: Vec<u8>,
    pub case_channels: Vec<u8>,
}

impl Default for It87Config {
    fn default() -> Self {
        It87Config {
            chip: "it8689".to_string(),
            cpu_channels: vec![1],
            case_channels: vec![3, 4],
        }
    }
}

impl It87Config {
    /// Every PWM channel this module manages (CPU zone + case zone), de-duplicated, sorted.
    pub fn managed_channels(&self) -> Vec<u8> {
        let mut all: Vec<u8> = self
            .cpu_channels
            .iter()
            .chain(&self.case_channels)
            .copied()
            .collect();
        all.sort_unstable();
        all.dedup();
        all
    }
}

/// Load the operator config, or the built-in defaults when the file is absent/empty/unreadable.
pub fn load() -> It87Config {
    match std::fs::read_to_string(conf_path()) {
        Ok(body) => parse_conf(&body),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => It87Config::default(),
        Err(e) => {
            eprintln!("it87: cannot read config ({e}); using built-in defaults");
            It87Config::default()
        }
    }
}

fn conf_path() -> String {
    match std::env::var("AIOLOS_ETC_DIR") {
        Ok(dir) => format!("{dir}/{CONF_FILENAME}"),
        Err(_) => DEFAULT_CONF_PATH.to_string(),
    }
}

/// Parse `it87.conf`. Any key absent from the file keeps its default. Pure (testable).
pub fn parse_conf(body: &str) -> It87Config {
    let mut cfg = It87Config::default();
    for line in body.lines() {
        let content = line.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = content.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "chip" if !value.is_empty() => cfg.chip = value.to_string(),
            "cpu" => cfg.cpu_channels = parse_channels(value),
            "case" => cfg.case_channels = parse_channels(value),
            _ => {}
        }
    }
    cfg
}

/// Parse a comma-separated 1-based channel list (`"3,4"`), dropping unparseable/zero entries.
fn parse_channels(value: &str) -> Vec<u8> {
    value
        .split(',')
        .filter_map(|s| s.trim().parse::<u8>().ok())
        .filter(|&c| c >= 1)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_this_host_wiring() {
        let d = It87Config::default();
        assert_eq!(d.chip, "it8689");
        assert_eq!(d.cpu_channels, vec![1]);
        assert_eq!(d.case_channels, vec![3, 4]);
        assert_eq!(d.managed_channels(), vec![1, 3, 4]);
    }

    #[test]
    fn parse_overrides_each_key_and_keeps_defaults_for_the_rest() {
        // Only `case` is overridden; chip and cpu keep defaults.
        let cfg = parse_conf("case = 2,5\n");
        assert_eq!(cfg.chip, "it8689");
        assert_eq!(cfg.cpu_channels, vec![1]);
        assert_eq!(cfg.case_channels, vec![2, 5]);
    }

    #[test]
    fn parse_strips_comments_and_ignores_unknowns_and_bad_channels() {
        let body = "\
# board wiring
chip = nct6798   # a different Super-I/O
cpu = 1
case = 3, 4, x, 0   # 'x' unparseable, 0 dropped (1-based)
bogus = whatever
";
        let cfg = parse_conf(body);
        assert_eq!(cfg.chip, "nct6798");
        assert_eq!(cfg.cpu_channels, vec![1]);
        assert_eq!(cfg.case_channels, vec![3, 4]);
    }

    #[test]
    fn managed_channels_dedups_overlap() {
        let cfg = parse_conf("cpu=1\ncase=1,3,4\n");
        assert_eq!(cfg.managed_channels(), vec![1, 3, 4]);
    }
}
