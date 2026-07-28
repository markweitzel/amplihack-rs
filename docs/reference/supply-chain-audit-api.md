# `amplihack-supply-chain-audit` — Supply Chain Audit Reference

> [Home](../index.md) > [Reference](../index.md#-reference--resources) > amplihack-supply-chain-audit API

The **`amplihack-supply-chain-audit`** crate is the native Rust implementation of
the `supply-chain-audit` skill. It audits software supply chain security across
CI/CD pipelines, container images, and language package ecosystems, and emits
structured findings with severity ratings, `file:line` references, and
copy-pasteable fix templates.

It ships as both a **library** (`amplihack_supply_chain_audit`) and a
**standalone binary** (`amplihack-supply-chain-audit`). The skill's
`SKILL.md` Prerequisites invoke the binary directly; other crates and tools may
depend on the library API.

The crate replaces the upstream Python package `supply_chain_audit/` with an
equivalent, dependency-light Rust implementation. Detection logic, the finding
schema, report format, error conditions, and security invariants are preserved
verbatim so that existing eval scenarios and the SKILL.md contract remain
stable.

## Contents

- [When to use it](#when-to-use-it)
- [Design guarantees](#design-guarantees)
- [Crate layout](#crate-layout)
- [Command-line usage](#command-line-usage)
  - [Synopsis](#synopsis)
  - [Options](#options)
  - [Exit codes](#exit-codes)
  - [Invocation examples](#invocation-examples)
- [The 12 audit dimensions](#the-12-audit-dimensions)
- [Scope detection and mapping](#scope-detection-and-mapping)
- [The finding schema](#the-finding-schema)
- [The report](#the-report)
  - [Markdown report (default)](#markdown-report-default)
  - [JSON report](#json-report)
- [External tools](#external-tools)
- [Configuration](#configuration)
  - [Accepted risks file](#accepted-risks-file)
  - [Environment variables](#environment-variables)
- [Library API reference](#library-api-reference)
  - [`run_audit`](#run_audit)
  - [`AuditConfig`](#auditconfig)
  - [`AuditResult`](#auditresult)
  - [`Finding` and `FindingId`](#finding-and-findingid)
  - [`Severity`](#severity)
  - [`detect_ecosystems` and `EcosystemScope`](#detect_ecosystems-and-ecosystemscope)
  - [External-tool helpers](#external-tool-helpers)
  - [Error type: `SupplyChainAuditError`](#error-type-supplychainauditerror)
- [Security invariants](#security-invariants)
- [Tutorial: auditing a repository end to end](#tutorial-auditing-a-repository-end-to-end)
- [Testing](#testing)
- [FAQ](#faq)

---

## When to use it

Reach for `amplihack-supply-chain-audit` whenever you need to:

- **Gate a PR** on High/Critical supply chain regressions before merge.
- **Audit CI/CD** for unpinned action refs, over-broad `permissions:`, or
  secret leakage in `run:` steps.
- **Check dependency pinning** across Python, Node, Go, Rust, and .NET.
- **Assess container supply chain** — mutable base tags, non-root execution,
  multi-stage minimal final images.
- **Map SLSA readiness** and drive SBOM generation guidance.

It is a **read-only** tool. It never modifies the repository under audit, never
escalates privileges, and never emits credential values.

---

## Design guarantees

The crate honours the same non-negotiable contract as the rest of amplihack:

- **`#![forbid(unsafe_code)]` crate-wide.** No `unsafe` blocks anywhere.
- **No panics on untrusted input.** All parsing returns `Result`; the binary
  maps errors to stderr and exit codes rather than unwinding.
- **No silent fallbacks.** A missing external tool degrades to an explicit
  `Info`-level note in the report — it never fails the audit silently.
- **Brick limit.** No source file exceeds 400 lines; large modules
  (`checkers/actions`, `report`) are split into submodules.
- **Schema parity.** JSON field names match the upstream `asdict()` snake_case
  keys exactly: the required `id`, `dimension`, `severity`, `file`, `line`,
  `current_value`, `expected_value`, `rationale`, `offline_detectable` plus the
  optional `tool_required`, `contains_secret`, `fix_url`, `accepted_risk`.

---

## Crate layout

```
crates/amplihack-supply-chain-audit/
├── Cargo.toml
├── README.md
└── src/
    ├── lib.rs              # public re-exports; #![forbid(unsafe_code)]
    ├── main.rs             # [[bin]] amplihack-supply-chain-audit (clap CLI)
    ├── error.rs           # SupplyChainAuditError (thiserror)
    ├── schema.rs          # Severity, Finding, FindingId, validation, serde
    ├── detector.rs        # scope allowlist + ecosystem detection
    ├── audit/
    │   ├── mod.rs         # run_audit entry + AuditResult wiring
    │   ├── paths.rs       # path-traversal + symlink guard
    │   ├── risks.rs       # accepted-risks parse/apply + ID reassignment
    │   ├── xpia.rs        # advisory XPIA marker detection
    │   └── handoffs.rs    # inter-skill handoff + SLSA build
    ├── checkers/
    │   ├── mod.rs         # DimChecker registry
    │   ├── utils.rs       # relative_path, is_lock_file, load_workflows
    │   ├── actions/       # dims 1-4 (split: sha_pinning/permissions/…)
    │   ├── containers.rs  # dims 5, 12
    │   ├── credentials.rs # dim 6
    │   ├── dotnet.rs      # dim 7
    │   ├── python.rs      # dim 8
    │   ├── rust.rs        # dim 9
    │   ├── node.rs        # dim 10
    │   └── go.rs          # dim 11
    ├── external_tools/
    │   ├── mod.rs         # availability, install metadata, timeouts
    │   └── circuit.rs     # circuit breaker + backoff
    └── report/
        ├── mod.rs         # AuditReport render orchestration
        ├── sections.rs    # summary / findings / next-steps
        ├── slsa.rs        # SLSA L0/L1/L2 assessment
        └── json.rs        # AuditResult::to_json parity
```

---

## Command-line usage

### Synopsis

```
amplihack-supply-chain-audit [PATH] [OPTIONS]
```

`PATH` defaults to `.` (the current directory / repo root).

### Options

| Flag                       | Value                                             | Default    | Description                                                              |
| -------------------------- | ------------------------------------------------- | ---------- | ------------------------------------------------------------------------ |
| `--scope <LIST>`           | comma-separated: `gha`, `containers`, `credentials`, `dotnet`, `python`, `rust`, `node`, `go`, `all` | auto-detect | Restrict the audit to the given ecosystems / dimensions.                |
| `--min-severity <LEVEL>`   | `Critical` \| `High` \| `Medium` \| `Info`        | `Info`     | Report only findings at or above this severity.                          |
| `--json`                   | —                                                 | off        | Emit the machine-readable JSON report instead of markdown.               |
| `--summary-only`           | —                                                 | off        | Emit only the severity-count summary table (no per-finding detail).      |
| `--generate-sbom`          | —                                                 | off        | Attempt SBOM generation via `syft` (requires the tool; degrades to Info).|
| `--check-tools`            | —                                                 | off        | Print external-tool availability and install options, then exit.         |
| `-h`, `--help`             | —                                                 | —          | Print help.                                                              |
| `-V`, `--version`          | —                                                 | —          | Print version.                                                          |

Markdown is the default output. `--json` and `--summary-only` are mutually
exclusive with respect to formatting; if both are given, `--json` wins.

### Exit codes

| Code | Meaning                                                                                 |
| ---- | --------------------------------------------------------------------------------------- |
| `0`  | Audit completed; no findings at or above `--min-severity`.                              |
| `1`  | Audit completed; one or more findings at or above `--min-severity` were reported.       |
| `2`  | Usage error — bad flags, or `INVALID_SCOPE`.                                            |
| `3`  | Refused to run — `PATH_TRAVERSAL` or `ACCEPTED_RISKS_OVERFLOW`.                         |
| `4`  | Runtime error — unreadable audit root, I/O failure.                                     |

Exit code `1` is a deliberate "findings present" signal so the binary can be
used directly as a pre-merge gate (`amplihack-supply-chain-audit --min-severity High`).

### Invocation examples

```bash
# Full audit of the current repo, all detected ecosystems, all severities
amplihack-supply-chain-audit

# Audit a subdirectory only
amplihack-supply-chain-audit ./services/api

# GitHub Actions dimensions only, suppress Medium/Info, machine-readable
amplihack-supply-chain-audit --scope gha --min-severity High --json

# Pre-merge gate: fail the job on any High/Critical finding
amplihack-supply-chain-audit --min-severity High || exit 1

# Check which external tools are installed before a full run
amplihack-supply-chain-audit --check-tools
```

---

## The 12 audit dimensions

| #   | Dimension                  | Ecosystem      | Key check                                                       |
| --- | -------------------------- | -------------- | -------------------------------------------------------------- |
| 1   | Action SHA pinning         | GitHub Actions | `uses:` refs must be `@<40-char-SHA>  # vX.Y.Z`                |
| 2   | Workflow permissions       | GitHub Actions | Top-level `permissions: read-all`; job-level minimal grants     |
| 3   | Secret exposure            | GitHub Actions | No secrets in `run:` echo/env; `ACTIONS_STEP_DEBUG` guard       |
| 4   | Cache poisoning            | GitHub Actions | `actions/cache` key collision; restore-keys breadth             |
| 5   | Base image pinning         | Containers     | `FROM image@sha256:<digest>` not `:latest` or semver tag        |
| 6   | OIDC vs long-lived secrets | Credentials    | Prefer `id-token: write` OIDC; verify subject constraints       |
| 7   | NuGet lock & audit         | .NET / NuGet   | `RestoreLockedMode`, authorized sources, `NuGetAudit` gate      |
| 8   | Python dep integrity       | Python         | `--require-hashes`, `--extra-index-url` risks, typosquat signals |
| 9   | Cargo supply chain         | Rust           | `Cargo.lock` committed, `build.rs` risk, `[patch]`/`[replace]`  |
| 10  | Node.js integrity          | Node.js        | `npm ci` not `npm install`, `npx` resolution, `postinstall`     |
| 11  | Go module integrity        | Go             | `go.sum` committed, `GONOSUMCHECK`, `replace` directive scope    |
| 12  | Docker build chain         | Containers     | Multi-stage scratch/distroless final stage; non-root `USER`     |

The per-dimension check criteria, fix templates, and SHA/digest lookup
procedures are documented in the skill definition,
[`SKILL.md`](../../amplifier-bundle/skills/supply-chain-audit/SKILL.md)
("12 Audit Dimensions"), which the crate implements.

Every triggered dimension runs; skipped dimensions are reported explicitly in
the report's "Dimensions Checked / Skipped" table so an empty result is always
distinguishable from a skipped audit.

---

## Scope detection and mapping

When `--scope` is omitted, ecosystems are auto-detected from file signals:

| Signal                                               | Ecosystem      | Dimensions |
| ---------------------------------------------------- | -------------- | ---------- |
| `.github/workflows/*.yml`                            | GitHub Actions | 1, 2, 3, 4 |
| `Dockerfile` / `docker-compose.yml`                  | Containers     | 5, 12      |
| `.github/workflows/` with `secrets.*`                | Credentials    | 6          |
| `*.csproj` / `NuGet.Config`                          | .NET / NuGet   | 7          |
| `requirements*.txt` / `pyproject.toml` / `setup.cfg` | Python         | 8          |
| `Cargo.toml` / `Cargo.lock`                          | Rust           | 9          |
| `package.json` / `package-lock.json` / `yarn.lock`   | Node.js        | 10         |
| `go.mod` / `go.sum`                                  | Go             | 11         |

When `--scope` is given, values are matched against a strict allowlist before
any conditional use. An unrecognised value produces `INVALID_SCOPE` (exit `2`)
with the valid list printed to stderr.

| `--scope` value | Dimensions |
| --------------- | ---------- |
| `gha`           | 1, 2, 3, 4 |
| `containers`    | 5, 12      |
| `credentials`   | 6          |
| `dotnet`        | 7          |
| `python`        | 8          |
| `rust`          | 9          |
| `node`          | 10         |
| `go`            | 11         |
| `all`           | 1–12       |

---

## The finding schema

Findings are the atomic output unit. The JSON representation uses these keys
(matching upstream verbatim):

```json
{
  "id": "CRITICAL-001",
  "dimension": 1,
  "severity": "Critical",
  "file": ".github/workflows/release.yml",
  "line": 14,
  "current_value": "uses: actions/checkout@v4",
  "expected_value": "uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683  # v4.2.2",
  "rationale": "Mutable semver tag allows silent code replacement without any file change in your repo.",
  "offline_detectable": true,
  "tool_required": null,
  "contains_secret": false,
  "fix_url": "https://github.com/actions/checkout/releases",
  "accepted_risk": false
}
```

| Field                | Required | Constraint                                                                  |
| -------------------- | -------- | --------------------------------------------------------------------------- |
| `id`                 | Yes      | `{SEVERITY}-{NNN}` — severity prefix + zero-padded sequence, unique per report |
| `dimension`          | Yes      | Integer 1–12                                                               |
| `severity`           | Yes      | `Critical` \| `High` \| `Medium` \| `Info`                                  |
| `file`               | Yes      | Relative POSIX path; never absolute; no `..` traversal or null bytes         |
| `line`               | Yes      | Integer ≥ 0; `0` = file-level finding                                       |
| `current_value`      | Yes      | Exact offending string (grep-able); rendered as `<REDACTED>` when `contains_secret` |
| `expected_value`     | Yes      | Ready-to-use replacement — no guessing required                             |
| `rationale`          | Yes      | 1–3 sentences explaining exploitability                                     |
| `offline_detectable` | Yes      | `true` if confirmable without network access                                |
| `tool_required`      | No       | `null` or one of `gh`, `crane`, `syft`, `grype`, `cosign`, `actionlint`, `zizmor`, `detect-secrets`, `cargo-audit`, `go-mod-verify`, `hadolint` (default `null`) |
| `contains_secret`    | No       | `bool`, default `false`; when `true` both value fields render as `<REDACTED>` |
| `fix_url`            | No       | HTTPS URL for authoritative SHA/digest lookup (default `null`)              |
| `accepted_risk`      | No       | `bool`, default `false`; `true` when matched by the accepted-risks config    |

The nine required fields (`id`, `dimension`, `severity`, `file`, `line`,
`current_value`, `expected_value`, `rationale`, `offline_detectable`) plus the
four optional fields (`tool_required`, `contains_secret`, `fix_url`,
`accepted_risk`) reproduce the upstream `Finding` dataclass exactly, so the
serialized JSON is byte-for-byte key-compatible with upstream `asdict()`.

**Finding IDs are assigned last**, in a single global pass after scope
suppression and min-severity filtering, so the `{SEVERITY}-{NNN}` sequence is
gap-free within each severity band and stable for a given repository state.

---

## The report

### Markdown report (default)

The default output is the structured markdown report defined by the skill
([`SKILL.md`](../../amplifier-bundle/skills/supply-chain-audit/SKILL.md),
"Step 4: Report Generation"):
a header block (Date / Root / Scope / Skipped / Tool availability), a severity
summary table, ordered findings (Critical → High → Medium → Info), a SLSA
readiness assessment, recommended next steps, and an accepted-risks section.

An **empty report** still lists every dimension as Checked or Skipped so that
"no findings" is never confused with "audit did not run."

### JSON report

With `--json`, the report serializes to:

```json
{
  "date": "2026-07-28",
  "root": ".",
  "scope": ["gha", "python", "node"],
  "skipped": ["containers", "dotnet", "rust", "go"],
  "tool_availability": { "gh": true, "crane": false, "syft": false, "grype": false, "cosign": false },
  "summary": { "critical": 1, "high": 2, "medium": 3, "info": 1, "total": 7 },
  "findings": [ /* Finding objects, ordered by severity */ ],
  "slsa": { "level": "L1", "gaps": ["No signed provenance", "SBOM not published"] },
  "accepted_risks": [ /* accepted-risk annotated findings */ ]
}
```

The JSON schema is stable at `v1.x`: existing keys are never renamed or removed
within a major version.

---

## External tools

The audit runs fully offline. External tools only *enrich* checks that require
live lookups; their absence degrades to an `Info` note, never a failure.

| Tool     | Enriches                                   | Timeout | Lost without it                                    |
| -------- | ------------------------------------------ | ------- | -------------------------------------------------- |
| `gh`     | Action tag → SHA resolution (Dims 1–4)     | 15s     | Cannot resolve action tags to SHAs via GitHub API  |
| `crane`  | Container digest resolution (Dim 5)        | 20s     | Cannot resolve container image digests             |
| `syft`   | SBOM generation (`--generate-sbom`)        | 120s    | Cannot generate SPDX/CycloneDX SBOMs               |
| `grype`  | Known-CVE scanning                         | 60s     | Cannot scan for known CVEs                          |
| `cosign` | Signature / attestation verification       | 30s     | Cannot verify image signatures                     |

All tool invocations use **argument arrays with no shell** (`std::process::Command`
argv-only), resolve executables from the operator `PATH` only, and are wrapped
by a circuit breaker with per-tool timeouts. A timeout produces `TOOL_TIMEOUT`,
which is caught internally — the check is skipped and annotated, and the audit
continues in degraded mode.

`amplihack-supply-chain-audit --check-tools` lists which tools are present,
which are missing, what each does, and how to install each one.

---

## Configuration

### Accepted risks file

Place a `.supply-chain-accepted-risks.yml` at the audit root to acknowledge
known, reviewed findings:

```yaml
- id: HIGH-003
  dimension: 10
  file: package.json
  line: 0
  reason: "Internal registry mirror pinned by digest out of band."
  review_date: 2026-12-31
```

Behaviour:

1. The file is size-capped at **64 KiB**; larger files abort with
   `ACCEPTED_RISKS_OVERFLOW` (exit `3`). Parsing is line-based (not a general
   YAML loader) to resist YAML-bomb / billion-laughs inputs.
2. Entries with wildcard characters in `id` are rejected.
3. If `review_date` is in the past, the original severity is **restored** (the
   acknowledgement has expired).
4. Findings match by `dimension` + `file` + `line`.
5. **Critical findings are never suppressed** by an accepted-risk entry.
6. Matched non-Critical findings remain in the report, displayed as `Info` with
   an `[ACCEPTED RISK — review: YYYY-MM-DD]` annotation. They are never omitted.

### Environment variables

| Variable          | Effect                                                                   |
| ----------------- | ------------------------------------------------------------------------ |
| `RUST_LOG`        | Standard `tracing` filter (e.g. `RUST_LOG=debug`) for diagnostic output. |
| `NO_COLOR`        | Disables ANSI colour in the markdown report when set.                    |
| `PATH`            | The **only** source for resolving `gh`/`crane`/`syft`/`grype`/`cosign`.  |

---

## Library API reference

Add the crate as a workspace dependency:

```toml
[dependencies]
amplihack-supply-chain-audit = { workspace = true }
```

Then:

```rust
use amplihack_supply_chain_audit::{run_audit, AuditConfig, Severity};

let config = AuditConfig::new(".")
    .with_min_severity(Severity::High);

let result = run_audit(&config)?;
println!("{}", result.to_markdown());
for finding in result.findings() {
    eprintln!("{} {} {}:{}", finding.id(), finding.severity(), finding.file(), finding.line());
}
```

### `run_audit`

```rust
pub fn run_audit(config: &AuditConfig) -> Result<AuditResult, SupplyChainAuditError>;
```

The single entry point. Validates the path, detects (or applies) scope, runs
every triggered dimension checker, applies accepted-risks and min-severity
filtering, assigns finding IDs, and builds the `AuditResult`. Returns an error
only for the named abort conditions (invalid scope, path traversal, accepted-
risks overflow, unreadable root).

### `AuditConfig`

```rust
pub struct AuditConfig { /* … */ }

impl AuditConfig {
    pub fn new(path: impl AsRef<Path>) -> Self;
    pub fn with_scope(self, scope: EcosystemScope) -> Self;
    pub fn with_min_severity(self, min: Severity) -> Self;
    pub fn with_generate_sbom(self, on: bool) -> Self;
}
```

Builder for an audit run. `scope` defaults to auto-detect; `min_severity`
defaults to `Severity::Info`.

### `AuditResult`

```rust
impl AuditResult {
    pub fn findings(&self) -> &[Finding];
    pub fn summary(&self) -> &SeveritySummary;   // counts per band + total
    pub fn scope(&self) -> &EcosystemScope;
    pub fn skipped(&self) -> &[Ecosystem];
    pub fn slsa(&self) -> &SlsaAssessment;       // L0 | L1 | L2 + gaps
    pub fn to_markdown(&self) -> String;         // default report
    pub fn to_json(&self) -> String;             // --json report (serde_json)
    pub fn highest_severity(&self) -> Option<Severity>;
}
```

### `Finding` and `FindingId`

```rust
impl Finding {
    pub fn id(&self) -> &FindingId;
    pub fn dimension(&self) -> u8;               // 1..=12
    pub fn severity(&self) -> Severity;
    pub fn file(&self) -> &str;                  // relative POSIX
    pub fn line(&self) -> u32;                   // 0 = file-level
    pub fn current_value(&self) -> &str;         // <REDACTED> when contains_secret
    pub fn expected_value(&self) -> &str;         // <REDACTED> when contains_secret
    pub fn fix_url(&self) -> Option<&str>;
    pub fn rationale(&self) -> &str;
    pub fn tool_required(&self) -> Option<&str>;
    pub fn contains_secret(&self) -> bool;        // drives value redaction
    pub fn offline_detectable(&self) -> bool;
    pub fn accepted_risk(&self) -> bool;          // set by accepted-risks config
}
```

`Finding::new(..)` validates ID format (rejecting wildcards), the `1..=12`
dimension range, path safety, and the tool allowlist, returning
`SupplyChainAuditError` on violation. `FindingId` renders as `{SEVERITY}-{NNN}`.

### `Severity`

```rust
pub enum Severity { Critical, High, Medium, Info }
```

Ordered `Critical > High > Medium > Info`. Serializes to the capitalized string
form (`"Critical"`, …). `Severity::from_str` accepts the same forms and is used
to parse `--min-severity`.

### `detect_ecosystems` and `EcosystemScope`

```rust
pub fn detect_ecosystems(root: &Path) -> Result<EcosystemScope, SupplyChainAuditError>;

impl EcosystemScope {
    pub fn from_scope_flag(csv: &str) -> Result<Self, SupplyChainAuditError>; // allowlist
    pub fn dimensions(&self) -> Vec<u8>;         // triggered dimension numbers
    pub fn ecosystems(&self) -> &[Ecosystem];
}
```

`from_scope_flag` enforces the strict scope allowlist and returns
`INVALID_SCOPE` for anything else.

### External-tool helpers

```rust
pub fn check_tool_availability(tool: &str) -> bool;
pub fn check_missing_tools() -> Vec<ToolInfo>;   // name, description, install_options
pub fn install_tool(name: &str) -> Result<String, SupplyChainAuditError>;
pub fn install_all_missing() -> Vec<(String, Result<String, SupplyChainAuditError>)>;
```

These mirror the upstream `external_tools.py` surface so the SKILL.md
Prerequisites step can enumerate and optionally install missing tools.

### Error type: `SupplyChainAuditError`

A `thiserror`-derived enum whose `Display` output carries the verbatim upstream
message prefixes so downstream tooling can match on them:

| Variant                 | Message prefix              | Exit code |
| ----------------------- | --------------------------- | --------- |
| `InvalidScope`          | `INVALID_SCOPE:`            | `2`       |
| `PathTraversal`         | `PATH_TRAVERSAL:`           | `3`       |
| `AcceptedRisksOverflow` | `ACCEPTED_RISKS_OVERFLOW:`  | `3`       |
| `ToolTimeout`           | `TOOL_TIMEOUT:`             | (internal — degraded mode) |
| `XpiaEscalation`        | `XPIA_ESCALATION:`          | (internal — advisory)      |
| `Io`                    | (source I/O error)          | `4`       |

`ToolTimeout` and `XpiaEscalation` are handled internally and do not abort the
audit; the others propagate out of `run_audit`.

---

## Security invariants

Seven invariants are enforced unconditionally and covered by
`tests/security_invariants.rs`:

1. **Path traversal rejection** — paths containing `../`, a null byte, or a
   symlink escaping the audit root produce `PATH_TRAVERSAL`; the audit does not
   begin. Directory walking uses `follow_links(false)`.
2. **Scope enum validation** — `--scope` is matched against the strict allowlist
   before any use; unknown values produce `INVALID_SCOPE`.
3. **Subprocess argument arrays** — all external tools are invoked argv-only with
   no shell; user input is never interpolated into a command string.
4. **Secret redaction** — a finding whose `contains_secret` flag is set has both
   `current_value` and `expected_value` rendered as `<REDACTED>`; the original
   secret never appears in markdown or JSON output.
5. **XPIA escalation** — LLM-instruction markers in scanned content trigger an
   advisory `XPIA_ESCALATION`; the dimension check halts for that file and file
   content is omitted from the report. Detection is advisory (never aborts) and
   uses a hand-rolled left-boundary check (the `regex` crate has no lookbehind).
6. **Critical findings are never suppressed** — not by accepted-risks, not by
   `--min-severity`. This prevents hidden findings.
7. **Read-only operation** — the tool never modifies the repository, escalates
   privileges, or ingests/emits credentials; it runs at caller privilege.

---

## Tutorial: auditing a repository end to end

**1. Check tools (optional).**

```bash
amplihack-supply-chain-audit --check-tools
# Missing: gh — GitHub CLI, resolves action tags to SHAs
#   Install: brew install gh | apt install gh | winget install GitHub.cli
```

**2. Run a full audit.**

```bash
cd my-repo
amplihack-supply-chain-audit
```

You get a markdown report: a summary table, ordered findings with
`file:line` + copy-pasteable fixes, a SLSA readiness section, and next steps.

**3. Fix the Critical finding.** Each finding's `Expected` value is
ready-to-paste — e.g. replace `uses: actions/checkout@v4` with the pinned SHA
line shown.

**4. Acknowledge a reviewed non-Critical risk.** Add it to
`.supply-chain-accepted-risks.yml` with a future `review_date`. Re-run: the
finding now shows as `Info [ACCEPTED RISK — review: …]` but remains visible.

**5. Wire it into CI as a gate.**

```yaml
- name: Supply chain audit
  run: amplihack-supply-chain-audit --min-severity High
  # exit code 1 fails the job when any High/Critical finding is present
```

**6. Emit JSON for downstream tooling.**

```bash
amplihack-supply-chain-audit --json > supply-chain-report.json
```

---

## Testing

The crate mirrors the upstream test suite:

| Test target                    | Covers                                                          |
| ------------------------------ | -------------------------------------------------------------- |
| `tests/scope_detection.rs`     | Ecosystem detection + scope allowlist                          |
| `tests/pattern_detection.rs`   | Dimension 1–12 regex/pattern checkers                          |
| `tests/finding_schema.rs`      | `Finding` validation + serde key parity                        |
| `tests/error_conditions.rs`    | The five named error conditions                                |
| `tests/external_tools.rs`      | Availability, install metadata, timeout/degrade                |
| `tests/report_schema.rs`       | Markdown + JSON report shape, SLSA logic                       |
| `tests/security_invariants.rs` | All seven security invariants                                  |
| `tests/audit_workflow.rs`      | `run_audit` end-to-end wiring                                  |
| `tests/eval_scenarios.rs`      | Fixtures `scenario_a/b/c` with documented expected counts      |
| `tests/full_audit_e2e.rs`      | Binary run against a fixture repo                              |

Fixtures live under `tests/fixtures/scenario_{a,b,c}/` and are copied verbatim
from upstream (7 / 5 / 6 planted findings respectively).

Run:

```bash
cargo test -p amplihack-supply-chain-audit
cargo clippy -p amplihack-supply-chain-audit --all-targets
cargo fmt --check
```

---

## FAQ

**Does it modify my repository?** No. It is strictly read-only.

**What happens without `gh`/`crane`/`syft`/`grype`/`cosign`?** The audit runs in
degraded mode: only offline-detectable findings are produced, and each degraded
check is noted in the report. Run `--check-tools` to see (and optionally
install) what's missing.

**Why does the binary exit `1` on a clean-looking run?** Exit `1` means findings
at or above `--min-severity` were reported — it is the pre-merge-gate signal, not
an error. Runtime/usage errors use `2`, `3`, and `4`.

**How do I suppress a known finding?** Add it to
`.supply-chain-accepted-risks.yml` with a future `review_date`. Critical
findings cannot be suppressed.

**Is the JSON schema stable?** Yes — field names match the upstream Python
`asdict()` keys and are stable within a major version.
