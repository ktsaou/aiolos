# SOW-0019 - GitHub Code Scanning and Codacy SARIF

## Status

Status: paused

Sub-state: paused on 2026-07-26 by user decision so the thermal-safety SOW-0022 is the sole
in-progress SOW. Prior state: activated by the user on 2026-06-06 despite existing SOWs in `.agents/sow/current/`.
The scope is now a best-in-class open-source quality program: eliminate or justify Codacy findings,
reduce real code complexity and duplication, submit coverage to Codacy, and add durable quality gates.

## Requirements

### Purpose

Give this small public Rust workspace useful, repeatable static-analysis and coverage visibility in
GitHub and Codacy. The goal is now stricter than baseline visibility: enable GitHub Code Scanning,
publish Codacy SARIF to GitHub, submit Rust coverage to Codacy, eliminate noise, and fix or explicitly
justify every reported issue so that the final remote state reports zero findings.

### User Request

The user asked to add GitHub Code Scanning plus Codacy SARIF to this repo, noting that Codacy is already
enabled and reported 946 issues at the first check. On 2026-06-04, the user approved D1-D5 as recommended,
expanded D6 to eliminate all noise and fix all issues until zero findings are reported, and added the
`CODACY_API_TOKEN` repository secret for GitHub Actions.
On 2026-06-06, the user activated this SOW and cited the current Codacy state: 948 issues, 29%
complexity, and 23% duplication.

### Assistant Understanding

Facts:

- GitHub remote is `ktsaou/aiolos`, public, default branch `master`.
- GitHub Actions has no workflows yet.
- GitHub CodeQL default setup is `not-configured`.
- GitHub workflow default token permission is read-only, so SARIF upload needs explicit
  `security-events: write`.
- Codacy Cloud is enabled for `ktsaou/aiolos` and reports 948 issues on `master`.
- Codacy issue distribution is dominated by Markdown style findings: 720 Markdown issues and 677
  CodeStyle issues; markdownlint `MD022` reports 316 issues and `MD032` reports 297 issues.
- Codacy reports 61 security-category findings and 100 Error/High findings across Rust, JavaScript,
  Shell, and Markdown.
- The installed Codacy Analysis CLI supports SARIF output:
  `codacy-analysis analyze . --output-format sarif --output results.sarif`.
- A temporary clone was able to fetch Codacy remote configuration with
  `codacy-analysis init --remote gh ktsaou aiolos`, producing 10 configured tools:
  PMD, markdownlint, ESLint8, Lizard, Agentlinter, Semgrep, jackson, shellcheck, Stylelint, and Trivy.
- In the temporary clone, local inspection found 9 tools ready and PMD missing until dependencies are
  installed.
- Live recheck on 2026-06-04 confirmed GitHub still has zero workflows, CodeQL default setup is still
  `not-configured`, GitHub workflow default token permission is still `read`, and Codacy still reports
  946 issues on `master`.
- Live recheck on 2026-06-04 found Codacy's current enabled tool names differ from the earlier CLI
  output: the enabled set includes PMD, markdownlint, ESLint, Lizard, Agentlinter, Opengrep, Jackson
  Linter, ShellCheck, Stylelint, and Trivy.
- The workspace has Rust unit/integration tests and no existing coverage workflow or coverage upload
  configuration.
- `CODACY_API_TOKEN` is present as a GitHub repository secret. Secret value was not read and must never be
  written to durable artifacts.
- GitHub currently lists no repository variables.

Inferences:

- CI should still start as visibility-only for Codacy findings because D3 selected non-blocking
  behavior, but completion of this SOW now requires remote reanalysis to report zero findings.
- CodeQL and Codacy SARIF should use separate GitHub Code Scanning categories to avoid analysis/result
  confusion.
- Pull-request scanning with a Codacy API token needs careful event handling because GitHub does not
  expose secrets to fork pull requests.
- "Zero findings" is interpreted as zero active Codacy issues/security findings for the selected
  branch after reanalysis, plus no active GitHub Code Scanning alerts from the newly configured scans.
- Coverage upload can use the user-provided `CODACY_API_TOKEN`, but GitHub Actions needs the additional
  Codacy coverage identity variables required by Codacy's account-token upload flow unless the upload
  command can infer them reliably in CI.

Unknowns:

- Whether all current Codacy issues should be fixed in code/docs, disabled as false/noisy policy, or
  split case-by-case between fixes and rule tuning. The user has stated the outcome: zero findings.
- The repository does not currently list `CODACY_ORGANIZATION_PROVIDER`, `CODACY_USERNAME`, or
  `CODACY_PROJECT_NAME` as GitHub repository variables. During implementation, either define safe
  non-secret environment values in the workflow or add repository variables if preferred.

### Acceptance Criteria

- GitHub Code Scanning is enabled by committed workflow configuration and uploads CodeQL results for the
  relevant languages.
- Codacy Analysis CLI runs in GitHub Actions, emits SARIF, and uploads it to GitHub Code Scanning with a
  distinct category.
- Codacy token usage is documented in workflow comments or SOW, but no raw tokens are written to files.
- Existing active Codacy issues do not make every build fail unless the user explicitly chooses strict
  enforcement.
- Codacy Cloud is reanalyzed after fixes/configuration and reports zero active issues/security findings
  for `master` or the branch under review.
- GitHub Code Scanning reports no active alerts from the newly configured CodeQL and Codacy SARIF
  categories after the final workflow run.
- Rust coverage is generated in CI and uploaded to Codacy using `CODACY_API_TOKEN`, without writing any
  raw token value to repository files.
- Coverage upload requirements are documented with placeholders only, including any required Codacy
  organization/project identity variables.
- Workflow YAML is syntax-checked locally.
- The workflow design accounts for fork pull requests and missing Codacy secrets.
- GitHub/Codacy official docs and at least one open-source workflow reference are recorded.

## Analysis

Sources checked:

- `.agents/sow/current/SOW-0017-20260531-component-report-model.md`
- `.agents/sow/current/SOW-0018-20260601-signal-label-schema.md`
- `.agents/sow/SOW.template.md`
- `Cargo.toml:1` through `Cargo.toml:46`
- `.gitignore:1` through `.gitignore:14`
- `aiolos/src/assets/aiolos.js:25` through `aiolos/src/assets/aiolos.js:34`
- `aiolos/src/assets/aiolos.js:760` through `aiolos/src/assets/aiolos.js:773`
- `tech/ipmi/src/lib.rs:139`
- `packaging/update.sh:11` through `packaging/update.sh:18`
- `AGENTS.md:28`
- GitHub API: repository metadata, workflow permissions, workflow list, CodeQL default setup.
- Codacy Cloud CLI: repository overview, issue overview, enabled tools, security findings.
- Codacy Analysis CLI: discovery, SARIF-capable help output, temporary remote-config probe.
- Official GitHub docs: uploading SARIF with `github/codeql-action/upload-sarif`.
- Official GitHub docs: SARIF support and fingerprint/path stability.
- Official Codacy docs: client-side tools, GitHub integration, build-server analysis requirement.
- GitHub Marketplace: Codacy Analysis CLI Action usage and token requirements.

Current state:

- No `.github/` directory exists.
- No `.codacy/` configuration exists in the repo.
- No coverage workflow/configuration exists.
- `.gitignore` ignores Rust build artifacts and local agent/editor files only.
- The repository contains Rust workspace crates, embedded browser assets, shell packaging, Markdown docs,
  JSON/TOML/config files, and systemd service files.
- Representative Codacy findings include JavaScript `innerHTML` at `aiolos/src/assets/aiolos.js:29`,
  JavaScript generated HTML at `aiolos/src/assets/aiolos.js:771`, Rust `unsafe` at
  `tech/ipmi/src/lib.rs:139`, and a ShellCheck finding in `packaging/update.sh:17`.

Risks:

- Strict failure on current Codacy findings would make CI useless immediately.
- The expanded zero-findings requirement is no longer a small CI plumbing change; it may require broad
  documentation/style cleanup, code changes, security review, Codacy rule tuning, and remote reanalysis
  cycles.
- Pull-request jobs that require Codacy secrets will not run for untrusted forks unless guarded.
- Uploading multiple SARIF producers without stable categories can confuse GitHub Code Scanning results.
- Committing the full Codacy remote config may freeze a very broad, noisy policy into the repo and still
  would not change Codacy Cloud unless imported.
- Fetching Codacy config in CI avoids committing broad config, but requires an Actions secret and an
  authenticated Codacy account/API token.
- Uploading coverage with an account token may fail if required Codacy identity variables are missing or
  wrong. Mitigation: use documented placeholders/env values and validate in GitHub Actions before
  closing.

## 2026-06-06 Quality Baseline

Live Codacy state for `ktsaou/aiolos` on `master`:

- Last analyzed commit: `177a45a5cc7b568c22378e316535e793c9d7fd11`.
- Issues: 948.
- Complex files: 29%.
- Duplication: 23%.
- Codacy goals currently configured: max issue percentage 20, max duplicated files 10%, minimum
  coverage 60%, max complex files 10%, file complexity threshold 20.
- Enabled tools: PMD, markdownlint, ESLint, Lizard, Agentlinter, Opengrep, Jackson Linter,
  ShellCheck, Stylelint, and Trivy.
- Remote Codacy config has no excludes and a broad tool policy:
  PMD 229 patterns, markdownlint 43, ESLint8 955, Lizard 4, Agentlinter 102, Semgrep 2310,
  Jackson 2, ShellCheck 440, Stylelint 134, Trivy 6.

Issue distribution:

- By language: Markdown 720, JavaScript 82, Rust 81, CSS 50, Shell 15.
- By category: CodeStyle 677, BestPractice 69, Security 61, Complexity 55, ErrorProne 47,
  Comprehensibility 19, Compatibility 18, Performance 2.
- By severity: Info 732, Warning 116, High 91, Error 9.
- Top patterns:
  - markdownlint `MD022`: 316.
  - markdownlint `MD032`: 297.
  - markdownlint `MD033`: 32.
  - Lizard NLOC medium: 29.
  - Agentlinter duplicate instructions: 22.
  - Lizard CCN medium: 19.
  - Stylelint alpha/color notation: 32 total.
  - Rust unsafe usage: 15.
  - Rust `temp_dir` for security operations: 9.
  - Rust `args` for security operations: 6.

Hot files by issue count:

- `aiolos/src/assets/aiolos.js`: 82 issues, including XSS/HTML-template/security findings and Lizard
  complexity.
- `AGENTS.md`: 70 issues, mostly agent/documentation linting.
- `aiolos/src/assets/aiolos.css`: 50 issues, mostly Stylelint policy/style.
- `.agents/sow/**`: many Markdown findings in historical work ledgers.
- `aiolos/src/status_page.rs`, `aiolos/src/main.rs`, `anemos/src/run.rs`,
  `anemoi/rome2d-fans/src/main.rs`, and `anemoi/it87/src/main.rs`: complexity/NLOC findings.

GitHub state:

- No GitHub Actions workflows exist.
- CodeQL default setup is `not-configured`.
- GitHub reports CodeQL default-setup languages as JavaScript/TypeScript and Rust, but no schedule is
  configured.
- `CODACY_API_TOKEN` exists as a repository secret; no Codacy repository variables exist.
- No existing coverage workflow or coverage configuration exists.

Official docs checked:

- GitHub SARIF upload docs: third-party SARIF can be uploaded to Code Scanning; `upload-sarif` uses
  `sarif_file` and optional `category`; workflows need `security-events: write`; unique categories
  are needed for multiple analyses.
- GitHub SARIF support docs: fingerprints prevent duplicate alerts; `upload-sarif` can populate
  partial fingerprints when source files are present.
- Codacy coverage docs: coverage must be generated on CI and uploaded for each commit; supported
  formats include LCOV and Cobertura.
- Codacy Files/API docs: per-file metrics include issues, complexity, duplicated blocks, and coverage;
  the API can export these metrics for cleanup ordering.

Reality check:

- The repo is not "948 real code bugs." Most findings are Markdown/style policy issues in historical
  process files. Treating them as product-code failures would create large churn with little quality
  value.
- The repo does have real quality problems: JavaScript status-page DOM construction, Rust unsafe audit
  visibility, Shell quoting/printf issues, and long/complex Rust and JavaScript functions.
- The 23% duplication metric cannot be responsibly fixed until per-file duplication data is exported
  from Codacy; guessing from aggregate percentage would be wasteful.

## Pre-Implementation Gate

Status: needs-user-decision

Problem / root-cause model:

- GitHub currently has no Actions workflows and CodeQL default setup is disabled, so GitHub Code Scanning
  is not receiving results. Codacy Cloud is already analyzing the repo but its current configuration is
  broad enough to produce 948 issues, mostly Markdown/CodeStyle noise. The workspace has tests but no
  coverage upload. Evidence: no `.github/` files, GitHub workflow count 0, CodeQL default setup
  `not-configured`, Codacy overview with 948 issues, Rust tests found by `#[test]`/integration-test
  search, and no existing coverage workflow/configuration.

Evidence reviewed:

- Local repo shape: `Cargo.toml:1` through `Cargo.toml:18` lists the Rust workspace members; `.gitignore:1`
  through `.gitignore:14` has no Codacy/GitHub workflow exclusions or config.
- Local Codacy finding examples: `aiolos/src/assets/aiolos.js:29`,
  `aiolos/src/assets/aiolos.js:771`, `tech/ipmi/src/lib.rs:139`, `packaging/update.sh:17`.
- Coverage setup evidence: Rust workspace tests exist in many crates plus `aiolos/tests/orchestrator.rs`;
  no `.github/workflows/`, `.codacy/`, or coverage upload configuration exists.
- GitHub docs say third-party SARIF uploads use `github/codeql-action/upload-sarif`, require
  `security-events: write`, and support an optional `category`.
- GitHub SARIF docs stress stable file paths and fingerprints to avoid duplicate alerts.
- Codacy docs say client-side tools can run locally/build-server and upload results to Codacy, and GitHub
  users are recommended to use Codacy's GitHub Action for client-side tools.
- Codacy docs say open-source GitHub repositories need Codacy Status checks enabled for Codacy to analyze.
- Codacy coverage setup requires either a repository/project token or an account token plus provider,
  organization/username, and project-name identity variables.
- Temporary Codacy remote-config probe confirmed the Cloud config can be fetched and inspected without
  writing repo files.

Affected contracts and surfaces:

- `.github/workflows/*.yml` new CI/security workflows.
- Optional `.codacy/codacy.config.json` if the user chooses committed local config.
- Optional `.gitignore` changes if generated Codacy artifacts are used locally.
- Optional Rust coverage tooling/configuration.
- GitHub repository Security / Code Scanning alerts.
- Codacy Cloud project analysis behavior if the user chooses to import/tune configuration.
- Codacy coverage reporting.
- Future PR merge behavior if the user chooses strict failure gates.

Existing patterns to reuse:

- Local shell scripts already use explicit, visible command execution; if a helper script is added, mirror
  `packaging/update.sh:11` through `packaging/update.sh:18`.
- The repo has no prior GitHub workflow pattern; use official GitHub/Codacy examples plus conservative
  least-privilege permissions.
- Keep CI scoped and explicit, matching the repo's lean style.

Risk and blast radius:

- Low code-runtime risk: this is CI/configuration only.
- Medium-to-high operational risk: strict gates, wrong secret handling, or broad zero-finding cleanup could
  block development or fail all PRs.
- Medium security risk: token misuse in workflows can expose Codacy permissions. Mitigation: secrets only,
  no `pull_request_target`, no token on fork PRs, explicit least-privilege GitHub permissions.
- Medium noise/policy risk: reaching zero findings may require disabling noisy rules; every rule change
  needs evidence so real issues are not hidden.
- Medium coverage risk: Rust coverage tools may need CI packages/components and can be slower than normal
  tests.

Sensitive data handling plan:

- Do not write tokens, account emails, personal names, generated credentials, or raw sensitive CLI output
  to durable artifacts.
- Use secret placeholders only, such as `secrets.CODACY_API_TOKEN`, `secrets.CODACY_PROJECT_TOKEN`, or
  non-secret project identity values.
- Do not record Codacy author/account fields in SOWs or docs.

Implementation plan:

1. Get user decisions for D8-D12 before implementation.
2. Export Codacy per-file metrics and full issue inventory, including duplication details.
3. Apply approved analysis boundaries and rule policy.
4. Fix real security/error-prone findings first.
5. Refactor complexity and duplication hot spots using tests to preserve behavior.
6. Add GitHub CodeQL, Codacy SARIF, CI, and Rust coverage upload workflows with full-SHA action pins.
7. Reanalyze Codacy and GitHub Code Scanning until the final remote state satisfies the quality bar.
8. Record validation and every ignore/rule-disable justification.

Validation plan:

- `git diff --check`
- YAML syntax validation with an available parser/tool.
- `cargo test`
- Rust coverage generation command selected during implementation, producing a Codacy-supported report.
- `codacy-analysis analyze --inspect` with the selected configuration path.
- If a committed/fetched config exists locally, generate a SARIF file in `/tmp` and confirm it contains
  SARIF `version` and `runs`.
- Codacy coverage upload dry-run or GitHub Actions run evidence showing coverage submitted to Codacy.
- Codacy Cloud reanalysis evidence showing zero active issues/security findings.
- GitHub Code Scanning evidence showing no active alerts from CodeQL and Codacy SARIF categories.
- If workflow files are added, confirm `security-events: write` is present only on SARIF/CodeQL jobs.
- Same-failure scan: search workflows for `pull_request_target`, unguarded Codacy secrets, missing
  `contents: read`, missing `security-events: write`, and duplicate SARIF categories.

Artifact impact plan:

- AGENTS.md: likely unaffected; this does not change runtime/protocol/operator behavior.
- Runtime project skills: unaffected; no anemos/protocol work.
- Specs: unaffected; no runtime behavior or protocol contract changes.
- End-user/operator docs: may need a contributor/CI note if coverage or security scanning commands become
  part of expected development workflow.
- End-user/operator skills: unaffected.
- SOW lifecycle: active in `.agents/sow/current/` by explicit user exception despite existing current
  SOWs; on completion, set `Status: completed`, move to `.agents/sow/done/`, and commit SOW +
  workflow/config/code changes together.

Open-source reference evidence:

- `infracost/infracost @ 47f3938407e6e8606e44703dee89d53ad87b3350`,
  `.github/workflows/code-scanning.yml:3` through `.github/workflows/code-scanning.yml:35`: CodeQL on
  push, pull request, schedule with `security-events: write`.
- `grafana/agent @ 372611b64f2cace1b684c43f6cf0b265b07dbee1`,
  `.github/workflows/trivy.yml:14` through `.github/workflows/trivy.yml:42`: least-privilege job
  permissions and third-party SARIF upload.
- `charmbracelet/crush @ aeda508da29bc2f6e22e84c97007c87d83496466`,
  `.github/workflows/security.yml:10` through `.github/workflows/security.yml:78`: separate CodeQL and
  third-party SARIF jobs, concurrency, pinned actions, and explicit `security-events: write`.
- `prometheus/prometheus @ c0b4c5ef183275641526cebb01898b0821fbd527`,
  `.github/workflows/codeql-analysis.yml:9` through `.github/workflows/codeql-analysis.yml:39`: top-level
  empty permissions with job-level `contents: read` and `security-events: write`.

Open decisions:

1. **D1 - Scope**
   - **A. CodeQL + Codacy SARIF to GitHub Code Scanning.** Pros: satisfies GitHub Code Scanning and adds
     Codacy-originated SARIF visibility. Cons: GitHub will show many current Codacy alerts unless tuned.
     Risk: duplicated-looking alerts between CodeQL and Codacy.
   - **B. Codacy SARIF only.** Pros: smaller change, focuses on the user's Codacy context. Cons: misses
     first-party GitHub CodeQL scanning; GitHub CodeQL remains not configured.
   - **C. CodeQL only, leave Codacy in Codacy Cloud.** Pros: clean GitHub setup and no Codacy token.
     Cons: does not add Codacy SARIF.
   - **Recommendation: A.** This directly matches "GitHub Code Scanning + Codacy SARIF" and can be kept
     non-blocking while the baseline is triaged.
   - **Decision: A.** Selected by the user on 2026-06-04.

2. **D2 - Codacy configuration source for CI**
   - **A. Fetch Codacy Cloud config in CI with `CODACY_API_TOKEN`.** Pros: no huge committed Codacy config;
     mirrors the Codacy UI configuration. Cons: requires a GitHub Actions secret; fork PRs cannot use it.
     Risk: workflow must be carefully guarded to avoid token exposure.
   - **B. Commit `.codacy/codacy.config.json`.** Pros: no Codacy secret required for GitHub SARIF and fork
     PRs can run. Cons: commits thousands of broad pattern entries; local config does not affect Codacy
     Cloud unless imported. Risk: freezes noisy policy into the repo.
   - **C. Use Codacy's GitHub Action only for Codacy upload, not GitHub SARIF.** Pros: official Codacy
     upload path. Cons: does not by itself populate GitHub Code Scanning with Codacy SARIF.
   - **Recommendation: A.** It keeps the repo lean and uses Codacy Cloud as the policy source. Add an
     explicit manual requirement for `CODACY_API_TOKEN` or `CODACY_PROJECT_TOKEN`.
   - **Decision: A.** Selected by the user on 2026-06-04. The user reports `CODACY_API_TOKEN` exists as a
     repository secret.

3. **D3 - Failure policy**
   - **A. Baseline visibility only: do not fail on existing Codacy issues.** Pros: CI becomes usable
     immediately. Cons: issues are visible but not enforced.
   - **B. Fail on any Codacy issue.** Pros: strict. Cons: current 946 issues will block every run.
   - **C. Fail only on new PR issues after a baseline is established.** Pros: best long-term policy.
     Cons: more complex and should follow baseline/tuning work.
   - **Recommendation: A now, C as follow-up.** Strict enforcement before triage is counterproductive.
   - **Decision: A.** Selected by the user on 2026-06-04. CI remains visibility-only for findings, while
     this SOW's completion criteria require zero final findings.

4. **D4 - Workflow events**
   - **A. CodeQL on push/PR/schedule; Codacy SARIF on push/schedule and same-repo PRs only when secrets
     are available.** Pros: good coverage without exposing secrets to forks. Cons: fork PRs get CodeQL but
     not Codacy SARIF.
   - **B. Push/schedule only.** Pros: simplest and safest. Cons: no PR-time GitHub Code Scanning feedback.
   - **C. Push/PR/schedule for everything using committed Codacy config.** Pros: PR feedback everywhere.
     Cons: requires D2-B and commits noisy config.
   - **Recommendation: A.** It gives PR feedback where safe and avoids `pull_request_target`.
   - **Decision: A.** Selected by the user on 2026-06-04.

5. **D5 - Action pinning**
   - **A. Pin to major version tags, for example `actions/checkout@v6` and `github/codeql-action/*@v4`.**
     Pros: readable and follows official examples. Cons: tags are mutable.
   - **B. Pin to full commit SHAs with comments naming the version.** Pros: stronger supply-chain
     control. Cons: manual updates and more verbose workflow files.
   - **Recommendation: B.** This is a security workflow; stronger pinning is worth the maintenance.
   - **Decision: B.** Selected by the user on 2026-06-04.

6. **D6 - Codacy noise reduction in this SOW**
   - **A. Do not tune Codacy now; add scanning plumbing only.** Pros: small, reversible, no policy change.
     Cons: GitHub/Codacy will still show a noisy baseline.
   - **B. Also tune Codacy Cloud/local config to reduce Markdown/CodeStyle noise.** Pros: makes the 946
     issues more actionable. Cons: changes quality policy and may detach from an org coding standard.
     Risk: hiding real style/doc issues if done too aggressively.
   - **C. Fix a first batch of high/security findings.** Pros: improves real code. Cons: materially expands
     scope beyond CI setup.
   - **Recommendation: A for this SOW, then open a follow-up for B and selected C.** The plumbing and the
     quality policy should not be mixed unless the user explicitly wants that.
   - **D. Eliminate all noise and fix all issues until zero findings are reported; also submit coverage to
     Codacy.** Pros: produces a clean, useful baseline immediately. Cons: much larger scope than scanning
     plumbing. Risks: broad Markdown/code churn, possible Codacy policy changes, remote reanalysis delays,
     and a real chance of uncovering security/code fixes that need deeper review.
   - **Decision: D.** Selected by the user on 2026-06-04.

7. **D7 - Activation despite current-SOW conflict**
   - **A. Activate SOW 19 now despite two existing current SOWs.** Pros: starts the requested security and
     coverage work immediately. Cons: violates the normal one-SOW-at-a-time operating rule unless
     explicitly accepted by the user. Risk: more parallel work-in-progress.
   - **B. Keep SOW 19 pending until SOW 17 and SOW 18 are paused/completed.** Pros: follows the project SOW
     discipline. Cons: delays GitHub/Codacy work.
   - **Recommendation: B unless the user explicitly wants this security/coverage work to preempt the active
     SOWs.**
   - **Decision: A.** Selected by the user on 2026-06-06 with the request to open SOW 19 and plan a
     best-in-class open-source quality program.

8. **D8 - Codacy analysis boundary**
   - **A. Analyze everything, including historical SOW ledgers.** Pros: no exclusions. Cons: the cleanup
     becomes dominated by old work logs and template repetition. Risk: large low-value Markdown churn and
     misleading duplication.
   - **B. Exclude historical SOW ledgers only: `.agents/sow/{pending,current,done}/**`; keep product code,
     public docs, specs, and skills quality-gated.** Pros: removes low-value historical noise while keeping
     source, operator docs, specs, and skills honest. Cons: SOW Markdown style is not enforced by Codacy.
     Risk: SOW history can accumulate style issues, but that is acceptable because it is a work ledger.
   - **C. Exclude all `.agents/**`.** Pros: fastest noise reduction. Cons: hides specs and project skills
     that are part of how contributors/agents work in this repo. Risk: real documentation problems in
     specs/skills can be missed.
   - **Recommendation: B.** Best-in-class quality should measure product and maintained docs, not historical
     process ledgers.

9. **D9 - Markdown/style policy**
   - **A. Reformat every Markdown file to strict markdownlint.** Pros: no rule tuning. Cons: heavy churn in
     SOW history and possible loss of readable work-log formatting.
   - **B. Disable noisy Markdown rules globally.** Pros: fast. Cons: hides real documentation quality issues
     in README/DESIGN/specs/skills.
   - **C. Keep Markdown rules for maintained docs, but exclude SOW ledgers and fix/tune remaining docs with
     evidence.** Pros: high-signal policy. Cons: requires a small amount of rule/config work and doc cleanup.
   - **Recommendation: C.** This removes noise without lowering the bar for public docs.

10. **D10 - Security findings**
   - **A. Fix every security finding in code and do not ignore anything.** Pros: strictest interpretation.
     Cons: not realistic for necessary Rust `unsafe` and test-only temp dirs; could force worse code.
   - **B. Fix real JavaScript/Shell/Rust security issues; audit and document necessary `unsafe`/test-only
     findings; mark only proven accepted-use/false-positive cases in Codacy.** Pros: zero active findings
     with a real audit trail. Cons: requires disciplined justification comments and Codacy ignores.
   - **C. Ignore all current security findings as noise.** Pros: fast. Cons: unacceptable for open-source
     quality; likely hides real XSS issues.
   - **Recommendation: B.** JavaScript DOM/XSS and Shell issues should be fixed; necessary Rust `unsafe`
     should be isolated, documented, tested, and then accepted only if still reported.

11. **D11 - Quality targets**
   - **A. Meet current Codacy goals only: complexity <=10%, duplication <=10%, coverage >=60%.** Pros:
     matches existing Codacy policy. Cons: not best-in-class.
   - **B. Best-in-class target: zero active issues/security findings, complex files <=5%, duplication <=3%,
     coverage >=80%, no PR regression.** Pros: clear high bar suitable for a public Rust project. Cons:
     larger refactor and test work.
   - **C. Absolute perfection: 0% complex files and 0% duplication.** Pros: simple target. Cons: unrealistic
     and may create worse abstractions just to satisfy metrics.
   - **Recommendation: B.** It is ambitious but technically meaningful.

12. **D12 - Final gate behavior**
   - **A. Visibility only forever.** Pros: never blocks development. Cons: quality will regress.
   - **B. Phased gates: visibility during cleanup; after clean baseline, block PRs on new Critical/High,
     security findings, failed tests, coverage regression, and Codacy/GitHub scan failures.** Pros:
     practical migration path with a real final gate. Cons: requires a baseline transition step.
   - **C. Immediately block on every existing issue.** Pros: strict. Cons: all CI becomes red until the full
     cleanup lands.
   - **Recommendation: B.** This avoids a useless red CI while still ending with strict quality control.

## Implications And Decisions

1. **D1: A - CodeQL + Codacy SARIF to GitHub Code Scanning.**
   - Implication: GitHub will receive first-party CodeQL results and Codacy-originated SARIF in separate
     categories.

2. **D2: A - Fetch Codacy Cloud config in CI with `CODACY_API_TOKEN`.**
   - Implication: the repository stays lean, but Codacy jobs must be guarded so secrets are not used on
     untrusted fork pull requests.

3. **D3: A - Visibility-only failure policy for Codacy findings.**
   - Implication: CI will not fail just because Codacy reports findings, but this SOW cannot close until
     Codacy/GitHub report zero active findings after reanalysis.

4. **D4: A - CodeQL on push/PR/schedule; Codacy SARIF on push/schedule and same-repo PRs only.**
   - Implication: fork PRs get CodeQL feedback but skip Codacy jobs that need secrets.

5. **D5: B - Pin GitHub Actions to full commit SHAs.**
   - Implication: stronger supply-chain control with more manual maintenance during action upgrades.

6. **D6: D - Eliminate all noise and fix all issues until zero findings are reported; submit coverage to
   Codacy.**
   - Implication: this is now a broad quality/security cleanup and coverage setup SOW, not just CI
     plumbing. Findings must be fixed, justified as false positives, or removed through evidence-backed
     rule tuning.

7. **D7: A - Activate SOW 19 now despite the current-SOW conflict.**
   - Implication: SOW 19 moves to `current/` while SOW 17 and SOW 18 remain present in `current/`.
     This is an explicit exception accepted by the user because Codacy quality is now urgent.

8. **D8-D12 remain open before implementation.**
   - Implication: the plan is ready, but source/config changes need the user's policy choices first.

## Best-In-Class Quality Plan

### Quality Bar

Target end state:

- Codacy Cloud: 0 active issues and 0 active security findings on `master`.
- GitHub Code Scanning: 0 active alerts from CodeQL and Codacy SARIF categories.
- Complexity: <=5% complex files after excluding historical SOW ledgers.
- Duplication: <=3% duplicated files/blocks after excluding historical SOW ledgers.
- Coverage: >=80% overall Rust line coverage, generated on every push and uploaded to Codacy.
- CI: `cargo fmt`, `cargo clippy --all-targets`, `cargo test`, coverage, CodeQL, Codacy SARIF,
  ShellCheck/actionlint/YAML validation, and Codacy coverage upload.
- PR gate after cleanup: no new Critical/High/security findings, no coverage regression, no failed
  required checks.

### Phase 0 - Inventory Before Fixing

1. Export Codacy per-file metrics through the Codacy API for issues, complexity, duplication, and
   coverage. This is required because the repository dashboard gives only aggregate duplication.
2. Export the full issue list grouped by file, pattern, severity, category, and language.
3. Pull the remote Codacy config into `/tmp`, not the repo, and classify tools/patterns as:
   - keep as-is,
   - tune,
   - disable as irrelevant,
   - exclude by path,
   - fix in source.
4. Produce a machine-readable cleanup ledger in `/tmp` while working, and record only sanitized
   summaries in this SOW.

### Phase 1 - Codacy Policy And Scope

1. Apply D8/D9 once approved.
2. Recommended policy:
   - exclude `.agents/sow/{pending,current,done}/**` from Codacy quality metrics;
   - keep `README.md`, `DESIGN.md`, `AGENTS.md`, `.agents/sow/specs/**`, and `.agents/skills/**`
     quality-gated;
   - keep Markdown rules for maintained docs, with narrow exceptions only when documented;
   - keep security, error-prone, complexity, ShellCheck, Rust, JS, and CSS analysis enabled.
3. Import the tuned Codacy policy to Cloud only after local validation proves the issue count falls for
   the right reasons.

### Phase 2 - Real Security And Error-Prone Fixes

1. JavaScript status page:
   - replace `innerHTML`/HTML-template construction with safe DOM construction or a small explicit
     sanitizer boundary;
   - remove jQuery-style unsafe append patterns if present;
   - replace weak/random placeholder behavior if it is not security-relevant, or document why it is not
     used for security.
2. Shell scripts:
   - fix unquoted expansion findings;
   - fix `printf` format-string findings while preserving the existing transparent `run()` style.
3. Rust:
   - audit all `unsafe` blocks;
   - isolate unsafe operations into small functions;
   - add `SAFETY:` comments explaining invariants;
   - add tests around unsafe wrappers where possible;
   - replace `std::env::temp_dir()` in tests with safer tempdir helpers where practical;
   - accepted-use ignore only for necessary audited unsafe/test-only findings that still trigger
     generic rules.

### Phase 3 - Complexity Reduction

Prioritize files with real product impact:

1. `aiolos/src/assets/aiolos.js`: split the 819-line status-page script into small modules/helpers;
   reduce `escapeHtml`/DOM helper complexity; remove unsafe HTML generation.
2. `aiolos/src/status_page.rs`: split request handling, JSON rendering, metrics rendering, curve JSON,
   ANSI stripping, and aggregation into focused modules/functions.
3. `aiolos/src/main.rs`: split orchestration command handling, dispatch, restore, and input routing.
4. `anemos/src/run.rs`: split lifecycle, one-shot info/collect, apply loop, and signal handling.
5. `anemoi/rome2d-fans/src/main.rs` and `anemoi/it87/src/main.rs`: extract report construction and
   decision/provenance helpers.
6. `protocol/src/lib.rs`: split tests and large wire contract cases if Lizard still counts them after
   path/tool tuning.

### Phase 4 - Duplication Reduction

1. Use Codacy file metrics/API to identify actual duplicate blocks.
2. Expected likely duplicate areas to check:
   - repeated signal/component report construction across anemoi;
   - duplicated fan sink/provenance builders in `nvidia`, `it87`, and `rome2d-fans`;
   - repeated test harness helpers in protocol/orchestrator/anemos tests;
   - repeated Markdown/SOW templates if D8 is not accepted.
3. Add abstractions only where they reduce real maintenance risk:
   - shared report/signal builders in `anemos` when multiple modules already follow the same pattern;
   - test helper modules for repeated protocol/device fixtures;
   - no abstraction for one-off code just to satisfy a metric.

### Phase 5 - Coverage

1. Add Rust coverage generation with `cargo llvm-cov`, producing LCOV or Cobertura from the whole
   workspace.
2. Upload coverage to Codacy on every push using `CODACY_API_TOKEN` and non-secret identity values:
   provider `gh`, organization/user `ktsaou`, project `aiolos`.
3. Make file paths relative to repo root.
4. Add tests where coverage exposes real risk:
   - status page rendering and escaping;
   - registry/input routing;
   - protocol error handling;
   - anemos lifecycle/fail-safe behavior;
   - hardware tech crates via fixtures/mocks.

### Phase 6 - GitHub And Codacy Gates

1. Add GitHub Actions:
   - `ci.yml`: fmt, clippy, tests, coverage.
   - `codeql.yml`: CodeQL for Rust and JavaScript/TypeScript.
   - `codacy.yml`: Codacy SARIF generation/upload and coverage upload.
2. Use pinned GitHub Actions full SHAs, per D5.
3. Use least-privilege permissions:
   - `contents: read` by default;
   - `security-events: write` only in CodeQL/SARIF jobs.
4. Avoid `pull_request_target`.
5. Guard Codacy-token jobs so fork PRs cannot access secrets.
6. Use stable Code Scanning categories for CodeQL and Codacy SARIF.

### Phase 7 - Remote Reanalysis And Closure

1. Trigger Codacy reanalysis after each meaningful cleanup batch.
2. Verify:
   - 0 active Codacy issues;
   - 0 active Codacy security findings;
   - Codacy complexity and duplication are below target;
   - Codacy shows coverage for the analyzed commit;
   - GitHub Code Scanning has no active alerts from the new categories.
3. Record every Codacy ignore/rule disable with:
   - pattern;
   - affected path;
   - reason;
   - why it is not hiding real risk.
4. Close the SOW only after local tests, CI, Codacy, GitHub Code Scanning, and coverage evidence are all
   recorded.

## Execution Log

### 2026-06-02

- Created pending SOW after checking current SOWs, repo state, GitHub state, Codacy state, official docs,
  and open-source workflow references.

### 2026-06-04

- Recorded user decisions D1-D6.
- Expanded scope to include zero Codacy/GitHub scanning findings and Codacy coverage upload.
- Rechecked live GitHub/Codacy state: zero GitHub workflows, CodeQL default setup `not-configured`,
  workflow default token permission `read`, and 946 Codacy issues still reported.
- Verified `CODACY_API_TOKEN` exists as a GitHub repository secret without reading any secret value; no
  GitHub repository variables are currently listed.
- Confirmed this remains pending because D7 activation/current-SOW handling is still open.

### 2026-06-06

- The user activated SOW 19 and asked for a plan to make aiolos best-in-class open-source software,
  citing Codacy's current poor quality indicators: 948 issues, 29% complexity, and 23% duplication.
- Recorded D7 as accepted and moved this SOW to `current/`.

## Validation

Acceptance criteria evidence:

- Pending implementation.

Tests or equivalent validation:

- Pending implementation.

Real-use evidence:

- Pending implementation.

Reviewer findings:

- Pending implementation.

Same-failure scan:

- Pending implementation.

Sensitive data gate:

- No raw tokens, account emails, personal names, or sensitive Codacy/GitHub credential values were written
  to this SOW.

Artifact maintenance gate:

- AGENTS.md: pending final assessment.
- Runtime project skills: pending final assessment.
- Specs: pending final assessment.
- End-user/operator docs: pending final assessment.
- End-user/operator skills: pending final assessment.
- SOW lifecycle: active in `current/`; implementation is blocked on D8-D12 quality-policy decisions.

Specs update:

- Pending final assessment.

Project skills update:

- Pending final assessment.

End-user/operator docs update:

- Pending final assessment.

End-user/operator skills update:

- Pending final assessment.

Lessons:

- Pending.

Follow-up mapping:

- Pending after expanded scope. Codacy baseline/noise tuning is now in scope rather than a follow-up.

## Outcome

Pending.

## Lessons Extracted

Pending.

## Followup

None yet.

## Regression Log

None yet.
