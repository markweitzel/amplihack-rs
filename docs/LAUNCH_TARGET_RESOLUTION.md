# Launch Target Resolution

**Scope:** `crates/amplihack-utils` — `launch_target` module; `crates/amplihack-cli` — bootstrap, tool update check, and the `amplihack claude` / `amplihack copilot` launch path.

Every question amplihack asks about an agent CLI — *which binary do I run? is it healthy? is it out of date? am I allowed to touch it?* — is answered by one function: `amplihack_utils::launch_target::resolve`. This document explains the contract that function enforces, the authorization model built on it, and how to diagnose a launch that does not go the way you expected.

## Why one resolver

Before this contract existed, amplihack answered those questions in four different places, and they disagreed. A single real launch could:

- run the **version check** against `/usr/bin/claude` (a root npm install, on `PATH`),
- **install** the "upgrade" into `~/.npm-global` via `npm --prefix`, which was not on `PATH`,
- **launch** `~/.local/bin/claude`, a third binary neither of the first two had inspected.

Because the version check never read the binary it installed, the upgrade never appeared to take, so amplihack re-downloaded a ~339 MB package on *every* launch, forever. And because the install produced a non-functional placeholder rather than a real binary, each of those launches also dropped a broken `claude` into `~/.npm-global/bin` — which on hosts where that directory leads `PATH` shadows the user's working install and breaks bare `claude` system-wide.

The fix is structural, not defensive: **check, install, repair, PATH promotion, and exec all resolve through `resolve`.** Validation exists as defense-in-depth for genuine failures (network, disk, partial extraction). It is not load-bearing for correctness.

## The resolver contract

```rust
use amplihack_utils::launch_target::{self, Health, LaunchAction, LaunchContext, Ownership};

let resolution = launch_target::resolve("claude");
```

`resolve` is **infallible by design**. A missing tool, an unreadable `PATH` entry, a probe that fails, and a probe that times out are all *data*, not errors — they land in `Resolution::rejected` with a reason. Callers branch on the resolution, never on an `Err`.

Its central invariant:

> If `resolution.selected` is `Some`, that binary passed the full health gate and its `Health::Working { version }` is a real string produced by running it. There is no path by which an unvalidated binary is selected.

### Precedence

Candidates are gathered in this fixed order, de-duplicated by canonical path, and capped at 8 probes per launch:

1. **Env override** — `AMPLIHACK_CLAUDE_BINARY_PATH`, then `CLAUDE_BINARY_PATH` (`AMPLIHACK_{TOOL}_BINARY_PATH` / `{TOOL}_BINARY_PATH` generally).
2. **`PATH` scan**, in `PATH` order.
3. **Known install directories**, whether or not they are on `PATH`: `~/.npm-global/bin`, `~/.cargo/bin`, `~/.local/bin`.

This is the same precedence `BinaryFinder::find` has always used. What changed is that **a candidate failing the health gate is skipped, never selected** — resolution continues to the next candidate rather than handing back something broken.

### The health gate

Each candidate passes through three checks, in this order. The order is a security requirement, not an optimization:

| Step | Check | Cost |
| --- | --- | --- |
| 1 | **Structural filter** (Unix): resolve the symlink, then reject if the target is under 4096 bytes and its magic bytes are not ELF / Mach-O / PE | zero subprocesses |
| 2 | **Executable bit** | one `stat` |
| 3 | **Version probe**: `<binary> --version`, 10 s timeout, stdin `/dev/null` | one subprocess |

Step 1 runs first so that a small hostile script sitting in an early `PATH` directory is rejected **without ever being executed**. Do not reorder it.

The 10 s validation timeout is deliberate and much longer than the 500 ms used for ordinary discovery of `npm`, `node`, and `git`. A 339 MB binary's cold first run does not finish in 500 ms, and under this contract a false "unknown" means a *rejected install*. The probe runs at most once per launch per candidate; warm, it returns in ~150 ms.

A probe that times out is treated **identically** to one that fails. Neither binary is executed.

### Rejection reasons

`BrokenReason` is what `render_rejections` turns into human-readable remedies:

| Reason | Meaning |
| --- | --- |
| `Stub` | Resolved target is a small non-binary file — the classic placeholder left by an interrupted npm install |
| `NotExecutable` | Present, but the executable bit is clear |
| `ProbeFailed` | `--version` exited non-zero or could not be spawned |
| `ProbeTimedOut` | `--version` did not finish within 10 s |

## Ownership is authorization

`launch_target::is_amplihack_owned` is the **sole** predicate that authorizes mutation. If it returns `false`, amplihack will not install over the binary, will not repair it, will not delete it, and will not promote its directory onto the child's `PATH`.

```rust
// True only when the binary's *directory* lives inside amplihack's own npm prefix.
launch_target::is_amplihack_owned(&candidate.path)
```

Two details that are easy to get wrong and are pinned by tests:

- Containment is component-wise (`Path::starts_with`), never string-prefix. `~/.npm-global-backup/bin/claude` is a string-prefix match and correctly a **non**-match.
- The **parent directory of the `PATH` entry** is canonicalized, not the symlink target. Ownership answers "does this link live in our prefix"; health answers "is its target a stub". Collapsing the two would let a symlink pointing back into the prefix authorize itself.

Deny-by-default applies on every unknown path: `HOME` unset, empty, or relative → not owned; a prefix that is a filesystem root or has fewer than two components → not owned; a binary reached via an env override → **never** owned, even if it physically sits inside the prefix.

### What this means in practice

**amplihack upgrades only the binaries it installed.** If your `claude` came from a system package, a root `npm install -g`, or Anthropic's native installer, amplihack prints an update notice and does nothing else:

```
amplihack: claude 2.1.237 is installed at /usr/bin/claude; 2.1.238 is available.
amplihack did not install this binary and will not modify it.
To upgrade:  npm install -g @anthropic-ai/claude-code
```

This is intentional. Auto-installing over a binary amplihack does not own is how the shadow copy at a different `PATH` precedence got created in the first place — the very defect this design removes. It is also the correct security posture: mutating binaries you do not own is privilege overreach regardless of intent.

## Launch decisions

`decide_launch_action` is pure — no I/O, no environment reads — so its full behavior is unit-tested without a filesystem:

```rust
pub fn decide_launch_action(
    resolution: &Resolution,
    latest: Option<&str>,
    ctx: LaunchContext,
) -> LaunchAction
```

| `LaunchAction` | When | Effect |
| --- | --- | --- |
| `Launch` | Healthy, current — or healthy and not ours | exec it |
| `InstallFresh` | No healthy candidate anywhere | install, re-resolve, then exec |
| `Upgrade { from, to }` | Healthy, amplihack-owned, stale | install, re-resolve, then exec |
| `NoticeOnly { from, to }` | Healthy, **not** amplihack-owned, stale | print the notice above, exec the existing binary |
| `Fail` | No healthy candidate and install is impossible or disabled | error naming every rejected candidate |

`LaunchContext { npm_backed, interactive }` carries the two situational bits. It is a struct rather than two adjacent bare `bool` parameters specifically so a call site cannot silently transpose them.

Version comparison uses `extract_semver`, which pulls the first `MAJOR.MINOR.PATCH[-prerelease]` out of a version string. This matters more than it looks: `claude --version` prints `2.1.238 (Claude Code)`, and the older sanitizer mangled that into `2.1.238ClaudeCode`, which never compared equal to npm's `2.1.238` — a second, independent always-stale loop. `extract_semver` returning `None` fails closed: no semver, no upgrade.

## Repair and purge

`decide_repair_action` is pure and total, and receives a precomputed `Ownership` so that containment logic lives in exactly one audited place:

```rust
pub fn decide_repair_action(
    ownership: Ownership,
    health: &Health,
    source: Source,
    already_attempted: bool,
) -> RepairAction
```

| `RepairAction` | Conditions |
| --- | --- |
| `None` | Anything not explicitly authorized below — the default arm |
| `CompleteInstall` | Amplihack-owned, broken, not yet attempted this launch → re-run the install, once |
| `Purge` | Amplihack-owned, broken, and repair already failed → remove the file |

`Purge` is the only destructive filesystem operation on the launch path, and it requires **all three** of:

1. the path is inside amplihack's own npm prefix (canonical, component-wise), and
2. it is confirmed non-functional — resolved target under 4096 bytes and not ELF/Mach-O/PE, **or** the version probe failed, and
3. resolution follows the symlink for the health check, while removal targets the **link** via `symlink_metadata` + `remove_file`, never follow-then-delete.

Nothing outside amplihack's prefix is ever removed. This is what lets a host that already accumulated a stub recover: the stub is repaired or removed on the next launch, and bare `claude` starts working again.

## Installing `claude` properly

`@anthropic-ai/claude-code` ships its real binary as platform-specific `optionalDependencies` and materializes it in a `postinstall` (`node install.cjs`), which hardlinks the platform package's binary over a placeholder at `bin/claude.exe`. Amplihack's npm invocations pass `--ignore-scripts` and `--omit=optional`, and **either flag alone yields the placeholder**: with `--ignore-scripts` the postinstall never runs; with `--omit=optional` it runs, finds no platform package, prints instructions, and leaves the placeholder in place.

Amplihack therefore installs `claude` as an **explicit three-step**, mirroring the two-step already used for `@github/copilot`:

```text
1. npm install -g --prefix <prefix> --ignore-scripts --omit=optional @anthropic-ai/claude-code
2. npm install -g --prefix <prefix> --ignore-scripts --omit=optional @anthropic-ai/claude-code-linux-x64
3. node <prefix>/lib/node_modules/@anthropic-ai/claude-code/install.cjs
```

Result: a ~339 MB ELF at `<prefix>/bin/claude` reporting `2.1.238 (Claude Code)`.

`run_npm_install` is **unchanged** by this design. Both flags stay on every npm invocation, globally, with no allowlist and no per-package exception — so the `@github/copilot` path behaves exactly as it did, and the three contract tests in `crates/amplihack-cli/tests/issue_585_copilot_npm_hang.rs` pass unmodified. The exception is expressed instead as one named, auditable branch in amplihack's own source, keyed on `==` against the `&'static str` from `npm_package_for_install`.

Step 3 does execute third-party code — and the honest framing is that this is strictly less privileged than what happens seconds later: amplihack is about to exec that same package's native binary. Refusing to run its postinstall while planning to exec its output is not a coherent threat model. The postinstall is nonetheless run under constraints: absolute path assembled from static components, `symlink_metadata` must show a regular file (a symlink there means package tampering — skip), `node` located through `BinaryFinder::find` rather than `Command::new("node")`, no shell, stdin `/dev/null` so a prompting script cannot hang the launch or read your terminal, and a kill-on-timeout runner. Failure is non-fatal; the health gate is the enforcement point.

### Platform packages

`claude_platform_package(os, arch, libc)` mirrors `install.cjs`'s `getPlatformKey` and returns one of eight `&'static str` values — never a string built from runtime input:

| | x64 | arm64 |
| --- | --- | --- |
| linux (glibc) | `@anthropic-ai/claude-code-linux-x64` | `@anthropic-ai/claude-code-linux-arm64` |
| linux (musl) | `@anthropic-ai/claude-code-linux-x64-musl` | `@anthropic-ai/claude-code-linux-arm64-musl` |
| darwin | `@anthropic-ai/claude-code-darwin-x64` | `@anthropic-ai/claude-code-darwin-arm64` |
| win32 | `@anthropic-ai/claude-code-win32-x64` | `@anthropic-ai/claude-code-win32-arm64` |

Unknown platform returns `None`, which skips the platform step rather than guessing a package name.

`detect_libc` checks for `/lib/ld-musl-*` first (decisive, no subprocess) and falls back to a `node` one-liner; ambiguity defaults to glibc. A wrong guess costs one wasted download, is caught by the health gate, and triggers exactly one bounded retry with the other libc — which retires the whole class of libc-detection risk.

## Why `--ignore-scripts` and `--omit=optional` both stay

The investigation behind that decision, recorded here so nobody has to re-derive it:

| Question | Answer |
| --- | --- |
| What caused the [#585](https://github.com/rysweet/amplihack-rs/issues/585) hang? | Cross-platform optional dependencies — npm stuck in `reify:@github/copilot-darwin-arm64` on Linux |
| What did PR #585 propose? | `--os` / `--cpu` flags. Rejected and closed as broken in npm 9.x; a contract test now *forbids* them |
| What actually shipped? | `--omit=optional` plus a separate platform-package install (commit `87dc3f6e`) |
| Where did `--ignore-scripts` come from? | Commit `48e76578`, PR #15 — it **predates #585** and is named nowhere in the issue |

So `--ignore-scripts` is cargo-culted with respect to #585, and the test file's framing of it as a #585 security requirement is a post-hoc attachment. That is an argument for leaving it alone, not for relaxing it: narrowing it would still yield a placeholder (the optional dependency is still absent), and narrowing `--omit=optional` too would reintroduce #585's exact hang condition for a package with eight cross-platform optional dependencies.

## Errors that name the real cause

Two diagnostics changed shape.

**A stub that reached exec** used to surface as:

```
error: failed to spawn child process: Exec format error (os error 8)
```

— which names nothing real and sends you hunting a CPU-architecture problem that does not exist. `ENOEXEC` is now special-cased before the generic renderer:

```
error: /home/you/.npm-global/bin/claude is not a runnable program.
       The file is a 512-byte placeholder, not the native claude binary —
       its npm install did not complete.
       amplihack will reinstall it on the next launch. To fix it now:
         rm ~/.npm-global/bin/claude && amplihack claude
```

**Nothing healthy found** lists every candidate and its reason, one remedy line per reason:

```
error: no working claude binary found. Checked 3 candidates:
  /usr/bin/claude          — probe failed (`--version` exited 127)
  ~/.npm-global/bin/claude — stub (512-byte placeholder, npm install incomplete)
  ~/.local/bin/claude      — not executable
Try: npm install -g @anthropic-ai/claude-code
```

**`amplihack install-tool` is not a subcommand — do not print it.** An earlier draft of this document used it as the remedy line. It does not exist anywhere in the tree; the only `install_tool` is a private function in `crates/amplihack-cli/src/bootstrap.rs:346`. The remedy must be a command the reader can actually run, which is the `npm install -g` line above.

**Acceptance-test constraint on this text.** `crates/amplihack-cli/tests/issue_585_copilot_npm_hang.rs:205-207` reads the *source text* of `ensure_tool_available` out of `bootstrap.rs` and asserts its body contains at least one of three literals:

```rust
let has_actionable_guidance = fn_body.contains("PATH")
    || fn_body.contains("npm install")
    || fn_body.contains("Try running");
```

Three things follow, and all three have bitten a previous reader:

1. It is an **OR**, not an AND. Any one literal satisfies it.
2. It scans **source, not runtime output**. Moving the remedy text wholesale into a `render_rejections` helper in another file empties `ensure_tool_available`'s body of all three literals and fails the test even though the user-visible message is unchanged. If you extract the renderer, keep one of the literals in `ensure_tool_available` itself.
3. The `npm install -g @anthropic-ai/claude-code` remedy above contains `npm install`, so it satisfies the assertion on its own. The rejected `amplihack install-tool claude` phrasing contained **none** of the three.

Every path printed by these renderers goes through a display sanitizer that strips C0 controls and `ESC` and truncates. The existing `strip_ansi` is CSI-only and misses OSC, `ESC c`, DCS, and bare `\r`; filenames in a writable `PATH` directory are attacker-influenceable, so the sanitizer — not `strip_ansi` — is the control here. Detected version strings are likewise only ever displayed after passing through `extract_semver`, which is a strict allowlist.

## Child process environment

`augment_claude_launch_env` used to prepend `~/.npm-global/bin` to the child's `PATH` unconditionally — handing the child a `PATH` whose first entry could contain a stub. It now takes the selected binary's directory:

- **Not amplihack-owned** → prepend nothing; pass the validated absolute path instead. Prepending someone else's directory would promote *every* executable in it (`git`, `node`, `rg`) for the child.
- **No selection** → prepend nothing. Never fall back to the npm prefix.
- **Amplihack-owned** → prepend that directory, as before.

Amplihack execs the same canonicalized path it validated. The TOCTOU window between probe and exec is accepted — an attacker who can swap a binary in that window could have swapped it a second earlier and been selected legitimately — but it is commented at the exec site so nobody "simplifies" it back to `Command::new("claude")` and silently reopens the whole defect.

## No probe cache

Health verdicts are not persisted. A cached "healthy" verdict is an authorization decision with no revocation path, and it fails silently in precisely the scenario that motivated this work. If the ~150 ms warm gate ever profiles as material, the escalation is in-process memoization within a single launch — never an on-disk store.

## Configuration

| Variable | Effect |
| --- | --- |
| `AMPLIHACK_CLAUDE_BINARY_PATH` | Exact binary to use. Still health-gated; **never** treated as amplihack-owned, so it is never installed over, repaired, purged, or `PATH`-promoted |
| `CLAUDE_BINARY_PATH` | Same, checked second (parity with the Python implementation) |
| `AMPLIHACK_{TOOL}_BINARY_PATH` / `{TOOL}_BINARY_PATH` | The general forms, for any tool |
| `AMPLIHACK_NONINTERACTIVE=1` | Suppresses the interactive upgrade prompt; resolution and health gating are unaffected. Note the var is only the fast path — `is_noninteractive()` (`crates/amplihack-cli/src/util.rs:36-43`) also returns `true` when stdin is not a TTY, so the prompt is already suppressed in pipes, redirects, CI runners, and test harnesses without anyone setting the var. Do not treat "var unset" as "prompt will appear". |

There is no environment variable that disables the health gate. Selecting an unvalidated binary is the defect this design removes.

## Verifying behavior on your host

```sh
# 1. Two consecutive launches. The second must not download anything.
amplihack claude --version
amplihack claude --version

# 2. Confirm what was selected and why.
RUST_LOG=info amplihack claude --version 2>&1 | grep 'launching claude'
# INFO launching claude binary=/usr/bin/claude version="2.1.237 (Claude Code)" owned=false

# 3. Confirm an external install was left byte-identical.
sha256sum /usr/bin/claude ~/.local/bin/claude
```

`command -v claude` is **not** a valid probe on a host that has accumulated a stub, because the stub may be the first `PATH` entry. Compare sizes and hashes of the actual files.

## API reference

`amplihack_utils::launch_target`:

| Item | Signature | Notes |
| --- | --- | --- |
| `resolve` | `fn resolve(tool: &str) -> Resolution` | Infallible; the single entry point |
| `Resolution` | `{ selected: Option<Candidate>, rejected: Vec<Candidate> }` | `selected.is_some()` ⇒ health is `Working` |
| `Candidate` | `{ path: PathBuf, source: Source, health: Health, ownership: Ownership }` | `path` is canonicalized. `ownership` is **not** independently computed — it is the cached result of calling `is_amplihack_owned(&path)` once during candidate construction. |
| `Source` | `EnvOverride \| Path \| FallbackDir` | Drives the `Purge` denial for env overrides |
| `Health` | `Working { version: String, semver: Option<String> } \| Broken(BrokenReason)` | `version` exists by construction when `Working` |
| `Ownership` | `AmplihackOwned \| External` | The authorization token for mutation |
| `is_amplihack_owned` | `fn is_amplihack_owned(path: &Path) -> bool` | Component-wise containment; deny-by-default. The **single** ownership predicate in the codebase — `Candidate.ownership` is its cached output, not a parallel implementation. Adding a second way to decide ownership breaks the authorization model in [Ownership is authorization](#ownership-is-authorization): two predicates that can disagree mean the one guarding a given mutation is whichever the caller happened to reach for. Read `Candidate.ownership`; call `is_amplihack_owned` only where no `Candidate` exists yet. |
| `npm_prefix_dir` | `fn npm_prefix_dir() -> Option<PathBuf>` | The single definition of `~/.npm-global` |
| `extract_semver` | `fn extract_semver(s: &str) -> Option<String>` | First `MAJOR.MINOR.PATCH[-pre]`; doubles as an output allowlist |
| `decide_launch_action` | see above | Pure |
| `decide_repair_action` | see above | Pure and total |
| `render_rejections` | `fn render_rejections(r: &Resolution) -> String` | Sanitizes every path it prints |

`amplihack_utils::binary_finder`:

| Item | Notes |
| --- | --- |
| `detect_version_with_timeout(path, timeout)` | Sets `stdin(Stdio::null())`. Used for validation at 10 s |
| `detect_version(path)` | Unchanged 500 ms wrapper, used for discovery of `npm` / `node` / `git` |

## See also

- [System Prompt Append](SYSTEM_PROMPT_APPEND.md) — the other half of the launch path: how amplihack's routing contract reaches the agent
- [Prerequisites](PREREQUISITES.md) — Node, npm, and toolchain requirements
- [GitHub Copilot CLI](COPILOT_CLI.md) — the copilot two-step this design mirrors
