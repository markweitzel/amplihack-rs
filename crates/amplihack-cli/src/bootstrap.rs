//! First-run bootstrap for framework assets and host CLIs.

use crate::binary_finder::{BinaryFinder, BinaryInfo};
use crate::claude_plugin;
use crate::commands::install;
use crate::copilot_setup;
use crate::freshness;
use crate::tool_update_check::{get_latest_version, sanitize_version};
use crate::util::{
    format_output_diagnostics, is_noninteractive, run_output_with_timeout, run_with_timeout,
};
use amplihack_utils::launch_target::{
    self, Health, LaunchAction, LaunchContext, PurgeOutcome, RepairAction, Resolution,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Timeout for tool installation commands (npm install, uv tool install).
/// These involve network downloads and can be legitimately slow, so we allow
/// 5 minutes before treating them as hung.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

pub fn prepare_launcher(tool: &str) -> Result<()> {
    // SEC-WS2-02: Non-interactive mode (CI, pipes, AMPLIHACK_NONINTERACTIVE=1)
    // skips all interactive setup. The environment is assumed pre-provisioned.
    // This matches Python launcher behavior and prevents hangs in sandboxes.
    if is_noninteractive() {
        tracing::debug!(
            tool,
            "non-interactive mode detected — skipping interactive bootstrap"
        );
        return Ok(());
    }

    check_required_tools()?;
    install::ensure_framework_installed()?;

    // Best-effort: bring the recipe runner up to date with upstream HEAD.
    // Runs on a 24h cooldown and can be disabled via
    // AMPLIHACK_NO_FRESHNESS_CHECK=1 or the standard non-interactive guards.
    // Network failures are logged and swallowed — launch must not depend on
    // reaching GitHub.
    freshness::ensure_recipe_runner_up_to_date();

    match tool {
        "copilot" => {
            // Hard gate: Copilot CLI requires Node.js >= 24.
            // If the system version is insufficient, auto-install a managed
            // copy to ~/.amplihack/runtimes/node/ and prepend it to PATH.
            if let Some(managed_bin_dir) = ensure_node_for_copilot()? {
                prepend_path(&managed_bin_dir)?;
                persist_path_hint(&managed_bin_dir)?;
            }
            copilot_setup::ensure_copilot_home_staged()?;
        }
        "claude" => {
            // Register amplihack as a Claude Code plugin so the agents,
            // skills, and commands staged under ~/.amplihack/.claude/ are
            // discoverable through Claude Code's plugin system. A failure
            // here must not block the launch — hooks are still wired via
            // settings.json even if the plugin registration fails.
            if let Err(err) = claude_plugin::ensure_claude_plugin_installed() {
                tracing::warn!(%err, "failed to register amplihack Claude plugin");
                eprintln!("⚠️  Failed to register amplihack as a Claude Code plugin: {err}");
            }
        }
        "codex" => configure_codex()?,
        _ => {}
    }

    Ok(())
}

/// Check that required system tools are available.
/// Prints warnings for missing tools but only fails for critical ones.
fn check_required_tools() -> Result<()> {
    // tmux is required for recipe runner workflow execution
    if which("tmux").is_none() {
        eprintln!("⚠️  tmux is not installed. Recipe workflow execution requires tmux.");
        eprintln!("   Install it:");
        eprintln!("     macOS:  brew install tmux");
        eprintln!("     Ubuntu: sudo apt install tmux");
        eprintln!("     Fedora: sudo dnf install tmux");
    }
    Ok(())
}

fn which(tool: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let full = dir.join(tool);
            if full.is_file() { Some(full) } else { None }
        })
    })
}

/// Ensure Node.js >= 24 is available for Copilot CLI. If the system version
/// is insufficient, downloads a managed copy to `~/.amplihack/runtimes/node/`.
/// Returns `Some(bin_dir)` when a managed install was used, `None` when the
/// system node is sufficient.
fn ensure_node_for_copilot() -> Result<Option<PathBuf>> {
    use amplihack_utils::prerequisites::{
        NODE_AUTO_INSTALL_VERSION, check_node_minimum_version, node_platform_triple,
    };

    const MIN: u32 = 24;

    // Fast path: system node is sufficient.
    if check_node_minimum_version(MIN).is_ok() {
        return Ok(None);
    }

    // Non-interactive environments should not auto-install.
    if is_noninteractive() {
        bail!(
            "Node.js >= v{MIN} is required but not found, and \
             auto-install is disabled in non-interactive mode.\n\
             Install Node.js manually: https://nodejs.org/"
        );
    }

    let (os_name, arch_name) = node_platform_triple().ok_or_else(|| {
        anyhow!(
            "Node.js >= v{MIN} is required but auto-install is not supported \
             on this platform.\nInstall Node.js manually: https://nodejs.org/"
        )
    })?;

    let runtimes_dir = home_dir()?.join(".amplihack").join("runtimes");
    let dir_name = format!("node-{NODE_AUTO_INSTALL_VERSION}-{os_name}-{arch_name}");
    let install_dir = runtimes_dir.join(&dir_name);
    let bin_dir = install_dir.join("bin");

    // Already installed?
    if bin_dir.join("node").exists() {
        tracing::info!(path = %bin_dir.display(), "managed Node.js already present");
        println!("  ✅ Managed Node.js {NODE_AUTO_INSTALL_VERSION} already installed");
        return Ok(Some(bin_dir));
    }

    let ext = "tar.xz";
    let filename = format!("node-{NODE_AUTO_INSTALL_VERSION}-{os_name}-{arch_name}.{ext}");
    let url = format!("https://nodejs.org/dist/{NODE_AUTO_INSTALL_VERSION}/{filename}");
    let checksum_filename = "SHASUMS256.txt";
    let checksum_url =
        format!("https://nodejs.org/dist/{NODE_AUTO_INSTALL_VERSION}/{checksum_filename}");

    println!("  ⬇️  Downloading Node.js {NODE_AUTO_INSTALL_VERSION} ({os_name}-{arch_name})...");
    tracing::info!(%url, "downloading Node.js");

    fs::create_dir_all(&runtimes_dir)
        .with_context(|| format!("failed to create {}", runtimes_dir.display()))?;

    let tmp_path = runtimes_dir.join(&filename);
    let checksum_path = runtimes_dir.join(format!("{filename}.{checksum_filename}"));

    if let Err(err) = download_with_curl(&url, &tmp_path, "Node.js archive") {
        let _ = fs::remove_file(&tmp_path);
        bail!("{err:#}\nInstall Node.js manually: https://nodejs.org/");
    }
    if let Err(err) = download_with_curl(&checksum_url, &checksum_path, "Node.js checksum manifest")
    {
        let _ = fs::remove_file(&tmp_path);
        let _ = fs::remove_file(&checksum_path);
        bail!("{err:#}\nInstall Node.js manually: https://nodejs.org/");
    }
    if let Err(err) = verify_node_archive_sha256(&tmp_path, &checksum_path, &filename) {
        let _ = fs::remove_file(&tmp_path);
        let _ = fs::remove_file(&checksum_path);
        bail!("{err:#}\nInstall Node.js manually: https://nodejs.org/");
    }
    let _ = fs::remove_file(&checksum_path);

    println!("  📦 Installing Node.js {NODE_AUTO_INSTALL_VERSION}...");

    // Extract to a temp directory, then atomically rename to install_dir.
    // This prevents partial extraction (disk full, interrupted) from leaving
    // a broken install that the next run would accept as valid.
    let temp_dir = runtimes_dir.join(format!("{dir_name}.extracting"));

    // Clean up any stale temp dir from a prior crash
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir)
        .with_context(|| format!("failed to create temp dir {}", temp_dir.display()))?;

    let mut extract_cmd = Command::new("tar");
    extract_cmd
        .args(["--strip-components=1", "-xJf"])
        .arg(&tmp_path)
        .arg("-C")
        .arg(&temp_dir);
    let extract_status =
        run_with_timeout(extract_cmd, INSTALL_TIMEOUT).context("failed to run tar")?;

    let _ = fs::remove_file(&tmp_path);

    if !extract_status.success() {
        let _ = fs::remove_dir_all(&temp_dir);
        bail!(
            "failed to extract Node.js tarball (exit {})",
            extract_status.code().unwrap_or(-1)
        );
    }

    if !temp_dir.join("bin").join("node").exists() {
        let _ = fs::remove_dir_all(&temp_dir);
        bail!(
            "Node.js extraction succeeded but bin/node not found in {}",
            temp_dir.display()
        );
    }

    fs::rename(&temp_dir, &install_dir).with_context(|| {
        let _ = fs::remove_dir_all(&temp_dir);
        format!(
            "failed to rename {} to {}",
            temp_dir.display(),
            install_dir.display()
        )
    })?;

    println!(
        "  ✅ Node.js {NODE_AUTO_INSTALL_VERSION} installed to {}",
        install_dir.display()
    );
    Ok(Some(bin_dir))
}

fn download_with_curl(url: &str, destination: &Path, label: &str) -> Result<()> {
    let mut cmd = Command::new("curl");
    cmd.args(["-fsSL", "-o"]).arg(destination).arg(url);
    let output = run_output_with_timeout(cmd, DOWNLOAD_TIMEOUT)
        .with_context(|| format!("{label} download timed out or failed to execute: {url}"))?;
    if !output.status.success() {
        bail!(
            "{label} download failed from {url}: {}",
            format_output_diagnostics(&output, 400)
        );
    }
    Ok(())
}

fn verify_node_archive_sha256(
    archive_path: &Path,
    checksum_path: &Path,
    archive_filename: &str,
) -> Result<()> {
    let manifest = fs::read_to_string(checksum_path).with_context(|| {
        format!(
            "failed to read Node.js checksum manifest {}",
            checksum_path.display()
        )
    })?;
    let expected = find_sha256_for_archive(&manifest, archive_filename)?;
    let mut archive = fs::File::open(archive_path)
        .with_context(|| format!("failed to read Node.js archive {}", archive_path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut archive, &mut hasher)
        .with_context(|| format!("failed to hash Node.js archive {}", archive_path.display()))?;
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        bail!(
            "Node.js archive SHA-256 verification failed for {archive_filename}: expected {expected}, got {actual}"
        );
    }
    Ok(())
}

fn find_sha256_for_archive(manifest: &str, archive_filename: &str) -> Result<String> {
    let mut matches = manifest.lines().filter_map(|line| {
        let mut parts = line.split_whitespace();
        let digest = parts.next()?;
        let filename = parts.next()?;
        (filename == archive_filename).then(|| digest.to_ascii_lowercase())
    });
    let digest = matches
        .next()
        .ok_or_else(|| anyhow!("Node.js checksum manifest does not list {archive_filename}"))?;
    if matches.next().is_some() {
        bail!("Node.js checksum manifest lists {archive_filename} more than once");
    }
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Node.js checksum manifest has an invalid SHA-256 digest for {archive_filename}");
    }
    Ok(digest)
}

/// Resolve the binary amplihack will launch for `tool`, installing or
/// repairing it first when — and only when — that is amplihack's to do.
///
/// # The defect this replaces (issue #1266)
///
/// This function used to ask three different questions of three different
/// binaries. `BinaryFinder::find` decided what to launch; a separate upgrade
/// check asked `npm list -g` (which, with no `--prefix`, reports on `/usr`)
/// whether an upgrade was needed; and the install wrote somewhere else again. On any
/// host where those three locations differ — which is most of them — the
/// version check was *permanently* stale, so every single launch re-downloaded
/// 339MB, reinstalled, and clobbered any hand-repair.
///
/// Now there is one resolver. [`launch_target::resolve`] decides what would be
/// exec'd, [`launch_target::decide_launch_action`] decides what to do about
/// it from that same resolution, and nothing is ever exec'd that did not pass
/// the health gate.
pub fn ensure_tool_available(tool: &str) -> Result<BinaryInfo> {
    let package = npm_package_for_install(tool);
    let ctx = LaunchContext {
        npm_backed: package.is_some(),
        interactive: !is_noninteractive(),
    };

    let mut resolution = launch_target::resolve(tool);
    let latest = latest_published_version(package, ctx);

    match launch_target::decide_launch_action(&resolution, latest.as_deref(), ctx) {
        LaunchAction::Launch => {}

        // amplihack does not own this binary, so it does not write to it.
        // Installing here is exactly what created the second copy at a
        // different PATH precedence that made the version check stale forever.
        LaunchAction::NoticeOnly { from, to } => {
            let pkg = package.unwrap_or(tool);
            println!("📦 {tool} {from} → {to} is available ({pkg}).");
            println!(
                "   Not installed automatically: this binary is not one amplihack manages."
            );
            println!("   To update it yourself:  npm install -g {pkg}");
        }

        LaunchAction::Upgrade { from, to } => {
            let pkg = package.unwrap_or(tool);
            println!("📦 Upgrading {tool} ({pkg}): {from} → {to}");
            if let Err(err) = install_tool(tool) {
                tracing::warn!(%err, tool, pkg, "tool upgrade failed; continuing with existing install");
            }
            // Re-resolve rather than trusting the install: the health gate
            // refuses to select a result that came back broken, so a failed
            // upgrade degrades to "launch what we had" instead of "launch the
            // wreckage".
            resolution = launch_target::resolve(tool);
        }

        LaunchAction::InstallFresh => {
            repair_or_install(tool, &resolution)?;
            resolution = launch_target::resolve(tool);
        }

        LaunchAction::Fail => {
            bail!(
                "no working '{tool}' binary is available, and amplihack cannot install one \
                 (it is not distributed via npm).\n\
                 Candidates considered:\n{rejected}",
                rejected = launch_target::render_rejections(&resolution),
            );
        }
    }

    match resolution.selected {
        Some(candidate) => Ok(BinaryInfo {
            name: tool.to_string(),
            path: candidate.path,
            version: match candidate.health {
                Health::Working { version, .. } => Some(version),
                // Unreachable by the resolver's contract; kept as an explicit
                // arm so a future change to Resolution cannot silently
                // reintroduce a version-less launch.
                Health::Broken(_) => None,
            },
        }),
        None => {
            let prefix_hint = npm_prefix_dir()
                .map(|p| p.join("bin").display().to_string())
                .unwrap_or_else(|_| "the amplihack npm prefix".to_string());
            bail!(
                "failed to locate a working '{tool}' binary after installation.\n\
                 Candidates considered:\n{rejected}\
                 If the install succeeded, '{tool}' may not be on your PATH.\n\
                 Try running:\n  \
                 export PATH=\"{prefix_hint}:$PATH\"\n\
                 You can also try installing manually:\n  \
                 npm install -g --prefix {prefix_hint} {pkg}",
                rejected = launch_target::render_rejections(&resolution),
                pkg = package.unwrap_or(tool),
            )
        }
    }
}

/// Query npm for the latest published version, sanitized for display.
///
/// SEC-A17: `sanitize_version` is retained *here* because this string comes
/// from the npm registry, which is untrusted. The installed version takes the
/// other path — `extract_semver` on the binary's own output — because
/// `sanitize_version` mangles `"2.1.238 (Claude Code)"` into
/// `"2.1.238ClaudeCode"`, which never compares equal to npm's `"2.1.238"`.
fn latest_published_version(package: Option<&'static str>, ctx: LaunchContext) -> Option<String> {
    if !ctx.npm_backed || !ctx.interactive {
        return None;
    }
    let sanitized = sanitize_version(&get_latest_version(package?)?);
    if sanitized.is_empty() {
        None
    } else {
        Some(sanitized)
    }
}

/// Install `tool`, first noting whether this is a repair of amplihack's own
/// broken install, and afterwards removing anything that is *still* broken.
///
/// A stub left in `~/.npm-global/bin` is not merely useless: on a host where
/// that directory sits early on `$PATH` — first, on the repo owner's WSL
/// machine — it shadows a working native install and breaks bare `claude`
/// system-wide. Purging is bounded by [`launch_target::purge_binary_under`],
/// which re-checks containment and refuses everything outside amplihack's own
/// prefix.
fn repair_or_install(tool: &str, before: &Resolution) -> Result<()> {
    let repairing = before.rejected.iter().any(|candidate| {
        launch_target::decide_repair_action(
            candidate.ownership,
            &candidate.health,
            candidate.source,
            false,
        ) == RepairAction::CompleteInstall
    });
    if repairing {
        println!("🔧 Repairing amplihack's own {tool} install (the previous one is not functional)...");
    }

    let install_result = install_tool(tool);

    // Second pass: repair has now been attempted, so anything still broken and
    // still ours is purged rather than left to shadow a working binary.
    let after = launch_target::resolve(tool);
    let prefix = launch_target::npm_prefix_dir();
    for candidate in &after.rejected {
        let action = launch_target::decide_repair_action(
            candidate.ownership,
            &candidate.health,
            candidate.source,
            true,
        );
        if action != RepairAction::Purge {
            continue;
        }
        match launch_target::purge_binary_under(prefix.as_deref(), &candidate.path) {
            PurgeOutcome::Removed => {
                println!(
                    "  🧹 Removed non-functional {}: it was shadowing working binaries on PATH",
                    launch_target::sanitize_display_path(&candidate.path)
                );
            }
            PurgeOutcome::Denied | PurgeOutcome::Failed => {}
        }
    }

    install_result
}

/// Map a tool name to the npm package used for installation and upgrades.
///
/// This is the single source of truth — `install_tool`, the upgrade decision
/// in `ensure_tool_available`, and `needs_claude_two_step` all read through
/// here so they can never disagree on which package backs a given tool.
fn npm_package_for_install(tool: &str) -> Option<&'static str> {
    match tool {
        "claude" => Some("@anthropic-ai/claude-code"),
        "copilot" => Some("@github/copilot"),
        "codex" => Some("@openai/codex"),
        _ => None,
    }
}

fn install_tool(tool: &str) -> Result<()> {
    if let Some(pkg) = npm_package_for_install(tool) {
        return install_npm_package(tool, pkg);
    }
    match tool {
        "amplifier" => install_amplifier(),
        other => bail!("automatic installation is not implemented for '{other}'"),
    }
}

fn install_npm_package(tool: &str, package: &str) -> Result<()> {
    let npm = BinaryFinder::find("npm")
        .context("npm is required to install Node-based host CLIs")?
        .path;

    let prefix = npm_prefix_dir()?;
    let bin_dir = prefix.join("bin");
    fs::create_dir_all(&bin_dir)
        .with_context(|| format!("failed to create {}", bin_dir.display()))?;

    prepend_path(&bin_dir)?;
    println!("📦 Installing {tool} via npm package {package}...");

    // Clean any stale temp dirs npm left behind from a prior failed install
    // (e.g. `@github/.copilot-YYsO5Mpa`). Left in place, these cause
    // `ENOTEMPTY: directory not empty, rename ...` on every subsequent install.
    clean_stale_npm_temp_dirs(&prefix, package);

    match run_npm_install(&npm, &prefix, package) {
        Ok(()) => {}
        Err(err) => {
            // Last-ditch: clean again and retry once. npm's own rename can fail
            // if a concurrent install (or even the first part of this one) raced.
            tracing::warn!(%err, "npm install failed; cleaning stale temp dirs and retrying once");
            clean_stale_npm_temp_dirs(&prefix, package);
            remove_package_install_dir(&prefix, package);
            run_npm_install(&npm, &prefix, package)?;
        }
    }

    // Issue #585: After installing @github/copilot with --omit=optional,
    // install the platform-specific native binary package separately.
    // This avoids the npm reify hang caused by cross-platform optional deps
    // while still getting the correct native binary for the current platform.
    if package == "@github/copilot" {
        let (os_name, arch) = current_platform();
        if let Some(platform_pkg) = copilot_platform_package(os_name, arch) {
            println!("📦 Installing platform binary {platform_pkg}...");
            if let Err(err) = run_npm_install(&npm, &prefix, platform_pkg) {
                // Non-fatal: Node.js may have a JS fallback via index.js on
                // sufficiently recent versions. Warn but don't fail the install.
                tracing::warn!(
                    %err,
                    platform_pkg,
                    "platform-specific binary install failed; \
                     copilot may fall back to JS implementation"
                );
                eprintln!(
                    "⚠️  Platform binary {platform_pkg} failed to install: {err}\n   \
                     Copilot may still work via JS fallback on recent Node.js versions."
                );
            }
        } else {
            tracing::info!(
                os_name,
                arch,
                "no known platform binary for this OS/arch; skipping"
            );
        }
    }

    // Issue #1266, Defect 1: make the claude install an actual install.
    //
    // `@anthropic-ai/claude-code` ships its real binary exactly the way
    // `@github/copilot` does — as platform-specific `optionalDependencies` —
    // and materializes it in a `postinstall` (`node install.cjs`) that
    // hardlinks the platform package's binary over a ~500-byte ASCII
    // placeholder. amplihack passes BOTH `--omit=optional` and
    // `--ignore-scripts` on every npm invocation, and EITHER ONE ALONE is
    // enough to leave that placeholder in place:
    //
    //   --omit=optional   the platform package is never fetched, so
    //                     install.cjs runs, finds nothing, and leaves the stub
    //   --ignore-scripts  install.cjs never runs at all
    //
    // So this cannot be fixed by relaxing a flag. Narrowing `--ignore-scripts`
    // for this package still yields a stub; narrowing `--omit=optional` too
    // would reintroduce issue #585's indefinite npm reify hang for a package
    // with 8 cross-platform optional deps. Instead amplihack performs the two
    // missing steps itself, explicitly, for one package named in its own
    // source — mirroring the copilot two-step immediately above.
    //
    // `run_npm_install` is therefore untouched: the flag policy for every
    // other package is unchanged and #585's contract tests still pass
    // unmodified. The exception is one auditable branch here, not a relaxation
    // applied to a class of packages.
    //
    // On running install.cjs at all: amplihack is about to exec this package's
    // native binary anyway. Declining to run the package's own postinstall
    // while planning to exec its binary seconds later is not a coherent threat
    // model — the postinstall is strictly less privileged than what follows.
    if needs_claude_two_step(package) {
        install_claude_native_binary(&npm, &prefix);
    }

    persist_path_hint(&bin_dir)?;
    Ok(())
}

fn run_npm_install(npm: &Path, prefix: &Path, package: &str) -> Result<()> {
    let mut npm_cmd = Command::new(npm);
    npm_cmd
        .arg("install")
        .arg("-g")
        .arg("--prefix")
        .arg(prefix)
        .arg("--omit=optional")
        .arg(package)
        .arg("--ignore-scripts");
    let status = run_with_timeout(npm_cmd, INSTALL_TIMEOUT).with_context(|| {
        format!(
            "npm install timed out for package '{package}' after {}s.\n\
             This is often caused by npm hanging on cross-platform optional deps.\n\
             Try running manually:\n  \
             npm install -g --prefix {} --omit=optional --ignore-scripts {package}",
            INSTALL_TIMEOUT.as_secs(),
            prefix.display(),
        )
    })?;

    if !status.success() {
        bail!(
            "npm install failed for package '{package}' (exit code: {code}).\n\
             Try running manually:\n  \
             npm install -g --prefix {prefix} --omit=optional --ignore-scripts {package}\n\
             If the problem persists, check npm logs:\n  \
             npm cache clean --force && npm install -g --prefix {prefix} {package}",
            package = package,
            code = status
                .code()
                .map_or("unknown".to_string(), |c| c.to_string()),
            prefix = prefix.display(),
        );
    }
    Ok(())
}

/// Determine the correct `@github/copilot-{os}-{arch}` package for the
/// current platform. Returns `None` for unrecognized OS/arch combinations,
/// which signals the caller to skip the platform binary install (non-fatal).
fn copilot_platform_package(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("@github/copilot-linux-x64"),
        ("linux", "aarch64") => Some("@github/copilot-linux-arm64"),
        ("macos", "x86_64") => Some("@github/copilot-darwin-x64"),
        ("macos", "aarch64") => Some("@github/copilot-darwin-arm64"),
        ("windows", "x86_64") => Some("@github/copilot-win32-x64"),
        ("windows", "aarch64") => Some("@github/copilot-win32-arm64"),
        _ => None,
    }
}

/// The C library the host runs, which decides whether claude's platform
/// package carries the `-musl` suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Libc {
    /// Standard GNU libc (the overwhelming majority of hosts).
    Glibc,
    /// musl libc (Alpine and similar).
    Musl,
}

/// The other libc. Used for the single bounded retry after a failed install.
fn other_libc(libc: Libc) -> Libc {
    match libc {
        Libc::Glibc => Libc::Musl,
        Libc::Musl => Libc::Glibc,
    }
}

/// Detect the host's C library.
///
/// SEC-A21: this is a filesystem check and spawns nothing. musl's dynamic
/// loader is installed as `/lib/ld-musl-<arch>.so.1` on every musl system, so
/// its presence is decisive and its absence means glibc.
///
/// Ambiguity therefore defaults to [`Libc::Glibc`]. A wrong guess is not
/// silently fatal: `install.cjs` finds no matching platform package, the
/// placeholder survives, the health gate rejects it, and
/// `install_claude_native_binary` retries once with the other libc. That is
/// validation acting as defense-in-depth rather than as the fix.
fn detect_libc() -> Libc {
    for dir in ["/lib", "/usr/lib"] {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("ld-musl-") {
                return Libc::Musl;
            }
        }
    }
    Libc::Glibc
}

/// Determine the `@anthropic-ai/claude-code-{platform}` package for this host.
///
/// Mirrors `install.cjs`'s `getPlatformKey`, including that the `-musl` suffix
/// exists on Linux only — inventing `...-darwin-x64-musl` would request a
/// package the registry does not have.
///
/// SEC-A20: the return type is `Option<&'static str>`, and that is the
/// security control. It makes "concatenate a detected string into a package
/// spec" unrepresentable, so no runtime-derived OS/arch/libc string can ever
/// reach npm's argv. An unrecognized platform yields `None`, which skips the
/// step — non-fatal, exactly as the copilot path already does.
fn claude_platform_package(os: &str, arch: &str, libc: Libc) -> Option<&'static str> {
    match (os, arch, libc) {
        ("linux", "x86_64", Libc::Glibc) => Some("@anthropic-ai/claude-code-linux-x64"),
        ("linux", "x86_64", Libc::Musl) => Some("@anthropic-ai/claude-code-linux-x64-musl"),
        ("linux", "aarch64", Libc::Glibc) => Some("@anthropic-ai/claude-code-linux-arm64"),
        ("linux", "aarch64", Libc::Musl) => Some("@anthropic-ai/claude-code-linux-arm64-musl"),
        ("macos", "x86_64", _) => Some("@anthropic-ai/claude-code-darwin-x64"),
        ("macos", "aarch64", _) => Some("@anthropic-ai/claude-code-darwin-arm64"),
        ("windows", "x86_64", _) => Some("@anthropic-ai/claude-code-win32-x64"),
        ("windows", "aarch64", _) => Some("@anthropic-ai/claude-code-win32-arm64"),
        _ => None,
    }
}

/// Is this exactly the package whose postinstall amplihack will run?
///
/// SEC-A22: exact equality against the `&'static str` the install table
/// returns — never `contains()`, never `starts_with()`, never a match on the
/// caller-supplied tool name. This predicate is the *entire* basis for
/// accepting that amplihack executes `install.cjs` at all. If it ever widens,
/// a hostile package name reaches a `node <script>` execution, so it is
/// covered by a negative test listing the near-miss spellings.
fn needs_claude_two_step(package: &str) -> bool {
    npm_package_for_install("claude").is_some_and(|claude_package| package == claude_package)
}

/// Absolute path to claude's postinstall script inside `prefix`.
///
/// SEC-A7: built from `&'static str` components only, so no runtime string
/// contributes a path segment and `..` traversal is unrepresentable.
fn claude_postinstall_script(prefix: &Path) -> PathBuf {
    prefix
        .join("lib")
        .join("node_modules")
        .join("@anthropic-ai")
        .join("claude-code")
        .join("install.cjs")
}

/// May we execute the postinstall script we just found?
///
/// SEC-A8: `symlink_metadata`, deliberately not `metadata`. A symlink where
/// npm should have unpacked a regular file means package tampering, and
/// following it would hand `node` an arbitrary file to execute. A missing
/// script is a skip, not a failure — the health gate is the enforcement point.
fn claude_postinstall_script_is_trusted(prefix: &Path) -> bool {
    fs::symlink_metadata(claude_postinstall_script(prefix))
        .map(|meta| meta.file_type().is_file())
        .unwrap_or(false)
}

/// Run claude's postinstall, which hardlinks the platform package's binary
/// over the placeholder shim. This is the step that turns ~500 bytes of ASCII
/// into the ~339MB native binary.
///
/// Every failure mode is non-fatal and logged: the health gate decides whether
/// the result is launchable, not this function.
fn run_claude_postinstall(prefix: &Path) {
    if !claude_postinstall_script_is_trusted(prefix) {
        tracing::warn!(
            prefix = %prefix.display(),
            "claude postinstall script is missing or is not a regular file; skipping it"
        );
        return;
    }
    // SEC-A9: resolve `node` through BinaryFinder rather than
    // `Command::new("node")`. Spawning a bare name re-enters the very PATH
    // trust problem this change exists to fix.
    let node = match BinaryFinder::find("node") {
        Ok(info) => info.path,
        Err(err) => {
            tracing::warn!(%err, "node was not found; cannot run the claude postinstall");
            return;
        }
    };

    println!("📦 Materializing the claude native binary...");
    let mut cmd = Command::new(node);
    cmd.arg(claude_postinstall_script(prefix));
    // SEC-A10: no shell, and stdin closed. A postinstall that prompts must not
    // be able to hang the launch or read the user's terminal. SEC-A11: the
    // environment is not widened — `~/.npmrc` may hold a registry auth token.
    cmd.stdin(std::process::Stdio::null());
    cmd.current_dir(prefix);

    // Killed on timeout, not abandoned: a postinstall that hangs forever would
    // reintroduce #585's failure class through a new door.
    match run_with_timeout(cmd, INSTALL_TIMEOUT) {
        Ok(status) if status.success() => {}
        Ok(status) => {
            tracing::warn!(code = ?status.code(), "claude postinstall exited non-zero");
        }
        Err(err) => {
            tracing::warn!(%err, "claude postinstall failed to run");
        }
    }
}

/// Is the claude binary in `prefix` actually launchable?
///
/// Reads through the same health gate the launch path uses, so "installed"
/// here means exactly what "launchable" means there.
fn claude_install_is_healthy(prefix: &Path) -> bool {
    let bin_dir = prefix.join("bin");
    ["claude", "claude.cmd", "claude.exe"]
        .iter()
        .map(|name| bin_dir.join(name))
        .filter(|path| path.exists())
        .any(|path| matches!(launch_target::probe_health(&path), Health::Working { .. }))
}

/// Fetch claude's platform binary package and run its postinstall.
///
/// Steps 2 and 3 of the three-step install (step 1, the base package, ran in
/// `install_npm_package` through the untouched `run_npm_install`).
///
/// Non-fatal throughout, by design: the health gate refuses to launch a broken
/// result, so failing loudly here would only convert a recoverable state into
/// an outage. One bounded retry with the other libc retires the whole
/// wrong-libc risk class for the cost of a single extra download.
fn install_claude_native_binary(npm: &Path, prefix: &Path) {
    let (os_name, arch) = current_platform();
    let mut libc = detect_libc();

    for attempt in 1..=2 {
        let Some(platform_pkg) = claude_platform_package(os_name, arch, libc) else {
            tracing::info!(
                os_name,
                arch,
                ?libc,
                "no known claude platform binary for this OS/arch; skipping"
            );
            return;
        };

        println!("📦 Installing platform binary {platform_pkg}...");
        if let Err(err) = run_npm_install(npm, prefix, platform_pkg) {
            tracing::warn!(%err, platform_pkg, "claude platform binary install failed");
            eprintln!("⚠️  Platform binary {platform_pkg} failed to install: {err}");
        }
        run_claude_postinstall(prefix);

        if claude_install_is_healthy(prefix) {
            return;
        }

        // Retrying is only meaningful on Linux, where the libc guess is the
        // one thing that could have selected the wrong package.
        if attempt == 2 || os_name != "linux" {
            tracing::warn!(
                os_name,
                arch,
                ?libc,
                "the claude native binary is still not functional after installing"
            );
            return;
        }
        libc = other_libc(libc);
        println!("   Native binary still not functional; retrying as {libc:?}...");
    }
}

/// Returns `(os_name, arch)` using Rust's compile-time target constants.
/// Values match `copilot_platform_package` keys directly ("linux", "macos",
/// "windows" for OS; "x86_64", "aarch64" for arch).
fn current_platform() -> (&'static str, &'static str) {
    (std::env::consts::OS, std::env::consts::ARCH)
}

/// Remove stale `.<name>-XXXX` temp dirs that npm leaves behind in the scope
/// directory after a crashed install.
///
/// For a scoped package like `@github/copilot`, npm stages the new copy in
/// `$prefix/lib/node_modules/@github/.copilot-XXXX` and then renames over the
/// final directory. If the rename fails (or npm is killed mid-install), the
/// temp dir is left behind and every subsequent `npm install` trips ENOTEMPTY.
///
/// For an unscoped package `foo`, npm stages it as
/// `$prefix/lib/node_modules/.foo-XXXX`.
fn clean_stale_npm_temp_dirs(prefix: &Path, package: &str) {
    let node_modules = prefix.join("lib").join("node_modules");
    let (scope_dir, dot_prefix) = match split_npm_package(package) {
        Some((scope, name)) => (node_modules.join(format!("@{scope}")), format!(".{name}-")),
        None => (node_modules, format!(".{package}-")),
    };
    let Ok(entries) = fs::read_dir(&scope_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.starts_with(&dot_prefix) {
            continue;
        }
        let path = entry.path();
        tracing::warn!(path = %path.display(), "removing stale npm temp dir");
        if let Err(err) = fs::remove_dir_all(&path) {
            tracing::warn!(%err, path = %path.display(), "failed to remove stale npm temp dir");
        } else {
            println!("  🧹 Removed stale npm temp dir: {}", path.display());
        }
    }
}

/// Remove the installed package directory (if present) so `npm install` can
/// recreate it from scratch. Used as a final fallback when the rename path is
/// still wedged.
fn remove_package_install_dir(prefix: &Path, package: &str) {
    let node_modules = prefix.join("lib").join("node_modules");
    let install_dir = match split_npm_package(package) {
        Some((scope, name)) => node_modules.join(format!("@{scope}")).join(name),
        None => node_modules.join(package),
    };
    if install_dir.exists() {
        tracing::warn!(
            path = %install_dir.display(),
            "removing existing package install dir before retry"
        );
        let _ = fs::remove_dir_all(&install_dir);
    }
}

fn split_npm_package(package: &str) -> Option<(&str, &str)> {
    let rest = package.strip_prefix('@')?;
    let (scope, name) = rest.split_once('/')?;
    if scope.is_empty() || name.is_empty() {
        return None;
    }
    Some((scope, name))
}

fn install_amplifier() -> Result<()> {
    let uv = BinaryFinder::find("uv")
        .context("uv is required to install amplifier")?
        .path;
    let bin_dir = uv_bin_dir()?;
    prepend_path(&bin_dir)?;

    println!("📦 Installing amplifier via uv tool...");
    let mut uv_cmd = Command::new(uv);
    uv_cmd
        .arg("tool")
        .arg("install")
        .arg("git+https://github.com/microsoft/amplifier");
    let status =
        run_with_timeout(uv_cmd, INSTALL_TIMEOUT).context("failed to execute uv tool install")?;

    if !status.success() {
        bail!("uv tool install failed for amplifier");
    }

    persist_path_hint(&bin_dir)?;
    Ok(())
}

fn configure_codex() -> Result<()> {
    let config_dir = home_dir()?.join(".openai").join("codex");
    fs::create_dir_all(&config_dir)
        .with_context(|| format!("failed to create {}", config_dir.display()))?;
    let config_path = config_dir.join("config.json");

    let mut value = if config_path.exists() {
        let raw = fs::read_to_string(&config_path).with_context(|| {
            format!(
                "refusing to overwrite unreadable existing Codex config {}",
                config_path.display()
            )
        })?;
        let parsed: Value = serde_json::from_str(&raw).with_context(|| {
            format!(
                "refusing to overwrite malformed existing Codex config {}",
                config_path.display()
            )
        })?;
        if !parsed.is_object() {
            bail!(
                "refusing to overwrite existing Codex config {} because it is not an object",
                config_path.display()
            );
        }
        parsed
    } else {
        json!({})
    };

    let object = value
        .as_object_mut()
        .expect("value is guaranteed an object");
    if object.get("approval_mode").and_then(Value::as_str) != Some("auto") {
        object.insert(
            "approval_mode".to_string(),
            Value::String("auto".to_string()),
        );
        fs::write(&config_path, serde_json::to_string_pretty(&value)? + "\n")
            .with_context(|| format!("failed to write {}", config_path.display()))?;
    }

    Ok(())
}

fn prepend_path(dir: &Path) -> Result<()> {
    let current = std::env::var_os("PATH").unwrap_or_default();
    // Check membership without allocating a Vec in the common already-present case.
    if std::env::split_paths(&current).any(|existing| existing == dir) {
        return Ok(());
    }

    let mut updated = vec![dir.to_path_buf()];
    updated.extend(std::env::split_paths(&current));
    let joined = std::env::join_paths(updated).context("failed to rebuild PATH")?;
    // SAFETY: This CLI is single-process during bootstrap and updates PATH intentionally.
    unsafe {
        std::env::set_var("PATH", joined);
    }
    Ok(())
}

fn persist_path_hint(bin_dir: &Path) -> Result<()> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let profile = if shell.ends_with("/zsh") || shell.ends_with("/zsh5") {
        home_dir()?.join(".zshrc")
    } else {
        home_dir()?.join(".bashrc")
    };
    let export_line = format!("export PATH=\"{}:$PATH\"", bin_dir.display());

    let existing = fs::read_to_string(&profile).unwrap_or_default();
    if existing.contains(&export_line) {
        return Ok(());
    }

    let mut content = existing;
    if !content.ends_with('\n') && !content.is_empty() {
        content.push('\n');
    }
    content.push_str("# Added by amplihack\n");
    content.push_str(&export_line);
    content.push('\n');

    fs::write(&profile, content).with_context(|| format!("failed to update {}", profile.display()))
}

/// amplihack's npm prefix.
///
/// Delegates to `launch_target`, which owns the single definition. Four call
/// sites used to compute this independently, which is how the version check,
/// the install target, and the exec target came to disagree (issue #1266).
fn npm_prefix_dir() -> Result<PathBuf> {
    launch_target::npm_prefix_dir()
        .ok_or_else(|| anyhow!("HOME is not set to a usable absolute path"))
}

fn uv_bin_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("UV_TOOL_BIN_DIR") {
        let path = PathBuf::from(dir);
        if !path.as_os_str().is_empty() {
            fs::create_dir_all(&path)
                .with_context(|| format!("failed to create {}", path.display()))?;
            return Ok(path);
        }
    }

    let path = home_dir()?.join(".local").join("bin");
    fs::create_dir_all(&path).with_context(|| format!("failed to create {}", path.display()))?;
    Ok(path)
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("HOME is not set"))
}

#[cfg(test)]
#[path = "bootstrap_claude_install_tests.rs"]
mod claude_install_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_codex_sets_auto_mode() {
        let _guard = crate::test_support::home_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let previous_home = crate::test_support::set_home(temp.path());
        configure_codex().unwrap();

        let config = fs::read_to_string(temp.path().join(".openai/codex/config.json")).unwrap();
        let value: Value = serde_json::from_str(&config).unwrap();
        assert_eq!(value["approval_mode"], "auto");

        crate::test_support::restore_home(previous_home);
    }

    #[test]
    fn configure_codex_refuses_malformed_existing_config_without_overwriting() {
        let _guard = crate::test_support::home_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let previous_home = crate::test_support::set_home(temp.path());
        let config_dir = temp.path().join(".openai/codex");
        fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("config.json");
        let original = "{ this is not json";
        fs::write(&config_path, original).unwrap();

        let error = configure_codex().expect_err("malformed config must be preserved");

        assert_eq!(fs::read_to_string(&config_path).unwrap(), original);
        assert!(
            error.to_string().contains("malformed")
                || error.to_string().contains("refusing to overwrite"),
            "error should clearly explain malformed config preservation; got {error:#}"
        );

        crate::test_support::restore_home(previous_home);
    }

    #[test]
    fn configure_codex_refuses_non_object_existing_config_without_overwriting() {
        let _guard = crate::test_support::home_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let previous_home = crate::test_support::set_home(temp.path());
        let config_dir = temp.path().join(".openai/codex");
        fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("config.json");
        let original = "[\"not\", \"an\", \"object\"]\n";
        fs::write(&config_path, original).unwrap();

        let error = configure_codex().expect_err("non-object config must be preserved");

        assert_eq!(fs::read_to_string(&config_path).unwrap(), original);
        assert!(
            error.to_string().contains("not an object")
                || error.to_string().contains("refusing to overwrite"),
            "error should clearly explain non-object config preservation; got {error:#}"
        );

        crate::test_support::restore_home(previous_home);
    }

    #[test]
    fn node_checksum_manifest_requires_exact_archive_entry() {
        let expected = "a".repeat(64);
        let other = "b".repeat(64);
        let manifest = format!("{expected}  node-v1-linux-x64.tar.xz\n{other}  other.tar.xz\n");

        let digest = find_sha256_for_archive(&manifest, "node-v1-linux-x64.tar.xz").unwrap();

        assert_eq!(digest, expected);
        assert!(find_sha256_for_archive(&manifest, "missing.tar.xz").is_err());
    }

    #[test]
    fn node_archive_sha256_mismatch_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("node-test.tar.xz");
        let checksum = temp.path().join("SHASUMS256.txt");
        fs::write(&archive, b"not the expected archive").unwrap();
        fs::write(&checksum, format!("{}  node-test.tar.xz\n", "0".repeat(64))).unwrap();

        let error = verify_node_archive_sha256(&archive, &checksum, "node-test.tar.xz")
            .expect_err("checksum mismatch must fail closed");

        assert!(
            error.to_string().contains("SHA-256 verification failed"),
            "checksum mismatch should be explicit; got {error:#}"
        );
    }

    // ========================================================================
    // Issue #585: copilot_platform_package() helper
    // ========================================================================

    #[test]
    fn copilot_platform_package_returns_correct_linux_x64() {
        // Contract: On linux/x86_64, must return @github/copilot-linux-x64
        let result = copilot_platform_package("linux", "x86_64");
        assert_eq!(
            result,
            Some("@github/copilot-linux-x64"),
            "linux + x86_64 must map to copilot-linux-x64"
        );
    }

    #[test]
    fn copilot_platform_package_returns_correct_linux_arm64() {
        let result = copilot_platform_package("linux", "aarch64");
        assert_eq!(
            result,
            Some("@github/copilot-linux-arm64"),
            "linux + aarch64 must map to copilot-linux-arm64"
        );
    }

    #[test]
    fn copilot_platform_package_returns_correct_macos_arm64() {
        let result = copilot_platform_package("macos", "aarch64");
        assert_eq!(
            result,
            Some("@github/copilot-darwin-arm64"),
            "macos + aarch64 must map to copilot-darwin-arm64"
        );
    }

    #[test]
    fn copilot_platform_package_returns_correct_macos_x64() {
        let result = copilot_platform_package("macos", "x86_64");
        assert_eq!(
            result,
            Some("@github/copilot-darwin-x64"),
            "macos + x86_64 must map to copilot-darwin-x64"
        );
    }

    #[test]
    fn copilot_platform_package_returns_correct_windows_x64() {
        let result = copilot_platform_package("windows", "x86_64");
        assert_eq!(
            result,
            Some("@github/copilot-win32-x64"),
            "windows + x86_64 must map to copilot-win32-x64"
        );
    }

    #[test]
    fn copilot_platform_package_returns_none_for_unknown_os() {
        let result = copilot_platform_package("freebsd", "x86_64");
        assert_eq!(
            result, None,
            "unknown OS must return None (non-fatal fallback)"
        );
    }

    #[test]
    fn copilot_platform_package_returns_none_for_unknown_arch() {
        let result = copilot_platform_package("linux", "riscv64");
        assert_eq!(
            result, None,
            "unknown arch must return None (non-fatal fallback)"
        );
    }

    // ========================================================================
    // Issue #585: split_npm_package (existing helper, verify edge cases)
    // ========================================================================

    #[test]
    fn split_npm_package_handles_copilot_platform_packages() {
        // Contract: platform-specific packages like @github/copilot-linux-x64
        // must parse correctly through split_npm_package.
        assert_eq!(
            split_npm_package("@github/copilot-linux-x64"),
            Some(("github", "copilot-linux-x64"))
        );
        assert_eq!(
            split_npm_package("@github/copilot-darwin-arm64"),
            Some(("github", "copilot-darwin-arm64"))
        );
    }

    #[test]
    fn persist_path_hint_is_idempotent() {
        let _guard = crate::test_support::home_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let previous_home = crate::test_support::set_home(temp.path());
        let previous_shell = std::env::var_os("SHELL");
        // SAFETY: Test-only shell override.
        unsafe {
            std::env::set_var("SHELL", "/bin/bash");
        }

        let bin_dir = temp.path().join(".npm-global/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        persist_path_hint(&bin_dir).unwrap();
        persist_path_hint(&bin_dir).unwrap();

        let profile = fs::read_to_string(temp.path().join(".bashrc")).unwrap();
        assert_eq!(profile.matches("Added by amplihack").count(), 1);

        match previous_shell {
            Some(value) => unsafe { std::env::set_var("SHELL", value) },
            None => unsafe { std::env::remove_var("SHELL") },
        }
        crate::test_support::restore_home(previous_home);
    }
}
