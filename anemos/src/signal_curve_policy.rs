//! Signal matching with one temperature curve per matched source group.
//!
//! Every rule independently sees every matching numeric producer. Each rule reduces its matches
//! with `max`, applies its own curve/damper, then the policy selects the maximum requested overlay.
//! Configured policy failures deliberately fail high rather than retaining last-good configuration.

use crate::curve::Curve;
use crate::{Damper, Driving, Inputs, Provenance, Role, Signal};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

const POLICY_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyConfig {
    version: u32,
    enabled: bool,
    #[serde(default)]
    rules: Vec<RuleConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleConfig {
    name: String,
    #[serde(rename = "match")]
    selector: Selector,
    curve: String,
    #[serde(default)]
    required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selector {
    #[serde(default)]
    module: Vec<String>,
    #[serde(default)]
    instance: Vec<String>,
    #[serde(default)]
    signal: Vec<String>,
    #[serde(default)]
    component: Vec<String>,
    #[serde(default)]
    uom: Vec<String>,
    #[serde(default)]
    labels: BTreeMap<String, Vec<String>>,
}

struct RuleState {
    config: RuleConfig,
    damper: Damper,
}

/// One rule's hottest matching signal after reduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDriver {
    pub rule: String,
    pub module: String,
    pub instance: String,
    pub signal: String,
    pub value: i32,
    pub uom: Option<String>,
}

/// A successful source-matched curve decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    pub pct: u32,
    pub winning_rule: String,
    pub winning_signal: String,
    pub raw: i32,
    pub smoothed: i32,
    pub uom: Option<String>,
    pub drivers: Vec<PolicyDriver>,
}

impl PolicyDecision {
    /// Human- and machine-readable provenance for sink reporting.
    pub fn driven_by(&self) -> Vec<Provenance> {
        self.drivers
            .iter()
            .map(|driver| {
                let mut provenance = Provenance::new(format!(
                    "{}:{} / {} (rule {})",
                    driver.module, driver.instance, driver.signal, driver.rule
                ))
                .value(driver.value)
                .signal(driver.signal.clone());
                if let Some(uom) = &driver.uom {
                    provenance = provenance.uom(uom.clone());
                }
                provenance
            })
            .collect()
    }

    pub fn driving(&self) -> Driving {
        let mut driving = Driving::new()
            .kind("temperature")
            .raw(self.raw as f64)
            .input(self.smoothed as f64)
            .output(self.pct as f64)
            .how(format!(
                "case-overlay; rule={}; signal={}; reduce=max; combine=max",
                self.winning_rule, self.winning_signal
            ));
        if let Some(uom) = &self.uom {
            driving = driving.uom(uom.clone());
        }
        driving
    }
}

/// Result of evaluating the live policy files for one tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyOutcome {
    /// No enabled policy or matching optional rule contributes an overlay.
    Inactive,
    /// Policy evaluation succeeded.
    Applied(PolicyDecision),
    /// A configured policy, curve, or required input is unsafe. Case fans must be commanded to 100%.
    FailHigh { warning: String },
}

impl PolicyOutcome {
    pub fn overlay_pct(&self) -> Option<u32> {
        match self {
            PolicyOutcome::Inactive => None,
            PolicyOutcome::Applied(decision) => Some(decision.pct),
            PolicyOutcome::FailHigh { .. } => Some(100),
        }
    }

    pub fn warning(&self) -> Option<&str> {
        match self {
            PolicyOutcome::FailHigh { warning } => Some(warning),
            PolicyOutcome::Inactive | PolicyOutcome::Applied(_) => None,
        }
    }

    pub fn driven_by(&self) -> Vec<Provenance> {
        match self {
            PolicyOutcome::Applied(decision) => decision.driven_by(),
            PolicyOutcome::FailHigh { warning } => {
                vec![
                    Provenance::new(format!("case-fan policy safety fallback: {warning}"))
                        .value(true),
                ]
            }
            PolicyOutcome::Inactive => Vec::new(),
        }
    }

    pub fn driving(&self) -> Option<Driving> {
        match self {
            PolicyOutcome::Applied(decision) => Some(decision.driving()),
            PolicyOutcome::FailHigh { .. } => Some(
                Driving::new()
                    .kind("configuration-safety")
                    .raw(1.0)
                    .input(1.0)
                    .output(100.0)
                    .how("case-policy:fail-high"),
            ),
            PolicyOutcome::Inactive => None,
        }
    }
}

/// Live signal→curve overlay policy owned by a fan-control device.
pub struct SignalCurvePolicy {
    path: PathBuf,
    rules: Vec<RuleState>,
    active_config: Option<PolicyConfig>,
    /// Once an enabled policy is observed, deleting it cannot silently remove its safety demand.
    configured_seen: bool,
}

impl SignalCurvePolicy {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        SignalCurvePolicy {
            path: path.into(),
            rules: Vec::new(),
            active_config: None,
            configured_seen: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Re-read the policy and every referenced curve, then evaluate current routed and local
    /// signals. Local signals are attributed to module/instance `self:self`.
    pub fn evaluate(&mut self, inputs: Option<&Inputs>, local_signals: &[Signal]) -> PolicyOutcome {
        let config = match self.load_policy() {
            Ok(Some(config)) => config,
            Ok(None) if !self.configured_seen => return PolicyOutcome::Inactive,
            Ok(None) => return self.fail_high("configured policy file is missing or unreadable"),
            Err(reason) => {
                self.configured_seen = true;
                return self.fail_high(reason);
            }
        };

        if !config.enabled {
            self.configured_seen = false;
            self.active_config = None;
            self.rules.clear();
            return PolicyOutcome::Inactive;
        }
        self.configured_seen = true;

        if self.active_config.as_ref() != Some(&config) {
            self.rules = config
                .rules
                .iter()
                .cloned()
                .map(|config| RuleState {
                    config,
                    damper: Damper::default(),
                })
                .collect();
            self.active_config = Some(config);
        }

        // Validate every configured curve on every tick, even when an optional rule currently has
        // no signals. A latent broken rule must not become active later without first failing high.
        let policy_dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        let curve_names: Vec<String> = self
            .rules
            .iter()
            .map(|rule| rule.config.curve.clone())
            .collect();
        let mut curves = Vec::with_capacity(curve_names.len());
        for curve_name in curve_names {
            let path = match contained_curve_path(policy_dir, &curve_name) {
                Ok(path) => path,
                Err(reason) => return self.fail_high(reason),
            };
            match load_curve(&path) {
                Ok(curve) => curves.push(curve),
                Err(reason) => return self.fail_high(reason),
            }
        }

        let samples = collect_samples(inputs, local_signals);
        let mut matched: Vec<Vec<&Sample<'_>>> = vec![Vec::new(); self.rules.len()];
        for sample in &samples {
            for (index, rule) in self.rules.iter().enumerate() {
                if rule.config.selector.matches(sample) {
                    matched[index].push(sample);
                }
            }
        }

        let mut results = Vec::new();
        for index in 0..self.rules.len() {
            let Some(hottest) = hottest(&matched[index]) else {
                let (required, name) = {
                    let rule = &mut self.rules[index];
                    rule.damper.reset();
                    (rule.config.required, rule.config.name.clone())
                };
                if required {
                    return self.fail_high(format!(
                        "required policy rule {:?} has no fresh matching signal",
                        name
                    ));
                }
                continue;
            };

            let rule_name = {
                let rule = &self.rules[index];
                rule.config.name.clone()
            };
            let (curve, alpha) = &curves[index];
            let rule = &mut self.rules[index];
            rule.damper.set_alpha(*alpha);
            let smoothed = rule.damper.smooth(hottest.value);
            let pct = rule.damper.deadband(curve.eval(smoothed).clamp(0, 100)) as u32;
            results.push(RuleResult {
                rule_index: index,
                rule: rule_name.clone(),
                pct,
                raw: hottest.value,
                smoothed,
                driver: PolicyDriver {
                    rule: rule_name,
                    module: hottest.module.to_string(),
                    instance: hottest.instance.to_string(),
                    signal: hottest.signal.id.clone(),
                    value: hottest.value,
                    uom: hottest.signal.uom.clone(),
                },
            });
        }

        if results.is_empty() {
            return PolicyOutcome::Inactive;
        }

        let winner = results
            .iter()
            .max_by(|a, b| {
                a.pct
                    .cmp(&b.pct)
                    .then_with(|| b.rule_index.cmp(&a.rule_index))
            })
            .expect("results is non-empty");
        PolicyOutcome::Applied(PolicyDecision {
            pct: winner.pct,
            winning_rule: winner.rule.clone(),
            winning_signal: winner.driver.signal.clone(),
            raw: winner.raw,
            smoothed: winner.smoothed,
            uom: winner.driver.uom.clone(),
            drivers: results.into_iter().map(|result| result.driver).collect(),
        })
    }

    fn load_policy(&self) -> Result<Option<PolicyConfig>, String> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "cannot read policy file {}: {error}",
                    self.path.display()
                ))
            }
        };
        let config: PolicyConfig = serde_json::from_str(&raw)
            .map_err(|error| format!("invalid policy file {}: {error}", self.path.display()))?;
        validate_policy(&config)?;
        Ok(Some(config))
    }

    fn fail_high(&mut self, warning: impl Into<String>) -> PolicyOutcome {
        for rule in &mut self.rules {
            rule.damper.reset();
        }
        PolicyOutcome::FailHigh {
            warning: warning.into(),
        }
    }
}

struct Sample<'a> {
    module: &'a str,
    instance: &'a str,
    signal: &'a Signal,
    value: i32,
}

struct RuleResult {
    rule_index: usize,
    rule: String,
    pct: u32,
    raw: i32,
    smoothed: i32,
    driver: PolicyDriver,
}

impl Selector {
    fn matches(&self, sample: &Sample<'_>) -> bool {
        matches_list(&self.module, sample.module)
            && matches_list(&self.instance, sample.instance)
            && matches_list(&self.signal, &sample.signal.id)
            && matches_list(&self.component, &sample.signal.component)
            && matches_optional_list(&self.uom, sample.signal.uom.as_deref())
            && self.labels.iter().all(|(key, accepted)| {
                sample
                    .signal
                    .labels
                    .get(key)
                    .is_some_and(|actual| accepted.iter().any(|value| value == actual))
            })
    }
}

fn matches_list(accepted: &[String], actual: &str) -> bool {
    accepted.is_empty() || accepted.iter().any(|value| value == actual)
}

fn matches_optional_list(accepted: &[String], actual: Option<&str>) -> bool {
    accepted.is_empty() || actual.is_some_and(|actual| matches_list(accepted, actual))
}

fn collect_samples<'a>(inputs: Option<&'a Inputs>, local_signals: &'a [Signal]) -> Vec<Sample<'a>> {
    let mut samples = Vec::new();
    if let Some(inputs) = inputs {
        for (source, signals) in inputs {
            let (module, instance) = source.split_once(':').unwrap_or((source.as_str(), ""));
            for signal in signals {
                if let Some(value) = numeric_producer(signal) {
                    samples.push(Sample {
                        module,
                        instance,
                        signal,
                        value,
                    });
                }
            }
        }
    }
    for signal in local_signals {
        if let Some(value) = numeric_producer(signal) {
            samples.push(Sample {
                module: "self",
                instance: "self",
                signal,
                value,
            });
        }
    }
    samples.sort_by(|a, b| {
        a.module
            .cmp(b.module)
            .then_with(|| a.instance.cmp(b.instance))
            .then_with(|| a.signal.id.cmp(&b.signal.id))
    });
    samples
}

fn numeric_producer(signal: &Signal) -> Option<i32> {
    if signal.role != Role::Producer {
        return None;
    }
    let value = signal.value.as_ref()?.as_f64()?;
    if !value.is_finite() || value < i32::MIN as f64 || value > i32::MAX as f64 {
        return None;
    }
    Some(value.round() as i32)
}

fn hottest<'a>(samples: &'a [&'a Sample<'a>]) -> Option<&'a Sample<'a>> {
    let mut result = None;
    for sample in samples {
        if result.is_none_or(|current: &Sample<'_>| sample.value > current.value) {
            result = Some(*sample);
        }
    }
    result
}

fn validate_policy(config: &PolicyConfig) -> Result<(), String> {
    if config.version != POLICY_VERSION {
        return Err(format!(
            "unsupported policy version {}; expected {POLICY_VERSION}",
            config.version
        ));
    }
    if !config.enabled {
        return Ok(());
    }
    if config.rules.is_empty() {
        return Err("enabled policy must contain at least one rule".to_string());
    }

    let mut names = HashSet::new();
    for rule in &config.rules {
        if rule.name.trim().is_empty() {
            return Err("policy rule name must not be empty".to_string());
        }
        if !names.insert(rule.name.as_str()) {
            return Err(format!("duplicate policy rule name {:?}", rule.name));
        }
        validate_curve_name(&rule.curve)?;
        validate_selector(&rule.selector, &rule.name)?;
    }
    Ok(())
}

fn validate_curve_name(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    let is_single_normal_component = !name.is_empty()
        && path.components().count() == 1
        && matches!(path.components().next(), Some(Component::Normal(_)))
        && !name.contains('\\');
    if !is_single_normal_component {
        return Err(format!(
            "curve reference {name:?} must be a relative basename in the policy directory"
        ));
    }
    Ok(())
}

fn contained_curve_path(policy_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let directory = fs::canonicalize(policy_dir).map_err(|error| {
        format!(
            "cannot resolve policy directory {}: {error}",
            policy_dir.display()
        )
    })?;
    let requested = policy_dir.join(name);
    let resolved = fs::canonicalize(&requested)
        .map_err(|error| format!("cannot resolve curve {}: {error}", requested.display()))?;
    if !resolved.starts_with(&directory) {
        return Err(format!(
            "curve reference {name:?} resolves outside policy directory {}",
            policy_dir.display()
        ));
    }
    Ok(resolved)
}

fn validate_selector(selector: &Selector, rule_name: &str) -> Result<(), String> {
    for (field, values) in [
        ("module", &selector.module),
        ("instance", &selector.instance),
        ("signal", &selector.signal),
        ("component", &selector.component),
        ("uom", &selector.uom),
    ] {
        if values.iter().any(|value| value.is_empty()) {
            return Err(format!(
                "policy rule {rule_name:?} selector {field} contains an empty value"
            ));
        }
    }
    for (key, values) in &selector.labels {
        if key.is_empty() || values.is_empty() || values.iter().any(|value| value.is_empty()) {
            return Err(format!(
                "policy rule {rule_name:?} has an empty label selector"
            ));
        }
    }
    Ok(())
}

struct StrictObject(BTreeMap<String, Value>);

impl<'de> Deserialize<'de> for StrictObject {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StrictObjectVisitor;

        impl<'de> Visitor<'de> for StrictObjectVisitor {
            type Value = StrictObject;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object with unique keys")
            }

            fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut values = BTreeMap::new();
                while let Some(key) = access.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom(format!("duplicate JSON key {key:?}")));
                    }
                    values.insert(key, access.next_value()?);
                }
                Ok(StrictObject(values))
            }
        }

        deserializer.deserialize_map(StrictObjectVisitor)
    }
}

fn load_curve(path: &Path) -> Result<(Curve, f64), String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("cannot read curve {}: {error}", path.display()))?;
    let StrictObject(map) = serde_json::from_str(&raw)
        .map_err(|error| format!("invalid curve {}: {error}", path.display()))?;

    let mut points = BTreeMap::new();
    let mut alpha = crate::damper::DEFAULT_EMA_ALPHA;
    for (key, value) in &map {
        if key == "sensitivity" {
            alpha = value
                .as_f64()
                .ok_or_else(|| format!("curve {} sensitivity must be numeric", path.display()))?;
            if !(alpha.is_finite() && 0.0 < alpha && alpha <= 1.0) {
                return Err(format!(
                    "curve {} sensitivity must be greater than 0 and at most 1",
                    path.display()
                ));
            }
            continue;
        }

        let temperature = parse_rounded_i32(key).ok_or_else(|| {
            format!(
                "curve {} has invalid temperature key {key:?}",
                path.display()
            )
        })?;
        let duty = value.as_f64().and_then(rounded_i32).ok_or_else(|| {
            format!(
                "curve {} duty at {key:?} must be a finite number",
                path.display()
            )
        })?;
        if !(0..=100).contains(&duty) {
            return Err(format!(
                "curve {} duty at {key:?} is outside 0..100",
                path.display()
            ));
        }
        if points.insert(temperature, duty).is_some() {
            return Err(format!(
                "curve {} has duplicate normalized temperature {temperature}",
                path.display()
            ));
        }
    }
    if points.is_empty() {
        return Err(format!(
            "curve {} has no temperature points",
            path.display()
        ));
    }
    let mut previous = None;
    for (&temperature, &duty) in &points {
        if previous.is_some_and(|(_, previous_duty)| duty < previous_duty) {
            return Err(format!(
                "curve {} decreases at temperature {temperature}",
                path.display()
            ));
        }
        previous = Some((temperature, duty));
    }
    Ok((Curve::from_points(points), alpha))
}

fn parse_rounded_i32(value: &str) -> Option<i32> {
    value.parse::<f64>().ok().and_then(rounded_i32)
}

fn rounded_i32(value: f64) -> Option<i32> {
    if value.is_finite() && value >= i32::MIN as f64 && value <= i32::MAX as f64 {
        Some(value.round() as i32)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    struct TempPolicy {
        dir: PathBuf,
        policy: PathBuf,
    }

    impl TempPolicy {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "aiolos-source-policy-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            TempPolicy {
                policy: dir.join("case.policy.json"),
                dir,
            }
        }

        fn write(&self, name: &str, contents: &str) {
            fs::write(self.dir.join(name), contents).unwrap();
        }
    }

    impl Drop for TempPolicy {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn temp_signal(id: &str, value: i32) -> Signal {
        Signal::producer(id, format!("{id}:component"), "temperature")
            .value(value)
            .uom("C")
    }

    fn configured_policy(required: bool) -> String {
        format!(
            r#"{{
                "version": 1,
                "enabled": true,
                "rules": [
                    {{
                        "name": "nvme",
                        "match": {{
                            "module": ["nvme"],
                            "labels": {{"type": ["temperature"]}}
                        }},
                        "curve": "nvme.curve.json",
                        "required": {required}
                    }},
                    {{
                        "name": "fallback",
                        "match": {{"labels": {{"type": ["temperature"]}}}},
                        "curve": "fallback.curve.json"
                    }}
                ]
            }}"#
        )
    }

    #[test]
    fn absent_or_explicitly_disabled_policy_has_no_overlay() {
        let files = TempPolicy::new();
        let mut policy = SignalCurvePolicy::new(&files.policy);
        assert_eq!(policy.evaluate(None, &[]), PolicyOutcome::Inactive);

        fs::write(&files.policy, r#"{"version":1,"enabled":false,"rules":[]}"#).unwrap();
        assert_eq!(policy.evaluate(None, &[]), PolicyOutcome::Inactive);
    }

    #[test]
    fn hottest_nvme_reaches_100_at_70_immediately() {
        let files = TempPolicy::new();
        fs::write(&files.policy, configured_policy(true)).unwrap();
        files.write("nvme.curve.json", r#"{"50":30,"70":100,"sensitivity":1.0}"#);
        files.write(
            "fallback.curve.json",
            r#"{"40":35,"90":80,"sensitivity":1.0}"#,
        );

        let mut inputs = Inputs::new();
        inputs.insert("nvme:one".to_string(), vec![temp_signal("nvme:a", 55)]);
        inputs.insert("nvme:two".to_string(), vec![temp_signal("nvme:b", 60)]);
        inputs.insert("nvme:three".to_string(), vec![temp_signal("nvme:c", 65)]);
        inputs.insert("nvme:four".to_string(), vec![temp_signal("nvme:d", 70)]);
        inputs.insert("nvidia:one".to_string(), vec![temp_signal("gpu:a", 65)]);

        let mut policy = SignalCurvePolicy::new(&files.policy);
        let PolicyOutcome::Applied(decision) = policy.evaluate(Some(&inputs), &[]) else {
            panic!("policy should apply");
        };
        assert_eq!(decision.pct, 100);
        assert_eq!(decision.winning_rule, "nvme");
        assert_eq!(decision.raw, 70);
        assert_eq!(
            decision
                .drivers
                .iter()
                .find(|driver| driver.rule == "nvme")
                .map(|driver| driver.signal.as_str()),
            Some("nvme:d")
        );
    }

    #[test]
    fn nvme_curve_rises_gradually_between_50_and_70() {
        let files = TempPolicy::new();
        fs::write(
            &files.policy,
            r#"{
                "version":1,
                "enabled":true,
                "rules":[{
                    "name":"nvme",
                    "match":{"module":["nvme"],"labels":{"type":["temperature"]}},
                    "curve":"nvme.curve.json"
                }]
            }"#,
        )
        .unwrap();
        files.write("nvme.curve.json", r#"{"50":30,"70":100,"sensitivity":1.0}"#);
        let mut inputs = Inputs::new();
        inputs.insert("nvme:one".to_string(), vec![temp_signal("nvme:a", 60)]);

        let mut policy = SignalCurvePolicy::new(&files.policy);
        let PolicyOutcome::Applied(decision) = policy.evaluate(Some(&inputs), &[]) else {
            panic!("policy should apply");
        };
        assert_eq!(decision.pct, 65);
    }

    #[test]
    fn overlapping_rules_each_receive_the_same_matching_sensor() {
        let files = TempPolicy::new();
        fs::write(&files.policy, configured_policy(false)).unwrap();
        files.write("nvme.curve.json", r#"{"50":30,"70":100,"sensitivity":1.0}"#);
        files.write(
            "fallback.curve.json",
            r#"{"40":35,"60":100,"sensitivity":1.0}"#,
        );
        let mut inputs = Inputs::new();
        inputs.insert("nvme:one".to_string(), vec![temp_signal("nvme:a", 60)]);

        let mut policy = SignalCurvePolicy::new(&files.policy);
        let PolicyOutcome::Applied(decision) = policy.evaluate(Some(&inputs), &[]) else {
            panic!("policy should apply");
        };
        assert_eq!(decision.pct, 100);
        assert_eq!(decision.winning_rule, "fallback");
        assert_eq!(decision.drivers.len(), 2);
        assert_eq!(decision.drivers[0].rule, "nvme");
        assert_eq!(decision.drivers[1].rule, "fallback");
    }

    #[test]
    fn selector_ands_fields_and_ors_values() {
        let files = TempPolicy::new();
        fs::write(
            &files.policy,
            r#"{
                "version":1,
                "enabled":true,
                "rules":[
                    {
                        "name":"exact",
                        "match":{
                            "module":["other","nvme"],
                            "instance":["target"],
                            "signal":["wanted"],
                            "component":["ssd:thermal"],
                            "uom":["C"],
                            "labels":{
                                "type":["temperature"],
                                "zone":["controller","composite"]
                            }
                        },
                        "curve":"exact.curve.json"
                    },
                    {
                        "name":"fallback",
                        "match":{"labels":{"type":["temperature"]}},
                        "curve":"fallback.curve.json"
                    }
                ]
            }"#,
        )
        .unwrap();
        files.write(
            "exact.curve.json",
            r#"{"50":30,"70":100,"sensitivity":1.0}"#,
        );
        files.write(
            "fallback.curve.json",
            r#"{"40":35,"90":60,"sensitivity":1.0}"#,
        );

        let matching = Signal::producer("wanted", "ssd:thermal", "temperature")
            .value(70)
            .uom("C")
            .label("zone", "composite");
        let wrong_instance = Signal::producer("wanted", "ssd:thermal", "temperature")
            .value(75)
            .uom("C")
            .label("zone", "composite");
        let mut inputs = Inputs::new();
        inputs.insert("nvme:target".into(), vec![matching]);
        inputs.insert("nvme:not-target".into(), vec![wrong_instance]);

        let mut policy = SignalCurvePolicy::new(&files.policy);
        let PolicyOutcome::Applied(decision) = policy.evaluate(Some(&inputs), &[]) else {
            panic!("policy should apply");
        };
        assert_eq!(decision.winning_rule, "exact");
        assert_eq!(decision.winning_signal, "wanted");
        assert_eq!(decision.pct, 100);
        assert_eq!(decision.drivers.len(), 2);
    }

    #[test]
    fn required_rule_without_fresh_match_fails_high() {
        let files = TempPolicy::new();
        fs::write(&files.policy, configured_policy(true)).unwrap();
        files.write("nvme.curve.json", r#"{"50":30,"70":100,"sensitivity":1.0}"#);
        files.write(
            "fallback.curve.json",
            r#"{"40":35,"90":100,"sensitivity":1.0}"#,
        );

        let mut policy = SignalCurvePolicy::new(&files.policy);
        assert!(matches!(
            policy.evaluate(None, &[]),
            PolicyOutcome::FailHigh { .. }
        ));
    }

    #[test]
    fn unmatched_optional_rules_contribute_no_overlay() {
        let files = TempPolicy::new();
        fs::write(
            &files.policy,
            r#"{
                "version":1,
                "enabled":true,
                "rules":[{
                    "name":"nvme",
                    "match":{"module":["nvme"]},
                    "curve":"nvme.curve.json"
                }]
            }"#,
        )
        .unwrap();
        files.write("nvme.curve.json", r#"{"50":30,"70":100,"sensitivity":1.0}"#);
        let mut policy = SignalCurvePolicy::new(&files.policy);
        assert_eq!(policy.evaluate(None, &[]), PolicyOutcome::Inactive);
    }

    #[test]
    fn broken_curve_after_valid_tick_fails_high_without_last_good() {
        let files = TempPolicy::new();
        fs::write(&files.policy, configured_policy(true)).unwrap();
        files.write("nvme.curve.json", r#"{"50":30,"70":100,"sensitivity":1.0}"#);
        files.write(
            "fallback.curve.json",
            r#"{"40":35,"90":100,"sensitivity":1.0}"#,
        );
        let mut inputs = Inputs::new();
        inputs.insert("nvme:one".to_string(), vec![temp_signal("nvme:a", 60)]);

        let mut policy = SignalCurvePolicy::new(&files.policy);
        assert!(matches!(
            policy.evaluate(Some(&inputs), &[]),
            PolicyOutcome::Applied(_)
        ));
        files.write("nvme.curve.json", "}{");
        assert!(matches!(
            policy.evaluate(Some(&inputs), &[]),
            PolicyOutcome::FailHigh { .. }
        ));

        fs::write(&files.policy, "}{ broken policy").unwrap();
        assert!(matches!(
            policy.evaluate(Some(&inputs), &[]),
            PolicyOutcome::FailHigh { .. }
        ));
        fs::remove_file(&files.policy).unwrap();
        assert!(matches!(
            policy.evaluate(Some(&inputs), &[]),
            PolicyOutcome::FailHigh { .. }
        ));
        fs::write(&files.policy, r#"{"version":1,"enabled":false,"rules":[]}"#).unwrap();
        assert_eq!(policy.evaluate(Some(&inputs), &[]), PolicyOutcome::Inactive);
        fs::remove_file(&files.policy).unwrap();
        assert_eq!(policy.evaluate(Some(&inputs), &[]), PolicyOutcome::Inactive);
    }

    #[test]
    fn every_referenced_curve_is_validated_even_when_optional_rule_is_unmatched() {
        let files = TempPolicy::new();
        fs::write(&files.policy, configured_policy(false)).unwrap();
        files.write("nvme.curve.json", r#"{"50":30,"70":100,"sensitivity":1.0}"#);
        files.write("fallback.curve.json", "}{ broken");
        let mut inputs = Inputs::new();
        inputs.insert("nvme:one".to_string(), vec![temp_signal("nvme:a", 60)]);

        let mut policy = SignalCurvePolicy::new(&files.policy);
        assert!(matches!(
            policy.evaluate(Some(&inputs), &[]),
            PolicyOutcome::FailHigh { .. }
        ));
    }

    #[test]
    fn unsafe_path_and_decreasing_curve_are_rejected() {
        let files = TempPolicy::new();
        fs::write(
            &files.policy,
            r#"{
                "version":1,
                "enabled":true,
                "rules":[{
                    "name":"unsafe",
                    "match":{"module":["nvme"]},
                    "curve":"../outside.json"
                }]
            }"#,
        )
        .unwrap();
        let mut policy = SignalCurvePolicy::new(&files.policy);
        assert!(matches!(
            policy.evaluate(None, &[]),
            PolicyOutcome::FailHigh { .. }
        ));

        fs::write(&files.policy, configured_policy(false)).unwrap();
        files.write("nvme.curve.json", r#"{"50":80,"70":70,"sensitivity":1.0}"#);
        let mut inputs = Inputs::new();
        inputs.insert("nvme:one".to_string(), vec![temp_signal("nvme:a", 60)]);
        assert!(matches!(
            policy.evaluate(Some(&inputs), &[]),
            PolicyOutcome::FailHigh { .. }
        ));

        files.write(
            "nvme.curve.json",
            r#"{"50":30,"50":40,"70":100,"sensitivity":1.0}"#,
        );
        assert!(matches!(
            policy.evaluate(Some(&inputs), &[]),
            PolicyOutcome::FailHigh { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn curve_symlink_cannot_escape_the_policy_directory() {
        use std::os::unix::fs::symlink;

        let files = TempPolicy::new();
        let outside = files.dir.with_extension("outside.curve.json");
        fs::write(&outside, r#"{"50":30,"70":100,"sensitivity":1.0}"#).unwrap();
        symlink(&outside, files.dir.join("nvme.curve.json")).unwrap();
        fs::write(
            &files.policy,
            r#"{
                "version":1,
                "enabled":true,
                "rules":[{
                    "name":"nvme",
                    "match":{"module":["nvme"]},
                    "curve":"nvme.curve.json"
                }]
            }"#,
        )
        .unwrap();
        let mut policy = SignalCurvePolicy::new(&files.policy);
        assert!(matches!(
            policy.evaluate(None, &[]),
            PolicyOutcome::FailHigh { .. }
        ));
        fs::remove_file(outside).unwrap();
    }

    #[test]
    fn non_producers_and_non_numeric_values_do_not_drive_policy() {
        let files = TempPolicy::new();
        fs::write(&files.policy, configured_policy(true)).unwrap();
        files.write("nvme.curve.json", r#"{"50":30,"70":100,"sensitivity":1.0}"#);
        files.write(
            "fallback.curve.json",
            r#"{"40":35,"90":100,"sensitivity":1.0}"#,
        );
        let mut inputs = Inputs::new();
        inputs.insert(
            "nvme:one".to_string(),
            vec![
                Signal::sink("fan", "fan:component", "fan-duty").value(100),
                temp_signal("invalid", 60).value("not-a-number"),
            ],
        );

        let mut policy = SignalCurvePolicy::new(&files.policy);
        assert!(matches!(
            policy.evaluate(Some(&inputs), &[]),
            PolicyOutcome::FailHigh { .. }
        ));
    }

    #[test]
    fn packaged_policies_enable_cleanly_and_command_100_for_nvme_at_70() {
        let packaging = Path::new(env!("CARGO_MANIFEST_DIR")).join("../packaging");
        for module in ["rome2d-fans", "it87"] {
            let files = TempPolicy::new();
            let policy_name = format!("{module}.case.policy.json");
            let mut config: Value =
                serde_json::from_str(&fs::read_to_string(packaging.join(&policy_name)).unwrap())
                    .unwrap();
            config["enabled"] = Value::Bool(true);
            fs::write(&files.policy, serde_json::to_string(&config).unwrap()).unwrap();
            let curve_name = format!("{module}.case.nvme.curve.json");
            fs::copy(packaging.join(&curve_name), files.dir.join(&curve_name)).unwrap();

            let mut inputs = Inputs::new();
            inputs.insert("nvme:one".into(), vec![temp_signal("nvme:composite", 70)]);
            let mut policy = SignalCurvePolicy::new(&files.policy);
            let PolicyOutcome::Applied(decision) = policy.evaluate(Some(&inputs), &[]) else {
                panic!("{module} packaged policy should apply");
            };
            assert_eq!(decision.winning_rule, "nvme");
            assert_eq!(decision.pct, 100);
        }
    }
}
