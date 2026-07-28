# amplihack-supply-chain-audit

Native Rust implementation of the **`supply-chain-audit`** skill. Audits software
supply chain security across CI/CD pipelines, container images, and language
package ecosystems, and emits structured findings with severity ratings,
`file:line` references, and copy-pasteable fix templates.

Ships as both a library (`amplihack_supply_chain_audit`) and a standalone binary
(`amplihack-supply-chain-audit`). It replaces the upstream Python package
`supply_chain_audit/` with equivalent detection logic, the same finding schema,
the same report format, and the same security invariants.

## Quick start

```bash
# Full audit of the current repository (markdown report)
amplihack-supply-chain-audit

# GitHub Actions only, suppress Medium/Info, JSON output
amplihack-supply-chain-audit --scope gha --min-severity High --json

# Pre-merge gate: exit 1 on any High/Critical finding
amplihack-supply-chain-audit --min-severity High

# List / install missing external tools
amplihack-supply-chain-audit --check-tools
```

## Library usage

```rust
use amplihack_supply_chain_audit::{run_audit, AuditConfig, Severity};

let config = AuditConfig::new(".").with_min_severity(Severity::High);
let result = run_audit(&config)?;
println!("{}", result.to_markdown());
```

## What it checks

12 dimensions across GitHub Actions (SHA pinning, permissions, secret exposure,
cache poisoning), containers (base-image pinning, build chain), credentials
(OIDC vs long-lived secrets), and dependency integrity for .NET, Python, Rust,
Node.js, and Go. See the skill definition at
[`amplifier-bundle/skills/supply-chain-audit/SKILL.md`](../../amplifier-bundle/skills/supply-chain-audit/SKILL.md).

## Guarantees

- `#![forbid(unsafe_code)]`; no panics on untrusted input.
- Read-only: never modifies the repo, escalates privileges, or emits secrets.
- No silent degradation — missing external tools produce explicit `Info` notes.
- JSON field names match the upstream `asdict()` keys (schema parity).
- Every source file stays under the 400-line brick limit.

## CLI options

| Flag                     | Description                                                    |
| ------------------------ | ------------------------------------------------------------- |
| `PATH`                   | Directory to audit (default `.`).                             |
| `--scope <LIST>`         | `gha,containers,credentials,dotnet,python,rust,node,go,all`.  |
| `--min-severity <LEVEL>` | `Critical` \| `High` \| `Medium` \| `Info` (default `Info`).  |
| `--json`                 | Machine-readable JSON report.                                 |
| `--summary-only`         | Severity-count summary only.                                  |
| `--generate-sbom`        | Attempt SBOM generation via `syft`.                           |
| `--check-tools`          | Print external-tool availability, then exit.                  |

### Exit codes

| Code | Meaning                                              |
| ---- | ---------------------------------------------------- |
| `0`  | Clean at/above `--min-severity`.                     |
| `1`  | Findings present at/above `--min-severity`.          |
| `2`  | Usage error / `INVALID_SCOPE`.                       |
| `3`  | Refused (`PATH_TRAVERSAL` / `ACCEPTED_RISKS_OVERFLOW`). |
| `4`  | Runtime / I/O error.                                 |

## Configuration

Add a `.supply-chain-accepted-risks.yml` (≤ 64 KiB) at the audit root to
acknowledge reviewed, non-Critical findings with a future `review_date`.
Critical findings are never suppressed.

## Full reference

See [`docs/reference/supply-chain-audit-api.md`](../../docs/reference/supply-chain-audit-api.md)
for the complete API, finding/report schemas, security invariants, and tutorial.

## Testing

```bash
cargo test -p amplihack-supply-chain-audit
cargo clippy -p amplihack-supply-chain-audit --all-targets
cargo fmt --check
```
