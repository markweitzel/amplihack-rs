#!/usr/bin/env bash
#
# signal-setup.sh — end-to-end Signal device-linking for amplihack.
#
# Generalizes the proven zero-latency in-terminal QR linking loop into a single
# idempotent, reusable command for LOCAL and REMOTE (azlin VM) hosts.
#
# THE HARD-WON FACTS (see SKILL.md for the full write-up):
#
#   * 60-SECOND WINDOW — Signal's device-link provisioning websocket to
#     chat.signal.org closes with server code 1001 EXACTLY ~60s after it opens.
#     A minted link QR is therefore valid for only ~60s. Any delivery path
#     slower than a few seconds expires it ("invalid response from server").
#     => We mint and render the QR in-terminal with ZERO delivery latency.
#
#   * ANSIUTF8i — render with `qrencode -t ANSIUTF8i` (trailing "i" = inverted).
#     Plain ANSIUTF8 is dark-on-dark and INVISIBLE / unscannable on dark
#     terminal backgrounds. The inverted variant is required.
#
#   * NEVER route the QR through a Signal message/attachment during linking.
#     That is the DEPRECATED slow path (relay/daemon delivery = 40-60s) which
#     reliably blew the 60s window. QR goes to the TERMINAL, nowhere else.
#
#   * systemd-run persistence — the `signal-cli link` process runs under a
#     transient systemd unit (sig-link-<host>) so it survives the launching
#     shell and stays connected for the full window; it exits on a successful
#     scan. Remote hosts launch the same unit via `az vm run-command`.
#
# OPERATIONAL GOTCHAS (remote hosts):
#   (a) azlin is a Rust binary that ABORTS (SIGABRT/core-dump) on SIGPIPE.
#       NEVER pipe azlin into `grep -q` or any early-closing consumer.
#       Capture full output first, then filter.
#   (b) Only ONE bastion session to a given host at a time. Concurrent
#       azlin/az sessions to the same VM core-dump.
#   (c) azlin invocation form:
#         azlin connect <host> --resource-group rysweet-linux-vm-pool \
#           --no-tmux -y -- "<cmd>"
#
set -uo pipefail

# --------------------------------------------------------------------------- #
# Defaults / configuration
# --------------------------------------------------------------------------- #
SIGNAL_CLI="${SIGNAL_CLI:-$HOME/.local/bin/signal-cli}"
SIGNAL_CLI_REMOTE="/home/azureuser/.local/bin/signal-cli"
RESOURCE_GROUP="${SIGNAL_SETUP_RG:-rysweet-linux-vm-pool}"
DAEMON_TCP="127.0.0.1:7583"
QR_MARGIN=2
WINDOW_SECONDS=55   # advertise a hair under 60 so the user is never late

HOST=""
PHONE="${SIGNAL_PHONE:-}"
GROUP_NAME="amplihack"
MODE=""             # local | remote (auto-detected if empty)
DO_DAEMON=1
ASSUME_YES=0

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
  (exec 3<>"/dev/tcp/${DAEMON_TCP/:/\/}") 2>/dev/null
}

# Run a command ON the remote target host via az run-command. Captures FULL
# output first (never pipes az/azlin into an early-closing reader — SIGPIPE
# core-dumps azlin), then returns it for the caller to filter.
remote_run() {
  local script="$1" out
  out="$(az vm run-command invoke -g "$RESOURCE_GROUP" -n "$HOST" \
    --command-id RunShellScript --scripts "$script" \
    --query 'value[0].message' -o tsv 2>/dev/null)" || return 1
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
  printf '%s' "$2" | grep -Eq "$3" \
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

cleanup_secrets() {
  if [ "$MODE" = "local" ]; then
    rm -f "$URI_FILE" "$LOG_FILE" 2>/dev/null
    sudo rm -f "$URI_FILE" "$LOG_FILE" 2>/dev/null
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
    accounts="$("$SIGNAL_CLI" listAccounts 2>/dev/null)"
  else
    accounts="$(remote_run "$SIGNAL_CLI_REMOTE listAccounts 2>/dev/null")"
  fi
  # Prefer an explicit phone match when --phone given; otherwise any Number line.
  if [ -n "$PHONE" ]; then
    printf '%s\n' "$accounts" | grep -F "Number: $PHONE" >/dev/null 2>&1 && printf '%s' "$PHONE"
  else
    printf '%s\n' "$accounts" | sed -n 's/.*Number: \(+[0-9][0-9]*\).*/\1/p' | head -n1
  fi
}

# --------------------------------------------------------------------------- #
# Step 3: Mint the link URI under a transient systemd unit
# --------------------------------------------------------------------------- #
mint_local() {
  systemctl --user reset-failed "$UNIT" 2>/dev/null
  sudo systemctl reset-failed "$UNIT" 2>/dev/null
  sudo systemctl stop "$UNIT" 2>/dev/null
  sudo rm -f "$URI_FILE" "$LOG_FILE"
  sudo systemd-run --unit="$UNIT" --uid=azureuser --gid=azureuser \
    --property=UMask=0077 \
    --setenv=HOME=/home/azureuser \
    --setenv=PATH=/home/azureuser/.local/bin:/usr/bin:/bin \
    /bin/bash -c "$SIGNAL_CLI_REMOTE -vv --log-file $LOG_FILE link -n $NAME > $URI_FILE 2>&1" \
    >/dev/null 2>&1
  local _i
  for _i in $(seq 1 20); do
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
  local _i num unit_state
  for _i in $(seq 1 "$WINDOW_SECONDS"); do
    num="$(already_linked)"
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
daemon_group_posttest() {
  [ -n "$PHONE" ] || { warn "  --phone not set; skipping daemon/group/post-test."; return 0; }
  info "[5/6] Ensuring JSON-RPC daemon on $DAEMON_TCP ..."

  local sigcli
  if [ "$MODE" = "local" ]; then sigcli="$SIGNAL_CLI"; else
    warn "  Daemon/group/post-test target the LOCAL machine's daemon."
    warn "  For a remote-linked host, run these steps on that host separately."
    return 0
  fi

  if ! daemon_up; then
    "$sigcli" -a "$PHONE" daemon --tcp "$DAEMON_TCP" >/tmp/signal-daemon-"$HOST".log 2>&1 &
    local _i
    for _i in $(seq 1 20); do
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

# --------------------------------------------------------------------------- #
# Main
# --------------------------------------------------------------------------- #
main() {
  if [ "$MODE" = "local" ]; then check_prereqs_local; else check_prereqs_remote; fi

  # Idempotency: if already linked, do not re-mint.
  local existing
  existing="$(already_linked)"
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
