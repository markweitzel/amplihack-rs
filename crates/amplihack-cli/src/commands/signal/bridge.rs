//! `amplihack signal bridge <topic>` — runtime orchestration (gated I/O shell).
//!
//! This drives a Copilot session from a fresh, operator-only Signal group. All
//! decision logic lives in the reusable, unit-tested cores in
//! `amplihack_signal::bridge`; this file performs the effects: config/link
//! check, loopback validation, resume probe, daemon connect, group create, the
//! announcement, the first turn, and the subscriber loop.
//!
//! Security posture (see `docs/SIGNAL_BRIDGE.md`): least-privilege tools by
//! default, **fail-closed** outbound membership verification before every post,
//! loopback-only daemon unless an explicit opt-in, an audit log of every
//! accepted prompt, and outbound secret redaction before chunking. No silent
//! fallbacks — every fatal condition maps to a stable [`BridgeError`] exit code.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use amplihack_signal::bridge::allowlist::ToolAllowlist;
use amplihack_signal::bridge::control::{Control, parse_control};
use amplihack_signal::bridge::membership::{Membership, expected_members};
use amplihack_signal::bridge::outbound::redact_and_chunk;
use amplihack_signal::bridge::turn::{CopilotTurnRunner, PreemptSlot, SerialTurnDriver};
use amplihack_signal::bridge::{BridgeError, connect_daemon, validate_endpoint, verified_send};
use amplihack_signal::config::SignalConfig;
use amplihack_signal::gating::Gate;
use amplihack_signal::session_channel::Inbox;
use amplihack_signal::transport::{GroupId, SignalTransport};

use crate::SignalBridgeArgs;

/// Default reconnect attempts before a clean daemon-down shutdown.
const DEFAULT_RETRY_BUDGET: u32 = 10;

/// The `copilot` binary the bridge drives (turn-based `--session-id` resume).
const COPILOT_BIN: &str = "copilot";

/// Entry point for `amplihack signal bridge`. Blocks on an async runtime and
/// returns the stable [`BridgeError`] taxonomy on failure.
pub fn run_bridge(args: SignalBridgeArgs) -> Result<(), BridgeError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            eprintln!("error: failed to start async runtime: {e}");
            BridgeError::NotLinked
        })?;
    runtime.block_on(run_bridge_async(args))
}

/// Best-effort system hostname for the group-name `<host>` token.
fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "host".to_string())
}

/// The current tmux session name, if the bridge is running inside tmux.
fn tmux_session() -> Option<String> {
    std::env::var_os("TMUX")?;
    let out = std::process::Command::new("tmux")
        .args(["display-message", "-p", "#S"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

/// Probe that the installed `copilot` accepts `--session-id` resume. Without it,
/// turn continuity cannot be guaranteed, so the bridge refuses to start.
fn probe_copilot_resume() -> Result<(), BridgeError> {
    let out = std::process::Command::new(COPILOT_BIN)
        .arg("--help")
        .output()
        .map_err(|_| BridgeError::ResumeProbeFailed)?;
    let help = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if help.contains("--session-id") {
        Ok(())
    } else {
        Err(BridgeError::ResumeProbeFailed)
    }
}

/// Verify group membership, then relay `body` (redacted + chunked) — FAIL
/// CLOSED, re-verifying **before every post**.
///
/// The security posture promises membership is checked before *each* outbound
/// message, not once per body: an operator-only group whose membership changes
/// mid-relay (an unexpected member added between chunks) must not receive any
/// further chunk. So this re-queries and re-classifies membership immediately
/// before each `send_group` chunk. On any verification failure — the first
/// chunk or a later one — it alerts the local terminal and stops sending the
/// remaining chunks (the withheld relay is surfaced, never silently dropped).
pub async fn verify_and_post(
    transport: &mut SignalTransport,
    group_id: &GroupId,
    expected: &[String],
    gate: &mut Gate,
    body: &str,
) {
    for chunk in redact_and_chunk(body) {
        match verified_send(transport, group_id, expected, &chunk).await {
            Ok(Membership::Verified) => gate.record_outbound(&chunk),
            Ok(Membership::Unverified(reason)) => {
                eprintln!(
                    "signal bridge: WITHHOLDING outbound relay — group membership unverified before post: {reason}"
                );
                return;
            }
            Err(e) => {
                eprintln!("signal bridge: failed to post to group: {e}");
                return;
            }
        }
    }
}

/// Audit-log an accepted prompt (redacted) to the local terminal.
fn audit_accepted(session_id: &str, sender: &str, device: Option<u32>, prompt: &str) {
    let redacted = amplihack_signal::bridge::outbound::redact_for_relay(prompt);
    let preview: String = redacted.chars().take(120).collect();
    tracing::info!(
        session_id,
        sender,
        device = device.unwrap_or(0),
        "signal bridge accepted prompt: {preview}"
    );
    eprintln!("signal bridge: accepted prompt from {sender} (device {device:?}): {preview}");
}

async fn run_bridge_async(args: SignalBridgeArgs) -> Result<(), BridgeError> {
    // 1. Load config (also our linked/configured check). A missing/invalid
    //    config means the host is not onboarded → guide the operator.
    let cfg = SignalConfig::load().map_err(|e| {
        eprintln!(
            "error: signal is not configured on this host ({e}).\n\
             Run `amplihack signal setup` to link a device and start the daemon."
        );
        BridgeError::NotLinked
    })?;

    // 2. Loopback safety (fail closed unless explicit opt-in).
    validate_endpoint(&cfg.endpoint, args.unsafe_remote_endpoint).inspect_err(|_| {
        eprintln!(
            "error: signal-cli endpoint {} is not loopback. Pass --unsafe-remote-endpoint to \
             override (never across an untrusted network).",
            cfg.endpoint
        );
    })?;

    // 3. Copilot resume probe (turn continuity precondition).
    probe_copilot_resume().inspect_err(|_| {
        eprintln!(
            "error: the installed `copilot` did not accept `--session-id` resume; turn \
             continuity cannot be guaranteed."
        );
    })?;

    // 4. Connect to the daemon with a bounded retry budget.
    let retry_budget = args.retry_budget.unwrap_or(DEFAULT_RETRY_BUDGET);
    let mut transport = connect_daemon(&cfg.endpoint, retry_budget)
        .await
        .inspect_err(|_| {
            eprintln!(
                "error: signal-cli daemon at {} was unreachable after {retry_budget} attempts; \
                 shutting down cleanly.",
                cfg.endpoint
            );
        })?;

    // 5. Derive the group name and create a fresh operator-only group.
    let host = args.host.clone().unwrap_or_else(hostname);
    let tmux = tmux_session();
    let group_name = args.group_name.clone().unwrap_or_else(|| {
        amplihack_signal::bridge::naming::group_name(&host, tmux.as_deref(), &args.topic)
    });
    let group_id = transport.create_group(&group_name).await.map_err(|e| {
        eprintln!("error: failed to create Signal group '{group_name}': {e}");
        BridgeError::GroupCreateFailed
    })?;
    eprintln!(
        "signal bridge: created group '{group_name}' ({})",
        group_id.as_str()
    );

    // 6. Fresh pinned session id + effective allowlist.
    let session_id = uuid::Uuid::new_v4().to_string();
    let allowlist = ToolAllowlist::from_flags(&args.allow_tool, args.dangerous_all_tools);
    let expected = expected_members(&cfg);
    let mut gate = Gate::new(&cfg, group_id.as_str());

    // Shared child-bound pre-empt trigger so a control `stop`/`kill` can
    // pre-empt an in-flight turn even mid-execution, immune to PID reuse.
    let preempt: PreemptSlot = Arc::new(Mutex::new(None));
    let driver = Arc::new(SerialTurnDriver::new(
        CopilotTurnRunner::new(COPILOT_BIN, preempt.clone()),
        &session_id,
        allowlist.clone(),
    ));

    // 7. Announce topic, blast radius, and control phrases.
    let announcement = format!(
        "amplihack signal bridge started.\n\
         topic: {}\n\
         session: {}\n\
         tools ({}): {}\n\
         controls: `status`, `stop`, `kill` (exact word).",
        args.topic,
        session_id,
        if allowlist.is_dangerous() {
            "DANGEROUS"
        } else {
            "least-privilege"
        },
        allowlist.describe(),
    );
    verify_and_post(
        &mut transport,
        &group_id,
        &expected,
        &mut gate,
        &announcement,
    )
    .await;

    // Bounded turn queue (operator-configurable), mirroring the session inbox.
    let capacity = args.inbox_capacity.unwrap_or_else(Inbox::default_capacity);
    let mut queue: VecDeque<String> = VecDeque::new();

    // 8. First turn: the topic itself is the opening prompt.
    let (turn_tx, mut turn_rx) = tokio::sync::mpsc::unbounded_channel::<std::io::Result<String>>();
    let mut turn_in_flight = spawn_turn(&driver, &turn_tx, args.topic.clone());

    // 9. Subscriber loop.
    loop {
        tokio::select! {
            biased;
            // Post completed turn output promptly, then start the next queued turn.
            Some(result) = turn_rx.recv() => {
                turn_in_flight = false;
                match result {
                    Ok(body) if !body.trim().is_empty() => {
                        verify_and_post(&mut transport, &group_id, &expected, &mut gate, &body).await;
                    }
                    Ok(_) => {
                        verify_and_post(&mut transport, &group_id, &expected, &mut gate,
                            "(turn produced no output)").await;
                    }
                    Err(e) => {
                        // Surface the failure but keep the bridge alive; the next
                        // turn resumes the SAME session (context preserved).
                        verify_and_post(&mut transport, &group_id, &expected, &mut gate,
                            &format!("turn failed: {e}")).await;
                    }
                }
                if !turn_in_flight {
                    let next = queue.pop_front();
                    if let Some(next) = next {
                        turn_in_flight = spawn_turn(&driver, &turn_tx, next);
                    }
                }
            }
            // Inbound Signal frames.
            recv = transport.receive() => {
                let env = match recv {
                    Ok(Some(env)) => env,
                    Ok(None) => {
                        eprintln!("signal bridge: receive stream closed; shutting down.");
                        break;
                    }
                    Err(e) => {
                        eprintln!("signal bridge: receive error: {e}");
                        continue;
                    }
                };
                let Some(body) = gate.evaluate(&env) else { continue };
                if body.is_empty() {
                    continue;
                }
                // Control phrases are parsed BEFORE a body becomes a prompt.
                match parse_control(&body) {
                    Control::Status => {
                        let status = format!(
                            "status: session {} | {} | queue depth {} | membership: verifying before each post",
                            session_id,
                            if turn_in_flight { "turn in flight" } else { "idle" },
                            queue.len(),
                        );
                        verify_and_post(&mut transport, &group_id, &expected, &mut gate, &status).await;
                    }
                    Control::Stop => {
                        eprintln!("signal bridge: stop received; terminating child and closing group.");
                        preempt_child(&preempt);
                        let _ = transport.quit_group(&group_id).await;
                        break;
                    }
                    Control::Prompt(prompt) => {
                        let sender = env.source.as_deref().unwrap_or_default();
                        audit_accepted(&session_id, sender, env.source_device, &prompt);
                        if turn_in_flight {
                            queue.push_back(prompt);
                            while queue.len() > capacity {
                                queue.pop_front();
                                eprintln!(
                                    "signal bridge: turn queue at capacity ({capacity}); dropped oldest pending prompt."
                                );
                            }
                        } else {
                            turn_in_flight = spawn_turn(&driver, &turn_tx, prompt);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Spawn one serialized turn on the driver, delivering its captured stdout (or
/// error) over `tx`. Returns `true` (a turn is now in flight).
fn spawn_turn(
    driver: &Arc<SerialTurnDriver<CopilotTurnRunner>>,
    tx: &tokio::sync::mpsc::UnboundedSender<std::io::Result<String>>,
    prompt: String,
) -> bool {
    let driver = driver.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let output = driver.run_turn(&prompt).await;
        let _ = tx.send(output);
    });
    true
}

/// Pre-empt the in-flight turn, if any, by firing its child-bound trigger.
///
/// Takes the one-shot sender out of the shared [`PreemptSlot`] and sends `()`.
/// The turn task selecting on the paired receiver then kills its **owned**
/// [`tokio::process::Child`] via `Child::start_kill()` — bound by the runtime to
/// that exact process, so there is no PID-reuse (TOCTOU) window and no raw PID
/// is ever passed to `kill(2)`. If no turn is in flight the slot is empty and
/// this is a harmless no-op.
fn preempt_child(preempt: &PreemptSlot) {
    if let Some(tx) = preempt.lock().expect("preempt mutex not poisoned").take() {
        let _ = tx.send(());
    }
}
