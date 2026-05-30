//! Registry: which anemoi to run and how their data is wired.
//!
//! One module per line in `aiolos.conf`. A module line is `<name> [key=value ...]`.
//! Recognized directive: `input=<peer>` (relay that peer's readings into this module's `apply`).
//! Multiple sources are supported — repeat `input=` and/or use a comma list
//! (`input=nvidia input=nvme` ≡ `input=nvidia,nvme`); order is preserved and duplicates dropped.
//! Unknown directives are preserved verbatim for forward-compatibility but otherwise ignored.

#[derive(Debug, Clone, PartialEq)]
pub struct RegistryEntry {
    pub module_name: String,
    /// `input=<peer>` sources: relay each named peer module's last readings into this module's
    /// `apply.inputs`. Empty when no `input=` is wired.
    pub inputs: Vec<String>,
    /// Any directives we don't yet interpret (kept so a future field doesn't silently vanish).
    pub unknown_directives: Vec<String>,
}

impl RegistryEntry {
    /// A module name is routable only if it is non-empty and contains no `:`. The blackboard keys
    /// readings by `module:id`, so a `:` in the name would make a source prefix match the wrong
    /// module. `Config::parse` drops a non-routable line loudly; the invariant lives here so any
    /// caller building a registry directly can enforce it too.
    pub fn name_is_routable(&self) -> bool {
        !self.module_name.is_empty() && !self.module_name.contains(':')
    }
}

/// Parse one already-trimmed, non-empty, non-comment module line.
pub fn parse_module_line(line: &str) -> RegistryEntry {
    let mut tokens = line.split_whitespace();
    let module_name = tokens.next().unwrap_or_default().to_string();

    let mut inputs: Vec<String> = Vec::new();
    let mut unknown_directives = Vec::new();
    for tok in tokens {
        if let Some(val) = tok.strip_prefix("input=") {
            for peer in val.split(',') {
                let peer = peer.trim();
                if !peer.is_empty() && !inputs.iter().any(|p| p == peer) {
                    inputs.push(peer.to_string());
                }
            }
        } else {
            unknown_directives.push(tok.to_string());
        }
    }

    RegistryEntry {
        module_name,
        inputs,
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
        assert!(e.inputs.is_empty());
        assert!(e.unknown_directives.is_empty());
    }

    #[test]
    fn module_with_input() {
        let e = parse_module_line("asrock16-2t  input=nvidia");
        assert_eq!(e.module_name, "asrock16-2t");
        assert_eq!(e.inputs, vec!["nvidia".to_string()]);
    }

    #[test]
    fn multiple_inputs_repeated_or_comma_dedup_and_ordered() {
        let repeated = parse_module_line("asrock16-2t input=nvidia input=nvme");
        assert_eq!(
            repeated.inputs,
            vec!["nvidia".to_string(), "nvme".to_string()]
        );

        let comma = parse_module_line("asrock16-2t input=nvidia,nvme");
        assert_eq!(comma.inputs, vec!["nvidia".to_string(), "nvme".to_string()]);

        // Duplicates are dropped; first-seen order preserved.
        let dup = parse_module_line("asrock16-2t input=nvidia input=nvidia,nvme");
        assert_eq!(dup.inputs, vec!["nvidia".to_string(), "nvme".to_string()]);
    }

    #[test]
    fn unknown_directive_preserved() {
        let e = parse_module_line("nvidia  input=nvme  every=5");
        assert_eq!(e.inputs, vec!["nvme".to_string()]);
        assert_eq!(e.unknown_directives, vec!["every=5".to_string()]);
    }
}
