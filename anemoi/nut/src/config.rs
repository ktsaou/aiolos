//! Operator config for the `nut` anemos: which UPS(es) to monitor.
//!
//! `nut.conf` is a plain list — one UPS id per line; blank lines and `#` comments ignored. An id is
//! a NUT name (`pr3000-nova`) or `name@host[:port]` for a remote upsd. The file lives at
//! `$AIOLOS_ETC_DIR/nut.conf` (tests/dev) else `/opt/aiolos/etc/nut.conf`. It is the place for the
//! operator's UPS host (which is environment-specific) — so NO host/id is hardcoded or committed.
//! When the file is absent or lists nothing, the module auto-discovers via `upsc -l` (local upsd).

const DEFAULT_CONF_PATH: &str = "/opt/aiolos/etc/nut.conf";
const CONF_FILENAME: &str = "nut.conf";

/// Resolve the operator config path: `$AIOLOS_ETC_DIR/nut.conf` if that env is set, else the default
/// install path. Mirrors the SDK's curve-path convention so config sits next to the curves.
fn conf_path() -> String {
    match std::env::var("AIOLOS_ETC_DIR") {
        Ok(dir) => format!("{dir}/{CONF_FILENAME}"),
        Err(_) => DEFAULT_CONF_PATH.to_string(),
    }
}

/// The UPS ids to monitor: the operator config list if it yields any, otherwise local upsd
/// discovery (`upsc -l`). Empty only when neither configures nor discovers a UPS (a real result the
/// SDK reports as an empty `found`).
pub fn ups_ids() -> Vec<String> {
    let configured = read_conf(&conf_path());
    if !configured.is_empty() {
        return configured;
    }
    nut::list()
}

/// Read + parse the operator config file at `path`. Missing/unreadable file -> empty (fall back to
/// discovery). Pure parsing is in `parse_conf` (testable without a filesystem).
fn read_conf(path: &str) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(body) => parse_conf(&body),
        Err(_) => Vec::new(),
    }
}

/// Parse `nut.conf`: one UPS id per line; strip `#` comments and surrounding whitespace; drop blanks;
/// de-duplicate while preserving first-seen order. Pure (testable).
pub fn parse_conf(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in body.lines() {
        // Strip a `#` comment (anywhere on the line) and trim.
        let content = line.split('#').next().unwrap_or("").trim();
        if content.is_empty() {
            continue;
        }
        let id = content.to_string();
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ids_stripping_comments_and_blanks() {
        let body = "\
# UPS list for this host
pr3000-nova        # the rack UPS

  ups2@10.0.0.5:3493
# trailing comment only
";
        assert_eq!(
            parse_conf(body),
            vec!["pr3000-nova".to_string(), "ups2@10.0.0.5:3493".to_string()]
        );
    }

    #[test]
    fn deduplicates_preserving_order() {
        let body = "a\nb\na\n";
        assert_eq!(parse_conf(body), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn empty_or_comment_only_yields_nothing() {
        assert!(parse_conf("").is_empty());
        assert!(parse_conf("# only a comment\n\n   \n").is_empty());
    }
}
