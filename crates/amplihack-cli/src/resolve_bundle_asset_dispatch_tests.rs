#[cfg(test)]
mod cli_dispatch_tests {
    //! Verify the `amplihack resolve-bundle-asset <asset>` clap subcommand
    //! parses correctly and that recipes don't regress to the old legacy
    //! runtime-asset invocation.
    use crate::{Cli, Commands};
    #[test]
    fn parses_named_asset_argument() {
        let cli = Cli::try_parse_from([
            "amplihack",
            "resolve-bundle-asset",
            "multitask-orchestrator",
        ])
        .unwrap();
        match cli.command {
            Commands::ResolveBundleAsset { asset } => assert_eq!(asset, "multitask-orchestrator"),
            other => panic!("expected ResolveBundleAsset, got {other:?}"),
        }
    }

    #[test]
    fn parses_relative_path_argument() {
        let cli = Cli::try_parse_from([
            "amplihack",
            "resolve-bundle-asset",
            "amplifier-bundle/tools/statusline.sh",
        ])
        .unwrap();
        match cli.command {
            Commands::ResolveBundleAsset { asset } => {
                assert_eq!(asset, "amplifier-bundle/tools/statusline.sh")
            }
            other => panic!("expected ResolveBundleAsset, got {other:?}"),
        }
    }

    #[test]
    fn rejects_missing_argument() {
        let result = Cli::try_parse_from(["amplihack", "resolve-bundle-asset"]);
        assert!(
            result.is_err(),
            "missing asset argument should be a parse error"
        );
    }

    #[test]
    fn recipes_do_not_invoke_legacy_runtime_assets() {
        // Regression guard for the bug where smart-orchestrator preflight
        // depended on the legacy runtime-asset resolver instead of the Rust
        // binary installed on the machine.
        // Recipes must use `amplihack resolve-bundle-asset` instead.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let recipes_dir = manifest
            .join("..")
            .join("..")
            .join("amplifier-bundle")
            .join("recipes");
        if !recipes_dir.is_dir() {
            // Crate may be built outside the workspace (e.g., crates.io
            // packaging); recipes only exist in the source repo.
            eprintln!(
                "skipping: recipes dir not found at {}",
                recipes_dir.display()
            );
            return;
        }
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&recipes_dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
                continue;
            }
            let body = std::fs::read_to_string(&path).unwrap();
            if body.contains(concat!("amplihack", ".runtime_assets")) {
                offenders.push(path.display().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "recipes still invoke the legacy Python runtime_assets module \
             instead of `amplihack resolve-bundle-asset`: {offenders:?}"
        );
    }

    /// Regression for #588: recipes must not contain dead HOOKS_DIR assignments
    /// that resolve `hooks-dir` (removed in #285). The `|| true` suppresses
    /// the error but the variable is never read — it's dead code that masks
    /// the underlying asset-resolver mismatch.
    #[test]
    fn recipes_do_not_contain_dead_hooks_dir_assignments() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let recipes_dir = manifest
            .join("..")
            .join("..")
            .join("amplifier-bundle")
            .join("recipes");
        if !recipes_dir.is_dir() {
            eprintln!(
                "skipping: recipes dir not found at {}",
                recipes_dir.display()
            );
            return;
        }
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&recipes_dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
                continue;
            }
            let body = std::fs::read_to_string(&path).unwrap();
            // Match the pattern: HOOKS_DIR="$(amplihack resolve-bundle-asset hooks-dir ..."
            if body.contains("resolve-bundle-asset hooks-dir") {
                offenders.push(path.display().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "recipes still resolve the removed hooks-dir asset (see #285/#588). \
             Remove dead HOOKS_DIR assignments: {offenders:?}"
        );
    }
}
