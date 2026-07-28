# Worked Example — Classify & route a signal

Behavior: given an inbound signal, classify it as `urgent | normal | ignore` and route
accordingly. Below: the naive "just write a function" version vs. the agentic-first
version.

## Naive version (rejected — AP-2)

```rust
// Hardcoded judgment in Rust: brittle, cannot explain itself, rots on new phrasings.
fn classify_signal(text: &str) -> Signal {
    if text.contains("URGENT") || text.contains("down") { Signal::Urgent }
    else if text.contains("fyi") { Signal::Ignore }
    else { Signal::Normal }
}
```

Problems: keyword branches are judgment masquerading as code (AP-2); adding a class
means editing/recompiling code; it cannot handle "the site is unreachable" (no keyword
match) and cannot explain its call.

## Agentic-first version

**Step 1 — goal + success criteria:** "Route each signal by urgency." Success: every
signal produces a typed record with `decision ∈ {urgent,normal,ignore}` and the router
takes the matching branch.

**Step 2 — workflow:** `classify` (judgment) → `persist-record` (write typed record) →
`route` (rail reads fail-closed, branches).

**Step 3 — classify:** `classify` = **(a) agentic** (judgment). `persist-record` =
**(b) tool**. `route` = **(c) thin rail**.

**Step 4 — recipe** (uses Templates A–C):

```yaml
steps:
  - id: "classify"
    type: "agent"
    agent: "amplihack:core:architect"
    prompt: |
      === [RECIPE PROGRESS] Step: classify ===
      Read the signal at {{input_ref}}. Classify urgency as one of:
      urgent (service impact / time-critical), normal (routine), ignore (FYI/noise).
      Judge intent, not keywords. Emit ONLY this typed record (no prose):
      {"schema":"signal.decision","schema_version":1,"nonce":"{{nonce}}",
       "created_at":"{{now}}","decision":"<urgent|normal|ignore>",
       "confidence":"<low|medium|high>","rationale":"<one line>"}
    output: "signal_record"

  - id: "persist-record"
    type: "bash"
    command: |
      set -euo pipefail
      ( umask 177; : > "$RECORD_PATH" )        # 0o600 at creation (AP-1)
      cat > "$RECORD_PATH"                       # payload via stdin, not argv (AP-4)
    output: "record_written"

  - id: "route"
    type: "bash"
    command: |
      set -euo pipefail
      decision=$("{{repo_path}}/scripts/read_record.sh" "$RECORD_PATH")  # fail-closed
      case "$decision" in
        urgent) gh issue create --label urgent --title "signal: urgent" ;;
        normal) echo "queued: normal" ;;
        ignore) echo "dropped: ignore" ;;
        *)      echo "unknown decision: $decision" >&2; exit 1 ;;   # fail-closed
      esac
    output: "route_result"
```

**Step 5 — thin rail:** only `scripts/read_record.sh` (Template D, ~10 lines). No
imperative *judgment* — the `case` merely routes on a typed field.

**Step 6 — verify:** run the recipe; confirm a record with a valid `decision` is
written `0o600`, the router branches correctly, and no stdout scraping is involved.

**Contrast:** judgment moved from brittle Rust branches into an explainable prompt;
adding a class is a prompt edit, not a recompile; the rail shrank to a fail-closed
reader plus a `case`.
