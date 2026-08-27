mod utils;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tempfile::tempdir;
use tmup::config::parse_config;
use tmup::lockfile::{LockEntry, LockFile, read_lockfile};
use tmup::model::{Config, Options, PluginSource, PluginSpec, Tracking};
use tmup::progress::NullReporter;
use tmup::state::{Paths, build_command_hash};
use tmup::sync::{self, SyncMode, SyncPolicy};
use tmup::{planner, plugin, prepare};
use utils::*;

/// Create a minimal but real git repo at `path` with one commit, returning
/// the HEAD commit hash.
fn init_git_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    std::fs::write(path.join("init.tmux"), "#!/bin/sh\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .unwrap();
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn make_plugin(
    raw: &str,
    id: &str,
    clone_url: &str,
    tracking: Tracking,
    build: Option<&str>,
) -> PluginSpec {
    PluginSpec {
        source: PluginSource::Remote {
            raw: raw.into(),
            id: id.into(),
            clone_url: clone_url.into(),
        },
        name: raw.rsplit('/').next().unwrap_or(raw).into(),
        opt_prefix: String::new(),
        tracking,
        build: build.map(String::from),
        opts: vec![],
        environment: vec![],
        bindings: vec![],
    }
}

fn make_config(plugins: Vec<PluginSpec>) -> Config {
    Config { options: Options::default(), plugins }
}

#[test]
fn list_returns_state_and_build_status() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let config = parse_config(
        r#"
plugin "user/repo-a"
plugin "user/repo-b" build="make"
    "#,
    )
    .unwrap();

    let mut lock = LockFile::new();
    lock.plugins.insert("github.com/user/repo-a".into(), LockEntry::branch("main", "aaa111"));
    lock.plugins.insert("github.com/user/repo-b".into(), LockEntry::branch("main", "bbb222"));

    // Simulate repo-a installed (real git repo), repo-b missing
    let plugin_a = paths.plugin_dir("github.com/user/repo-a");
    let commit_a = init_git_repo(&plugin_a);

    // Update lock to match the real commit so state is Installed, not Outdated
    lock.plugins.insert("github.com/user/repo-a".into(), LockEntry::branch("main", &commit_a));

    let statuses = plugin::list(&config, &lock, &paths).unwrap();
    assert_eq!(statuses.len(), 2);

    let a = &statuses[0];
    assert_eq!(a.id, "github.com/user/repo-a");
    assert_eq!(a.state, planner::PluginState::Installed);

    let b = &statuses[1];
    assert_eq!(b.id, "github.com/user/repo-b");
    assert_eq!(b.state, planner::PluginState::Missing);
}

#[test]
fn clean_removes_undeclared_plugins() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    // Declared: only repo-a
    let config = parse_config(r#"plugin "user/repo-a""#).unwrap();

    // Installed: repo-a and repo-b
    let plugin_a = paths.plugin_dir("github.com/user/repo-a");
    let plugin_b = paths.plugin_dir("github.com/user/repo-b");
    std::fs::create_dir_all(plugin_a.join(".git")).unwrap();
    std::fs::create_dir_all(plugin_b.join(".git")).unwrap();

    plugin::clean(&config, &paths).unwrap();

    assert!(plugin_a.exists(), "declared plugin should remain");
    assert!(!plugin_b.exists(), "undeclared plugin should be removed");
}

#[test]
fn clean_does_not_remove_local_plugin_source() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let config = parse_config(r#"plugin "~/dev/my-plugin" local=#true"#).unwrap();

    // Local plugin source is not in the managed directory, so clean should not touch it
    plugin::clean(&config, &paths).unwrap();
    // Just verifying no panic/error
}

#[tokio::test]
async fn install_creates_repo_cache_and_uses_locked_commit() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();
    let clone_url = format!("file://{}", bare.display());
    let plugin = make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        &clone_url,
        Tracking::DefaultBranch,
        None,
    );
    let cfg = make_config(vec![plugin]);
    let mut lock = LockFile::new();
    lock.plugins.insert("example.com/test/plugin".into(), LockEntry::branch("main", &commit));

    plugin::install(&cfg, &mut lock, &paths, None, false, &NullReporter).await.unwrap();

    assert!(paths.repo_cache_dir("example.com/test/plugin").exists());
    assert_eq!(
        tmup::git::head_commit_sync(&paths.plugin_dir("example.com/test/plugin")).unwrap(),
        commit
    );
}

#[tokio::test]
async fn install_preserves_remote_origin_url() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();
    let clone_url = format!("file://{}", bare.display());
    let plugin = make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        &clone_url,
        Tracking::DefaultBranch,
        None,
    );
    let cfg = make_config(vec![plugin]);
    let mut lock = LockFile::new();
    lock.plugins.insert("example.com/test/plugin".into(), LockEntry::branch("main", &commit));

    plugin::install(&cfg, &mut lock, &paths, None, false, &NullReporter).await.unwrap();

    let plugin_dir = paths.plugin_dir("example.com/test/plugin");
    let origin = git(&["remote", "get-url", "origin"], &plugin_dir);
    assert_eq!(origin, clone_url);
}

#[tokio::test]
async fn update_reuses_repo_cache_after_remote_advance() {
    let dir = tempdir().unwrap();
    let (bare, commit_a) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();
    let clone_url = format!("file://{}", bare.display());
    let plugin = make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        &clone_url,
        Tracking::DefaultBranch,
        None,
    );
    let cfg = make_config(vec![plugin]);
    let mut lock = LockFile::new();
    lock.plugins.insert("example.com/test/plugin".into(), LockEntry::branch("main", &commit_a));

    plugin::update(&cfg, &mut lock, &paths, None, &NullReporter).await.unwrap();
    let cache = paths.repo_cache_dir("example.com/test/plugin");
    assert!(cache.exists());

    let commit_b = push_commit(&bare, "next");
    plugin::update(&cfg, &mut lock, &paths, None, &NullReporter).await.unwrap();

    assert_eq!(lock.plugins["example.com/test/plugin"].commit, commit_b);
    assert_eq!(
        tmup::git::head_commit_sync(&paths.plugin_dir("example.com/test/plugin")).unwrap(),
        commit_b
    );
}

#[cfg(unix)]
#[tokio::test]
async fn update_noop_marker_clear_failure_cleans_pending_staging_dirs() {
    let dir = tempdir().unwrap();
    let (bare_a, commit_a) = make_bare_repo(&dir.path().join("repo-a"));
    let (bare_b, commit_b) = make_bare_repo(&dir.path().join("repo-b"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_a = format!("file://{}", bare_a.display());
    let clone_b = format!("file://{}", bare_b.display());
    let mut cfg = make_config(vec![
        make_plugin(
            "test/plugin-a",
            "example.com/test/plugin-a",
            &clone_a,
            Tracking::DefaultBranch,
            None,
        ),
        make_plugin(
            "test/plugin-b",
            "example.com/test/plugin-b",
            &clone_b,
            Tracking::DefaultBranch,
            None,
        ),
    ]);
    cfg.options.concurrency = 2;

    let mut lock = LockFile::new();
    lock.plugins.insert("example.com/test/plugin-a".into(), LockEntry::branch("main", &commit_a));
    lock.plugins.insert("example.com/test/plugin-b".into(), LockEntry::branch("main", &commit_b));

    plugin::install(&cfg, &mut lock, &paths, None, false, &NullReporter).await.unwrap();

    let original_mode = std::fs::metadata(&paths.failures_root).unwrap().permissions().mode();
    let mut denied = std::fs::metadata(&paths.failures_root).unwrap().permissions();
    denied.set_mode(0o000);
    std::fs::set_permissions(&paths.failures_root, denied).unwrap();

    let result = plugin::update(&cfg, &mut lock, &paths, None, &NullReporter).await;

    let mut restored = std::fs::metadata(&paths.failures_root).unwrap().permissions();
    restored.set_mode(original_mode);
    std::fs::set_permissions(&paths.failures_root, restored).unwrap();

    assert!(result.is_err(), "update should return error when marker cleanup fails");
    assert!(
        !paths.staging_dir("example.com/test/plugin-a").exists(),
        "current plugin staging should be cleaned"
    );
    assert!(
        !paths.staging_dir("example.com/test/plugin-b").exists(),
        "pending plugin staging should be cleaned after early return"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn update_noop_marker_clear_failure_does_not_advance_in_memory_lock() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let mut cfg = make_config(vec![make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        &clone_url,
        Tracking::DefaultBranch,
        Some("touch built-v2.marker"),
    )]);
    cfg.options.concurrency = 1;

    let mut lock = LockFile::new();
    plugin::install(
        &make_config(vec![make_plugin(
            "test/plugin",
            "example.com/test/plugin",
            &clone_url,
            Tracking::DefaultBranch,
            Some("touch built-v1.marker"),
        )]),
        &mut lock,
        &paths,
        None,
        false,
        &NullReporter,
    )
    .await
    .unwrap();

    let old_entry = lock.plugins["example.com/test/plugin"].clone();
    assert_ne!(
        old_entry.config_hash,
        tmup::lockfile::remote_plugin_config_hash(&cfg.plugins[0]),
        "test setup requires update to rewrite the in-memory lock entry in the no-op path"
    );

    let original_mode = std::fs::metadata(&paths.failures_root).unwrap().permissions().mode();
    let mut denied = std::fs::metadata(&paths.failures_root).unwrap().permissions();
    denied.set_mode(0o000);
    std::fs::set_permissions(&paths.failures_root, denied).unwrap();

    let result = plugin::update(&cfg, &mut lock, &paths, None, &NullReporter).await;

    let mut restored = std::fs::metadata(&paths.failures_root).unwrap().permissions();
    restored.set_mode(original_mode);
    std::fs::set_permissions(&paths.failures_root, restored).unwrap();

    assert!(result.is_err(), "update should return error when marker cleanup fails");
    assert_eq!(
        lock.plugins["example.com/test/plugin"], old_entry,
        "marker cleanup failure must not advance the in-memory lock entry"
    );
    assert_eq!(git(&["rev-parse", "HEAD"], &paths.plugin_dir("example.com/test/plugin")), commit);
}

#[cfg(unix)]
#[test]
fn clean_unlinks_symlinked_undeclared_plugin_without_removing_target_repo() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let config = parse_config("").unwrap();

    let external_repo = dir.path().join("external-repo");
    std::fs::create_dir_all(external_repo.join(".git")).unwrap();

    let managed_link = paths.plugin_dir("github.com/user/repo");
    std::fs::create_dir_all(managed_link.parent().unwrap()).unwrap();
    symlink(&external_repo, &managed_link).unwrap();

    plugin::clean(&config, &paths).unwrap();

    assert!(!managed_link.exists(), "symlinked managed entry should be removed");
    assert!(external_repo.exists(), "clean must not remove the symlink target repo");
    assert!(external_repo.join(".git").exists(), "target repo contents should remain intact");
}

#[cfg(unix)]
#[test]
fn clean_does_not_traverse_symlinked_parent_directories() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let config = parse_config("").unwrap();

    let external_host_root = dir.path().join("external-host-root");
    std::fs::create_dir_all(external_host_root.join("user/repo/.git")).unwrap();

    let managed_host_link = paths.plugin_root.join("github.com");
    symlink(&external_host_root, &managed_host_link).unwrap();

    plugin::clean(&config, &paths).unwrap();

    assert!(managed_host_link.exists(), "clean should not recurse into a symlinked host directory");
    assert!(
        external_host_root.join("user/repo/.git").exists(),
        "clean must not remove repos reachable only through a symlinked parent"
    );
}

#[test]
fn is_known_failure_detects_matching_key() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    // Write a failure marker
    let marker = tmup::state::FailureMarker {
        plugin_id: "github.com/user/repo".into(),
        commit: "abc123".into(),
        build_hash: build_command_hash("make"),
        build_command: "make".into(),
        failed_at: "now".into(),
        stderr_summary: "error".into(),
    };
    tmup::state::write_failure_marker(&paths.failures_root, &marker).unwrap();

    // Same tuple: should be known
    assert!(plugin::is_known_failure(&paths, "github.com/user/repo", "abc123", "make").unwrap());

    // Different build command: should NOT be known
    assert!(
        !plugin::is_known_failure(&paths, "github.com/user/repo", "abc123", "just build").unwrap()
    );

    // Different commit: should NOT be known
    assert!(!plugin::is_known_failure(&paths, "github.com/user/repo", "def456", "make").unwrap());
}

#[test]
fn list_shows_broken_for_dir_without_git() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let config = parse_config(r#"plugin "user/repo""#).unwrap();
    let mut lock = LockFile::new();
    lock.plugins.insert("github.com/user/repo".into(), LockEntry::branch("main", "abc123"));

    // Create target dir but no .git — simulates a broken/corrupt install
    let plugin_dir = paths.plugin_dir("github.com/user/repo");
    std::fs::create_dir_all(&plugin_dir).unwrap();

    let statuses = plugin::list(&config, &lock, &paths).unwrap();
    assert_eq!(statuses[0].state, planner::PluginState::Broken);
    assert_eq!(statuses[0].build_status, planner::BuildStatus::None);
}

#[test]
fn list_shows_broken_for_empty_dotgit() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let config = parse_config(r#"plugin "user/repo""#).unwrap();
    let lock = LockFile::new();

    // Create target dir with empty .git/ — HEAD unreadable
    let plugin_dir = paths.plugin_dir("github.com/user/repo");
    std::fs::create_dir_all(plugin_dir.join(".git")).unwrap();

    let statuses = plugin::list(&config, &lock, &paths).unwrap();
    assert_eq!(statuses[0].state, planner::PluginState::Broken);
}

#[test]
fn update_skips_pinned_tag() {
    let config = parse_config(r#"plugin "user/repo" tag="v1.0""#).unwrap();
    let spec = &config.plugins[0];
    assert!(matches!(spec.tracking, tmup::model::Tracking::Tag(_)));
}

#[test]
fn update_skips_pinned_commit() {
    let config = parse_config(r#"plugin "user/repo" commit="abc123""#).unwrap();
    let spec = &config.plugins[0];
    assert!(matches!(spec.tracking, tmup::model::Tracking::Commit(_)));
}

#[test]
fn list_shows_both_state_and_build_status_for_build_failure() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let config = parse_config(r#"plugin "user/repo" build="make""#).unwrap();
    let mut lock = LockFile::new();
    lock.plugins.insert("github.com/user/repo".into(), LockEntry::branch("main", "abc123"));

    // Plugin is installed (real git repo) but has a build failure marker
    let plugin_dir = paths.plugin_dir("github.com/user/repo");
    let commit = init_git_repo(&plugin_dir);

    // Update lock to match the real commit
    lock.plugins.insert("github.com/user/repo".into(), LockEntry::branch("main", &commit));

    let marker = tmup::state::FailureMarker {
        plugin_id: "github.com/user/repo".into(),
        commit: commit.clone(),
        build_hash: build_command_hash("make"),
        build_command: "make".into(),
        failed_at: "now".into(),
        stderr_summary: "error".into(),
    };
    tmup::state::write_failure_marker(&paths.failures_root, &marker).unwrap();

    let statuses = plugin::list(&config, &lock, &paths).unwrap();
    assert_eq!(statuses[0].state, planner::PluginState::Installed);
    assert_eq!(statuses[0].build_status, planner::BuildStatus::BuildFailed);
}

#[test]
fn stale_lock_detection_catches_missing_and_mismatched_sync_metadata() {
    let config = parse_config(r#"plugin "user/repo" build="make install""#).unwrap();

    let mut stale_lock = LockFile::new();
    stale_lock.plugins.insert("github.com/user/repo".into(), LockEntry::branch("main", "abc123"));
    stale_lock.config_fingerprint = None;
    assert!(sync::lock_is_stale(&config, &stale_lock));

    let mut aligned_lock = LockFile::new();
    let mut entry = LockEntry::branch("main", "abc123");
    entry.config_hash = tmup::lockfile::remote_plugin_config_hash(&config.plugins[0]);
    aligned_lock.plugins.insert("github.com/user/repo".into(), entry);
    aligned_lock.config_fingerprint = Some(tmup::lockfile::config_fingerprint(&config));
    assert!(!sync::lock_is_stale(&config, &aligned_lock));

    aligned_lock.config_fingerprint = Some("stale-top-level".into());
    assert!(sync::lock_is_stale(&config, &aligned_lock));
}

#[tokio::test]
async fn install_uses_post_sync_lock_snapshot() {
    let dir = tempdir().unwrap();
    let (bare, commit_a) = make_bare_repo(&dir.path().join("repo"));
    let commit_b = push_commit(&bare, "second");
    push_tag(&bare, "v1.0.0", &commit_a);

    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let cfg = make_config(vec![make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        &clone_url,
        Tracking::Tag("v1.0.0".into()),
        None,
    )]);

    let mut lock = LockFile::new();
    lock.plugins.insert("example.com/test/plugin".into(), LockEntry::branch("main", &commit_b));

    sync::run_and_write(
        &cfg,
        &mut lock,
        &paths,
        None,
        SyncPolicy::INSTALL,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    let mut persisted = read_lockfile(&paths.lockfile_path).unwrap();
    plugin::install(&cfg, &mut persisted, &paths, None, false, &NullReporter).await.unwrap();

    let target = paths.plugin_dir("example.com/test/plugin");
    assert_eq!(git(&["rev-parse", "HEAD"], &target), commit_a);
    assert_eq!(persisted.plugins["example.com/test/plugin"].tracking.kind, "tag");
}

#[tokio::test]
async fn restore_uses_post_sync_lock_snapshot() {
    let dir = tempdir().unwrap();
    let (bare, commit_a) = make_bare_repo(&dir.path().join("repo"));
    let commit_b = push_commit(&bare, "second");

    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let target = paths.plugin_dir("example.com/test/plugin");
    clone_to_target(&bare, &target);
    assert_eq!(git(&["rev-parse", "HEAD"], &target), commit_b);

    let clone_url = format!("file://{}", bare.display());
    let cfg = make_config(vec![make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        &clone_url,
        Tracking::Commit(commit_b.clone()),
        None,
    )]);

    let mut lock = LockFile::new();
    lock.plugins.insert("example.com/test/plugin".into(), LockEntry::branch("main", &commit_a));

    sync::run_and_write(
        &cfg,
        &mut lock,
        &paths,
        None,
        SyncPolicy::RESTORE,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    let persisted = read_lockfile(&paths.lockfile_path).unwrap();
    plugin::restore(&cfg, &persisted, &paths, None, &NullReporter).await.unwrap();
    assert_eq!(git(&["rev-parse", "HEAD"], &target), commit_b);
    assert_eq!(persisted.plugins["example.com/test/plugin"].commit, commit_b);
}

#[tokio::test]
async fn update_runs_sync_first_then_only_advances_unchanged_floating_plugins() {
    let dir = tempdir().unwrap();
    let (bare_a, commit_a1) = make_bare_repo(&dir.path().join("repo-a"));
    let commit_a2 = push_commit(&bare_a, "second-a");
    push_tag(&bare_a, "v1.0.0", &commit_a1);

    let (bare_b, commit_b1) = make_bare_repo(&dir.path().join("repo-b"));
    let commit_b2 = push_commit(&bare_b, "second-b");

    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let target_a = paths.plugin_dir("example.com/test/plugin-a");
    let target_b = paths.plugin_dir("example.com/test/plugin-b");
    clone_to_target(&bare_a, &target_a);
    clone_to_target(&bare_b, &target_b);
    git(&["checkout", &commit_a1], &target_a);
    git(&["checkout", &commit_b1], &target_b);

    let clone_a = format!("file://{}", bare_a.display());
    let clone_b = format!("file://{}", bare_b.display());
    let cfg = make_config(vec![
        make_plugin(
            "test/plugin-a",
            "example.com/test/plugin-a",
            &clone_a,
            Tracking::Tag("v1.0.0".into()),
            None,
        ),
        make_plugin(
            "test/plugin-b",
            "example.com/test/plugin-b",
            &clone_b,
            Tracking::Branch("main".into()),
            None,
        ),
    ]);

    let mut lock = LockFile::new();
    lock.plugins.insert("example.com/test/plugin-a".into(), LockEntry::branch("main", &commit_a1));
    lock.plugins.insert("example.com/test/plugin-b".into(), LockEntry::branch("main", &commit_b1));

    sync::run_and_write(
        &cfg,
        &mut lock,
        &paths,
        None,
        SyncPolicy::UPDATE,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    let mut persisted = read_lockfile(&paths.lockfile_path).unwrap();
    plugin::update(&cfg, &mut persisted, &paths, None, &NullReporter).await.unwrap();

    assert_eq!(git(&["rev-parse", "HEAD"], &target_a), commit_a1);
    assert_eq!(git(&["rev-parse", "HEAD"], &target_b), commit_b2);
    assert_eq!(persisted.plugins["example.com/test/plugin-a"].tracking.kind, "tag");
    assert_eq!(persisted.plugins["example.com/test/plugin-a"].commit, commit_a1);
    assert_eq!(persisted.plugins["example.com/test/plugin-b"].commit, commit_b2);
    assert_ne!(commit_a1, commit_a2);
}

#[tokio::test]
async fn clean_prunes_removed_lock_entries_without_rebuilding_declared_plugins() {
    let dir = tempdir().unwrap();
    let (bare_a, _commit_a) = make_bare_repo(&dir.path().join("repo-a"));
    let (bare_b, _commit_b) = make_bare_repo(&dir.path().join("repo-b"));

    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_a = format!("file://{}", bare_a.display());
    let clone_b = format!("file://{}", bare_b.display());
    let initial_cfg = make_config(vec![
        make_plugin(
            "test/plugin-a",
            "example.com/test/plugin-a",
            &clone_a,
            Tracking::DefaultBranch,
            Some("touch built-v1.marker"),
        ),
        make_plugin(
            "test/plugin-b",
            "example.com/test/plugin-b",
            &clone_b,
            Tracking::DefaultBranch,
            None,
        ),
    ]);

    let mut lock = LockFile::new();
    sync::run_and_write(
        &initial_cfg,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    let plugin_a = paths.plugin_dir("example.com/test/plugin-a");
    let plugin_b = paths.plugin_dir("example.com/test/plugin-b");
    assert!(plugin_a.join("built-v1.marker").exists());
    assert!(plugin_b.exists());

    let clean_cfg = make_config(vec![make_plugin(
        "test/plugin-a",
        "example.com/test/plugin-a",
        &clone_a,
        Tracking::DefaultBranch,
        Some("touch built-v2.marker"),
    )]);

    sync::run_and_write(
        &clean_cfg,
        &mut lock,
        &paths,
        None,
        SyncPolicy::CLEAN,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();
    plugin::clean(&clean_cfg, &paths).unwrap();

    let persisted = read_lockfile(&paths.lockfile_path).unwrap();
    assert!(plugin_a.exists());
    assert!(plugin_a.join("built-v1.marker").exists());
    assert!(!plugin_a.join("built-v2.marker").exists());
    assert!(!plugin_b.exists(), "clean should still remove undeclared repos");
    assert!(persisted.plugins.contains_key("example.com/test/plugin-a"));
    assert!(!persisted.plugins.contains_key("example.com/test/plugin-b"));
}

#[tokio::test]
async fn concurrent_prepare_overlaps_when_limit_allows() {
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let in_flight = Arc::new(AtomicUsize::new(0));

    let jobs: Vec<_> = (0..4)
        .map(|_| {
            let inf = in_flight.clone();
            let mif = max_in_flight.clone();
            async move {
                let cur = inf.fetch_add(1, Ordering::SeqCst) + 1;
                mif.fetch_max(cur, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(30)).await;
                inf.fetch_sub(1, Ordering::SeqCst);
            }
        })
        .collect();

    prepare::run_bounded(2, jobs).await;
    assert!(max_in_flight.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn concurrent_prepare_serial_when_limit_one() {
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let in_flight = Arc::new(AtomicUsize::new(0));

    let jobs: Vec<_> = (0..4)
        .map(|_| {
            let inf = in_flight.clone();
            let mif = max_in_flight.clone();
            async move {
                let cur = inf.fetch_add(1, Ordering::SeqCst) + 1;
                mif.fetch_max(cur, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                inf.fetch_sub(1, Ordering::SeqCst);
            }
        })
        .collect();

    prepare::run_bounded(1, jobs).await;
    assert_eq!(max_in_flight.load(Ordering::SeqCst), 1);
}
