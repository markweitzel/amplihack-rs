---
name: signal-setup
version: 1.0.0
description: |
  Link a host (local or a remote azlin Azure VM) to Signal as a linked device
  END-TO-END and wire it into amplihack's Signal channel — the entire manual
  device-linking loop collapsed into one idempotent command. Encodes the
  hard-won facts about Signal's 60-second provisioning window, dark-terminal-safe
  in-terminal QR rendering, systemd-run link persistence, and the remote
  az-run-command path. Use when onboarding a machine to Signal notifications.
auto_activates:
  - "Set up Signal"
  - "Link Signal device"
  - "Link a host to Signal"
  - "Onboard host to Signal"
  - "Signal setup for a VM"
  - "Configure Signal notifications for a host"
priority_score: 34.0
---

# signal-setup

Links a host to **Signal** as a linked device and wires it into amplihack's
Signal channel in one idempotent invocation. Works for the **local** host or a
**remote azlin Azure VM**.

## When to use

- Onboarding a new machine (local or an azlin VM) so amplihack can post to
  Signal.
- Re-establishing a Signal link after a device was removed.
- You previously did the manual device-linking dance by hand and want it
  reduced to a single command.

## Invoke

```bash
# Local host
amplifier-bundle/skills/signal-setup/scripts/signal-setup.sh --host "$(hostname -s)"

# Remote azlin VM (auto-detected as remote; links via az vm run-command)
amplifier-bundle/skills/signal-setup/scripts/signal-setup.sh --host deva2

# Non-interactive (you confirm the scan screen is already open)
amplifier-bundle/skills/signal-setup/scripts/signal-setup.sh --host dev -y
```

Key options: `--remote/--local` (force the path), `--group <name>` (default
`amplihack`), `--endpoint 127.0.0.1:7583`, `--resource-group <rg>`,
`--no-daemon`, `--skip-prereqs`, `-y/--yes`. Run with `--help` for all flags.

## End-to-end flow

1. **Prereqs** — verify/install `signal-cli` (0.14.5 known-good, expected at
   `~/.local/bin/signal-cli`), `qrencode`, and (for remote hosts) the `az` CLI.
2. **Idempotency** — if the host already shows a `Number: +…` in
   `signal-cli listAccounts`, the link step is **skipped**.
3. **Prompt** — tell the user to open
   **Signal → Settings → Linked Devices → "Link New Device"** and wait for
   confirmation that the scan screen is open (skipped with `-y`).
4. **Mint** — start `signal-cli link -n amplihack-<host>` under `systemd-run`
   (transient unit `sig-link-<host>`) so the process survives the shell and
   stays connected the full window. Remote hosts mint via
   `az vm run-command … RunShellScript` calling `systemd-run --uid=azureuser …`.
5. **Render** — capture the `sgnl://…` URI and render it **directly in this
   terminal** with `qrencode -t ANSIUTF8i` (zero delivery latency).
6. **Verify** — poll `signal-cli listAccounts` for `Number: +<phone>`; the
   transient unit exits (inactive) on success; the trace log shows
   `Associated with: +<phone>` → `Finishing new device registration`.
7. **Channel** (optional, `--no-daemon` to skip) — start the JSON-RPC daemon
   (`signal-cli -a +<phone> daemon --tcp 127.0.0.1:7583`), ensure a self-only
   group via `updateGroup{name}` → `groupId`, and post-test with
   `send{groupId,message}`. An empty `{"results":[],"timestamp":…}` is the
   **expected success** for a self-group. For remote hosts this phase hands off
   to `amplihack signal setup` on the VM.

## ⚠ Critical facts (do NOT lose these)

### 1. The 60-second window (root cause of hours of failure)
Signal's device-link provisioning websocket to `chat.signal.org` closes with
**server code 1001 EXACTLY 60 seconds after it opens** (verified in
`signal-cli -vv` trace: `onOpen → onClosing(1001)` at +60s). A link QR is valid
for only **~60s from mint**. Any delivery path slower than a few seconds expires
it and the phone shows *"invalid response from server"*. Routing the QR through
a remote Signal daemon (azlin/bastion round-trip = 40–60s) is exactly what
caused the failures. **Get to the scan screen first; scan the instant the QR
prints.**

### 2. ANSIUTF8i — required for dark terminals
Render with `qrencode -t ANSIUTF8i`. The trailing **`i` (inverted) is
required**: plain `ANSIUTF8` is dark-on-dark and **invisible/unscannable** on
dark terminal backgrounds.

### 3. Never route the QR through Signal
**NEVER** deliver the link QR as a Signal message/attachment during linking —
that is the deprecated slow path (relay/daemon delivery) that blows the 60s
window. Always render in-terminal, locally.

### 4. systemd-run persistence
The link process runs under `systemd-run` (unit `sig-link-<host>`) so it
survives the launching shell and stays connected the full window. For **remote**
hosts, `az vm run-command` runs as **root**, so `systemd-run` must pass
`--uid=azureuser --gid=azureuser --setenv=HOME=/home/azureuser` so the account
lands under `azureuser`, not root.

## Operational gotchas (documented, avoided by the script)

- **azlin SIGPIPE core-dump** — azlin is a Rust binary that ABORTS
  (SIGABRT/core-dump) on a broken pipe. **Never** pipe azlin into `grep -q`,
  `head`, or any early-closing consumer — capture full output first, then
  filter. The script always uses command substitution before grepping.
- **One bastion session per host** — concurrent azlin/bastion sessions to the
  same VM core-dump. Run one host at a time.
- **azlin form** —
  `azlin connect <host> --resource-group rysweet-linux-vm-pool --no-tmux -y -- "<cmd>"`.

## Verifying / troubleshooting

- `signal-cli listAccounts` → shows `Number: +<phone>` when linked.
- Transient unit: `systemctl is-active sig-link-<host>` → `inactive` on success.
- Trace log: `/tmp/scli-<host>.log` (search for `Associated with:`).
- Link URI capture: `/tmp/slink-<host>.out`.
- If linkage times out, the 60s window almost certainly expired — re-run and
  scan faster.

## Integration with amplihack's Signal channel

The daemon endpoint (`127.0.0.1:7583`), device-name convention
(`amplihack-<host>`), and self-group model match the existing
`amplihack signal setup` command (crate `amplihack-signal`,
`crates/amplihack-cli/src/commands/signal/`). Remote onboarding defers the
daemon+config write to `amplihack signal setup` on the VM.
