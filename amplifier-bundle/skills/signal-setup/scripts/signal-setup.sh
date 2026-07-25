#!/usr/bin/env bash
#
# signal-setup.sh — link a host as a Signal linked device END-TO-END and wire it
# into amplihack's Signal channel, collapsing the whole manual device-linking
# loop into a single idempotent command.
#
# Supported hosts:
#   --host <name>       local host name  (linked LOCALLY via systemd-run)
#   --host <azlin-vm>   remote Azure VM  (linked via `az vm run-command` +
#                       systemd-run on the VM, QR relayed back and rendered
#                       LOCALLY in this terminal)
#
# ─────────────────────────────────────────────────────────────────────────────
# HARD-WON FACTS THIS SCRIPT ENCODES (do not "optimise" any of these away):
#
#  1. THE 60-SECOND WINDOW  (critical root cause)
#     Signal's device-link provisioning websocket to chat.signal.org closes
#     with server code 1001 EXACTLY 60s after it opens (verified in
#     `signal-cli -vv` trace: onOpen → onClosing(1001) at +60s). A link QR is
#     therefore valid for only ~60s from mint. Any delivery path slower than a
#     few seconds expires it and the phone shows "invalid response from
#     server". Routing the QR through a remote Signal daemon (azlin/bastion
#     round-trip = 40-60s) is what caused hours of failures. We mint and render
#     with essentially zero delivery latency instead.
#
#  2. ZERO-LATENCY, DARK-TERMINAL-SAFE QR DELIVERY
#     Render the QR DIRECTLY in this terminal with `qrencode -t ANSIUTF8i`.
#     The trailing "i" = INVERTED and is REQUIRED: plain ANSIUTF8 is
#     dark-on-dark and invisible/unscannable on dark terminal backgrounds.
#     NEVER deliver the QR as a Signal message/attachment during linking.
#
#  3. PERSISTENT LINK PROCESS
#     Run `signal-cli link` under `systemd-run` (transient unit sig-link-<host>)
#     so it survives the launching shell and stays connected the full window.
#     For REMOTE hosts, `az vm run-command` runs as ROOT, so systemd-run needs
#     --uid=azureuser --gid=azureuser --setenv=HOME=/home/azureuser to land the
#     account under the azureuser user (not root).
#
#  4. OPERATIONAL GOTCHAS
#     (a) azlin is a Rust binary that ABORTS (SIGABRT/core-dump) on a broken
#         pipe (SIGPIPE). NEVER pipe azlin into `grep -q`/`head` or any
#         early-closing consumer — capture full output first, then filter.
#     (b) Only ONE bastion session per host at a time — concurrent
#         azlin/bastion sessions to the same VM core-dump.
#     (c) azlin form:
#         azlin connect <host> --resource-group <rg> --no-tmux -y -- "<cmd>"
#
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

# ── Defaults / configuration ────────────────────────────────────────────────
SIGNAL_CLI_VERSION="0.14.5"                      # known-good pin
SIGNAL_CLI_BIN="${SIGNAL_CLI_BIN:-$HOME/.local/bin/signal-cli}"
RG="${AMPLIHACK_SIGNAL_RG:-rysweet-linux-vm-pool}"
ENDPOINT="${AMPLIHACK_SIGNAL_ENDPOINT:-127.0.0.1:7583}"
GROUP_NAME="${AMPLIHACK_SIGNAL_GROUP:-amplihack}"
LINK_TIMEOUT="${AMPLIHACK_SIGNAL_LINK_TIMEOUT:-75}" # >60s so we outlive the window

HOST=""
REMOTE=0
DO_DAEMON=1
DO_PREREQS=1
ASSUME_YES=0

usage() {
  cat <<'USAGE'
signal-setup.sh — link a host to Signal and wire it into amplihack (idempotent)

Usage:
  signal-setup.sh --host <name> [options]

Options:
  --host <name>       Host to link. A local hostname links LOCALLY; an Azure VM
                      name links REMOTELY via `az vm run-command` (auto-detected,
                      or force with --remote / --local).
  --remote            Force remote (azlin VM) linking path.
  --local             Force local linking path.
  --group <name>      amplihack Signal group name (default: amplihack).
  --endpoint <h:port> Loopback JSON-RPC endpoint (default: 127.0.0.1:7583).
  --resource-group <rg>  Azure RG for remote hosts (default: rysweet-linux-vm-pool).
  --no-daemon         Skip the daemon + self-group + post-test phase.
  --skip-prereqs      Skip prerequisite verification/installation.
  -y, --yes           Non-interactive: assume the scan screen is open.
  -h, --help          Show this help.

Flow: prereqs → prompt to open scan screen → mint under systemd-run →
      print ANSIUTF8i QR in-terminal → verify linkage →
      (optional) start daemon + ensure self-group + post-test.
USAGE
}

log()  { printf '\033[1;36m[signal-setup]\033[0m %s\n' "$*" >&2; }
warn() { printf '\033[1;33m[signal-setup] WARN:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m[signal-setup] ERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# ── Argument parsing ─────────────────────────────────────────────────────────
FORCE_REMOTE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --host)           HOST="${2:-}"; shift 2 ;;
    --host=*)         HOST="${1#*=}"; shift ;;
    --remote)         FORCE_REMOTE=1; shift ;;
    --local)          FORCE_REMOTE=0; shift ;;
    --group)          GROUP_NAME="${2:-}"; shift 2 ;;
    --group=*)        GROUP_NAME="${1#*=}"; shift ;;
    --endpoint)       ENDPOINT="${2:-}"; shift 2 ;;
    --endpoint=*)     ENDPOINT="${1#*=}"; shift ;;
    --resource-group) RG="${2:-}"; shift 2 ;;
    --resource-group=*) RG="${1#*=}"; shift ;;
    --no-daemon)      DO_DAEMON=0; shift ;;
    --skip-prereqs)   DO_PREREQS=0; shift ;;
    -y|--yes)         ASSUME_YES=1; shift ;;
    -h|--help)        usage; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
done

[ -n "$HOST" ] || { usage; die "--host is required"; }

DEVICE_NAME="amplihack-$HOST"
UNIT="sig-link-$HOST"
URI_FILE="/tmp/slink-$HOST.out"
TRACE_LOG="/tmp/scli-$HOST.log"
LOCAL_HOST="$(hostname -s 2>/dev/null || hostname)"

# ── Decide local vs remote ───────────────────────────────────────────────────
detect_remote() {
  if [ -n "$FORCE_REMOTE" ]; then REMOTE="$FORCE_REMOTE"; return; fi
  # Local if the requested host matches this machine's short hostname.
  if [ "$HOST" = "$LOCAL_HOST" ] || [ "$HOST" = "localhost" ] || [ "$HOST" = "local" ]; then
    REMOTE=0
  else
    REMOTE=1
  fi
}
detect_remote
log "host=$HOST  mode=$([ "$REMOTE" -eq 1 ] && echo REMOTE || echo LOCAL)  device=$DEVICE_NAME  unit=$UNIT"

# azlin invocation (form c): capture output FULLY (gotcha a: never pipe azlin
# into an early-closing consumer or the Rust binary SIGABRTs on the broken pipe).
azlin_run() {
  local h="$1" cmd="$2"
  timeout 180 azlin connect "$h" --resource-group "$RG" --no-tmux -y -- "$cmd" 2>&1
}

# ─────────────────────────────────────────────────────────────────────────────
# PHASE 1 — PREREQUISITES
# ─────────────────────────────────────────────────────────────────────────────
have() { command -v "$1" >/dev/null 2>&1; }

install_qrencode() {
  have qrencode && return 0
  log "installing qrencode…"
  if have apt-get; then sudo apt-get update -qq && sudo apt-get install -y -qq qrencode
  elif have dnf; then sudo dnf install -y -q qrencode
  elif have brew; then brew install qrencode
  else die "qrencode missing and no known package manager (apt/dnf/brew)"; fi
}

check_signal_cli_local() {
  if [ -x "$SIGNAL_CLI_BIN" ]; then
    local v; v="$("$SIGNAL_CLI_BIN" --version 2>/dev/null | awk '{print $NF}')"
    log "signal-cli present: ${v:-unknown} ($SIGNAL_CLI_BIN)"
    [ "$v" = "$SIGNAL_CLI_VERSION" ] || warn "signal-cli $v != known-good $SIGNAL_CLI_VERSION (continuing)"
    return 0
  fi
  if have signal-cli; then SIGNAL_CLI_BIN="$(command -v signal-cli)"; log "signal-cli on PATH: $SIGNAL_CLI_BIN"; return 0; fi
  die "signal-cli not found at $SIGNAL_CLI_BIN or on PATH. Install signal-cli $SIGNAL_CLI_VERSION\n\
  e.g. unpack to ~/.local/opt/signal-cli-$SIGNAL_CLI_VERSION and symlink its bin/signal-cli into ~/.local/bin."
}

prereqs_local() {
  export PATH="$HOME/.local/bin:$PATH"
  check_signal_cli_local
  install_qrencode
  have systemd-run || die "systemd-run not available (needed for a persistent link process)"
}

prereqs_remote() {
  # qrencode is needed LOCALLY (we render here); az CLI is needed for run-command.
  install_qrencode
  have az || die "az CLI not found — required to reach remote host '$HOST'. Install the Azure CLI."
  log "verifying signal-cli + systemd-run on remote host '$HOST' (single azlin round-trip)…"
  local probe; probe="$(azlin_run "$HOST" \
    "test -x $SIGNAL_CLI_BIN && echo HAVE_CLI; command -v systemd-run >/dev/null && echo HAVE_SYSTEMD; $SIGNAL_CLI_BIN --version 2>/dev/null")" || true
  printf '%s\n' "$probe" | grep -qa HAVE_CLI     || die "remote host '$HOST' missing signal-cli at $SIGNAL_CLI_BIN"
  printf '%s\n' "$probe" | grep -qa HAVE_SYSTEMD || die "remote host '$HOST' missing systemd-run"
  local rv; rv="$(printf '%s\n' "$probe" | grep -ao "$SIGNAL_CLI_VERSION" | head -1)"
  if [ -n "$rv" ]; then log "remote signal-cli $SIGNAL_CLI_VERSION OK"; else warn "remote signal-cli version != $SIGNAL_CLI_VERSION (continuing)"; fi
}

if [ "$DO_PREREQS" -eq 1 ]; then
  log "── Phase 1: prerequisites ──"
  if [ "$REMOTE" -eq 1 ]; then prereqs_remote; else prereqs_local; fi
else
  [ "$REMOTE" -eq 1 ] || { export PATH="$HOME/.local/bin:$PATH"; }
  warn "skipping prerequisite checks (--skip-prereqs)"
fi

# ─────────────────────────────────────────────────────────────────────────────
# IDEMPOTENCY — bail out early if this host is already linked.
# ─────────────────────────────────────────────────────────────────────────────
list_accounts_local()  { "$SIGNAL_CLI_BIN" listAccounts 2>/dev/null; }
list_accounts_remote() { azlin_run "$HOST" "$SIGNAL_CLI_BIN listAccounts 2>/dev/null"; }

get_linked_number() {
  local out
  if [ "$REMOTE" -eq 1 ]; then out="$(list_accounts_remote)"; else out="$(list_accounts_local)"; fi
  printf '%s\n' "$out" | sed -n 's/.*Number: *\(+[0-9][0-9]*\).*/\1/p' | head -1
}

EXISTING="$(get_linked_number)"
if [ -n "$EXISTING" ]; then
  log "host '$HOST' is ALREADY linked as $EXISTING — skipping the link step (idempotent)."
  PHONE="$EXISTING"
  ALREADY_LINKED=1
else
  ALREADY_LINKED=0
fi

# ─────────────────────────────────────────────────────────────────────────────
# PHASE 2 — PROMPT: open the scan screen BEFORE we mint (60s window!)
# ─────────────────────────────────────────────────────────────────────────────
if [ "$ALREADY_LINKED" -eq 0 ]; then
  cat >&2 <<'BANNER'

  ┌──────────────────────────────────────────────────────────────────────┐
  │  ⚠  60-SECOND WINDOW — get ready to scan BEFORE the QR is minted.      │
  │                                                                        │
  │  On your phone, open:                                                  │
  │      Signal → Settings → Linked Devices → "Link New Device"           │
  │  and get to the camera / scan screen NOW.                             │
  │                                                                        │
  │  The QR expires ~60s after it is minted (Signal closes the            │
  │  provisioning socket with code 1001). Scan it the instant it prints.  │
  └──────────────────────────────────────────────────────────────────────┘

BANNER
  if [ "$ASSUME_YES" -eq 0 ]; then
    if [ -t 0 ]; then
      printf '\033[1;32m[signal-setup]\033[0m Press ENTER once you are on the scan screen (Ctrl-C to abort)… ' >&2
      read -r _ || die "aborted"
    else
      warn "no TTY and no -y/--yes; proceeding immediately — make sure the scan screen is open."
    fi
  fi
fi

# ─────────────────────────────────────────────────────────────────────────────
# PHASE 3 — MINT the link under systemd-run (persistent process)
# ─────────────────────────────────────────────────────────────────────────────
mint_local() {
  sudo systemctl reset-failed "$UNIT" 2>/dev/null
  sudo systemctl stop "$UNIT" 2>/dev/null
  sudo rm -f "$URI_FILE" "$TRACE_LOG"
  sudo systemd-run --unit="$UNIT" --uid="$USER" --gid="$USER" \
    --setenv=HOME="$HOME" --setenv=PATH="$HOME/.local/bin:/usr/bin:/bin" \
    /bin/bash -c "'$SIGNAL_CLI_BIN' -vv --log-file '$TRACE_LOG' link -n '$DEVICE_NAME' > '$URI_FILE' 2>&1" \
    >/dev/null 2>&1
  local _i
  for _i in $(seq 1 40); do
    sudo grep -qa '^sgnl://' "$URI_FILE" 2>/dev/null && break
    sleep 0.5
  done
  sudo grep -m1 -a '^sgnl://' "$URI_FILE" 2>/dev/null
}

mint_remote() {
  # ONE azlin round-trip: start the link under systemd-run (as azureuser — see
  # fact #3), poll the URI file on the VM, echo the URI back between markers.
  local script
  script=$(cat <<REMOTE
systemctl reset-failed $UNIT 2>/dev/null
systemctl stop $UNIT 2>/dev/null
rm -f $URI_FILE $TRACE_LOG
systemd-run --unit=$UNIT --uid=azureuser --gid=azureuser \
  --setenv=HOME=/home/azureuser --setenv=PATH=/home/azureuser/.local/bin:/usr/bin:/bin \
  /bin/bash -c '$SIGNAL_CLI_BIN -vv --log-file $TRACE_LOG link -n $DEVICE_NAME > $URI_FILE 2>&1' >/dev/null 2>&1
for i in \$(seq 1 40); do grep -qa '^sgnl://' $URI_FILE 2>/dev/null && break; sleep 0.5; done
echo URI_START; grep -m1 -a '^sgnl://' $URI_FILE 2>/dev/null; echo URI_END
REMOTE
)
  local out
  out="$(az vm run-command invoke -g "$RG" -n "$HOST" --command-id RunShellScript \
        --scripts "$script" --query 'value[0].message' -o tsv 2>/dev/null)"
  printf '%s\n' "$out" | sed -n 's/.*URI_START//p' | grep -m1 -a '^sgnl://'
}

if [ "$ALREADY_LINKED" -eq 0 ]; then
  log "── Phase 3: minting device-link (unit=$UNIT) ──"
  if [ "$REMOTE" -eq 1 ]; then URI="$(mint_remote)"; else URI="$(mint_local)"; fi
  [ -n "${URI:-}" ] || die "failed to obtain an sgnl:// link URI for '$HOST'. Check signal-cli/systemd-run (trace: $TRACE_LOG)."
  MINT_TS="$(date -u +%H:%M:%SZ)"

  # ───────────────────────────────────────────────────────────────────────────
  # PHASE 4 — RENDER the QR IN-TERMINAL (zero latency, dark-terminal-safe)
  # ───────────────────────────────────────────────────────────────────────────
  cat >&2 <<BANNER

############################################################
#  SCAN NOW — you have ~55 seconds (Signal closes at 60s)  #
#  Signal > Linked Devices > Link New Device > scan below  #
############################################################
BANNER
  # ANSIUTF8i — the trailing "i" (inverted) is REQUIRED for dark terminals.
  qrencode -t ANSIUTF8i -m 2 "$URI"
  echo >&2
  log "minted at $MINT_TS  (host=$HOST unit=$UNIT). If it will not scan, paste this URI into Link New Device:"
  printf '%s\n' "$URI" >&2

  # ───────────────────────────────────────────────────────────────────────────
  # PHASE 5 — VERIFY linkage (poll until listAccounts shows the number, or the
  # transient unit goes inactive; trace log confirms "Associated with").
  # ───────────────────────────────────────────────────────────────────────────
  log "── Phase 5: waiting for you to scan (up to ${LINK_TIMEOUT}s) ──"
  PHONE=""
  DEADLINE=$(( $(date +%s) + LINK_TIMEOUT ))
  while [ "$(date +%s)" -lt "$DEADLINE" ]; do
    PHONE="$(get_linked_number)"
    [ -n "$PHONE" ] && break
    sleep 3
  done
  if [ -z "$PHONE" ]; then
    warn "no linked account detected within ${LINK_TIMEOUT}s."
    warn "The 60s window likely expired — re-run signal-setup.sh and scan faster. Trace: $TRACE_LOG"
    die "linkage not confirmed for '$HOST'"
  fi
  log "✅ LINKED: '$HOST' is now Signal account $PHONE  (device: $DEVICE_NAME)"
fi

# ─────────────────────────────────────────────────────────────────────────────
# PHASE 6 — DAEMON + SELF-GROUP + POST-TEST (the amplihack Signal channel)
# For LOCAL hosts we can drive the JSON-RPC daemon directly. For remote hosts we
# report the linked account and hand off to `amplihack signal setup` / the
# fleet distribute path, which manage the remote daemon + config.
# ─────────────────────────────────────────────────────────────────────────────
json_rpc() {
  # json_rpc <host:port> <json-request-line>  → prints the first response line.
  local hp="$1" req="$2" host port
  host="${hp%:*}"; port="${hp##*:}"
  if have python3; then
    HP_HOST="$host" HP_PORT="$port" REQ="$req" python3 - <<'PY'
import os, socket, sys, time
host=os.environ["HP_HOST"]; port=int(os.environ["HP_PORT"]); req=os.environ["REQ"]
try:
    s=socket.create_connection((host,port),timeout=10); s.settimeout(20)
except OSError as e:
    print(f'{{"error":"connect: {e}"}}'); sys.exit(0)
s.sendall((req+"\n").encode())
buf=b""; end=time.time()+20
while time.time()<end:
    try: c=s.recv(65536)
    except socket.timeout: break
    if not c: break
    buf+=c
    if b"\n" in buf: break
line=buf.split(b"\n",1)[0].decode(errors="replace")
print(line)
PY
  else
    warn "python3 unavailable; cannot drive JSON-RPC directly"; return 1
  fi
}

daemon_running() {
  local hp="$1" host port
  host="${hp%:*}"; port="${hp##*:}"
  (exec 3<>"/dev/tcp/$host/$port") 2>/dev/null && { exec 3>&- 3<&-; return 0; } || return 1
}

setup_channel_local() {
  log "── Phase 6: daemon + self-group + post-test ──"
  if daemon_running "$ENDPOINT"; then
    log "JSON-RPC daemon already running on $ENDPOINT (idempotent)"
  else
    log "starting JSON-RPC daemon on $ENDPOINT under systemd-run…"
    local dunit="sig-daemon-$HOST"
    sudo systemctl reset-failed "$dunit" 2>/dev/null
    sudo systemctl stop "$dunit" 2>/dev/null
    sudo systemd-run --unit="$dunit" --uid="$USER" --gid="$USER" \
      --setenv=HOME="$HOME" --setenv=PATH="$HOME/.local/bin:/usr/bin:/bin" \
      "$SIGNAL_CLI_BIN" -a "$PHONE" daemon --tcp "$ENDPOINT" >/dev/null 2>&1
    local _i
    for _i in $(seq 1 20); do daemon_running "$ENDPOINT" && break; sleep 0.5; done
    daemon_running "$ENDPOINT" || { warn "daemon did not come up on $ENDPOINT"; return 1; }
    log "daemon up on $ENDPOINT (unit=$dunit)"
  fi

  # Ensure a self-only amplihack group exists → capture groupId.
  log "ensuring self-only group '$GROUP_NAME' exists…"
  local resp gid
  resp="$(json_rpc "$ENDPOINT" "{\"jsonrpc\":\"2.0\",\"id\":\"grp\",\"method\":\"updateGroup\",\"params\":{\"name\":\"$GROUP_NAME\"}}")"
  gid="$(printf '%s' "$resp" | sed -n 's/.*"groupId":"\([^"]*\)".*/\1/p')"
  if [ -z "$gid" ]; then
    warn "updateGroup did not return a groupId. Response: $resp"
    return 1
  fi
  log "group '$GROUP_NAME' groupId=$gid"

  # Post-test: send to the self-group. Empty results is NORMAL/success for a
  # self-only group ({"results":[],"timestamp":...}).
  log "post-test: sending a confirmation message to the self-group…"
  local msg send
  msg="amplihack signal-setup: '$HOST' linked as $PHONE at $(date -u +%FT%TZ)"
  send="$(json_rpc "$ENDPOINT" "{\"jsonrpc\":\"2.0\",\"id\":\"snd\",\"method\":\"send\",\"params\":{\"groupId\":\"$gid\",\"message\":\"$msg\"}}")"
  if printf '%s' "$send" | grep -q '"timestamp"'; then
    log "✅ post-test OK (self-group send returned a timestamp; empty \"results\" is expected)."
  else
    warn "post-test send did not return a timestamp. Response: $send"
    return 1
  fi
}

if [ "$DO_DAEMON" -eq 1 ]; then
  if [ "$REMOTE" -eq 1 ]; then
    log "── Phase 6 (remote) ──"
    log "Remote host '$HOST' is linked as $PHONE."
    if have amplihack; then
      log "Handing daemon+config off to amplihack's Signal channel on the VM (via azlin)…"
      azlin_run "$HOST" "amplihack signal setup --endpoint $ENDPOINT --device-name $DEVICE_NAME 2>&1 || true" \
        | sed 's/^/[remote amplihack signal setup] /' >&2 || true
    else
      warn "amplihack CLI not found locally; run on the VM to finish the channel:"
      warn "    amplihack signal setup --endpoint $ENDPOINT --device-name $DEVICE_NAME"
    fi
  else
    setup_channel_local || warn "channel phase incomplete — host is LINKED ($PHONE); re-run to finish daemon/group setup."
  fi
else
  log "skipping daemon/self-group/post-test (--no-daemon)."
fi

echo >&2
log "DONE. host='$HOST' account=$PHONE mode=$([ "$REMOTE" -eq 1 ] && echo remote || echo local)."
[ "$REMOTE" -eq 1 ] || log "Verify any time with:  $SIGNAL_CLI_BIN listAccounts"
