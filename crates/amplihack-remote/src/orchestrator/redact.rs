//! Sensitive-value redaction for azlin/Azure CLI diagnostics.
//!
//! Kept in its own submodule so `orchestrator` stays within the
//! `amplihack-remote` module-size budget (issue #536). Scrubs Azure secrets
//! (SAS token values, canonical GUIDs) from arbitrary text before it is folded
//! into a log line or [`crate::error::RemoteError`].

use std::sync::LazyLock;

use regex::Regex;

/// Placeholder substituted for any redacted sensitive segment.
const REDACTION_PLACEHOLDER: &str = "[REDACTED]";

/// GUIDs (subscription IDs, tenant IDs, resource GUIDs) in canonical
/// `8-4-4-4-12` hex form.
static GUID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
        .expect("valid GUID regex")
});

/// Azure Storage SAS query parameters (`sig`, `se`, `sp`, `sv`, `st`, `sr`,
/// `ss`, `srt`, `skoid`, `sktid`, ...). Redacts the *value* while keeping the
/// parameter name so the error stays diagnosable.
static SAS_PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)([?&](?:sig|se|sp|sv|st|sr|ss|srt|spr|skoid|sktid|skt|ske|sks|skv)=)[^&\s]+")
        .expect("valid SAS param regex")
});

/// Scrub known-sensitive Azure identifiers from arbitrary text (typically
/// `azlin`/Azure CLI stderr) before it is folded into a log line or
/// [`crate::error::RemoteError`]. Redacts SAS token query parameters and
/// canonical GUIDs (subscription/tenant/resource IDs), replacing each with
/// [`REDACTION_PLACEHOLDER`]. Benign error text is preserved verbatim.
pub(super) fn redact_sensitive(input: &str) -> String {
    // SAS params first so the placeholder does not disturb GUID matching.
    let sas_scrubbed = SAS_PARAM_RE.replace_all(input, |caps: &regex::Captures<'_>| {
        format!("{}{REDACTION_PLACEHOLDER}", &caps[1])
    });
    GUID_RE
        .replace_all(&sas_scrubbed, REDACTION_PLACEHOLDER)
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_sensitive_scrubs_subscription_guid() {
        let stderr = "ERROR: Subscription 12345678-90ab-cdef-1234-567890abcdef not found";
        let out = redact_sensitive(stderr);
        assert!(
            !out.contains("12345678-90ab-cdef-1234-567890abcdef"),
            "GUID leaked: {out}"
        );
        assert!(out.contains(REDACTION_PLACEHOLDER));
        // Benign surrounding text is preserved.
        assert!(out.contains("Subscription"));
        assert!(out.contains("not found"));
    }

    #[test]
    fn redact_sensitive_scrubs_sas_query_params() {
        let stderr = "failed to reach https://acct.blob.core.windows.net/c/b?sv=2021-08-06&se=2025-01-01T00:00:00Z&sr=b&sp=r&sig=abc123DEF456secretsignature%3D";
        let out = redact_sensitive(stderr);
        assert!(
            !out.contains("abc123DEF456secretsignature"),
            "SAS sig leaked: {out}"
        );
        assert!(!out.contains("2021-08-06"), "SAS sv value leaked: {out}");
        // Parameter names are retained for diagnosability.
        assert!(out.contains("sig="));
        assert!(out.contains("sv="));
        assert!(out.contains(REDACTION_PLACEHOLDER));
        // Host/path (non-secret) is preserved.
        assert!(out.contains("acct.blob.core.windows.net"));
    }

    #[test]
    fn redact_sensitive_preserves_benign_text() {
        let stderr = "azlin kill: VM 'sess-vm-1' is already deleted (no-op)";
        let out = redact_sensitive(stderr);
        assert_eq!(out, stderr);
        assert!(!out.contains(REDACTION_PLACEHOLDER));
    }

    #[test]
    fn redact_sensitive_handles_multiple_secrets() {
        let stderr = "sub 11111111-2222-3333-4444-555555555555 sas ?sig=topsecret&sp=racwd";
        let out = redact_sensitive(stderr);
        assert!(!out.contains("11111111-2222-3333-4444-555555555555"));
        assert!(!out.contains("topsecret"));
        assert!(!out.contains("racwd"));
        assert_eq!(out.matches(REDACTION_PLACEHOLDER).count(), 3);
    }
}
