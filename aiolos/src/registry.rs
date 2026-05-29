//! Registry: which anemoi to run and how their data is wired.
//!
//! One module per line in `aiolos.conf`. A module line is `<name> [key=value ...]`.
//! Recognized directive: `input=<peer>` (relay that peer's readings into this module's `apply`).
//! Unknown directives are preserved verbatim for forward-compatibility but otherwise ignored.

#[derive(Debug, Clone, PartialEq)]
pub struct RegistryEntry {
    pub module_name: String,
    /// `input=<peer>`: relay the peer module's last readings into this module's `apply.inputs`.
    pub input: Option<String>,
    /// Any directives we don't yet interpret (kept so a future field doesn't silently vanish).
    pub unknown_directives: Vec<String>,
}

/// Parse one already-trimmed, non-empty, non-comment module line.
pub fn parse_module_line(line: &str) -> RegistryEntry {
    let mut tokens = line.split_whitespace();
    let module_name = tokens.next().unwrap_or_default().to_string();

    let mut input = None;
    let mut unknown_directives = Vec::new();
    for tok in tokens {
        if let Some(val) = tok.strip_prefix("input=") {
            input = Some(val.to_string());
        } else {
            unknown_directives.push(tok.to_string());
        }
    }

    RegistryEntry {
        module_name,
        input,
        unknown_directives,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_module() {
        let e = parse_module_line("nvidia");
        assert_eq!(e.module_name, "nvidia");
        assert!(e.input.is_none());
        assert!(e.unknown_directives.is_empty());
    }

    #[test]
    fn module_with_input() {
        let e = parse_module_line("asrock16-2t  input=nvidia");
        assert_eq!(e.module_name, "asrock16-2t");
        assert_eq!(e.input.as_deref(), Some("nvidia"));
    }

    #[test]
    fn unknown_directive_preserved() {
        let e = parse_module_line("nvidia  input=nvme  every=5");
        assert_eq!(e.input.as_deref(), Some("nvme"));
        assert_eq!(e.unknown_directives, vec!["every=5".to_string()]);
    }
}
