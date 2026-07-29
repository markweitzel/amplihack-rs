//! Unit tests — per-dimension pattern detection.
//!
//! Ported from upstream `tests/unit/test_pattern_detection.py`. Each test
//! exercises a single dimension's checker and FAILs until it is implemented.

mod common;

use amplihack_supply_chain_audit::{
    Finding, Severity, check_action_sha_pinning, check_cache_poisoning, check_cargo_supply_chain,
    check_container_image_pinning, check_credential_hygiene, check_docker_build_chain,
    check_go_module_integrity, check_node_integrity, check_nuget_lock, check_python_integrity,
    check_secret_exposure, check_workflow_permissions,
};
use common::{temp_repo, write_file};

fn has_severity(findings: &[Finding], sev: Severity) -> bool {
    findings.iter().any(|f| f.severity() == sev)
}

// ── Dimension 1: Action SHA pinning ─────────────────────────────────────────

#[test]
fn dim1_semver_tag_detected_as_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n\
             steps:\n      - uses: actions/checkout@v4\n",
    );
    let findings = check_action_sha_pinning(repo.path());
    assert!(!findings.is_empty());
    let f = &findings[0];
    assert_eq!(f.severity(), Severity::High);
    assert!(f.current_value().contains("checkout@v4"));
    assert!(f.offline_detectable());
}

#[test]
fn dim1_branch_ref_detected_as_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n\
             steps:\n      - uses: my-org/my-action@main\n",
    );
    let findings = check_action_sha_pinning(repo.path());
    assert!(findings.iter().any(|f| f.current_value().contains("@main")));
}

#[test]
fn dim1_full_sha_with_comment_is_clean() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\npermissions: read-all\njobs:\n  build:\n\
             runs-on: ubuntu-latest\n    steps:\n\
               - uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683  # v4.2.2\n",
    );
    assert!(check_action_sha_pinning(repo.path()).is_empty());
}

#[test]
fn dim1_pull_request_target_with_unpinned_action_is_critical() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push, pull_request_target]\njobs:\n  test:\n\
             runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n",
    );
    let findings = check_action_sha_pinning(repo.path());
    assert!(has_severity(&findings, Severity::Critical));
}

#[test]
fn dim1_sha_without_version_comment_not_high_or_critical() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\npermissions: read-all\njobs:\n  build:\n\
             runs-on: ubuntu-latest\n    steps:\n\
               - uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683\n",
    );
    let findings = check_action_sha_pinning(repo.path());
    assert!(
        !findings
            .iter()
            .any(|f| matches!(f.severity(), Severity::High | Severity::Critical))
    );
}

#[test]
fn dim1_multiple_workflows_checked() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n\
             steps:\n      - uses: actions/checkout@v4\n",
    );
    write_file(
        repo.path(),
        ".github/workflows/release.yml",
        "name: Release\non: [push]\njobs:\n  release:\n    runs-on: ubuntu-latest\n\
             steps:\n      - uses: actions/upload-artifact@v3\n",
    );
    let findings = check_action_sha_pinning(repo.path());
    assert!(findings.iter().any(|f| f.file().contains("ci.yml")));
    assert!(findings.iter().any(|f| f.file().contains("release.yml")));
}

#[test]
fn dim1_finding_includes_sha_lookup_url() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n\
             steps:\n      - uses: actions/checkout@v4\n",
    );
    let findings = check_action_sha_pinning(repo.path());
    let fix = findings[0].fix_url().expect("fix_url present");
    assert!(fix.contains("github.com"));
}

// ── Dimension 2: Workflow permissions ───────────────────────────────────────

#[test]
fn dim2_missing_permissions_key_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n\
             steps:\n      - run: echo hello\n",
    );
    assert!(has_severity(
        &check_workflow_permissions(repo.path()),
        Severity::High
    ));
}

#[test]
fn dim2_permissions_write_all_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\npermissions: write-all\njobs:\n  build:\n\
             runs-on: ubuntu-latest\n    steps:\n      - run: echo hello\n",
    );
    let findings = check_workflow_permissions(repo.path());
    assert!(has_severity(&findings, Severity::High));
    assert!(
        findings
            .iter()
            .any(|f| f.current_value().contains("write-all"))
    );
}

#[test]
fn dim2_pull_request_target_without_permissions_is_critical() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push, pull_request_target]\njobs:\n  test:\n\
             runs-on: ubuntu-latest\n    steps:\n      - run: echo hello\n",
    );
    assert!(has_severity(
        &check_workflow_permissions(repo.path()),
        Severity::Critical
    ));
}

#[test]
fn dim2_permissions_read_all_is_clean() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\npermissions: read-all\njobs:\n  build:\n\
             runs-on: ubuntu-latest\n    steps:\n      - run: echo hello\n",
    );
    assert!(check_workflow_permissions(repo.path()).is_empty());
}

// ── Dimension 3: Secret exposure ────────────────────────────────────────────

#[test]
fn dim3_echo_secret_is_critical() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\njobs:\n  test:\n    runs-on: ubuntu-latest\n\
             steps:\n      - run: echo \"Token=${{ secrets.API_TOKEN }}\"\n",
    );
    assert!(has_severity(
        &check_secret_exposure(repo.path()),
        Severity::Critical
    ));
}

#[test]
fn dim3_print_secret_is_critical() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\njobs:\n  test:\n    runs-on: ubuntu-latest\n\
             steps:\n      - run: python -c \"print('${{ secrets.KEY }}')\"\n",
    );
    assert!(has_severity(
        &check_secret_exposure(repo.path()),
        Severity::Critical
    ));
}

#[test]
fn dim3_secret_not_echoed_is_clean() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\npermissions: read-all\njobs:\n  test:\n\
             runs-on: ubuntu-latest\n    steps:\n\
               - uses: some/action@<sha>  # v1\n\
                 with:\n          token: ${{ secrets.GITHUB_TOKEN }}\n",
    );
    assert!(check_secret_exposure(repo.path()).is_empty());
}

#[test]
fn dim3_secret_in_cache_key_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\npermissions: read-all\njobs:\n  test:\n\
             runs-on: ubuntu-latest\n    steps:\n\
               - uses: actions/cache@<sha>  # v3\n\
                 with:\n\
                   key: ${{ runner.os }}-${{ secrets.CACHE_SECRET }}\n",
    );
    assert!(has_severity(
        &check_secret_exposure(repo.path()),
        Severity::High
    ));
}

// ── Dimension 4: Cache poisoning ────────────────────────────────────────────

#[test]
fn dim4_cache_key_without_hashfiles_is_medium() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n\
             steps:\n      - uses: actions/cache@v3\n\
                 with:\n          key: ${{ runner.os }}-pip\n          path: ~/.cache/pip\n",
    );
    let findings = check_cache_poisoning(repo.path());
    assert!(
        findings
            .iter()
            .any(|f| f.dimension() == 4 && f.severity() == Severity::Medium)
    );
}

#[test]
fn dim4_cache_key_with_hashfiles_is_clean() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n\
             steps:\n      - uses: actions/cache@v3\n\
                 with:\n          key: ${{ runner.os }}-pip-${{ hashFiles('**/requirements*.txt') }}\n\
                   path: ~/.cache/pip\n",
    );
    let findings = check_cache_poisoning(repo.path());
    assert!(
        !findings
            .iter()
            .any(|f| f.dimension() == 4 && f.severity() == Severity::Medium)
    );
}

#[test]
fn dim4_no_workflows_returns_empty() {
    let repo = temp_repo();
    assert!(check_cache_poisoning(repo.path()).is_empty());
}

// ── Dimension 5: Container image pinning ────────────────────────────────────

#[test]
fn dim5_latest_tag_is_critical() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "Dockerfile",
        "FROM alpine:latest\nRUN echo hello\n",
    );
    let findings = check_container_image_pinning(repo.path());
    assert!(has_severity(&findings, Severity::Critical));
    assert!(
        findings
            .iter()
            .any(|f| f.current_value().contains(":latest"))
    );
}

#[test]
fn dim5_semver_tag_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "Dockerfile",
        "FROM golang:1.22-alpine AS builder\n",
    );
    assert!(has_severity(
        &check_container_image_pinning(repo.path()),
        Severity::High
    ));
}

#[test]
fn dim5_sha_digest_is_clean() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "Dockerfile",
        "FROM ubuntu@sha256:a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2\n",
    );
    assert!(check_container_image_pinning(repo.path()).is_empty());
}

#[test]
fn dim5_multi_stage_all_stages_checked() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "Dockerfile",
        "FROM golang:1.22-alpine AS builder\nFROM alpine:latest\nCOPY --from=builder /app /app\n",
    );
    assert!(check_container_image_pinning(repo.path()).len() >= 2);
}

#[test]
fn dim5_scratch_base_is_clean() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "Dockerfile",
        "FROM golang:1.22@sha256:abc123 AS builder\nFROM scratch\nCOPY --from=builder /app /app\n",
    );
    let findings = check_container_image_pinning(repo.path());
    assert!(
        !findings
            .iter()
            .any(|f| f.current_value().contains("scratch"))
    );
}

// ── Dimension 8: Python integrity ───────────────────────────────────────────

#[test]
fn dim8_requirements_without_hashes_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "requirements.txt",
        "requests==2.31.0\nflask==3.0.3\ngunicorn==22.0.0\n",
    );
    assert!(has_severity(
        &check_python_integrity(repo.path()),
        Severity::High
    ));
}

#[test]
fn dim8_requirements_with_hashes_is_clean() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "requirements.txt",
        "requests==2.31.0 \\\n    --hash=sha256:58cd2187423839c35a28f8b84a8f7db7e6bd2c9d\\\n\
             --hash=sha256:fc7f50f5c0e5d7b2a1c3e7e08e6f7f3b2a1c3e7e\n",
    );
    let findings = check_python_integrity(repo.path());
    assert!(
        !findings
            .iter()
            .any(|f| matches!(f.severity(), Severity::High | Severity::Critical))
    );
}

#[test]
fn dim8_extra_index_url_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "requirements.txt",
        "--extra-index-url https://pypi.evil.com/simple/\nrequests==2.31.0\n",
    );
    let findings = check_python_integrity(repo.path());
    assert!(
        findings
            .iter()
            .any(|f| f.current_value().contains("extra-index-url"))
    );
    assert!(
        findings
            .iter()
            .any(|f| matches!(f.severity(), Severity::High | Severity::Critical))
    );
}

#[test]
fn dim8_pip_install_without_require_hashes_in_workflow_is_medium() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\npermissions: read-all\njobs:\n  test:\n\
             runs-on: ubuntu-latest\n    steps:\n      - run: pip install -r requirements.txt\n",
    );
    let findings = check_python_integrity(repo.path());
    let medium: Vec<_> = findings
        .iter()
        .filter(|f| f.severity() == Severity::Medium)
        .collect();
    assert!(!medium.is_empty());
    assert!(
        medium
            .iter()
            .any(|f| f.rationale().contains("require-hashes")
                || f.expected_value().contains("require-hashes"))
    );
}

// ── Dimension 10: Node integrity ────────────────────────────────────────────

#[test]
fn dim10_npm_install_in_workflow_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\npermissions: read-all\njobs:\n  build:\n\
             runs-on: ubuntu-latest\n    steps:\n      - run: npm install\n",
    );
    write_file(repo.path(), "package.json", "{\"name\": \"app\"}\n");
    let findings = check_node_integrity(repo.path());
    assert!(
        findings
            .iter()
            .any(|f| f.current_value().contains("npm install"))
    );
    assert!(has_severity(&findings, Severity::High));
}

#[test]
fn dim10_npm_ci_is_clean() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\npermissions: read-all\njobs:\n  build:\n\
             runs-on: ubuntu-latest\n    steps:\n      - run: npm ci\n",
    );
    write_file(repo.path(), "package.json", "{\"name\": \"app\"}\n");
    write_file(
        repo.path(),
        "package-lock.json",
        "{\"lockfileVersion\": 3}\n",
    );
    let findings = check_node_integrity(repo.path());
    assert!(
        !findings
            .iter()
            .any(|f| matches!(f.severity(), Severity::High | Severity::Critical))
    );
}

#[test]
fn dim10_missing_lock_file_is_high() {
    let repo = temp_repo();
    write_file(repo.path(), "package.json", "{\"name\": \"app\"}\n");
    let findings = check_node_integrity(repo.path());
    assert!(has_severity(&findings, Severity::High));
    assert!(findings.iter().any(|f| f.line() == 0));
}

#[test]
fn dim10_unversioned_npx_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "package.json",
        "{\"name\": \"app\", \"scripts\": {\"build\": \"npx webpack --config webpack.config.js\"}}\n",
    );
    let findings = check_node_integrity(repo.path());
    assert!(
        findings
            .iter()
            .any(|f| f.current_value().contains("npx webpack"))
    );
    assert!(has_severity(&findings, Severity::High));
}

#[test]
fn dim10_versioned_npx_is_clean() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "package.json",
        "{\"name\": \"app\", \"scripts\": {\"build\": \"npx webpack@5.91.0 --config webpack.config.js\"}}\n",
    );
    let findings = check_node_integrity(repo.path());
    assert!(!findings.iter().any(|f| f.current_value().contains("npx")
        && matches!(f.severity(), Severity::High | Severity::Critical)));
}

// ── Dimension 11: Go module integrity ───────────────────────────────────────

#[test]
fn dim11_missing_go_sum_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "go.mod",
        "module github.com/org/app\n\ngo 1.22\n\nrequire github.com/pkg/errors v0.9.1\n",
    );
    assert!(has_severity(
        &check_go_module_integrity(repo.path()),
        Severity::High
    ));
}

#[test]
fn dim11_go_sum_present_is_clean() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "go.mod",
        "module github.com/org/app\n\ngo 1.22\n\nrequire github.com/pkg/errors v0.9.1\n",
    );
    write_file(
        repo.path(),
        "go.sum",
        "github.com/pkg/errors v0.9.1 h1:sIXre2Sh2E82tnWo9BZFQ3NZ48ZVKYSV94FtHI/r8+Q=\n",
    );
    let findings = check_go_module_integrity(repo.path());
    assert!(
        !findings
            .iter()
            .any(|f| matches!(f.severity(), Severity::High | Severity::Critical))
    );
}

#[test]
fn dim11_replace_with_mutable_branch_is_medium() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "go.mod",
        "module github.com/org/app\n\ngo 1.22\n\n\
         require github.com/some/package v1.0.0\n\n\
         replace github.com/some/package => github.com/myorg/fork main\n",
    );
    write_file(repo.path(), "go.sum", "# empty\n");
    let findings = check_go_module_integrity(repo.path());
    assert!(has_severity(&findings, Severity::Medium));
    assert!(
        findings
            .iter()
            .any(|f| f.current_value().contains("replace"))
    );
}

#[test]
fn dim11_gonosumcheck_bypass_is_flagged() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "go.mod",
        "module github.com/org/app\n\ngo 1.22\n",
    );
    write_file(repo.path(), "go.sum", "# empty\n");
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\npermissions: read-all\njobs:\n  test:\n\
             runs-on: ubuntu-latest\n    env:\n      GONOSUMCHECK: '*'\n\
             steps:\n      - run: go build ./...\n",
    );
    let findings = check_go_module_integrity(repo.path());
    assert!(
        findings
            .iter()
            .any(|f| f.current_value().contains("GONOSUMCHECK"))
    );
}

// ── Dimension 9: Cargo supply chain ─────────────────────────────────────────

#[test]
fn dim9_cargo_lock_in_gitignore_for_binary_is_medium() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "Cargo.toml",
        "[package]\nname = 'mytool'\nversion = '0.1.0'\n[[bin]]\nname = 'mytool'\npath = 'src/main.rs'\n",
    );
    write_file(repo.path(), ".gitignore", "target/\nCargo.lock\n");
    let findings = check_cargo_supply_chain(repo.path());
    assert!(has_severity(&findings, Severity::Medium));
    assert!(
        findings
            .iter()
            .any(|f| f.current_value().contains("Cargo.lock")
                || f.rationale().contains("Cargo.lock"))
    );
}

#[test]
fn dim9_cargo_lock_committed_for_binary_is_clean() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "Cargo.toml",
        "[package]\nname = 'mytool'\nversion = '0.1.0'\n[[bin]]\nname = 'mytool'\npath = 'src/main.rs'\n",
    );
    write_file(repo.path(), "Cargo.lock", "# Cargo.lock\n");
    let findings = check_cargo_supply_chain(repo.path());
    assert!(
        !findings
            .iter()
            .any(|f| f.current_value().contains("Cargo.lock")
                && matches!(f.severity(), Severity::High | Severity::Critical))
    );
}

#[test]
fn dim9_build_rs_present_triggers_info_finding() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "Cargo.toml",
        "[package]\nname = 'app'\nversion = '0.1.0'\n",
    );
    write_file(repo.path(), "Cargo.lock", "# Cargo.lock\n");
    write_file(
        repo.path(),
        "build.rs",
        "fn main() { println!(\"cargo:rerun-if-changed=build.rs\"); }\n",
    );
    let findings = check_cargo_supply_chain(repo.path());
    assert!(findings.iter().any(|f| f.file().contains("build.rs")));
}

#[test]
fn dim9_patch_section_with_git_source_is_medium() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "Cargo.toml",
        "[package]\nname = 'app'\nversion = '0.1.0'\n\
         [patch.crates-io]\nserde = { git = 'https://github.com/serde-rs/serde', branch = 'master' }\n",
    );
    write_file(repo.path(), "Cargo.lock", "# Cargo.lock\n");
    let findings = check_cargo_supply_chain(repo.path());
    assert!(
        findings
            .iter()
            .any(|f| f.current_value().to_lowercase().contains("patch")
                || f.rationale().to_lowercase().contains("patch"))
    );
}

// ── Dimension 7: NuGet lock ─────────────────────────────────────────────────

#[test]
fn dim7_csproj_without_lock_file_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "App.csproj",
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n\
             <TargetFramework>net8.0</TargetFramework>\n  </PropertyGroup>\n</Project>\n",
    );
    let findings = check_nuget_lock(repo.path());
    assert!(has_severity(&findings, Severity::High));
    assert!(findings.iter().any(|f| f.line() == 0));
}

#[test]
fn dim7_nuget_config_without_package_source_mapping_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "NuGet.Config",
        "<configuration>\n  <packageSources>\n\
             <add key='internal' value='https://pkgs.dev.azure.com/org/feed/nuget/v3/index.json' />\n\
             <add key='nuget.org' value='https://api.nuget.org/v3/index.json' />\n\
           </packageSources>\n</configuration>\n",
    );
    let findings = check_nuget_lock(repo.path());
    assert!(
        findings
            .iter()
            .any(|f| f.rationale().contains("packageSourceMapping")
                || f.expected_value().contains("packageSourceMapping"))
    );
    assert!(has_severity(&findings, Severity::High));
}

#[test]
fn dim7_nuget_config_with_clear_and_mapping_is_clean() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "NuGet.Config",
        "<configuration>\n  <packageSources>\n    <clear />\n\
             <add key='internal' value='https://pkgs.dev.azure.com/org/feed/nuget/v3/index.json' />\n\
           </packageSources>\n  <packageSourceMapping>\n\
             <packageSource key='internal'>\n      <package pattern='*' />\n\
             </packageSource>\n  </packageSourceMapping>\n</configuration>\n",
    );
    let findings = check_nuget_lock(repo.path());
    assert!(
        !findings
            .iter()
            .any(|f| matches!(f.severity(), Severity::High | Severity::Critical))
    );
}

// ── Dimension 6: Credential hygiene ─────────────────────────────────────────

#[test]
fn dim6_workflow_with_hardcoded_aws_keys_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/deploy.yml",
        "name: Deploy\non: [push]\njobs:\n  deploy:\n    runs-on: ubuntu-latest\n\
             steps:\n      - uses: aws-actions/configure-aws-credentials@v4\n\
                 with:\n          aws-access-key-id: ${{ secrets.AWS_KEY }}\n\
                   aws-secret-access-key: ${{ secrets.AWS_SECRET }}\n",
    );
    let findings = check_credential_hygiene(repo.path());
    assert!(has_severity(&findings, Severity::High));
    assert!(findings.iter().any(|f| f.current_value().contains("AWS")));
}

#[test]
fn dim6_workflow_using_oidc_is_clean() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/deploy.yml",
        "name: Deploy\non: [push]\npermissions:\n  id-token: write\n\
         jobs:\n  deploy:\n    runs-on: ubuntu-latest\n\
             steps:\n      - uses: aws-actions/configure-aws-credentials@v4\n\
                 with:\n          role-to-assume: arn:aws:iam::123456789:role/deploy\n",
    );
    let findings = check_credential_hygiene(repo.path());
    assert!(
        !findings
            .iter()
            .any(|f| matches!(f.severity(), Severity::High | Severity::Critical))
    );
}

#[test]
fn dim6_workflow_with_azure_static_creds_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/deploy.yml",
        "name: Deploy\non: [push]\njobs:\n  deploy:\n    runs-on: ubuntu-latest\n\
             steps:\n      - uses: azure/login@v2\n\
                 with:\n          creds: ${{ secrets.AZURE_CREDENTIALS }}\n",
    );
    assert!(has_severity(
        &check_credential_hygiene(repo.path()),
        Severity::High
    ));
}

// ── Dimension 12: Docker build chain ────────────────────────────────────────

#[test]
fn dim12_multi_stage_no_user_in_final_stage_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "Dockerfile",
        "FROM golang:1.22-alpine AS builder\nWORKDIR /app\nRUN go build -o /app/main .\n\
         FROM alpine:3.19\nCOPY --from=builder /app/main /main\nENTRYPOINT [\"/main\"]\n",
    );
    let findings = check_docker_build_chain(repo.path());
    assert!(has_severity(&findings, Severity::High));
    assert!(
        findings
            .iter()
            .any(|f| f.rationale().contains("USER") || f.rationale().contains("root"))
    );
}

#[test]
fn dim12_dockerfile_with_user_in_final_stage_is_clean() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "Dockerfile",
        "FROM golang:1.22-alpine AS builder\nWORKDIR /app\nRUN go build -o /app/main .\n\
         FROM alpine:3.19\nRUN addgroup -S appgroup && adduser -S appuser -G appgroup\n\
         USER appuser\nCOPY --from=builder /app/main /main\nENTRYPOINT [\"/main\"]\n",
    );
    let findings = check_docker_build_chain(repo.path());
    assert!(
        !findings
            .iter()
            .any(|f| matches!(f.severity(), Severity::High | Severity::Critical))
    );
}

#[test]
fn dim12_single_stage_no_user_is_high() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "Dockerfile",
        "FROM python:3.12-slim\nWORKDIR /app\nCOPY . .\nCMD [\"python\", \"app.py\"]\n",
    );
    assert!(has_severity(
        &check_docker_build_chain(repo.path()),
        Severity::High
    ));
}
