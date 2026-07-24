---
name: signal-setup
version: 1.0.0
description: |
  End-to-end, idempotent Signal device-linking for amplihack on a local or
  remote (azlin VM) host. Runs the full loop in one invocation: prerequisite
  check, prompt to open the phone's scan screen, mint a fresh device-link under
  systemd-run, render the QR DIRECTLY in the terminal (zero delivery latency,
  ANSIUTF8i inverted for dark terminals), verify linkage, and optionally start
  the JSON-RPC daemon, ensure the amplihack self-group, and post a test message.
  Use when onboarding a host to the amplihack Signal channel, when a prior link
  attempt showed "invalid response from server", or when you need to re-link /
  verify a fleet host's Signal account.
auto_activates:
  - "Set up Signal for a host"
  - "Link Signal device"
  - "Signal device linking"
  - "Link a fleet host to Signal"
  - "amplihack signal setup"
  - "Signal QR invalid response from server"
priority_score: 36.0
---

# Signal Setup Skill

Link a host (local or remote azlin VM) to Signal for the amplihack Signal
channel — reliably, in one command, without falling into the 60-second trap
that caused hours of failures.

## When to invoke

- Onboarding a new local or fleet host to the amplihack Signal channel.
- A device-link attempt failed with **"invalid response from server"** on the
  phone (almost always the expired-QR / 60s-window failure — see below).
- You need to verify or re-establish a host's Signal linkage and confirm the
  JSON-RPC daemon + self-group post-test works.

## How to invoke

```bash
# Local host, interactive (prompts you to confirm the scan screen is open):
amplifier-bundle/skills/signal-setup/scripts/signal-setup.sh \
  --host local --phone +15551234567

# Remote azlin VM (mint runs on the VM via az run-command; QR renders LOCALLY):
amplifier-bundle/skills/signal-setup/scripts/signal-setup.sh \
  --host deva --phone +15551234567 --resource-group rysweet-linux-vm-pool

# Skip the daemon/self-group/post-test step:
amplifier-bundle/skills/signal-setup/scripts/signal-setup.sh --host local --no-daemon

# Non-interactive (you have ALREADY opened the scan screen):
amplifier-bundle/skills/signal-setup/scripts/signal-setup.sh --host local -y --phone +1555...
```

Run `signal-setup.sh --help` for the full option list.

The skill is **idempotent**: if the host is already linked (detected via
`signal-cli listAccounts`), it does **not** re-mint — it skips straight to the
daemon/self-group/post-test verification and exits successfully.

## End-to-end flow

1. **Prereq check** — verifies `signal-cli` (known-good **0.14.5** at
   `~/.local/bin/signal-cli` → `~/.local/opt/signal-cli-0.14.5/bin/signal-cli`),
   `qrencode`, and `systemd-run` on the target host; plus the `az` CLI and a
   local `qrencode` for remote hosts.
2. **Idempotency probe** — `signal-cli listAccounts` showing `Number: +<phone>`
   means already linked → skip minting.
3. **Prompt** — you confirm you are on Signal ▸ Settings ▸ Linked Devices ▸
   **Link New Device** ▸ camera/scan screen **BEFORE** the QR is minted.
4. **Mint** — `signal-cli link -n amplihack-<host>` under a transient
   `systemd-run` unit (`sig-link-<host>`) so it survives the launching shell.
5. **Render QR in-terminal** — `qrencode -t ANSIUTF8i` immediately. Scan it now.
6. **Verify linkage** — polls `listAccounts` for `Number: +<phone>`; the
   transient unit goes `inactive` on success.
7. **Daemon + self-group + post-test** (optional, on by default) — starts the
   JSON-RPC daemon on `127.0.0.1:7583`, ensures a self-only group via
   `updateGroup{name}`, and confirms `send{groupId,message}` returns
   `{"results":[],"timestamp":...}` (empty `results` is normal success for a
   self-group).

---

## ⚠️ THE FOUR HARD-WON FACTS (do not lose these)

### 1. The 60-second window (the real root cause)

Signal's device-link **provisioning websocket** to `chat.signal.org` closes
with **server code 1001 EXACTLY ~60 seconds** after it opens — verified in a
`signal-cli -vv` trace: `onOpen` → `onClosing(1001)` at **+60s**. A minted link
QR is therefore valid for only **~60 seconds** from mint. Any delivery path
slower than a few seconds expires the QR and the phone shows
**"invalid response from server"**.

This is what caused hours of failures: the QR had been routed through a remote
Signal daemon (azlin/bastion round-trip = 40–60s), leaving no margin. The fix
is **zero-latency, in-terminal delivery** so the phone scans a fresh QR within
a couple of seconds.

### 2. ANSIUTF8i — inverted, for dark terminals

Render with **`qrencode -t ANSIUTF8i`**. The trailing **`i` = inverted**, which
is **required** so the QR is visible/scannable on **dark** terminal
backgrounds. Plain `ANSIUTF8` is dark-on-dark and effectively **invisible** on
dark terminals.

### 3. systemd-run persistence

The `signal-cli link` process runs under a **transient systemd unit**
(`sig-link-<host>`) via `systemd-run`, so it **survives the launching shell**
and stays connected for the full window; it exits cleanly on a successful scan.

For **remote** hosts the same unit is launched via
`az vm run-command invoke ... RunShellScript` calling:

```bash
systemd-run --unit=sig-link-<host> --uid=azureuser --gid=azureuser \
  --setenv=HOME=/home/azureuser ...
```

`run-command` runs as **root**, so `--uid=azureuser --gid=azureuser` is
**required** so the linked account lands under the `azureuser` home, not root's.
The script captures the `sgnl://` URI from the link stdout on the VM, returns it,
and **renders the QR LOCALLY** — the URI (a few hundred bytes of text) travels
fast; the QR image never leaves your terminal.

### 4. NEVER route the QR through Signal

Do **NOT** deliver the QR as a Signal message/attachment during linking. That is
the **DEPRECATED slow path** (relay / daemon delivery) that reliably blew the
60s window. The QR goes to the **terminal**, nowhere else. You must be on the
phone's scan screen **before** the QR prints.

---

## Verifying linkage manually

- `signal-cli listAccounts` shows `Number: +<phone>`.
- The `sig-link-<host>` systemd unit becomes `inactive` on success.
- Trace log `/tmp/scli-<host>.log` shows `Associated with: +<phone>` then
  `Finishing new device registration`.

## Daemon + self-group + post-test details

- Daemon: `signal-cli -a +<phone> daemon --tcp 127.0.0.1:7583` (loopback, the
  established amplihack convention — matches `crates/amplihack-signal` and the
  `amplihack signal setup` command).
- Self-group: JSON-RPC `updateGroup{account,name}` → returns a `groupId`.
- Post-test: JSON-RPC `send{account,groupId,message}` → `{"results":[],...}`.
  An **empty `results` array is expected success** for a self-only group.

This aligns with the existing Rust integration: see
`crates/amplihack-cli/src/commands/signal/` (`setup.rs` idempotency probes,
`render.rs`, daemon on `127.0.0.1:7583`) and `docs/SIGNAL_ONBOARDING.md`. This
skill provides the **zero-latency in-terminal linking loop** that those tools
assume you have already completed.

---

## ⚠️ Operational gotchas (remote / azlin)

- **azlin SIGPIPE core-dump** — `azlin` is a Rust binary that **aborts
  (SIGABRT / core-dump) on a broken pipe (SIGPIPE)**. **Never** pipe `azlin`
  (or `az` output you treat like it) into `grep -q` or any early-closing
  consumer. **Capture full output first**, then filter. The script follows this
  rule (`remote_run` captures the whole `az` message before `sed`/`grep`).
- **One bastion session per host** — only **ONE** bastion/azlin session to a
  given VM at a time. Concurrent sessions to the same VM core-dump. Do not run
  this skill against the same host from two places at once.
- **azlin invocation form**:
  ```bash
  azlin connect <host> --resource-group rysweet-linux-vm-pool --no-tmux -y -- "<cmd>"
  ```

## Prerequisites (install if missing)

- **signal-cli 0.14.5** (known-good) at `~/.local/bin/signal-cli`
  (symlink → `~/.local/opt/signal-cli-0.14.5/bin/signal-cli`).
- **qrencode** — `apt-get install -y qrencode` (on the machine that renders the
  QR: local host, or your local machine for remote linking).
- **systemd-run / systemctl** — present on the target host.
- **az CLI** (remote hosts only) with `az vm run-command` permission for the
  target resource group.

## See also

- `docs/SIGNAL_ONBOARDING.md`, `docs/signal-channel.md`
- `crates/amplihack-signal/`, `crates/amplihack-cli/src/commands/signal/`
