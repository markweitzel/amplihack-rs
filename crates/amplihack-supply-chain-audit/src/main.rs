//! CLI entry point for the supply-chain-audit skill.
//!
//! Runs a supply-chain security audit over a repository and emits a markdown
//! report (default), JSON (`--json`), or a summary-only view (`--summary-only`).

use amplihack_supply_chain_audit::{
    AuditConfig, Severity, SupplyChainAuditError, check_missing_tools, run_audit,
};
use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;

/// Audit software supply-chain security across CI/CD, containers, and packages.
#[derive(Parser, Debug)]
#[command(name = "amplihack-supply-chain-audit", version, about)]
struct Cli {
    /// Repository root to audit.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Comma-separated scope (e.g. "all", "gha", "python,node").
    #[arg(long, default_value = "all")]
    scope: String,

    /// Minimum severity to report: Critical, High, Medium, or Info.
    #[arg(long = "min-severity", default_value = "Info")]
    min_severity: String,

    /// Emit JSON instead of markdown.
    #[arg(long)]
    json: bool,

    /// Render only the summary + dimension status (markdown).
    #[arg(long = "summary-only")]
    summary_only: bool,

    /// Emit the SBOM write advisory in the report.
    #[arg(long = "generate-sbom")]
    generate_sbom: bool,

    /// Report external tool availability and exit.
    #[arg(long = "check-tools")]
    check_tools: bool,
}

fn parse_severity(s: &str) -> Result<Severity, String> {
    Severity::from_str(s).map_err(|_| {
        format!("invalid --min-severity '{s}' (expected Critical, High, Medium, or Info)")
    })
}

fn exit_code_for(err: &SupplyChainAuditError) -> u8 {
    match err.error_code() {
        "PATH_TRAVERSAL" => 3,
        "INVALID_SCOPE" => 4,
        "ACCEPTED_RISKS_OVERFLOW" => 5,
        _ => 1,
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if cli.check_tools {
        let missing = check_missing_tools();
        if missing.is_empty() {
            println!("All external tools are available.");
        } else {
            println!("Missing tools ({}):", missing.len());
            for tool in missing {
                println!("- {}: {}", tool.name, tool.description);
                for opt in tool.install_options {
                    println!("    {opt}");
                }
            }
        }
        return ExitCode::SUCCESS;
    }

    let min_severity = match parse_severity(&cli.min_severity) {
        Ok(s) => s,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::from(2);
        }
    };

    let config = AuditConfig::new(cli.path)
        .with_scope(cli.scope)
        .with_min_severity(min_severity)
        .with_generate_sbom(cli.generate_sbom);

    match run_audit(&config) {
        Ok(result) => {
            if cli.json {
                println!("{}", result.to_json());
            } else if cli.summary_only {
                println!("{}", result.render_report_summary_only());
            } else {
                println!("{}", result.render_report());
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(exit_code_for(&err))
        }
    }
}
