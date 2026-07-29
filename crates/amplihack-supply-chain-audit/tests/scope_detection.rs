//! Unit tests — ecosystem scope detection.
//!
//! Ported from upstream `tests/unit/test_scope_detection.py`.

mod common;

use amplihack_supply_chain_audit::detect_ecosystems;
use common::{temp_repo, write_file};

fn active(root: &std::path::Path, scope: &str) -> Vec<u32> {
    detect_ecosystems(root, scope)
        .expect("detection should succeed")
        .active_dimensions()
        .to_vec()
}

// ── Ecosystem detection ─────────────────────────────────────────────────────

#[test]
fn no_files_returns_empty_scope() {
    let repo = temp_repo();
    let scope = detect_ecosystems(repo.path(), "all").unwrap();
    assert_eq!(scope.active_dimensions(), &[] as &[u32]);
    assert_eq!(
        scope.skipped_dimensions(),
        &(1..=12).collect::<Vec<u32>>()[..]
    );
}

#[test]
fn github_workflows_triggers_dims_1_to_4() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", "name: CI\n");
    let dims = active(repo.path(), "all");
    for d in [1, 2, 3, 4] {
        assert!(dims.contains(&d), "expected dim {d} active");
    }
}

#[test]
fn dockerfile_triggers_dims_5_and_12() {
    let repo = temp_repo();
    write_file(repo.path(), "Dockerfile", "FROM ubuntu:22.04\n");
    let dims = active(repo.path(), "all");
    assert!(dims.contains(&5) && dims.contains(&12));
}

#[test]
fn docker_compose_triggers_dims_5_and_12() {
    let repo = temp_repo();
    write_file(repo.path(), "docker-compose.yml", "version: '3'\n");
    let dims = active(repo.path(), "all");
    assert!(dims.contains(&5) && dims.contains(&12));
}

#[test]
fn workflow_with_secrets_triggers_dim_6() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/deploy.yml",
        "name: Deploy\nsteps:\n  - run: echo ${{ secrets.TOKEN }}\n",
    );
    assert!(active(repo.path(), "all").contains(&6));
}

#[test]
fn csproj_triggers_dim_7() {
    let repo = temp_repo();
    write_file(repo.path(), "App.csproj", "<Project />\n");
    assert!(active(repo.path(), "all").contains(&7));
}

#[test]
fn nuget_config_triggers_dim_7() {
    let repo = temp_repo();
    write_file(repo.path(), "NuGet.Config", "<configuration />\n");
    assert!(active(repo.path(), "all").contains(&7));
}

#[test]
fn requirements_txt_triggers_dim_8() {
    let repo = temp_repo();
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    assert!(active(repo.path(), "all").contains(&8));
}

#[test]
fn pyproject_toml_triggers_dim_8() {
    let repo = temp_repo();
    write_file(repo.path(), "pyproject.toml", "[project]\nname = 'app'\n");
    assert!(active(repo.path(), "all").contains(&8));
}

#[test]
fn setup_cfg_triggers_dim_8() {
    let repo = temp_repo();
    write_file(repo.path(), "setup.cfg", "[metadata]\nname = app\n");
    assert!(active(repo.path(), "all").contains(&8));
}

#[test]
fn cargo_toml_triggers_dim_9() {
    let repo = temp_repo();
    write_file(repo.path(), "Cargo.toml", "[package]\nname = 'app'\n");
    assert!(active(repo.path(), "all").contains(&9));
}

#[test]
fn package_json_triggers_dim_10() {
    let repo = temp_repo();
    write_file(repo.path(), "package.json", "{\"name\": \"app\"}\n");
    assert!(active(repo.path(), "all").contains(&10));
}

#[test]
fn package_lock_json_triggers_dim_10() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "package-lock.json",
        "{\"lockfileVersion\": 3}\n",
    );
    assert!(active(repo.path(), "all").contains(&10));
}

#[test]
fn go_mod_triggers_dim_11() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        "go.mod",
        "module github.com/org/repo\n\ngo 1.22\n",
    );
    assert!(active(repo.path(), "all").contains(&11));
}

#[test]
fn go_sum_triggers_dim_11() {
    let repo = temp_repo();
    write_file(repo.path(), "go.sum", "# empty\n");
    assert!(active(repo.path(), "all").contains(&11));
}

#[test]
fn multiple_ecosystems_trigger_all_dimensions() {
    let repo = temp_repo();
    write_file(repo.path(), "Dockerfile", "FROM ubuntu:22.04\n");
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    write_file(repo.path(), "package.json", "{\"name\": \"app\"}\n");
    write_file(
        repo.path(),
        "go.mod",
        "module github.com/org/app\n\ngo 1.22\n",
    );
    write_file(repo.path(), "Cargo.toml", "[package]\nname = 'app'\n");
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\nenv:\n  TOKEN: ${{ secrets.GH_TOKEN }}\n",
    );
    let mut dims = active(repo.path(), "all");
    dims.sort_unstable();
    assert_eq!(dims, (1..=12).collect::<Vec<u32>>());
}

#[test]
fn workflow_without_secrets_does_not_trigger_dim6() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\n",
    );
    assert!(!active(repo.path(), "all").contains(&6));
}

// ── Scope filtering ─────────────────────────────────────────────────────────

#[test]
fn scope_gha_restricts_to_dims_1_to_4() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", "name: CI\n");
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    let mut dims = active(repo.path(), "gha");
    dims.sort_unstable();
    assert_eq!(dims, vec![1, 2, 3, 4]);
    assert!(!dims.contains(&8));
}

#[test]
fn scope_containers_restricts_to_dims_5_12() {
    let repo = temp_repo();
    write_file(repo.path(), "Dockerfile", "FROM ubuntu:22.04\n");
    let mut dims = active(repo.path(), "containers");
    dims.sort_unstable();
    assert_eq!(dims, vec![5, 12]);
}

#[test]
fn scope_all_enables_all_detected_dimensions() {
    let repo = temp_repo();
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    assert!(active(repo.path(), "all").contains(&8));
}

#[test]
fn invalid_scope_raises_invalid_scope_error() {
    let repo = temp_repo();
    let err = detect_ecosystems(repo.path(), "terraform").unwrap_err();
    assert_eq!(err.error_code(), "INVALID_SCOPE");
    assert!(err.to_string().contains("terraform"), "message: {err}");
}

#[test]
fn scope_semicolon_injection_rejected() {
    let repo = temp_repo();
    let err = detect_ecosystems(repo.path(), "gha; rm -rf /").unwrap_err();
    assert_eq!(err.error_code(), "INVALID_SCOPE");
}

#[test]
fn scope_pipe_injection_rejected() {
    let repo = temp_repo();
    let err = detect_ecosystems(repo.path(), "gha | cat /etc/passwd").unwrap_err();
    assert_eq!(err.error_code(), "INVALID_SCOPE");
}

// ── Skipped-dimension reporting ─────────────────────────────────────────────

#[test]
fn skipped_dimensions_annotated_with_reason() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", "name: CI\n");
    let scope = detect_ecosystems(repo.path(), "all").unwrap();
    assert!(scope.skipped_dimensions().contains(&5));
    let reason = scope.get_skip_reason(5).to_lowercase();
    assert!(reason.contains("docker"), "reason: {reason}");
}

#[test]
fn empty_repo_lists_all_12_as_skipped() {
    let repo = temp_repo();
    let scope = detect_ecosystems(repo.path(), "all").unwrap();
    assert_eq!(scope.skipped_dimensions().len(), 12);
    assert_eq!(scope.active_dimensions().len(), 0);
}
