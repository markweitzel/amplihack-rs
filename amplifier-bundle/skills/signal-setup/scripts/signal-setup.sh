#!/usr/bin/env bash
#
# signal-setup.sh — end-to-end Signal device-linking for amplihack.
#
# Generalizes the proven zero-latency in-terminal QR linking loop into a single
# idempotent, reusable command for LOCAL and REMOTE (azlin VM) hosts.
#
# See SKILL.md for the operational invariants: the ~60s Signal provisioning
# window, ANSIUTF8i QR rendering, terminal-only QR delivery, systemd-run
# persistence, and azlin/bastion gotchas.
#
set -uo pipefail

# --------------------------------------------------------------------------- #
# Defaults / configuration
# --------------------------------------------------------------------------- #
SIGNAL_CLI="${SIGNAL_CLI:-$HOME/.local/bin/signal-cli}"
SIGNAL_CLI_REMOTE="/home/azureuser/.local/bin/signal-cli"
RESOURCE_GROUP="${SIGNAL_SETUP_RG:-rysweet-linux-vm-pool}"
DAEMON_TCP="${SIGNAL_SETUP_DAEMON_TCP:-127.0.0.1:7583}"
QR_MARGIN=2
WINDOW_SECONDS=55   # advertise a hair under 60 so the user is never late
AZ_RUN_TIMEOUT_SECONDS="${SIGNAL_SETUP_AZ_TIMEOUT_SECONDS:-15}"
LOCAL_SIGNAL_TIMEOUT_SECONDS="${SIGNAL_SETUP_LOCAL_TIMEOUT_SECONDS:-10}"

HOST=""
PHONE="${SIGNAL_PHONE:-}"
GROUP_NAME="amplihack"
MODE=""             # local | remote (auto-detected if empty)
DO_DAEMON=1
ASSUME_YES=0
DAEMON_UNIT=""

export PATH="$HOME/.local/bin:$PATH"

# --------------------------------------------------------------------------- #
# Helpers
# --------------------------------------------------------------------------- #
log()  { printf '%s\n' "$*" >&2; }
info() { printf '\033[0;36m%s\033[0m\n' "$*" >&2; }
ok()   { printf '\033[0;32m%s\033[0m\n' "$*" >&2; }
warn() { printf '\033[0;33m%s\033[0m\n' "$*" >&2; }
err()  { printf '\033[0;31m%s\033[0m\n' "$*" >&2; }
die()  { err "!! $*"; exit 1; }

usage() {
  cat >&2 <<'USAGE'
signal-setup.sh — link a host to Signal for amplihack (end-to-end, idempotent).

USAGE:
  signal-setup.sh --host <name> [options]

OPTIONS:
  --host <name>        Target host. "local"/current hostname => mint locally.
                       Any other name => remote azlin VM (via az run-command).
  --phone <+E164>      Signal phone number for verify/daemon/group steps.
                       Falls back to $SIGNAL_PHONE. Required for daemon/group.
  --group <name>       Self-group name for the post-test (default: amplihack).
  --resource-group <rg>  Azure RG for remote hosts (default: rysweet-linux-vm-pool).
  --local              Force local mint.
  --remote             Force remote mint.
  --no-daemon          Skip the daemon + self-group + post-test step.
  --daemon             Force the daemon + self-group + post-test step (default).
  -y, --yes            Non-interactive: assume the phone scan screen is ready.
  -h, --help           Show this help.

FLOW:
  prereq check -> confirm scan screen open -> mint under systemd-run
  -> print ANSIUTF8i QR in-terminal (~60s window) -> verify linkage
  -> (optional) start daemon + ensure self-group + post-test.
USAGE
}

# JSON-escape a raw string for safe embedding inside a JSON string value.
json_escape() { # json_escape <raw>
  local s="$1"
  s="${s//\\/\\\\}"   # backslash first
  s="${s//\"/\\\"}"   # double-quote
  s="${s//$'\n'/\\n}" # newline
  s="${s//$'\r'/\\r}" # carriage return
  s="${s//$'\t'/\\t}" # tab
  printf '%s' "$s"
}

# Build a JSON-RPC request line for the signal-cli TCP daemon.
rpc() { # rpc <method> <params-json>
  printf '{"jsonrpc":"2.0","method":"%s","params":%s,"id":1}\n' "$1" "$2"
}

# True iff the signal-cli JSON-RPC daemon is accepting connections on DAEMON_TCP.
# Derives the /dev/tcp path from DAEMON_TCP so the endpoint is defined once.
daemon_up() {
  [ "${SIGNAL_SETUP_TEST_DAEMON_UP:-0}" = "1" ] && return 0
  (exec 3<>"/dev/tcp/${DAEMON_TCP/:/\/}") 2>/dev/null
}

# Run a command ON the remote target host via az run-command. Captures FULL
# output first (never pipes az/azlin into an early-closing reader — SIGPIPE
# core-dumps azlin), then returns it for the caller to filter.
remote_run() {
  local script="$1" out
  out="$(timeout "$AZ_RUN_TIMEOUT_SECONDS" az vm run-command invoke -g "$RESOURCE_GROUP" -n "$HOST" \
    --command-id RunShellScript --scripts "$script" \
    --query 'value[0].message' -o tsv 2>&1)"
  local rc=$?
  if [ "$rc" -ne 0 ]; then
    [ -n "$out" ] && printf '%s\n' "$out" >&2
    return "$rc"
  fi
  printf '%s' "$out"
}

# --------------------------------------------------------------------------- #
# Argument parsing
# --------------------------------------------------------------------------- #
while [ $# -gt 0 ]; do
  case "$1" in
    --host)           HOST="${2:-}"; shift 2 ;;
    --phone)          PHONE="${2:-}"; shift 2 ;;
    --group)          GROUP_NAME="${2:-}"; shift 2 ;;
    --resource-group) RESOURCE_GROUP="${2:-}"; shift 2 ;;
    --local)          MODE="local"; shift ;;
    --remote)         MODE="remote"; shift ;;
    --no-daemon)      DO_DAEMON=0; shift ;;
    --daemon)         DO_DAEMON=1; shift ;;
    -y|--yes)         ASSUME_YES=1; shift ;;
    -h|--help)        usage; exit 0 ;;
    *) die "unknown argument: $1 (use --help)" ;;
  esac
done

[ -n "$HOST" ] || { usage; die "--host is required"; }

# --------------------------------------------------------------------------- #
# Input validation — fail closed. These values flow into shell command lines,
# az run-command payloads (executed as root remotely), and JSON-RPC strings,
# so they MUST be strictly constrained to prevent command/argument injection.
# --------------------------------------------------------------------------- #
validate() { # validate <label> <value> <regex>
  case "$2" in
    "") die "$1 must not be empty" ;;
  esac
  # Bash's [[ =~ ]] applies the ERE natively, avoiding a grep subprocess per
  # call. $3 stays unquoted so it is treated as a pattern, not a literal.
  [[ "$2" =~ $3 ]] \
    || die "$1 contains invalid characters: '$2' (allowed: $3)"
}

# Hostnames / VM names: DNS-label + Azure resource charset.
validate "--host" "$HOST" '^[A-Za-z0-9._-]+$'
# Azure resource group naming charset.
validate "--resource-group" "$RESOURCE_GROUP" '^[A-Za-z0-9._()-]+$'
# Self-group name: printable, no shell/JSON metacharacters or whitespace tricks.
validate "--group" "$GROUP_NAME" '^[A-Za-z0-9._ -]+$'
# Phone (when provided): strict E.164.
if [ -n "$PHONE" ]; then
  validate "--phone" "$PHONE" '^\+[1-9][0-9]{7,14}$'
fi

# Auto-detect mode if not forced.
if [ -z "$MODE" ]; then
  if [ "$HOST" = "local" ] || [ "$HOST" = "localhost" ] || [ "$HOST" = "$(hostname)" ]; then
    MODE="local"
  else
    MODE="remote"
  fi
fi

NAME="amplihack-$HOST"
UNIT="sig-link-$HOST"
DAEMON_UNIT="sig-daemon-$HOST"

# Secret handling: the minted sgnl:// link URI is a short-lived provisioning
# secret and the -vv trace log can capture identity material. Restrict every
# byte we write:
#   * umask 077 covers files this script writes directly.
#   * The URI_FILE / LOG_FILE secrets are written by a redirect INSIDE the
#     systemd-run transient unit, which does NOT inherit this umask (systemd
#     defaults to UMask=0022). We therefore pin the unit's UMask explicitly via
#     `--property=UMask=0077` on both systemd-run calls so those files are
#     0600 from birth. See SECURITY.md §4.
#   * Unguessable, per-run path suffix (defeats /tmp symlink pre-creation and
#     disclosure to other local users on a predictable path).
#   * Trap-based cleanup on exit.
umask 077
RUN_TOKEN="$(date +%s)-$$-${RANDOM}${RANDOM}"
URI_FILE="/tmp/slink-${HOST}-${RUN_TOKEN}.out"
LOG_FILE="/tmp/scli-${HOST}-${RUN_TOKEN}.log"
# Local daemon log — routed through the same unguessable token so it is not a
# predictable /tmp path (defeats symlink pre-creation) and is covered by the
# cleanup trap alongside the other secrets. See SECURITY.md §4.
DAEMON_LOG="/tmp/signal-daemon-${HOST}-${RUN_TOKEN}.log"

cleanup_secrets() {
  if [ "$MODE" = "local" ]; then
    rm -f "$URI_FILE" "$LOG_FILE" "$DAEMON_LOG" 2>/dev/null
    sudo rm -f "$URI_FILE" "$LOG_FILE" "$DAEMON_LOG" 2>/dev/null
  else
    remote_run "rm -f $URI_FILE $LOG_FILE" >/dev/null 2>&1
  fi
}
trap cleanup_secrets EXIT INT TERM

info "=== signal-setup: host=$HOST mode=$MODE unit=$UNIT ==="

# --------------------------------------------------------------------------- #
# Step 1: Prerequisites
# --------------------------------------------------------------------------- #
check_prereqs_local() {
  info "[1/6] Checking prerequisites (local)..."
  [ -x "$SIGNAL_CLI" ] || die "signal-cli not found/executable at $SIGNAL_CLI (known-good: 0.14.5 at ~/.local/opt/signal-cli-0.14.5/bin/signal-cli symlinked to ~/.local/bin/signal-cli)"
  command -v qrencode >/dev/null 2>&1 || die "qrencode not installed (apt-get install -y qrencode)"
  command -v systemd-run >/dev/null 2>&1 || die "systemd-run not available on this host"
  ok "  signal-cli: $("$SIGNAL_CLI" --version 2>/dev/null || echo present)"
  ok "  qrencode + systemd-run present"
}

check_prereqs_remote() {
  info "[1/6] Checking prerequisites (remote: $HOST)..."
  command -v az >/dev/null 2>&1 || die "az CLI not installed (required for remote hosts)"
  # We render the QR LOCALLY, so qrencode must also exist locally.
  command -v qrencode >/dev/null 2>&1 || die "qrencode not installed locally (needed to render the remote QR)"
  local out
  out="$(remote_run "test -x $SIGNAL_CLI_REMOTE && echo SIGCLI_OK; command -v systemd-run >/dev/null 2>&1 && echo SYSTEMD_OK")" \
    || die "az vm run-command failed for $HOST in $RESOURCE_GROUP"
  case "$out" in
    *SIGCLI_OK*) : ;;
    *) die "signal-cli missing on $HOST at $SIGNAL_CLI_REMOTE" ;;
  esac
  case "$out" in
    *SYSTEMD_OK*) : ;;
    *) die "systemd-run missing on $HOST" ;;
  esac
  ok "  remote signal-cli + systemd-run present; local qrencode present"
}

# --------------------------------------------------------------------------- #
# Step 2: Idempotency — already linked?
# --------------------------------------------------------------------------- #
already_linked() {
  # Prints the linked number if the host already has an account, else nothing.
  local accounts
  if [ "$MODE" = "local" ]; then
    accounts="$(timeout "$LOCAL_SIGNAL_TIMEOUT_SECONDS" "$SIGNAL_CLI" listAccounts 2>&1)" || return 2
  else
    accounts="$(remote_run "out=\$($SIGNAL_CLI_REMOTE listAccounts 2>&1); rc=\$?; if [ \$rc -ne 0 ]; then echo __SIGNAL_CLI_FAILED__\$rc; printf '%s\n' \"\$out\"; else printf '%s\n' \"\$out\"; fi")" || return 2
    case "$accounts" in
      *__SIGNAL_CLI_FAILED__*) printf '%s\n' "$accounts" >&2; return 2 ;;
    esac
  fi
  # Prefer an explicit phone match when --phone given; otherwise any Number line.
  # NOTE: a non-matching [[ ]] test must NOT leak exit status 1 as the function's
  # return code — the caller ("$(already_linked)" || die) reserves non-zero for
  # a genuine probe FAILURE (return 2 above). "Probe succeeded, not linked yet"
  # is success with empty output, so force an explicit return 0 below.
  if [ -n "$PHONE" ]; then
    [[ "$accounts" == *"Number: $PHONE"* ]] && printf '%s' "$PHONE"
  else
    printf '%s\n' "$accounts" | sed -n 's/.*Number: \(+[0-9][0-9]*\).*/\1/p' | head -n1
  fi
  return 0
}

# --------------------------------------------------------------------------- #
# Step 3: Mint the link URI under a transient systemd unit
# --------------------------------------------------------------------------- #
mint_local() {
  local run_uid run_gid run_home
  run_uid="$(id -u)"
  run_gid="$(id -g)"
  run_home="$HOME"
  systemctl --user reset-failed "$UNIT" 2>/dev/null
  sudo systemctl reset-failed "$UNIT" 2>/dev/null
  sudo systemctl stop "$UNIT" 2>/dev/null
  sudo rm -f "$URI_FILE" "$LOG_FILE"
  sudo systemd-run --unit="$UNIT" --uid="$run_uid" --gid="$run_gid" \
    --property=UMask=0077 \
    --setenv=HOME="$run_home" \
    --setenv=PATH="$run_home/.local/bin:/usr/bin:/bin" \
    /bin/bash -c '"$1" -vv --log-file "$2" link -n "$3" > "$4" 2>&1' \
      bash "$SIGNAL_CLI" "$LOG_FILE" "$NAME" "$URI_FILE" \
    >/dev/null 2>&1
  local _i
  for ((_i = 1; _i <= 20; _i++)); do
    grep -q '^sgnl://' "$URI_FILE" 2>/dev/null && break
    sleep 0.5
  done
  grep -m1 '^sgnl://' "$URI_FILE" 2>/dev/null
}

mint_remote() {
  # run-command runs as ROOT, so --uid/--gid=azureuser is REQUIRED so the
  # linked account lands under the azureuser home, not root's.
  local script out
  script="$(cat <<REMOTE
umask 077
systemctl reset-failed $UNIT 2>/dev/null
systemctl stop $UNIT 2>/dev/null
rm -f $URI_FILE $LOG_FILE
systemd-run --unit=$UNIT --uid=azureuser --gid=azureuser \
  --property=UMask=0077 \
  --setenv=HOME=/home/azureuser \
  --setenv=PATH=/home/azureuser/.local/bin:/usr/bin:/bin \
  /bin/bash -c '$SIGNAL_CLI_REMOTE -vv --log-file $LOG_FILE link -n $NAME > $URI_FILE 2>&1' >/dev/null 2>&1
for i in \$(seq 1 20); do grep -q '^sgnl://' $URI_FILE 2>/dev/null && break; sleep 0.5; done
echo URI_START; grep -m1 '^sgnl://' $URI_FILE 2>/dev/null; echo URI_END
REMOTE
)"
  # Capture FULL output first (SIGPIPE gotcha), then extract.
  out="$(remote_run "$script")" || return 1
  printf '%s\n' "$out" | sed -n 's/.*URI_START//p; /sgnl:\/\//p' | grep -m1 '^sgnl://'
}

# --------------------------------------------------------------------------- #
# Step 4: Verify linkage
# --------------------------------------------------------------------------- #
verify_linkage() {
  info "[4/6] Verifying linkage (up to ${WINDOW_SECONDS}s)..."
  local num unit_state deadline now
  deadline="$(($(date +%s) + WINDOW_SECONDS))"
  while [ "$(date +%s)" -lt "$deadline" ]; do
    num="$(already_linked)" || { warn "  Could not query linked accounts yet; retrying."; num=""; }
    if [ -n "$num" ]; then
      # The transient unit exits (inactive) on success.
      if [ "$MODE" = "local" ]; then
        unit_state="$(systemctl is-active "$UNIT" 2>/dev/null)"
      else
        unit_state="$(remote_run "systemctl is-active $UNIT 2>/dev/null")"
      fi
      ok "  Linked: $num  (unit $UNIT: ${unit_state:-inactive})"
      [ -z "$PHONE" ] && PHONE="$num"
      return 0
    fi
    now="$(date +%s)"
    [ "$now" -lt "$deadline" ] || break
    sleep 1
  done
  err "  No linked account detected within the window."
  err "  Trace log on host: $LOG_FILE"
  err "  Look for: 'Associated with: +<phone>' then 'Finishing new device registration'."
  return 1
}

# --------------------------------------------------------------------------- #
# Step 5+6: Daemon + self-group + post-test (JSON-RPC on 127.0.0.1:7583)
# --------------------------------------------------------------------------- #
remote_daemon_group_posttest() {
  local sigcli="$1" acct_j group_j script out
  acct_j="$(json_escape "$PHONE")"
  group_j="$(json_escape "$GROUP_NAME")"
  script="$(cat <<REMOTE
daemon_up() { (exec 3<>/dev/tcp/127.0.0.1/7583) 2>/dev/null; }
if ! daemon_up; then
  systemctl reset-failed $DAEMON_UNIT 2>/dev/null
  systemd-run --unit=$DAEMON_UNIT --uid=azureuser --gid=azureuser \
    --setenv=HOME=/home/azureuser \
    --setenv=PATH=/home/azureuser/.local/bin:/usr/bin:/bin \
    $sigcli -a '$acct_j' daemon --tcp 127.0.0.1:7583 >/dev/null 2>&1
  for i in \$(seq 1 20); do daemon_up && break; sleep 0.5; done
fi
daemon_up || { echo DAEMON_DOWN; exit 0; }
command -v nc >/dev/null 2>&1 || { echo NC_MISSING; exit 0; }
resp=\$(printf '%s\n' '{"jsonrpc":"2.0","method":"updateGroup","params":{"account":"$acct_j","name":"$group_j"},"id":1}' | timeout 15 nc 127.0.0.1 7583 2>/dev/null | head -n1)
group_id=\$(printf '%s' "\$resp" | sed -n 's/.*"groupId":"\([^"]*\)".*/\1/p')
[ -n "\$group_id" ] || { echo GROUP_ID_MISSING; echo "\$resp"; exit 0; }
resp=\$(printf '{"jsonrpc":"2.0","method":"send","params":{"account":"$acct_j","groupId":"%s","message":"amplihack signal-setup: link verified"},"id":1}\n' "\$group_id" | timeout 15 nc 127.0.0.1 7583 2>/dev/null | head -n1)
case "\$resp" in
  *'"results":[]'*) echo POST_TEST_OK ;;
  *) echo POST_TEST_UNKNOWN; echo "\$resp" ;;
esac
REMOTE
)"
  out="$(remote_run "$script")" || { warn "  Remote daemon/group/post-test failed to run."; return 0; }
  case "$out" in
    *POST_TEST_OK*) ok "  Remote daemon reachable; post-test OK: {\"results\":[],...}" ;;
    *DAEMON_DOWN*) warn "  Remote daemon did not come up on $DAEMON_TCP; skipping post-test." ;;
    *NC_MISSING*) warn "  Remote 'nc' not available; skipping JSON-RPC self-group post-test." ;;
    *GROUP_ID_MISSING*) warn "  Remote updateGroup did not return groupId: $out" ;;
    *) warn "  Remote post-test response (verify manually): $out" ;;
  esac
}

local_daemon_group_posttest() {
  local sigcli="$1"
  if ! daemon_up; then
    "$sigcli" -a "$PHONE" daemon --tcp "$DAEMON_TCP" >"$DAEMON_LOG" 2>&1 &
    local _i
    for ((_i = 1; _i <= 20; _i++)); do
      daemon_up && break
      sleep 0.5
    done
  fi
  daemon_up \
    || { warn "  Daemon did not come up on $DAEMON_TCP; skipping post-test."; return 0; }
  ok "  Daemon reachable on $DAEMON_TCP"

  command -v nc >/dev/null 2>&1 || { warn "  'nc' not available; skipping JSON-RPC self-group post-test."; return 0; }

  info "[6/6] Ensuring self-group '$GROUP_NAME' + post-test ..."
  local resp group_id acct_j group_j nc_host nc_port
  nc_host="${DAEMON_TCP%:*}"; nc_port="${DAEMON_TCP##*:}"
  acct_j="$(json_escape "$PHONE")"
  group_j="$(json_escape "$GROUP_NAME")"
  resp="$(rpc updateGroup "{\"account\":\"$acct_j\",\"name\":\"$group_j\"}" \
    | timeout 15 nc "$nc_host" "$nc_port" 2>/dev/null | head -n1)"
  group_id="$(printf '%s' "$resp" | sed -n 's/.*"groupId":"\([^"]*\)".*/\1/p')"
  if [ -z "$group_id" ]; then
    warn "  Could not obtain groupId from updateGroup response:"
    warn "    $resp"
    return 0
  fi
  ok "  self-group id: $group_id"

  local gid_j
  gid_j="$(json_escape "$group_id")"
  resp="$(rpc send "{\"account\":\"$acct_j\",\"groupId\":\"$gid_j\",\"message\":\"amplihack signal-setup: link verified\"}" \
    | timeout 15 nc "$nc_host" "$nc_port" 2>/dev/null | head -n1)"
  # Empty results array is NORMAL/success for a self-only group.
  if printf '%s' "$resp" | grep -q '"results":\[\]'; then
    ok "  Post-test OK: {\"results\":[],...} (empty results = success for self-group)"
  else
    warn "  Post-test response (verify manually): $resp"
  fi
}

daemon_group_posttest() {
  [ -n "$PHONE" ] || { warn "  --phone not set; skipping daemon/group/post-test."; return 0; }
  info "[5/6] Ensuring JSON-RPC daemon on $DAEMON_TCP ..."
  if [ "$MODE" = "remote" ]; then
    remote_daemon_group_posttest "$SIGNAL_CLI_REMOTE"
  else
    local_daemon_group_posttest "$SIGNAL_CLI"
  fi
}

# --------------------------------------------------------------------------- #
# Main
# --------------------------------------------------------------------------- #
main() {
  if [ "$MODE" = "local" ]; then check_prereqs_local; else check_prereqs_remote; fi

  # Idempotency: if already linked, do not re-mint.
  local existing
  existing="$(already_linked)" \
    || die "Could not inspect existing Signal accounts on $HOST; refusing to mint a fresh link until the account probe succeeds."
  if [ -n "$existing" ]; then
    ok "[2/6] Host already linked as $existing — nothing to do (idempotent)."
    [ -z "$PHONE" ] && PHONE="$existing"
    [ "$DO_DAEMON" -eq 1 ] && daemon_group_posttest
    ok "=== signal-setup complete (already linked) ==="
    return 0
  fi

  # Prompt: the phone MUST be on the scan screen BEFORE we mint (60s window).
  info "[3/6] Prepare your phone: Signal > Settings > Linked Devices > 'Link New Device'."
  info "      Get to the CAMERA / scan screen NOW."
  if [ "$ASSUME_YES" -ne 1 ]; then
    printf '\033[0;36mAre you on the scan screen and ready? [y/N] \033[0m' >&2
    local answer=""
    read -r answer </dev/tty || answer=""
    case "$answer" in
      y|Y|yes|YES) : ;;
      *) die "Aborted — re-run when you are on the scan screen." ;;
    esac
  fi

  info "  Minting fresh link (unit $UNIT)..."
  local uri
  if [ "$MODE" = "local" ]; then uri="$(mint_local)"; else uri="$(mint_remote)"; fi
  [ -n "${uri:-}" ] || die "Failed to obtain a link URI for $HOST. Check signal-cli/systemd-run on the host (trace: $LOG_FILE)."

  # Zero-latency delivery: QR to the TERMINAL only, inverted for dark backgrounds.
  printf '\n' >&2
  printf '############################################################\n' >&2
  printf '#  SCAN NOW — ~%ss until Signal closes the socket (1001)   #\n' "$WINDOW_SECONDS" >&2
  printf '#  Signal > Linked Devices > Link New Device > scan below   #\n' >&2
  printf '############################################################\n\n' >&2
  qrencode -t ANSIUTF8i -m "$QR_MARGIN" "$uri"
  printf '\n(host=%s unit=%s minted=%s)\n' "$HOST" "$UNIT" "$(date -u +%H:%M:%SZ)" >&2

  # The URI is now on-screen; delete the on-disk copy of the secret immediately
  # rather than leaving it readable for the whole provisioning window.
  if [ "$MODE" = "local" ]; then
    rm -f "$URI_FILE" 2>/dev/null; sudo rm -f "$URI_FILE" 2>/dev/null
  else
    remote_run "rm -f $URI_FILE" >/dev/null 2>&1
  fi
  uri=""

  verify_linkage || die "Linkage not verified. Re-run to mint a fresh QR (the old one has expired)."

  [ "$DO_DAEMON" -eq 1 ] && daemon_group_posttest

  ok "=== signal-setup complete for $HOST ==="
}

main
