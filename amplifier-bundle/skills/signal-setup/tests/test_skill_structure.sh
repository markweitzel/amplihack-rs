#!/usr/bin/env bash
# TDD contract tests for the `signal-setup` skill — structure + documentation +
# script-source invariants.
#
# Run: bash amplifier-bundle/skills/signal-setup/tests/test_skill_structure.sh
#
# These tests are the executable specification for the skill. They lock every
# hard-won, verified learning the skill exists to preserve so it can never be
# silently dropped by a future edit:
#
#   1. the ~60s device-link window (server close code 1001);
#   2. ANSIUTF8i (inverted) QR rendering for DARK terminals;
#   3. systemd-run persistence (transient unit, remote via az run-command);
#   4. NEVER routing the QR through a Signal message/attachment (the deprecated
#      relay/daemon slow path that blew the 60s window).
#
# Plus prerequisites, linkage-verification signals, the daemon/self-group
# post-test, the azlin/bastion operational gotchas, idempotency, the --host
# contract, and the security model. Written test-first: each assertion names
# exactly which part of the contract is unmet when it fails.
#
# Self-contained: no network, no build, no signal-cli. Follows the grep-based
# pattern already used by pr-guide / code-atlas / code-philosophy in this repo.

set -uo pipefail

PASS=0
FAIL=0
pass() { echo "  PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $1"; FAIL=$((FAIL + 1)); }

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SKILL_DIR="$(dirname "$SCRIPT_DIR")"
SKILL_FILE="$SKILL_DIR/SKILL.md"
SECURITY_FILE="$SKILL_DIR/SECURITY.md"
IMPL="$SKILL_DIR/scripts/signal-setup.sh"

# grep helpers (case-insensitive by default; _f = fixed string; _cs = cased).
g()   { grep -qiE -- "$1" "$2" 2>/dev/null; }
gf()  { grep -qiF -- "$1" "$2" 2>/dev/null; }
gfc() { grep -qF  -- "$1" "$2" 2>/dev/null; }

echo "═══════════════════════════════════════════════════════"
echo "  Test Suite: signal-setup — Structure & Documentation"
echo "═══════════════════════════════════════════════════════"

# ─── Test 1: Required files exist and script is executable ───────────────────
echo ""
echo "Test 1: Required files exist"

if [[ -f "$SKILL_FILE" ]]; then
  pass "SKILL.md exists"
else
  fail "SKILL.md not found at $SKILL_FILE"
  echo "Results: $PASS passed, $FAIL failed"; exit 1
fi

if [[ -f "$IMPL" ]]; then
  pass "scripts/signal-setup.sh exists"
else
  fail "scripts/signal-setup.sh not found at $IMPL"
  echo "Results: $PASS passed, $FAIL failed"; exit 1
fi

if [[ -x "$IMPL" ]]; then
  pass "scripts/signal-setup.sh is executable"
else
  fail "scripts/signal-setup.sh must be executable (chmod +x)"
fi

if [[ -f "$SECURITY_FILE" ]]; then
  pass "SECURITY.md exists"
else
  fail "SECURITY.md not found at $SECURITY_FILE"
fi

# ─── Test 2: Frontmatter — name + description ───────────────────────────────
echo ""
echo "Test 2: YAML frontmatter identifies the skill"

if [[ "$(head -1 "$SKILL_FILE")" == "---" ]]; then
  pass "frontmatter starts with --- on the first byte"
else
  fail "frontmatter: first line must be '---'"
fi

FRONTMATTER="$(awk 'NR==1 && $0=="---"{f=1; next} f && $0=="---"{exit} f{print}' "$SKILL_FILE")"

if echo "$FRONTMATTER" | grep -qE "^name:[[:space:]]*signal-setup[[:space:]]*$"; then
  pass "frontmatter: name is exactly 'signal-setup'"
else
  fail "frontmatter: name must be exactly 'signal-setup'"
fi

if echo "$FRONTMATTER" | grep -qE "^description:"; then
  pass "frontmatter: description field present"
else
  fail "frontmatter: description field missing"
fi

# Description must carry the discovery trigger keywords and both host modes.
for kw in "Signal" "link" "amplihack"; do
  if echo "$FRONTMATTER" | grep -qiF "$kw"; then
    pass "description contains trigger keyword: '$kw'"
  else
    fail "description missing trigger keyword: '$kw'"
  fi
done

if echo "$FRONTMATTER" | grep -qiF "local" && echo "$FRONTMATTER" | grep -qiE "remote|azlin"; then
  pass "description mentions both local and remote (azlin) hosts"
else
  fail "description must mention both local and remote (azlin VM) hosts"
fi

# ─── Test 3: HARD-WON FACT #1 — the ~60s window / close code 1001 ───────────
echo ""
echo "Test 3: FACT #1 — the ~60-second device-link window (close code 1001)"

if gf "60" "$SKILL_FILE" && ( g "second" "$SKILL_FILE" || gf "60s" "$SKILL_FILE" || g "window" "$SKILL_FILE" ); then
  pass "documents the ~60-second window"
else
  fail "must document the ~60-second device-link window"
fi

if gf "1001" "$SKILL_FILE"; then
  pass "cites the server websocket close code 1001"
else
  fail "must cite websocket server close code 1001 (the verified root cause)"
fi

if g "chat.signal.org" "$SKILL_FILE" || g "websocket|provisioning" "$SKILL_FILE"; then
  pass "identifies the provisioning websocket as the mechanism"
else
  fail "must identify the provisioning websocket (chat.signal.org) mechanism"
fi

if gf "invalid response from server" "$SKILL_FILE"; then
  pass "documents the 'invalid response from server' expiry symptom"
else
  fail "must document the 'invalid response from server' expiry symptom"
fi

# The advertised window must be under the hard 60s ceiling.
if grep -qE 'WINDOW_SECONDS=([1-5][0-9])\b' "$IMPL"; then
  pass "script advertises a window strictly under 60s (safety margin)"
else
  fail "script WINDOW_SECONDS must be < 60 (a hair under the 60s ceiling)"
fi

# ─── Test 4: HARD-WON FACT #2 — ANSIUTF8i inverted for dark terminals ───────
echo ""
echo "Test 4: FACT #2 — ANSIUTF8i (inverted) for dark terminals"

if gf "ANSIUTF8i" "$SKILL_FILE"; then
  pass "SKILL.md names the ANSIUTF8i render mode"
else
  fail "SKILL.md must name 'ANSIUTF8i'"
fi

if g "invert" "$SKILL_FILE" && g "dark" "$SKILL_FILE"; then
  pass "explains inverted rendering is required for dark terminals"
else
  fail "must explain ANSIUTF8i is inverted and required for dark terminals"
fi

# The plain (non-inverted) variant must be called out as the failure mode.
if gf "ANSIUTF8 " "$SKILL_FILE" || g "dark-on-dark|invisible|unscannable" "$SKILL_FILE"; then
  pass "warns plain ANSIUTF8 is dark-on-dark / invisible on dark terminals"
else
  fail "must warn that plain ANSIUTF8 is invisible on dark backgrounds"
fi

# ─── Test 5: HARD-WON FACT #3 — systemd-run persistence ─────────────────────
echo ""
echo "Test 5: FACT #3 — systemd-run transient-unit persistence"

if gf "systemd-run" "$SKILL_FILE"; then
  pass "documents launching the link under systemd-run"
else
  fail "must document systemd-run for the persistent link process"
fi

if g "transient|survive|persist" "$SKILL_FILE"; then
  pass "explains the transient unit survives the launching shell"
else
  fail "must explain the transient unit survives the launching shell"
fi

if gf "az vm run-command" "$SKILL_FILE"; then
  pass "remote hosts launch systemd-run via az vm run-command"
else
  fail "must document remote launch via az vm run-command"
fi

if gfc "--uid=azureuser" "$SKILL_FILE"; then
  pass "documents --uid=azureuser (run-command runs as root)"
else
  fail "must document --uid=azureuser so the account lands under azureuser"
fi

# ─── Test 6: HARD-WON FACT #4 — never route the QR through Signal ───────────
echo ""
echo "Test 6: FACT #4 — never deliver the QR via a Signal message/attachment"

if g "never" "$SKILL_FILE" && g "signal (message|attachment)|through signal|via signal" "$SKILL_FILE"; then
  pass "states NEVER route the QR through a Signal message/attachment"
else
  fail "must state the QR is NEVER delivered as a Signal message/attachment"
fi

if g "relay|daemon delivery|40-?60" "$SKILL_FILE" && g "deprecated|slow path|do not" "$SKILL_FILE"; then
  pass "labels the relay/daemon-delivery path as the deprecated slow path"
else
  fail "must label relay/daemon QR delivery as the deprecated slow path"
fi

if g "scan screen|link new device" "$SKILL_FILE"; then
  pass "requires the user to be on the scan screen before minting"
else
  fail "must require the user to be on the scan screen before minting"
fi

# ─── Test 7: Prerequisites ──────────────────────────────────────────────────
echo ""
echo "Test 7: Prerequisites are documented"

if gf "signal-cli" "$SKILL_FILE" && gf "0.14.5" "$SKILL_FILE"; then
  pass "requires signal-cli (known-good 0.14.5)"
else
  fail "must require signal-cli 0.14.5 (known-good version)"
fi

if gf "qrencode" "$SKILL_FILE"; then
  pass "requires qrencode"
else
  fail "must require qrencode"
fi

if g "\baz\b|az cli|az vm" "$SKILL_FILE"; then
  pass "requires the az CLI for remote hosts"
else
  fail "must require the az CLI for remote hosts"
fi

if gf ".local/bin/signal-cli" "$SKILL_FILE"; then
  pass "documents the ~/.local/bin/signal-cli install location"
else
  fail "must document the ~/.local/bin/signal-cli symlink location"
fi

# ─── Test 8: Linkage-verification signals ───────────────────────────────────
echo ""
echo "Test 8: Linkage-verification signals are documented"

if gf "listAccounts" "$SKILL_FILE" && gf "Number:" "$SKILL_FILE"; then
  pass "verifies via listAccounts / 'Number: +<phone>'"
else
  fail "must verify linkage via listAccounts showing 'Number: +<phone>'"
fi

if gf "Associated with" "$SKILL_FILE" && gf "Finishing new device registration" "$SKILL_FILE"; then
  pass "documents the trace-log success markers"
else
  fail "must document 'Associated with:' + 'Finishing new device registration'"
fi

if g "inactive|exits" "$SKILL_FILE"; then
  pass "notes the transient unit goes inactive on success"
else
  fail "must note the systemd unit exits (inactive) on success"
fi

# ─── Test 9: Daemon + self-group + post-test ────────────────────────────────
echo ""
echo "Test 9: Daemon + self-group + post-test are documented"

if gf "127.0.0.1:7583" "$SKILL_FILE"; then
  pass "JSON-RPC daemon binds 127.0.0.1:7583"
else
  fail "must document the JSON-RPC daemon on 127.0.0.1:7583"
fi

if gf "updateGroup" "$SKILL_FILE" && gf "groupId" "$SKILL_FILE"; then
  pass "creates/verifies the self-group via updateGroup -> groupId"
else
  fail "must document updateGroup -> groupId for the self-group"
fi

if gfc '"results":[]' "$SKILL_FILE" && g "empty|success|normal|expected" "$SKILL_FILE"; then
  pass "documents empty results:[] is normal success for a self-group"
else
  fail "must document that {\"results\":[],...} is success for a self-group"
fi

# ─── Test 10: Operational gotchas (azlin / bastion) ─────────────────────────
echo ""
echo "Test 10: azlin / bastion operational gotchas"

if g "SIGPIPE|broken pipe|SIGABRT|core-?dump" "$SKILL_FILE"; then
  pass "warns azlin aborts (SIGABRT/core-dump) on broken pipe (SIGPIPE)"
else
  fail "must warn azlin aborts on SIGPIPE (never pipe into grep -q)"
fi

if g "grep -q" "$SKILL_FILE" || g "capture .*(full|output) first|early-closing" "$SKILL_FILE"; then
  pass "instructs capturing full output before filtering"
else
  fail "must instruct capturing full output first (no early-closing consumer)"
fi

if g "one bastion|single bastion|concurrent|one .* session at a time" "$SKILL_FILE"; then
  pass "warns only ONE bastion session per host at a time"
else
  fail "must warn only one bastion/azlin session per host at a time"
fi

if gf "rysweet-linux-vm-pool" "$SKILL_FILE" && gfc "--no-tmux" "$SKILL_FILE"; then
  pass "documents the azlin connect invocation form"
else
  fail "must document 'azlin connect <host> --resource-group ... --no-tmux -y --'"
fi

# ─── Test 11: Idempotency + --host contract ─────────────────────────────────
echo ""
echo "Test 11: Idempotency and the --host contract"

if g "idempotent|already linked|nothing to do" "$SKILL_FILE"; then
  pass "documents idempotent behavior (skip when already linked)"
else
  fail "must document idempotent behavior"
fi

if gfc "--host" "$SKILL_FILE"; then
  pass "documents the --host argument"
else
  fail "must document the --host argument (local name or remote azlin VM)"
fi

# ─── Test 12: Security model ────────────────────────────────────────────────
echo ""
echo "Test 12: Security model"

if gfc "UMask=0077" "$SECURITY_FILE"; then
  pass "SECURITY.md pins systemd unit UMask=0077 for secret files"
else
  fail "SECURITY.md must document --property=UMask=0077 for the link secrets"
fi

if gf "0600" "$SECURITY_FILE"; then
  pass "SECURITY.md requires 0600 on the link URI / trace log"
else
  fail "SECURITY.md must require 0600 permissions on secret temp files"
fi

if gf "127.0.0.1" "$SECURITY_FILE" && g "loopback|localhost only|local only" "$SECURITY_FILE"; then
  pass "SECURITY.md documents loopback-only daemon binding"
else
  fail "SECURITY.md must document the loopback-only daemon bind"
fi

if g "allowlist|validation|injection" "$SECURITY_FILE"; then
  pass "SECURITY.md documents input-validation / injection defense"
else
  fail "SECURITY.md must document input validation against injection"
fi

# ─── Test 13: SKILL.md is a substantive index ───────────────────────────────
echo ""
echo "Test 13: SKILL.md size sanity"

SKILL_LINES="$(wc -l <"$SKILL_FILE")"
if [[ "$SKILL_LINES" -ge 60 ]]; then
  pass "SKILL.md is substantive ($SKILL_LINES lines)"
else
  fail "SKILL.md is only $SKILL_LINES lines — likely incomplete"
fi

# ─── Test 14: Script-source invariants ──────────────────────────────────────
# The SKILL.md documents the facts; these assertions prove the *implementation*
# actually encodes them, so docs and code cannot drift apart.
echo ""
echo "Test 14: Script encodes the invariants it documents"

if gf "set -uo pipefail" "$IMPL"; then
  pass "script uses 'set -uo pipefail' (strict-ish mode, no -e footguns)"
else
  fail "script must set -uo pipefail"
fi

# QR MUST be rendered with the inverted ANSIUTF8i mode.
if grep -qE 'qrencode[^\n]*-t[[:space:]]+ANSIUTF8i' "$IMPL"; then
  pass "qrencode is invoked with '-t ANSIUTF8i'"
else
  fail "qrencode must be invoked with -t ANSIUTF8i (inverted)"
fi

# NEVER route the QR through Signal: the sgnl:// URI must never be sent.
if grep -qE 'send[^\n]*sgnl://' "$IMPL"; then
  fail "the sgnl:// link URI must NEVER be passed to a Signal 'send'"
else
  pass "the link URI is never passed to a Signal send (terminal-only delivery)"
fi

# The remote mint must run under systemd-run with UMask pinned, as root->azureuser.
if grep -qc 'property=UMask=0077' "$IMPL" >/dev/null && [[ "$(grep -c 'property=UMask=0077' "$IMPL")" -ge 2 ]]; then
  pass "both (local+remote) systemd-run calls pin --property=UMask=0077"
else
  fail "both systemd-run invocations must pin --property=UMask=0077"
fi

if gfc "--uid=azureuser" "$IMPL" && gf "az vm run-command" "$IMPL"; then
  pass "remote path uses az vm run-command + --uid=azureuser"
else
  fail "remote path must use az vm run-command with --uid=azureuser"
fi

# Strict E.164 phone validation guards the injection surface.
if grep -qE '\^\\\+\[1-9\]' "$IMPL" || grep -qF '^\+[1-9][0-9]' "$IMPL"; then
  pass "phone is validated against a strict E.164 pattern"
else
  fail "phone must be validated against a strict E.164 pattern"
fi

# An allowlist validate() helper must exist and fail closed via die().
if grep -qE '^validate\(\)' "$IMPL" && gf "die" "$IMPL"; then
  pass "validate() allowlist helper fails closed via die()"
else
  fail "must have a validate() allowlist helper that fails closed (die)"
fi

# Daemon post-test contract encoded in the script.
if gf "daemon --tcp" "$IMPL" && gf "127.0.0.1:7583" "$IMPL"; then
  pass "script starts the daemon on 127.0.0.1:7583"
else
  fail "script must start the JSON-RPC daemon on 127.0.0.1:7583"
fi

if gf "updateGroup" "$IMPL" && grep -qF '"results":\[\]' "$IMPL"; then
  pass "script ensures the self-group and treats results:[] as success"
else
  fail "script must ensure the self-group and treat results:[] as success"
fi

# ─── Test 15: No leaked secrets in skill files ──────────────────────────────
echo ""
echo "Test 15: No leaked secrets"

if grep -REq "(sk-[A-Za-z0-9]{20,}|ghp_[A-Za-z0-9]{20,}|xoxb-|AKIA[0-9A-Z]{16})" \
     "$SKILL_FILE" "$SECURITY_FILE" "$IMPL" 2>/dev/null; then
  fail "skill files may contain secrets / API keys"
else
  pass "no secrets detected in skill files"
fi

# A real sgnl:// provisioning URI must never be committed (only placeholders).
if grep -qE 'sgnl://linkdevice\?[A-Za-z0-9%]{20,}' "$SKILL_FILE" "$IMPL" 2>/dev/null; then
  fail "a concrete sgnl:// provisioning URI appears to be committed"
else
  pass "no concrete sgnl:// provisioning URI is committed"
fi

# ─── Summary ─────────────────────────────────────────────────────────────────
echo ""
echo "═══════════════════════════════"
echo "Results: $PASS passed, $FAIL failed"
echo "═══════════════════════════════"

[[ "$FAIL" -gt 0 ]] && exit 1
exit 0
