//! Dimension 6: credential hygiene — static credential detection / OIDC migration.

use super::utils::{Counters, build, mk, relative_path};
use crate::schema::{Finding, Severity};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::Path;

static AWS_KEY_ID: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)aws-access-key-id\s*:").unwrap());
static AWS_SECRET_KEY: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)aws-secret-access-key\s*:").unwrap());
static AZURE_CREDS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)(creds|credentials)\s*:\s*\$\{\{.*secrets\.").unwrap());

/// Dim 6: detect static credentials that should migrate to OIDC federation.
pub fn check_credential_hygiene(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut counters = Counters::new();

    for (wf_path, content) in super::utils::load_workflows(root) {
        let rel = relative_path(root, &wf_path);
        let lines: Vec<&str> = content.lines().collect();

        let mut has_aws_key_id = false;
        let mut aws_key_id_line = -1i64;
        let mut has_aws_secret = false;
        for (idx, line) in lines.iter().enumerate() {
            if AWS_KEY_ID.is_match(line) {
                has_aws_key_id = true;
                aws_key_id_line = (idx + 1) as i64;
            }
            if AWS_SECRET_KEY.is_match(line) {
                has_aws_secret = true;
            }
        }

        if has_aws_key_id && has_aws_secret {
            findings.push(build(mk(
                &mut counters,
                Severity::High,
                6,
                &rel,
                aws_key_id_line,
                "AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY (static credentials)",
                "Use OIDC federation:\n  permissions:\n    id-token: write\n  \
                 - uses: aws-actions/configure-aws-credentials@<sha>\n    with:\n      \
                 role-to-assume: arn:aws:iam::ACCOUNT:role/ROLE",
                "Static AWS credentials in secrets can be leaked, rotated infrequently, \
                 and grant persistent access. Use OIDC federation for short-lived tokens.",
            )));
        }

        for (idx, line) in lines.iter().enumerate() {
            if AZURE_CREDS.is_match(line) {
                findings.push(build(
                    mk(
                        &mut counters,
                        Severity::High,
                        6,
                        &rel,
                        (idx + 1) as i64,
                        line.trim(),
                        "Use Azure federated identity (OIDC) instead of service principal JSON key",
                        "Azure service principal JSON credentials are long-lived. \
                         Use federated identity with OIDC for short-lived, keyless authentication.",
                    )
                    .contains_secret(true),
                ));
            }
        }
    }

    findings
}
