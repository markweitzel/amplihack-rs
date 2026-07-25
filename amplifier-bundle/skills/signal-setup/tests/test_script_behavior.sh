#!/usr/bin/env bash
# TDD behavioral tests for signal-setup.sh — the safe, side-effect-free surface.
#
# Run: bash amplifier-bundle/skills/signal-setup/tests/test_script_behavior.sh
#
# These exercise the ACTUAL script with a sandboxed HOME and mocked binaries
# (signal-cli, qrencode, systemd-run, sudo, az, nc) so nothing ever touches the
# real Signal service, systemd, sudo, or Azure. We deliberately never drive the
# not-linked *mint* path (which would invoke a real device-link); that behavior
# is pinned by source-invariant assertions in test_skill_structure.sh instead.
#
# What is verified here:
#   * --help succeeds and prints usage;
#   * --host is required; unknown flags are rejected;
#   * strict input validation FAILS CLOSED for command/arg-injection attempts
#     in --host / --group / --phone / --resource-group;
#   * valid inputs pass validation;
#   * idempotency: an already-linked host is a no-op that NEVER renders a QR
#     (local and remote);
#   * remote mode genuinely drives the az CLI;
#   * prerequisite failures (missing signal-cli / qrencode) abort clearly.
#
# Each test asserts the exit status AND a content signal so a wrong-reason pass
# cannot slip through.

set -uo pipefail

PASS=0
FAIL=0
pass() { echo "  PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $1"; FAIL=$((FAIL + 1)); }

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SKILL_DIR="$(dirname "$SCRIPT_DIR")"
IMPL="$SKILL_DIR/scripts/signal-setup.sh"

if [[ ! -x "$IMPL" ]]; then
  echo "  FAIL: $IMPL is missing or not executable"
  echo "Results: 0 passed, 1 failed"
  exit 1
fi

# --------------------------------------------------------------------------- #
# Sandbox + mock binaries
# --------------------------------------------------------------------------- #
SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT
MOCKBIN="$SANDBOX/.local/bin"          # signal-setup.sh prepends $HOME/.local/bin
mkdir -p "$MOCKBIN"
QR_LOG="$SANDBOX/qrencode.calls"       # touched iff qrencode actually runs
AZ_LOG="$SANDBOX/az.calls"

# Mock signal-cli: honours --version, listAccounts (linked iff MOCK_LINKED_NUMBER
# is set), and is a harmless no-op for anything else (e.g. daemon).
cat >"$MOCKBIN/signal-cli" <<'EOF'
#!/usr/bin/env bash
for a in "$@"; do
  [ "$a" = "--version" ] && { echo "signal-cli 0.14.5"; exit 0; }
done
for a in "$@"; do
  if [ "$a" = "listAccounts" ]; then
    if [ -n "${MOCK_LINKED_NUMBER:-}" ]; then
      echo "Number: ${MOCK_LINKED_NUMBER}"
    fi
    exit 0
  fi
done
exit 0
EOF

# Mock qrencode: record every invocation. Its mere execution means a QR was
# rendered — the tests assert it is ABSENT on the idempotent path.
cat >"$MOCKBIN/qrencode" <<EOF
#!/usr/bin/env bash
echo "qrencode \$*" >> "$QR_LOG"
exit 0
EOF

# Mock systemd-run: present-but-inert (idempotent path never mints).
cat >"$MOCKBIN/systemd-run" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF

# Mock systemctl: reports units inactive.
cat >"$MOCKBIN/systemctl" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *is-active*) echo "inactive" ;;
esac
exit 0
EOF

# Mock sudo: transparent passthrough (never reached on the tested paths, but
# safe if it were).
cat >"$MOCKBIN/sudo" <<'EOF'
#!/usr/bin/env bash
exec "$@"
EOF

# Mock az: emulate `az vm run-command invoke --scripts <script>`. Prereq probe
# returns SIGCLI_OK/SYSTEMD_OK; listAccounts returns the linked number.
cat >"$MOCKBIN/az" <<EOF
#!/usr/bin/env bash
echo "az \$*" >> "$AZ_LOG"
script=""
prev=""
for a in "\$@"; do
  [ "\$prev" = "--scripts" ] && script="\$a"
  prev="\$a"
done
case "\$script" in
  *listAccounts*)
    [ -n "\${MOCK_LINKED_NUMBER:-}" ] && echo "Number: \${MOCK_LINKED_NUMBER}" ;;
  *"test -x"*|*SIGCLI_OK*)
    echo "SIGCLI_OK"; echo "SYSTEMD_OK" ;;
  *) : ;;
esac
exit 0
EOF

# Mock nc / qrencode-adjacent tools present but inert.
cat >"$MOCKBIN/nc" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF

chmod +x "$MOCKBIN"/*

# Curated real tools symlinked into MOCKBIN so we can run with PATH=$MOCKBIN
# ONLY. This is what makes the sandbox hermetic: without it, a real system
# qrencode (or signal-cli) on /usr/bin would leak in and defeat the
# missing-prerequisite tests.
for t in bash env grep sed seq sleep date hostname rm head cat timeout tr; do
  real="$(command -v "$t" 2>/dev/null)" || continue
  ln -sf "$real" "$MOCKBIN/$t"
done

# Run the script under the sandbox with a hermetic PATH. Extra env/args pass
# through.  Usage: run_ss <args...>  ; captures OUT (stdout+stderr) and RC.
run_ss() {
  OUT="$(env -i \
    HOME="$SANDBOX" \
    PATH="$MOCKBIN" \
    MOCK_LINKED_NUMBER="${MOCK_LINKED_NUMBER:-}" \
    /bin/bash "$IMPL" "$@" 2>&1)"
  RC=$?
}

reset_logs() { : >"$QR_LOG"; : >"$AZ_LOG"; }

echo "═══════════════════════════════════════════════════════"
echo "  Test Suite: signal-setup.sh — Behavior (mocked)"
echo "═══════════════════════════════════════════════════════"

# ─── Test 1: --help ─────────────────────────────────────────────────────────
echo ""
echo "Test 1: --help prints usage and exits 0"
reset_logs
MOCK_LINKED_NUMBER="" run_ss --help
if [[ "$RC" -eq 0 ]] && echo "$OUT" | grep -qi "USAGE"; then
  pass "--help exits 0 and prints USAGE"
else
  fail "--help must exit 0 and print USAGE (rc=$RC)"
fi

# ─── Test 2: --host is required ─────────────────────────────────────────────
echo ""
echo "Test 2: missing --host aborts non-zero"
reset_logs
MOCK_LINKED_NUMBER="" run_ss
if [[ "$RC" -ne 0 ]] && echo "$OUT" | grep -qi "host"; then
  pass "missing --host fails with a message about host"
else
  fail "missing --host must abort non-zero with a host message (rc=$RC)"
fi

# ─── Test 3: unknown flag is rejected ───────────────────────────────────────
echo ""
echo "Test 3: unknown argument is rejected"
reset_logs
MOCK_LINKED_NUMBER="" run_ss --host local --bogus-flag
if [[ "$RC" -ne 0 ]] && echo "$OUT" | grep -qi "unknown"; then
  pass "unknown flag rejected non-zero"
else
  fail "unknown flag must be rejected (rc=$RC)"
fi

# ─── Test 4: injection fails closed (the security contract) ─────────────────
echo ""
echo "Test 4: input validation FAILS CLOSED against injection"

# host injection
reset_logs
MOCK_LINKED_NUMBER="" run_ss --host 'x;reboot' -y
if [[ "$RC" -ne 0 ]] && echo "$OUT" | grep -qi "invalid"; then
  pass "malicious --host rejected"
else
  fail "malicious --host must be rejected (rc=$RC)"
fi
if [[ ! -s "$QR_LOG" ]]; then
  pass "no QR rendered for a rejected host"
else
  fail "a rejected host must not reach QR rendering"
fi

# host with command substitution (payload MUST stay literal, not expand)
reset_logs
# shellcheck disable=SC2016
MOCK_LINKED_NUMBER="" run_ss --host '$(touch /tmp/pwn)' -y
if [[ "$RC" -ne 0 ]]; then
  pass "--host with \$(...) rejected"
else
  fail "--host with command substitution must be rejected (rc=$RC)"
fi

# group injection
reset_logs
MOCK_LINKED_NUMBER="" run_ss --host local --group 'a";rm -rf /' -y
if [[ "$RC" -ne 0 ]] && echo "$OUT" | grep -qi "invalid"; then
  pass "malicious --group rejected"
else
  fail "malicious --group must be rejected (rc=$RC)"
fi

# resource-group injection
reset_logs
MOCK_LINKED_NUMBER="" run_ss --host somevm --resource-group 'rg;curl evil' -y
if [[ "$RC" -ne 0 ]] && echo "$OUT" | grep -qi "invalid"; then
  pass "malicious --resource-group rejected"
else
  fail "malicious --resource-group must be rejected (rc=$RC)"
fi

# phone: non-E.164
reset_logs
MOCK_LINKED_NUMBER="" run_ss --host local --phone '15551234567' -y   # missing +
if [[ "$RC" -ne 0 ]] && echo "$OUT" | grep -qi "invalid"; then
  pass "non-E.164 --phone rejected (missing +)"
else
  fail "non-E.164 --phone must be rejected (rc=$RC)"
fi

reset_logs
MOCK_LINKED_NUMBER="" run_ss --host local --phone '+1555;evil' -y
if [[ "$RC" -ne 0 ]]; then
  pass "--phone with metacharacters rejected"
else
  fail "--phone with metacharacters must be rejected (rc=$RC)"
fi

# ─── Test 5: valid inputs pass validation (reach prereqs/idempotency) ───────
echo ""
echo "Test 5: valid inputs pass validation"

# A valid group name WITH a space is allowed by the documented charset; paired
# with an already-linked host so we exit cleanly without minting.
reset_logs
MOCK_LINKED_NUMBER="+15551234567" run_ss \
  --host local --phone +15551234567 --group "amplihack test" --no-daemon -y
if [[ "$RC" -eq 0 ]]; then
  pass "valid --group (with space) + valid --phone accepted"
else
  fail "valid inputs must pass validation (rc=$RC): $OUT"
fi

# ─── Test 6: idempotency (LOCAL) — already linked is a no-op, NO QR ─────────
echo ""
echo "Test 6: LOCAL idempotency — already-linked host renders NO QR"
reset_logs
MOCK_LINKED_NUMBER="+15551234567" run_ss \
  --host local --phone +15551234567 --no-daemon -y
if [[ "$RC" -eq 0 ]] && echo "$OUT" | grep -qi "already linked"; then
  pass "already-linked local host reports 'already linked' and exits 0"
else
  fail "already-linked local host must be a clean no-op (rc=$RC): $OUT"
fi
if [[ ! -s "$QR_LOG" ]]; then
  pass "no QR rendered when already linked (idempotent)"
else
  fail "an already-linked host must NOT render a QR (found: $(cat "$QR_LOG"))"
fi

# ─── Test 7: idempotency (REMOTE) — uses az, renders NO QR ──────────────────
echo ""
echo "Test 7: REMOTE idempotency — drives az CLI, renders NO QR"
reset_logs
MOCK_LINKED_NUMBER="+15551234567" run_ss \
  --host devvm --phone +15551234567 --resource-group rysweet-linux-vm-pool --no-daemon -y
if [[ "$RC" -eq 0 ]] && echo "$OUT" | grep -qi "already linked"; then
  pass "already-linked remote host reports 'already linked' and exits 0"
else
  fail "already-linked remote host must be a clean no-op (rc=$RC): $OUT"
fi
if grep -q 'run-command' "$AZ_LOG" 2>/dev/null; then
  pass "remote path invoked 'az vm run-command'"
else
  fail "remote path must invoke az vm run-command (az.calls: $(cat "$AZ_LOG" 2>/dev/null))"
fi
if [[ ! -s "$QR_LOG" ]]; then
  pass "no QR rendered for an already-linked remote host"
else
  fail "already-linked remote host must NOT render a QR"
fi

# ─── Test 8: mode auto-detection ────────────────────────────────────────────
echo ""
echo "Test 8: --host local => local mode (no az); named host => remote (az)"

# local: az must NOT be touched at all
reset_logs
MOCK_LINKED_NUMBER="+15551234567" run_ss --host local --phone +15551234567 --no-daemon -y
if [[ ! -s "$AZ_LOG" ]]; then
  pass "local mode never invokes az"
else
  fail "local mode must not invoke az (az.calls: $(cat "$AZ_LOG"))"
fi

# ─── Test 9: prerequisite failures abort clearly ────────────────────────────
echo ""
echo "Test 9: missing prerequisites abort with a clear message"

# Remove qrencode from the sandbox -> local prereq check must fail.
mv "$MOCKBIN/qrencode" "$SANDBOX/qrencode.bak"
reset_logs
MOCK_LINKED_NUMBER="+15551234567" run_ss --host local --phone +15551234567 --no-daemon -y
if [[ "$RC" -ne 0 ]] && echo "$OUT" | grep -qi "qrencode"; then
  pass "missing qrencode aborts with a qrencode message"
else
  fail "missing qrencode must abort clearly (rc=$RC): $OUT"
fi
mv "$SANDBOX/qrencode.bak" "$MOCKBIN/qrencode"

# Remove signal-cli -> local prereq check must fail.
mv "$MOCKBIN/signal-cli" "$SANDBOX/signal-cli.bak"
reset_logs
MOCK_LINKED_NUMBER="+15551234567" run_ss --host local --phone +15551234567 --no-daemon -y
if [[ "$RC" -ne 0 ]] && echo "$OUT" | grep -qi "signal-cli"; then
  pass "missing signal-cli aborts with a signal-cli message"
else
  fail "missing signal-cli must abort clearly (rc=$RC): $OUT"
fi
mv "$SANDBOX/signal-cli.bak" "$MOCKBIN/signal-cli"

# ─── Summary ─────────────────────────────────────────────────────────────────
echo ""
echo "═══════════════════════════════"
echo "Results: $PASS passed, $FAIL failed"
echo "═══════════════════════════════"

[[ "$FAIL" -gt 0 ]] && exit 1
exit 0
