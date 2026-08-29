mod utils;
use std::collections::{HashMap, HashSet};

use tempfile::tempdir;
use tmup::lockfile::{LockEntry, LockFile};
use tmup::model::Config;
use tmup::planner::{
    BuildStatus, PluginState, RepoHealth, collect_failed_builds, compute_statuses,
};
use tmup::state::{FailureMarker, Paths, build_command_hash};
use tmup::sync;
use utils::{git, load_native_config};

fn make_config(kdl: &str) -> Config {
    load_native_config(kdl).unwrap().into_config()
}

#[test]
fn state_display_strings() {
    assert_eq!(PluginState::Installed.to_string(), "installed");
    assert_eq!(PluginState::Missing.to_string(), "missing");
    assert_eq!(PluginState::Outdated.to_string(), "outdated");
    assert_eq!(PluginState::PinnedTag.to_string(), "pinned-tag");
    assert_eq!(PluginState::PinnedCommit.to_string(), "pinned-commit");
}

#[test]
fn build_status_display_strings() {
    assert_eq!(BuildStatus::Ok.to_string(), "ok");
    assert_eq!(BuildStatus::BuildFailed.to_string(), "build-failed");
    assert_eq!(BuildStatus::None.to_string(), "none");
}

#[test]
fn preview_returns_false_when_lock_and_config_are_aligned() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let plugin_dir = paths.plugin_dir("github.com/user/repo");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    git(&["init", "-b", "main"], &plugin_dir);
    std::fs::write(plugin_dir.join("init.tmux"), "#!/bin/sh\n").unwrap();
    git(&["add", "."], &plugin_dir);
    git(&["commit", "-m", "init"], &plugin_dir);
    let commit = git(&["rev-parse", "HEAD"], &plugin_dir);

    let config = make_config(r#"plug "user/repo""#);
    let mut lock = LockFile::new();
    let mut entry = LockEntry::branch("main", &commit);
    entry.config_hash = tmup::lockfile::remote_plugin_config_hash(&config.plugins[0]);
    lock.plugins.insert("github.com/user/repo".into(), entry);

    let preview = sync::preview(&config, &lock, None, sync::SyncPolicy::init(true), &paths);
    assert!(!preview.needs_work);
}

#[test]
fn preview_returns_true_for_missing_plugin_dir_under_init_policy() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let config = make_config(r#"plug "user/repo""#);
    let mut lock = LockFile::new();
    let mut entry = LockEntry::branch("main", "abc123");
    entry.config_hash = tmup::lockfile::remote_plugin_config_hash(&config.plugins[0]);
    lock.plugins.insert("github.com/user/repo".into(), entry);

    assert!(!paths.plugin_dir("github.com/user/repo").exists());
    let preview = sync::preview(&config, &lock, None, sync::SyncPolicy::init(true), &paths);
    assert!(
        preview.needs_work,
        "preview should require work when plugin dir is missing even if lock hash matches"
    );
}

#[test]
fn preview_returns_true_for_broken_plugin_dir_under_init_policy() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let config = make_config(r#"plug "user/repo""#);
    let mut lock = LockFile::new();
    let mut entry = LockEntry::branch("main", "abc123");
    entry.config_hash = tmup::lockfile::remote_plugin_config_hash(&config.plugins[0]);
    lock.plugins.insert("github.com/user/repo".into(), entry);

    let plugin_dir = paths.plugin_dir("github.com/user/repo");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    assert!(matches!(tmup::planner::inspect_plugin_dir(&plugin_dir), RepoHealth::Broken));

    let preview = sync::preview(&config, &lock, None, sync::SyncPolicy::init(true), &paths);
    assert!(
        preview.needs_work,
        "preview should require work when plugin dir is broken even if lock hash matches"
    );
}

#[test]
fn preview_returns_true_for_same_commit_build_change_requires_republish() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    let old_config = make_config(r#"plug "user/repo" build="touch built-v1""#);
    let new_config = make_config(r#"plug "user/repo" build="touch built-v2""#);
    let mut lock = LockFile::new();
    let mut entry = LockEntry::branch("main", "abc123");
    entry.config_hash = tmup::lockfile::remote_plugin_config_hash(&old_config.plugins[0]);
    lock.plugins.insert("github.com/user/repo".into(), entry);

    let preview = sync::preview(&new_config, &lock, None, sync::SyncPolicy::init(true), &paths);
    assert!(
        preview.needs_work,
        "preview should require work when same-commit build config changes"
    );
}

#[test]
fn build_failure_keeps_state_and_result_separate() {
    let config = make_config(r#"plug "user/repo" build="make""#);
    let mut lock = LockFile::new();
    lock.plugins.insert("github.com/user/repo".into(), LockEntry::branch("main", "abc123"));
    let health: HashMap<String, RepoHealth> =
        [("github.com/user/repo".into(), RepoHealth::Healthy { commit: "abc123".into() })].into();

    let bh = build_command_hash("make");
    let marker = FailureMarker {
        plugin_id: "github.com/user/repo".into(),
        commit: "abc123".into(),
        build_hash: bh,
        build_command: "make".into(),
        failed_at: "2025-01-01T00:00:00Z".into(),
        stderr_summary: "error".into(),
    };
    let failed_builds = collect_failed_builds(&[marker]);

    let statuses = compute_statuses(&config, &lock, &health, &failed_builds);
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].state, PluginState::Installed);
    assert_eq!(statuses[0].build_status, BuildStatus::BuildFailed);
}

#[test]
fn missing_plugin_with_build_failure_shows_missing_and_failed() {
    let config = make_config(r#"plug "user/repo" build="make""#);
    let mut lock = LockFile::new();
    lock.plugins.insert("github.com/user/repo".into(), LockEntry::branch("main", "abc123"));
    let health: HashMap<String, RepoHealth> = HashMap::new();

    let bh = build_command_hash("make");
    let marker = FailureMarker {
        plugin_id: "github.com/user/repo".into(),
        commit: "abc123".into(),
        build_hash: bh,
        build_command: "make".into(),
        failed_at: "2025-01-01T00:00:00Z".into(),
        stderr_summary: "error".into(),
    };
    let failed_builds = collect_failed_builds(&[marker]);

    let statuses = compute_statuses(&config, &lock, &health, &failed_builds);
    assert_eq!(statuses[0].state, PluginState::Missing);
    assert_eq!(statuses[0].build_status, BuildStatus::BuildFailed);
}

#[test]
fn installed_plugin_without_build_shows_none_build_status() {
    let config = make_config(r#"plug "user/repo""#);
    let mut lock = LockFile::new();
    lock.plugins.insert("github.com/user/repo".into(), LockEntry::branch("main", "abc123"));
    let health: HashMap<String, RepoHealth> =
        [("github.com/user/repo".into(), RepoHealth::Healthy { commit: "abc123".into() })].into();
    let failed_builds = HashSet::new();

    let statuses = compute_statuses(&config, &lock, &health, &failed_builds);
    assert_eq!(statuses[0].state, PluginState::Installed);
    assert_eq!(statuses[0].build_status, BuildStatus::None);
}

#[test]
fn local_plugin_status() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("my-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let config = make_config(&format!(r#"plug "{}" local=#true"#, plugin_dir.display()));
    let lock = LockFile::new();
    let health = HashMap::new();
    let failed_builds = HashSet::new();

    let statuses = compute_statuses(&config, &lock, &health, &failed_builds);
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].state, PluginState::Local);
    assert_eq!(statuses[0].build_status, BuildStatus::None);
    assert_eq!(statuses[0].kind, "local");
}

#[test]
fn missing_local_plugin_shows_missing_and_none() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("missing-plugin");
    let config = make_config(&format!(r#"plug "{}" local=#true"#, plugin_dir.display()));
    let lock = LockFile::new();
    let health = HashMap::new();
    let failed_builds = HashSet::new();

    let statuses = compute_statuses(&config, &lock, &health, &failed_builds);
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].state, PluginState::Missing);
    assert_eq!(statuses[0].build_status, BuildStatus::None);
    assert_eq!(statuses[0].kind, "local");
}

#[test]
fn local_plugin_file_path_shows_broken_and_none() {
    let dir = tempdir().unwrap();
    let plugin_file = dir.path().join("plugin.tmux");
    std::fs::write(&plugin_file, "#!/bin/sh\n").unwrap();
    let config = make_config(&format!(r#"plug "{}" local=#true"#, plugin_file.display()));
    let lock = LockFile::new();
    let health = HashMap::new();
    let failed_builds = HashSet::new();

    let statuses = compute_statuses(&config, &lock, &health, &failed_builds);
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].state, PluginState::Broken);
    assert_eq!(statuses[0].build_status, BuildStatus::None);
    assert_eq!(statuses[0].kind, "local");
}

#[test]
fn pinned_tag_status() {
    let config = make_config(r#"plug "user/repo" tag="v1.0""#);
    let mut lock = LockFile::new();
    lock.plugins.insert("github.com/user/repo".into(), LockEntry::tag("v1.0", "abc123"));
    let health: HashMap<String, RepoHealth> =
        [("github.com/user/repo".into(), RepoHealth::Healthy { commit: "abc123".into() })].into();
    let failed_builds = HashSet::new();

    let statuses = compute_statuses(&config, &lock, &health, &failed_builds);
    assert_eq!(statuses[0].state, PluginState::PinnedTag);
}

#[test]
fn pinned_tag_with_drifted_head_shows_outdated() {
    let config = make_config(r#"plug "user/repo" tag="v1.0""#);
    let mut lock = LockFile::new();
    lock.plugins.insert("github.com/user/repo".into(), LockEntry::tag("v1.0", "abc123"));
    let health: HashMap<String, RepoHealth> =
        [("github.com/user/repo".into(), RepoHealth::Healthy { commit: "def456".into() })].into();
    let failed_builds = HashSet::new();

    let statuses = compute_statuses(&config, &lock, &health, &failed_builds);
    assert_eq!(statuses[0].state, PluginState::Outdated);
    assert_eq!(statuses[0].current_commit.as_deref(), Some("def456"));
    assert_eq!(statuses[0].lock_commit.as_deref(), Some("abc123"));
}

#[test]
fn pinned_commit_with_drifted_head_shows_outdated() {
    let config = make_config(r#"plug "user/repo" commit="abc123""#);
    let mut lock = LockFile::new();
    lock.plugins.insert("github.com/user/repo".into(), LockEntry::commit("abc123"));
    let health: HashMap<String, RepoHealth> =
        [("github.com/user/repo".into(), RepoHealth::Healthy { commit: "def456".into() })].into();
    let failed_builds = HashSet::new();

    let statuses = compute_statuses(&config, &lock, &health, &failed_builds);
    assert_eq!(statuses[0].state, PluginState::Outdated);
    assert_eq!(statuses[0].current_commit.as_deref(), Some("def456"));
    assert_eq!(statuses[0].lock_commit.as_deref(), Some("abc123"));
}

#[test]
fn outdated_state_when_installed_commit_differs_from_lock() {
    let config = make_config(r#"plug "user/repo""#);
    let mut lock = LockFile::new();
    lock.plugins.insert("github.com/user/repo".into(), LockEntry::branch("main", "abc123"));
    let health: HashMap<String, RepoHealth> =
        [("github.com/user/repo".into(), RepoHealth::Healthy { commit: "def456".into() })].into();
    let failed_builds = HashSet::new();

    let statuses = compute_statuses(&config, &lock, &health, &failed_builds);
    assert_eq!(statuses[0].state, PluginState::Outdated);
    assert_eq!(statuses[0].current_commit.as_deref(), Some("def456"));
    assert_eq!(statuses[0].lock_commit.as_deref(), Some("abc123"));
}

#[test]
fn inspect_missing_dir() {
    let dir = tempdir().unwrap();
    let health = tmup::planner::inspect_plugin_dir(&dir.path().join("nonexistent"));
    assert!(matches!(health, RepoHealth::Missing));
}

#[test]
fn inspect_healthy_git_repo() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("plugin");
    std::fs::create_dir_all(&repo).unwrap();
    std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    std::fs::write(repo.join("init.tmux"), "#!/bin/sh\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(&repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .unwrap();
    let health = tmup::planner::inspect_plugin_dir(&repo);
    assert!(matches!(health, RepoHealth::Healthy { .. }));
    if let RepoHealth::Healthy { commit } = health {
        assert_eq!(commit.len(), 40);
    }
}

#[test]
fn inspect_dir_exists_no_git() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("plugin");
    std::fs::create_dir_all(&repo).unwrap();
    let health = tmup::planner::inspect_plugin_dir(&repo);
    assert!(matches!(health, RepoHealth::Broken));
}

#[test]
fn inspect_dir_with_empty_dotgit() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("plugin");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let health = tmup::planner::inspect_plugin_dir(&repo);
    assert!(matches!(health, RepoHealth::Broken));
}

#[test]
fn scan_managed_finds_git_dirs() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("plugins");
    std::fs::create_dir_all(root.join("github.com/user/repo-a/.git")).unwrap();
    std::fs::create_dir_all(root.join("github.com/user/repo-b/.git")).unwrap();
    std::fs::create_dir_all(root.join("github.com/user/not-a-repo")).unwrap();
    let ids = tmup::planner::scan_managed_plugin_ids(&root);
    assert!(ids.contains("github.com/user/repo-a"));
    assert!(ids.contains("github.com/user/repo-b"));
    assert!(!ids.contains("github.com/user/not-a-repo"));
    assert_eq!(ids.len(), 2);
}

#[test]
fn broken_state_display_string() {
    assert_eq!(tmup::planner::PluginState::Broken.to_string(), "broken");
}

#[test]
fn broken_repo_shows_broken_in_list() {
    let config = make_config(r#"plug "user/repo""#);
    let mut lock = LockFile::new();
    lock.plugins.insert("github.com/user/repo".into(), LockEntry::branch("main", "abc123"));
    let health: HashMap<String, RepoHealth> =
        [("github.com/user/repo".into(), RepoHealth::Broken)].into();
    let failed_builds = HashSet::new();
    let statuses = compute_statuses(&config, &lock, &health, &failed_builds);
    assert_eq!(statuses[0].state, PluginState::Broken);
    assert_eq!(statuses[0].build_status, BuildStatus::None);
}
