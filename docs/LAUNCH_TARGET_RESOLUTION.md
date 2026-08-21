# Launch Target Resolution

**Issues:** [#1266](https://github.com/rysweet/amplihack-rs/issues/1266) (the defects this closes) · [#585](https://github.com/rysweet/amplihack-rs/issues/585) (the npm hang this must not regress)
**Status:** Implemented. `amplihack_utils::launch_target::resolve` is the one
resolver; `bootstrap::ensure_tool_available`, `claude_cli::get_claude_cli_path`,
`launcher_core::get_claude_cli_path`, and the fleet reasoner all read through it.
**Scope:** `crates/amplihack-utils` — `launch_target`, `claude_native` · `crates/amplihack-cli` — `bootstrap`, `launcher`, `commands/launch` · `crates/amplihack-launcher` — `launcher_core`

## Overview

Every `amplihack claude` launch has to answer three questions:

1. Which binary will we execute?
2. Is that binary usable?
3. Do we need to install or upgrade anything first?

`amplihack_utils::launch_target::resolve` answers all three. One function, one
answer, one binary. The version that gets compared against the registry is the
version of the binary that gets executed, and nothing else in the repository
independently resolves a launch path for these purposes.

That single-resolver rule is the whole design. The rest of this document is its
consequences.

## The resolution contract

### `resolve`

```rust
use amplihack_utils::launch_target::{resolve, TargetSource};

let resolution = resolve("claude");

match resolution.target {
    Some(target) => println!("{} @ {} ({:?})", target.path.display(), target.version, target.source),
    None => eprintln!("{}", resolution.rejection_report()),
}
// Output on a healthy host:
// /usr/bin/claude @ 2.1.238 (Path)
```

```rust
pub struct LaunchTarget {
    pub path: PathBuf,
    pub version: String,
    pub source: TargetSource,
}

pub enum TargetSource {
    /// AMPLIHACK_CLAUDE_BINARY_PATH / CLAUDE_BINARY_PATH.
    /// `user_supplied` is false when amplihack set the variable itself —
    /// see "The override is also set programmatically" below.
    ExplicitOverride { user_supplied: bool },
    Path,             // found on $PATH
    AmplihackPrefix,  // ~/.npm-global/bin — the prefix amplihack installs into
    FallbackDir,      // ~/.cargo/bin, ~/.local/bin
}

pub enum Rejection {
    Missing,            // no such path, or a dangling symlink
    NotAFile,           // resolves to a directory or other non-regular file
    NotExecutable,      // no executable bit for this user
    StubShape,          // small, and no native magic number — the placeholder
    ProbeFailed,        // `--version` ran but exited non-zero
    ProbeTimedOut,      // `--version` exceeded the per-candidate budget
    UnparseableVersion, // `--version` succeeded but emitted no semver
}

pub struct Resolution {
    pub target: Option<LaunchTarget>,
    pub rejected: Vec<(PathBuf, Rejection)>,
}

pub fn resolve(tool: &str) -> Resolution;

impl Resolution {
    /// Human-readable account of every candidate that was rejected and why.
    pub fn rejection_report(&self) -> String;
}
```

`Resolution` carries the rejection list because the error path needs it. A bare
`Option<LaunchTarget>` can say "nothing worked" but cannot say *what* it tried
and *why each one failed*, which is exactly the information the user needs when
a launch cannot proceed.

### Candidate order

Candidates are examined in this order and the **first healthy one wins**:

1. `AMPLIHACK_CLAUDE_BINARY_PATH`, then `CLAUDE_BINARY_PATH` (explicit override)
2. Each `$PATH` entry, in `$PATH` order
3. `~/.npm-global/bin` — amplihack's own npm prefix
4. The remaining install fallback directories: `~/.cargo/bin`, `~/.local/bin`

An explicit override that exists but fails the health gate is an **error**, not a
silent demotion. If you point amplihack at a specific binary and that binary is
broken, amplihack tells you so rather than quietly launching a different one.

#### The override is also set programmatically

`AMPLIHACK_CLAUDE_BINARY_PATH` is not exclusively a user-facing variable.
`configure_preferred_rustyclawd_binary`
(`crates/amplihack-cli/src/commands/rustyclawd.rs:34`) sets it **in-process**
whenever `amplihack rustyclawd` finds a `rustyclawd` or `claude-code` binary,
then delegates to the ordinary claude launch path.

So the "a broken override is a hard error" rule can fire on a value amplihack set
itself. The binary it selects has passed only `is_executable_file` — existence
plus the executable bit — which is strictly weaker than the health gate. A
`rustyclawd` that exists but cannot answer `--version` would turn a previously
working `amplihack rustyclawd` into a hard failure it never had.

**The two origins are therefore treated differently:**

| Override origin | When it fails the health gate |
| --- | --- |
| Set in the user's environment | **Hard error.** The user named this exact binary; quietly launching a different one is worse than failing. |
| Set programmatically by `configure_preferred_rustyclawd_binary` | **Warn, drop the candidate, continue** down the list. This is a *preference*, not an instruction. |

`resolve` records the origin in
`TargetSource::ExplicitOverride { user_supplied }`. The internally-set case is
marked via an in-process flag, **not** a second environment variable — the child
must not inherit it, or a nested `amplihack` invocation would silently downgrade
a genuine user override.

Callers that need `user_supplied` to be true should set the variable in the
environment before the process starts. Callers expressing a preference should go
through the internal path.

### The health gate

A candidate becomes a `LaunchTarget` only if **all** of the following hold:

| Check | Rejection when it fails |
| --- | --- |
| Path exists | `Rejection::Missing` |
| Path resolves to a regular file — `fs::metadata`, **following symlinks** | `Rejection::NotAFile` |
| File is executable | `Rejection::NotExecutable` |
| Head bytes are not the known stub shape | `Rejection::StubShape` |
| `--version` exits 0 within the per-candidate budget | `Rejection::ProbeFailed` / `Rejection::ProbeTimedOut` |
| `--version` output contains a parseable semver | `Rejection::UnparseableVersion` |

**The file-type check must follow symlinks.** On every npm-installed host,
`~/.npm-global/bin/claude` is a *symlink* into the package directory:

```sh
ls -l ~/.npm-global/bin/claude
# lrwxrwxrwx 1 you you 60 ... claude -> ../lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe
```

Using `symlink_metadata` — or any `is_file()` derived from it — rejects every
npm-installed claude on every host, including the one amplihack installs itself.
Use `fs::metadata`, which follows the link. A dangling symlink then surfaces as
`Rejection::Missing`, which is the correct answer for it.

**Health is a filter, never an annotation.** There is no such thing as a
`LaunchTarget` with `version: "unknown"`. A binary whose version probe fails,
times out, or returns something unparseable is not a degraded candidate — it is
not a candidate. amplihack will not execute it.

`classify_head` is the cheap pre-check that rejects the stub shape before paying
for a subprocess: a **small file that does not begin with a native executable
magic number** — `\x7fELF`, a Mach-O magic, or `MZ`. The test is the *absence* of
a magic number, not the presence of any particular text.

That distinction is load-bearing. The placeholder shipped by
`@anthropic-ai/claude-code` has **no shebang**. It is a 500-byte file whose first
bytes are:

```
echo "Error: claude native binary not installed." >&2
```

and `file` reports it as `ASCII text`. A check written to look for `#!` would
miss the exact stub this gate exists to catch — do not write one.

`classify_head` is a fast path, not the authority: a real shell wrapper that is
large enough passes it and is then settled by the probe.

### Probe budget

| Budget | Value |
| --- | --- |
| Per-candidate `--version` timeout | 3 s |
| Total probe budget across all candidates | 10 s |
| `MAX_PROBE_CANDIDATES` | 8 |

Probing stops at the first healthy candidate, so the common case is one
subprocess of a few tens of milliseconds. The total budget exists because a
single hung or hostile binary early in `$PATH` must not be able to stall a
launch: eight candidates at the per-candidate timeout would otherwise be 24
seconds of foreground hang.

The 3 s figure is deliberately larger than `binary_finder`'s 500 ms
`VERSION_DETECTION_TIMEOUT`. That constant gates an advisory annotation, where a
false negative costs nothing. This one gates a launch, where a false rejection
degrades the user's session.

## The install decision

`decide_install` is a pure function over the resolved target and the latest
published version. It is the entire fix for the reinstall-on-every-launch defect,
and it is unit-testable without touching npm.

```rust
pub enum InstallDecision {
    UseExisting,
    InstallMissing,
    UpgradeOwned,
}

pub fn decide_install(target: Option<&LaunchTarget>, latest: Option<&str>) -> InstallDecision;
```

| Resolved target | Latest version from registry | Decision |
| --- | --- | --- |
| `None` (nothing healthy anywhere) | any | `InstallMissing` |
| Healthy, source is `Path` / `FallbackDir` / `ExplicitOverride` | any | `UseExisting` |
| Healthy, source is `AmplihackPrefix` | `None` (query failed or timed out) | `UseExisting` |
| Healthy, source is `AmplihackPrefix`, version equals latest | `Some` | `UseExisting` |
| Healthy, source is `AmplihackPrefix`, version differs from latest | `Some` | `UpgradeOwned` |

Two rules are load-bearing:

**amplihack never upgrades a binary it does not own.** If the binary that will
actually be executed lives outside `~/.npm-global`, amplihack does not write
anything. Installing into its own prefix would not change what gets launched, so
the "upgrade" would be several hundred megabytes of download with no effect on
the next launch — and the launch after that would decide identically, forever.
Ownership is determined by `is_amplihack_owned_under`, which canonicalizes both
paths and **fails closed**: if either side cannot be canonicalized, the target is
treated as not owned and nothing is written.

**A failed registry query never triggers an install.** `latest == None` means the
network was unavailable or slow, not that the local install is stale. A network
blip must not cause a reinstall.

`get_installed_version` (`tool_update_check/version.rs`) ran `npm list -g` under
npm's *ambient* prefix — which is not necessarily the prefix amplihack installs
to, and not necessarily where the launched binary lives. It is **deleted**, not
merely demoted.

The design as frozen kept it for the advisory "update available" notice. Running
the fix against the dev VM showed why that is not good enough: with the healthy
`2.1.238` binary selected for launch, the notice still read the ambient prefix
and printed `update available: @anthropic-ai/claude-code 2.1.237 → 2.1.238` —
telling the user to upgrade to the version they were already running. A notice
that names a different binary than the one being launched is the same defect as
installing one, only quieter. `maybe_print_npm_update_notice` now takes its
installed-version side from `launch_target::resolve(tool).target`, and a grep
test enforces that no install decision consults npm's ambient prefix.

## Installing claude's native binary

`@anthropic-ai/claude-code` ships a small placeholder at `bin/claude.exe` and
materializes the real ~339 MB platform-native binary through its `postinstall`
script (`node install.cjs`), which copies the binary out of a platform-specific
`optionalDependencies` package.

Two independent npm flags each prevent that from happening:

| Flag | Mechanism | Result |
| --- | --- | --- |
| `--ignore-scripts` | `install.cjs` never runs | placeholder stub survives |
| `--omit=optional` | the platform package is never fetched, so `install.cjs` resolves nothing | placeholder stub survives |

amplihack keeps **both** flags on every npm invocation and materializes the
binary explicitly instead, in three steps:

1. **Base install** — `run_npm_install` with `-g --prefix <prefix> --omit=optional
   <package> --ignore-scripts`. Byte-for-byte the same invocation used for every
   other package.
2. **Platform package** — one explicit, exactly-named install of the single
   `@anthropic-ai/claude-code-<platform>` package for this host, pinned to the
   base install's version, still with both protective flags.
3. **Materialize** — run the vendor's `install.cjs` from the installed package
   directory with `node`.

Step 3 needs Node. amplihack locates it with `BinaryFinder::find("node")`; if
Node is absent it **warns and skips the step**. amplihack does not download or
manage a Node runtime for this. Node is a stated prerequisite (see
[Prerequisites](PREREQUISITES.md)), and a launcher that silently provisions a
language runtime is a far larger promise than this feature makes. Without Node
the placeholder survives, the health gate rejects it, and resolution falls
through to whatever else on the host is healthy.

This mirrors what `install_npm_package` already does for `@github/copilot`, and
it is why `run_npm_install` needed no change at all: copilot's argv is unchanged
by construction, so [#585](https://github.com/rysweet/amplihack-rs/issues/585)
cannot regress. Installing exactly one platform package by exact name also cannot
reproduce #585's failure mode, which was npm reifying optional dependencies for
*every* platform.

### Success is verified by outcome, not exit code

`install.cjs` exits 0 on most of its failure paths — unsupported platform, a
release channel with no native binaries, and a failed `require.resolve` all
return 0. Its exit code is not a success signal and amplihack ignores it.

Success means `is_materialized` returns true for the resulting file: larger than
1 MiB **and** carrying a native executable magic number.

```rust
pub fn is_materialized(head: &[u8], len: u64) -> bool;
```

This is the one place in the design where validation is genuinely load-bearing
rather than defense-in-depth. Everywhere else, validation is a safety net around
an install that is expected to work.

Every failure path in the three steps warns and returns. None of them fail the
launch: if materialization does not happen, the health gate rejects the stub and
resolution falls through to whatever else on the host is healthy.

### Platform selection

```rust
pub fn claude_platform_packages(os: &str, arch: &str, musl: bool) -> &'static [&'static str];
```

Returns the candidate platform packages in preference order — a slice rather than
a single value, so a musl/glibc misdetection can be corrected by one bounded
retry with the alternate. An empty slice means "no known package for this
platform", which skips the step non-fatally, exactly as the copilot path already
behaves.

Every element is a `&'static str`. That is a security control, not a style
choice: no runtime-derived string can reach npm's argv. The one runtime value
that does — the version read out of the installed `package.json` and pinned onto
the platform install — is validated against an anchored `^\d+\.\d+\.\d+$` regex
and rejected before use.

musl is detected with a zero-spawn filesystem probe for `/lib/ld-musl-*` and
`/usr/lib/ld-musl-*`, matching what the vendor's own `install.cjs` does when it
reads `process.report.getReport().header.glibcVersionRuntime` instead of
shelling out to `ldd`. Ambiguity defaults to glibc, and a wrong guess only
reorders the candidate list.

## Child process PATH

`augment_claude_launch_env` prepends the directory of the **resolved** target to
the child's `PATH`. `~/.npm-global/bin` is prepended only when the resolved
target actually lives there.

This matters because agents re-exec. A session launched by absolute path from
`/usr/bin/claude` will still resolve bare `claude` from its own `PATH` when it
spawns a subagent or shells out. Unconditionally putting an amplihack-writable
directory ahead of the system directories means any stub in that directory
shadows the working install for the entire session — and on a host where
`~/.npm-global/bin` is already first on the user's `PATH`, for every other shell
on the machine too.

When resolution finds no healthy target, nothing is prepended.

## When the launch cannot proceed

If the binary amplihack was about to execute fails the health gate, amplihack
does not execute it. It falls back to the next healthy candidate in the
resolution order — which includes the fallback directories, not just `$PATH`. If there is no healthy candidate at all, the launch fails with an
error built from `Resolution::rejection_report()`:

```
error: no usable claude binary found

  /home/you/.npm-global/bin/claude   incomplete install — 500-byte placeholder,
                                     the native binary was never materialized
  /home/you/.local/bin/claude        --version did not complete within 3s

  Remedy: install the CLI so its native binary is materialized:
    npm install -g @anthropic-ai/claude-code
  then run `amplihack claude` again.
```

The error names the real cause and states a remedy. Asserted properties, enforced
by test:

- names the actual cause (incomplete install / native binary not materialized)
- states a remedy
- does **not** surface a bare `Exec format error (os error 8)`
- does **not** mention CPU architecture or platform mismatch

The old failure mode was to launch the stub, get `Exec format error (os error 8)`
from the kernel, and hand the user a message that sent them hunting for a
CPU-architecture problem that did not exist. `enrich_spawn_error` translates the
raw OS error through the rejection report so the message describes the thing that
actually went wrong.

Error text carries paths, rejection reasons, and the remedy — never the
environment, never the full argv.

## Configuration reference

| Variable | Effect |
| --- | --- |
| `AMPLIHACK_CLAUDE_BINARY_PATH` | Explicit binary to use. Must pass the health gate; a broken override set *in your environment* is an error, not a fallback. amplihack also sets this variable internally for `amplihack rustyclawd`; that case warns and falls through instead. See [The override is also set programmatically](#the-override-is-also-set-programmatically). |
| `CLAUDE_BINARY_PATH` | Same, checked second (parity with the Python implementation). |

There is no environment variable that disables the health gate. A binary that
cannot report its version is not launched.

## Verifying behavior on your own host

Run `amplihack claude` twice and compare:

```sh
amplihack claude --version
# 📦 Installing claude via npm package @anthropic-ai/claude-code...
# 📦 Installing platform binary @anthropic-ai/claude-code-linux-x64...
# 2.1.238 (Claude Code)

amplihack claude --version
# 2.1.238 (Claude Code)
```

The second run performs no npm work at all.

### Is the native binary actually materialized?

The materialization target is `bin/claude.exe` inside the package directory.
Inspect **that file**, not the `bin/` entry that points at it:

```sh
CC=~/.npm-global/lib/node_modules/@anthropic-ai/claude-code

ls -l "$CC/bin/claude.exe"
# -rwxr-xr-x 1 you you 338860336 ... claude.exe

file "$CC/bin/claude.exe"
# ELF 64-bit LSB pie executable, x86-64, ...
```

A **~500-byte** `claude.exe` that `file` reports as `ASCII text` is the stub. Its
first line is `echo "Error: claude native binary not installed." >&2`. If you see
it, resolution rejects it and says so.

**`~/.npm-global/bin/claude` is a symlink**, so inspecting it without
dereferencing describes the link, not the binary — 60 bytes and `lrwxrwxrwx`,
which tells you nothing about materialization:

```sh
ls -l ~/.npm-global/bin/claude
# lrwxrwxrwx 1 you you 60 ... claude -> ../lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe

file ~/.npm-global/bin/claude
# symbolic link to ../lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe
```

To go through the symlink, dereference explicitly:

```sh
ls -lL  ~/.npm-global/bin/claude    # or: stat -Lc '%s' ~/.npm-global/bin/claude
file -L ~/.npm-global/bin/claude
```

Note that `cli.js` is **not** the materialization target and its size says
nothing about whether the native binary exists.

To confirm amplihack does not degrade a working install it does not own, record
size, version, and inode before and after a launch:

```sh
stat -Lc '%i %s' /usr/bin/claude && /usr/bin/claude --version
amplihack claude --version >/dev/null
stat -Lc '%i %s' /usr/bin/claude && /usr/bin/claude --version
# identical before and after
```

`-L` dereferences, so this reports the real binary even when the path on `$PATH`
is a symlink — which it usually is.

## Design notes

### Why one resolver

Before this design there were three independent resolutions in a single launch:
the version check read whatever `npm list -g` reported under npm's ambient
prefix, the install wrote into `~/.npm-global` via `--prefix`, and the exec
picked the first binary *found* on `$PATH`. On any host where those three
disagree — which is every host where npm's global prefix is not the directory
`claude` is served from — the version check compares a version that has nothing
to do with the binary being run, concludes an upgrade is needed, installs
somewhere that is never selected, and reaches the same conclusion on the next
launch. Forever.

Adding a health gate to that arrangement does not fix it; it just makes the
useless reinstall survivable. The fix is that check, install, and launch resolve
through one function.

### What this replaces

`ensure_claude_cli` (`crates/amplihack-utils/src/claude_cli.rs`) is the
pre-existing second resolver-and-installer. The single-resolver rule means it is
deleted, and the `ClaudeCliError` variants that only it constructed go with it.

`ClaudeCliError` is `pub`, so that is a **public API change**. The exact variant
list is settled by the implementation and enumerated in the PR body rather than
here, so this page does not carry a list that drifts.

### What #585 was actually about

[#585](https://github.com/rysweet/amplihack-rs/issues/585) was
`amplihack copilot` hanging while npm reified platform-specific optional
dependencies for *every* platform. The fix was `--os`/`--cpu`, since evolved into
`--omit=optional` plus an explicit single-platform follow-up install. Postinstall
scripts appear nowhere in #585's diagnosis or its remedy.

`--ignore-scripts` is asserted by a contract test that lives in
`tests/issue_585_copilot_npm_hang.rs` because that is where the npm-flag
assertions ended up, not because it was part of #585's fix. It is a generic
supply-chain protection and it is retained unchanged, for every package, on every
invocation.

### The threat model, stated honestly

The residual delta introduced by materializing claude's native binary is exactly
one named script, at an absolute path, under a prefix amplihack owns, for one
exactly-matched package name — run immediately before amplihack executes that
same package's native binary.

Declining to run a package's own postinstall while planning to exec its native
binary seconds later is not a coherent security posture. The postinstall is
strictly less privileged than what immediately follows it.

This is deliberately narrower than a script allowlist would be. An allowlist
re-enables *arbitrary* lifecycle scripts for a class of packages; this re-enables
*one script* for one exact package name. Exact string equality is enforced by
test, with negative cases for near-miss spellings such as
`@anthropic-ai/claude-code-evil` and `claude-code`.

### Environment variables are not a security boundary

Anyone who can set `AMPLIHACK_*` in this process's environment can already
execute code as this user. The override variables are a usability affordance, and
the health gate is a correctness control that stops amplihack from running things
that do not work. Neither is an integrity control and neither should be described
as one.

## Related documentation

- [System Prompt Append](SYSTEM_PROMPT_APPEND.md) — the other half of the launch path: how amplihack's routing contract reaches the agent
- [`amplihack copilot` — Subprocess-Safe Defaults](COPILOT_SUBPROCESS_SAFE.md) — the sibling flag-injection design in the same launch path
- [Copilot CLI](COPILOT_CLI.md) — the copilot install path this design mirrors
- [Prerequisites](PREREQUISITES.md) — npm and Node requirements
- [Security Recommendations](SECURITY_RECOMMENDATIONS.md) — repository-wide security posture
