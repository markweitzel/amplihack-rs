# Global Hook Cleanup & `amplihack doctor` Hook Detection

This guide explains how amplihack manages the relationship between **global**
hooks (`~/.claude/settings.json`) and **project-local** hooks
(`<project-root>/.claude/settings.json`) on session start, and how
`amplihack doctor` reports whether hooks are installed.

It documents the safety guarantees that prevent amplihack from ever
uninstalling itself, and the conditions under which `amplihack doctor` reports
a healthy install.

> Related: [Hook Configuration Guide](HOOK_CONFIGURATION_GUIDE.md),
> [Hook Payload Compatibility](HOOK_PAYLOAD_COMPATIBILITY.md).

## Background: two places hooks can live

Claude Code (and amplihack's native hook runner) resolves hooks from two
locations:

| Location        | Path                                      | Scope                         |
| --------------- | ----------------------------------------- | ----------------------------- |
| Global          | `~/.claude/settings.json`                 | Applies to every project      |
| Project-local   | `<project-root>/.claude/settings.json`    | Applies to one repository     |

Amplihack hooks are identified inside a `settings.json` `hooks` section by a
hook `command` string that references `amplihack-hooks` or
`tools/amplihack/`. Either location is a **valid, fully working** install.

## Session-start global hook cleanup

On every session start, amplihack runs a small compatibility step that decides
whether the **global** amplihack hooks are still needed. The governing
invariant is:

> **Never delete the only working copy of amplihack's hooks, and never report
> success for a pure deletion.**

### Decision table

The step reads global settings and probes the project-local
`.claude/settings.json` (read-only — it never writes hooks). It then behaves as
follows:

| Global has amplihack hooks | Project-local has amplihack hooks | Action                                            | Notice shown                                                                                                                        |
| :------------------------: | :-------------------------------: | ------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| No (or file absent)        | —                                 | **Nothing.**                                      | *(none)*                                                                                                                            |
| Yes                        | **No** (or file absent)           | **Nothing — global copy is preserved.**           | *(none)*                                                                                                                            |
| Yes                        | Yes                               | Remove the now-redundant global amplihack hooks.  | `Removed redundant global amplihack hooks from ~/.claude/settings.json; project-local hooks in .claude/settings.json remain active.` |

Key points:

- **The global hooks are removed only when a project-local copy already exists.**
  If there is no project-local copy, the global hooks are left completely
  untouched. This is what prevents the framework from silently uninstalling
  itself once per session.
- **No false "migration" message is ever printed.** A notice appears only after
  a redundant global copy is confirmed removed, and the wording describes a
  cleanup — not a move. If nothing was deleted, nothing is claimed.
- Third-party (non-amplihack) hooks in the global file are always preserved.
  Only amplihack's own hook entries are removed, and empty hook groups left
  behind by the removal are pruned.
- If the global `~/.claude/settings.json` **exists but cannot be read or
  parsed**, the step makes no changes and surfaces a cautionary notice
  (`⚠️ Global amplihack hooks may exist in ~/.claude/settings.json. Failed to
  read the file for migration.`) rather than silently proceeding. Likewise, if
  a confirmed-redundant cleanup fails to remove the hooks, a `⚠️` notice asks
  you to remove them manually instead of falsely reporting success.

### What you will (and won't) see

Fresh install with only global hooks — session start is silent about hooks and
your global hooks keep working:

```text
$ amplihack   # start a session
# (no hook migration/cleanup notice; global hooks remain active)
```

Once a project also has project-local hooks, the next session start reports the
one-time cleanup of the redundant global copy:

```text
Removed redundant global amplihack hooks from ~/.claude/settings.json;
project-local hooks in .claude/settings.json remain active.
```

## Data-loss protection for malformed settings

Cleanup of amplihack's hooks is defensive about malformed configuration files.
If `~/.claude/settings.json` does not parse to a JSON **object** — for example a
truncated write left an array, a bare string, a number, or `null` — the cleanup
routine **leaves the value untouched** and logs a warning:

```text
WARN  Global settings.json was not a JSON object; left unchanged
```

A cleanup of amplihack's own hooks will **never** overwrite an unexpected value
with an empty object (`{}`), so a partially written or hand-edited settings file
is never zeroed out.

All reads and writes use amplihack's atomic JSON file mechanism (temp-write +
atomic rename with a consistent backup), so a crash or a concurrent writer
cannot leave `settings.json` half-written by this step.

## `amplihack doctor`: hook detection

`amplihack doctor` includes a **hooks installed** health check. It passes when
**either** location contains amplihack hooks:

- global `~/.claude/settings.json`, **or**
- project-local `<cwd>/.claude/settings.json`

This means a project that is installed **only** project-locally is correctly
reported as healthy.

### Passing output

Run from a directory whose `.claude/settings.json` (or your global settings)
contains amplihack hooks:

```text
$ amplihack doctor
✓ amplihack hooks installed
...
```

### Failing output

If **neither** location contains amplihack hooks, the check fails and names both
places it looked:

```text
$ amplihack doctor
✗ amplihack hooks not found in settings.json (checked global
  ~/.claude/settings.json and project-local .claude/settings.json)
```

### Secret hygiene

The hooks check never prints the contents of any `settings.json`. It reports
only presence/validity, and any read/parse error messages are truncated. This
preserves the doctor command's existing secret-hygiene behavior.

## Frequently asked questions

**Will amplihack delete my global hooks if I only ever install globally?**
No. Global hooks are removed only when a project-local copy already exists. A
global-only install is never touched and `amplihack doctor` reports it as
healthy.

**I have a project-local install but `doctor` used to say hooks were missing.**
`amplihack doctor` now checks the project-local
`<cwd>/.claude/settings.json` in addition to the global file, so a
project-local install passes the check.

**Does this step write project-local hooks for me?**
No. This compatibility step only **reads** the project-local file to decide
whether the global copy is redundant. Writing project-local hooks (for example
choosing an install scope during the install wizard) is a separate feature.
