# Atomic `settings.json` Reads — TOCTOU Hardening (S2)

Status: **Implemented** · Issue: [#1123](https://github.com/rysweet/amplihack-rs/issues/1123) · Scope item: **S2**

## Overview

Every place where amplihack inspects a `settings.json` file performs the
presence check and the read as a **single atomic operation**. A file that is
absent is treated as the ordinary "not present" outcome by the read itself —
there is no separate `exists()` probe that precedes the read.

This eliminates a class of *time-of-check-to-time-of-use* (TOCTOU) races: with a
separate `exists()` check followed by a read, a file could be removed, created,
or swapped in the window between the two steps, producing an inconsistent
result (for example, `exists()` returns `true` but the subsequent read fails, or
vice versa). Collapsing the two steps means presence and content are decided by
one operation and can never disagree.

The change is **behavior-preserving hardening**. There is no observable change
in output for any input:

| Input state                        | Reported outcome (unchanged)                         |
| ---------------------------------- | ---------------------------------------------------- |
| File absent                        | Same "not present" result as before                  |
| File present and valid             | Same parsed result as before                         |
| File present but corrupt/invalid   | Same error/message as before                         |
| File present but unreadable        | Same read-error message as before                    |

Only the **atomicity** of "presence + read" changed.

## Where it applies

Four read sites were collapsed from `exists()`-then-read into a single atomic
read:

### `amplihack-hooks` — `session_start::migration`

- **`migrate_global_hooks`** — reads the global settings file
  (`~/.claude/settings.json`) via `AtomicJsonFile::read()`. A missing file
  returns `Ok(None)`, which maps to `None` ("nothing to migrate"). No
  standalone `exists()` probe is used.
- **`repo_local_contains_amplihack_hooks`** — reads the repo-local
  `<project-root>/.claude/settings.json` via `AtomicJsonFile::read()`. A missing
  file returns `Ok(None)`, which maps to `false` ("no repo-local hooks"). No
  standalone `exists()` probe is used.

In both cases a genuine read error (not "absent") is surfaced through the
existing `Err(e)` arms via `tracing::warn!`, and for `migrate_global_hooks` the
same user-facing warning string as before.

Note the one intentional exception to the "error ≠ absent" rule:
`repo_local_contains_amplihack_hooks` returns a `bool`, so its `Err(e)` arm
degrades to `false` — the *same value* it returns for an absent file — after
emitting a `tracing::warn!`. This is a deliberate fail-safe: an unreadable
repo-local settings file must not be treated as "amplihack hooks are present,"
so it is degraded to `false`-with-warning rather than propagating an error.
This single site is the exception to the byte-for-byte error-propagation
guarantee described in [Semantics](#semantics-absent-vs-error) below; the other
three sites preserve a distinct error outcome.

### `amplihack-cli` — `commands::doctor::checks`

- **`settings_has_amplihack_hooks`** — a single `std::fs::read_to_string`. A
  `NotFound` error maps to `None` (location absent → nothing to report). Every
  other error keeps the identical message:
  `hooks: cannot read settings.json: <truncated error>`.
- **`check_settings_valid_json`** — a single `std::fs::read_to_string`. A
  `NotFound` error maps to the same `(false, "settings.json: file not found")`
  outcome as before. Every other error keeps the identical message:
  `settings.json: cannot read: <truncated error>`.

## Semantics: "absent" vs. "error"

The hardening preserves the critical distinction between *absent* and
*unreadable*:

- **Absent** (`std::io::ErrorKind::NotFound`, or `AtomicJsonFile::read()`
  returning `Ok(None)`) → the "not present" outcome (`None` / `false` /
  `file not found`). This is a normal, expected state.
- **Present but unreadable** (permissions, a directory in the file's place, I/O
  error, etc.) → an *error* outcome with the original diagnostic message. These
  are never silently downgraded to "absent".

Only `NotFound` is mapped to the absent branch. All other `io::Error` kinds
propagate with byte-for-byte identical messages. This means a permission or
corruption failure is never masked as a missing file.

**One deliberate exception:** `repo_local_contains_amplihack_hooks` returns a
`bool` and therefore has no error channel to propagate through. On a read error
it emits a `tracing::warn!` and returns `false` — which happens to equal its
absent value. This is an intentional fail-safe (an unreadable repo-local file
must never be reported as "hooks present"), not a silent downgrade: the failure
is always logged. The distinction preserved here is behavioral safety, not the
absent-vs-error value distinction, which cannot be expressed in a `bool` return.
The other three sites (`migrate_global_hooks`, `settings_has_amplihack_hooks`,
`check_settings_valid_json`) keep a fully distinct error outcome.

## Error-message contract (unchanged)

The following strings are part of the observable contract and are preserved
exactly:

- `hooks: cannot read settings.json: <truncated>` (non-NotFound read error in
  `settings_has_amplihack_hooks`)
- `hooks: settings.json is not valid JSON` (parse error)
- `settings.json: file not found` (NotFound in `check_settings_valid_json`)
- `settings.json: cannot read: <truncated>` (non-NotFound read error in
  `check_settings_valid_json`)
- `settings.json is valid JSON` / `settings.json: invalid JSON`

Read-error messages are passed through `truncate_chars_with_notice(.., MAX_ERROR_LEN)`
and never include file contents.

## Relationship to `doctor`

`amplihack doctor` surfaces these checks to users:

- **Check "hooks installed"** aggregates the two-location probe
  (`settings_has_amplihack_hooks` over global and repo-local settings). An
  unreadable or corrupt file at one location no longer masks a valid result at
  the other (see regression coverage below).
- **Check "settings.json valid JSON"** uses `check_settings_valid_json`.

No `doctor` output changed as a result of this hardening.

## Symlink following (S3) — intentionally NOT changed

These reads deliberately **do not** refuse symlinked targets. No
`symlink_metadata`, `O_NOFOLLOW`, or symlink-refusal logic was added. This is a
conscious no-op, not a deferral:

- The targets stay within the user's own trust domain (`$HOME` and the current
  repository).
- Only *presence* and *validity* are ever reported — file **contents are never
  surfaced** to the user or logs.
- Refusing symlinked config would break legitimate dotfile-management setups
  where `~/.claude/settings.json` is a symlink into a managed dotfiles
  repository.

There is no security benefit to refusing symlinks at these read sites, so S3 is
a deliberate no-op.

## Regression coverage

The following behaviors are covered by tests and must remain green:

- `settings_has_amplihack_hooks` returns `None` for a path that does not exist
  (NotFound → absent), and `Some(Err(_))` for a present-but-unreadable path
  (e.g. a directory at the path, or a Unix file with read permission removed) —
  proving the collapse did not blur the absent-vs-error distinction.
- `migrate_global_hooks` returns `None` when the global settings file is absent.
- `test_check_hooks_installed_corrupt_global_does_not_mask_repo_local` (#1088):
  a corrupt global settings file does not hide valid repo-local hooks.
- The two-location `doctor` hooks checks report consistently regardless of which
  location is absent, valid, or corrupt.

## Rationale

Config-inspection code paths are attractive TOCTOU targets because they run at
session start and during `doctor`, often against files under user-writable
directories. Collapsing check-then-read removes the race window at essentially
zero cost and with no behavioral change, making the presence/validity report a
single, self-consistent decision.
