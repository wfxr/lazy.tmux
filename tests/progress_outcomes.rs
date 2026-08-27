mod utils;

use std::sync::Mutex;

use tempfile::tempdir;
use tmup::lockfile::{LockEntry, LockFile, TrackingRecord};
use tmup::model::{Config, Options, PluginSource, PluginSpec, Tracking};
use tmup::progress::{NullReporter, PluginOutcome, ProgressEvent, ProgressReporter, SkipReason};
use tmup::state::{self, Paths, build_command_hash};
use tmup::sync::{self, SyncMode, SyncPolicy};
use tmup::{plugin, short_hash};
use utils::{make_bare_repo, push_commit};

fn make_plugin(id: &str, clone_url: &str, tracking: Tracking, build: Option<&str>) -> PluginSpec {
    PluginSpec {
        source: PluginSource::Remote {
            raw: id.to_string(),
            id: id.to_string(),
            clone_url: clone_url.to_string(),
        },
        name: id.rsplit('/').next().unwrap_or(id).to_string(),
        opt_prefix: String::new(),
        tracking,
        build: build.map(String::from),
        opts: vec![],
        environment: vec![],
    }
}

fn make_config(plugins: Vec<PluginSpec>) -> Config {
    Config { options: Options::default(), plugins }
}

#[derive(Default)]
struct CaptureProgress {
    stages: Mutex<Vec<String>>,
    finished: Mutex<Vec<(String, PluginOutcome)>>,
}

impl CaptureProgress {
    fn stage_ids(&self) -> Vec<String> {
        let mut v = self.stages.lock().unwrap().clone();
        v.sort();
        v.dedup();
        v
    }

    fn finished_ids(&self) -> Vec<String> {
        let mut v: Vec<_> =
            self.finished.lock().unwrap().iter().map(|(id, _)| id.clone()).collect();
        v.sort();
        v.dedup();
        v
    }

    fn outcomes_for(&self, id: &str) -> Vec<PluginOutcome> {
        self.finished
            .lock()
            .unwrap()
            .iter()
            .filter(|(pid, _)| pid == id)
            .map(|(_, outcome)| outcome.clone())
            .collect()
    }

    fn all_stage_ids(&self) -> Vec<String> {
        self.stages.lock().unwrap().clone()
    }

    fn all_finished_ids(&self) -> Vec<String> {
        self.finished.lock().unwrap().iter().map(|(id, _)| id.clone()).collect()
    }
}

impl ProgressReporter for CaptureProgress {
    fn report(&self, event: ProgressEvent<'_>) {
        match event {
            ProgressEvent::PluginStage { id, .. } => {
                self.stages.lock().unwrap().push(id.to_string())
            }
            ProgressEvent::PluginFinished { id, outcome, .. } => {
                self.finished.lock().unwrap().push((id.to_string(), outcome.clone()))
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn install_target_filters_events() {
    let dir = tempdir().unwrap();
    let (bare_a, _commit_a) = make_bare_repo(&dir.path().join("repo-a"));
    let (bare_b, _commit_b) = make_bare_repo(&dir.path().join("repo-b"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let plugin_a = make_plugin(
        "example.com/test/plugin-a",
        &format!("file://{}", bare_a.display()),
        Tracking::DefaultBranch,
        None,
    );
    let plugin_b = make_plugin(
        "example.com/test/plugin-b",
        &format!("file://{}", bare_b.display()),
        Tracking::DefaultBranch,
        None,
    );
    let cfg = make_config(vec![plugin_a, plugin_b]);

    let mut lock = LockFile::new();
    let capture = CaptureProgress::default();

    plugin::install(&cfg, &mut lock, &paths, Some("example.com/test/plugin-a"), false, &capture)
        .await
        .unwrap();

    assert!(
        capture.all_stage_ids().iter().all(|id| id == "example.com/test/plugin-a"),
        "stage events should only target plugin-a"
    );
    assert!(
        capture.all_finished_ids().iter().all(|id| id == "example.com/test/plugin-a"),
        "finished events should only target plugin-a"
    );
    assert_eq!(capture.stage_ids(), vec!["example.com/test/plugin-a"]);
    assert_eq!(capture.finished_ids(), vec!["example.com/test/plugin-a"]);
}

#[tokio::test]
async fn update_target_filters_events() {
    let dir = tempdir().unwrap();
    let (bare_a, _commit_a) = make_bare_repo(&dir.path().join("repo-a"));
    let (bare_b, _commit_b) = make_bare_repo(&dir.path().join("repo-b"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let plugin_a = make_plugin(
        "example.com/test/plugin-a",
        &format!("file://{}", bare_a.display()),
        Tracking::DefaultBranch,
        None,
    );
    let plugin_b = make_plugin(
        "example.com/test/plugin-b",
        &format!("file://{}", bare_b.display()),
        Tracking::DefaultBranch,
        None,
    );
    let cfg = make_config(vec![plugin_a.clone(), plugin_b.clone()]);

    let mut lock = LockFile::new();
    plugin::install(&cfg, &mut lock, &paths, None, false, &NullReporter).await.unwrap();

    let capture = CaptureProgress::default();
    plugin::update(&cfg, &mut lock, &paths, Some("example.com/test/plugin-a"), &capture)
        .await
        .unwrap();

    assert!(
        capture.all_stage_ids().iter().all(|id| id == "example.com/test/plugin-a"),
        "stage events should only target plugin-a"
    );
    assert!(
        capture.all_finished_ids().iter().all(|id| id == "example.com/test/plugin-a"),
        "finished events should only target plugin-a"
    );
    assert_eq!(capture.stage_ids(), vec!["example.com/test/plugin-a"]);
    assert_eq!(capture.finished_ids(), vec!["example.com/test/plugin-a"]);
}

#[tokio::test]
async fn restore_target_filters_events() {
    let dir = tempdir().unwrap();
    let (bare_a, commit_a) = make_bare_repo(&dir.path().join("repo-a"));
    let (bare_b, commit_b) = make_bare_repo(&dir.path().join("repo-b"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let plugin_a = make_plugin(
        "example.com/test/plugin-a",
        &format!("file://{}", bare_a.display()),
        Tracking::DefaultBranch,
        None,
    );
    let plugin_b = make_plugin(
        "example.com/test/plugin-b",
        &format!("file://{}", bare_b.display()),
        Tracking::DefaultBranch,
        None,
    );
    let cfg = make_config(vec![plugin_a.clone(), plugin_b.clone()]);

    let mut lock = LockFile::new();
    lock.plugins.insert(
        "example.com/test/plugin-a".into(),
        LockEntry {
            tracking: TrackingRecord { kind: "default-branch".into(), value: "main".into() },
            commit: commit_a.clone(),
            config_hash: None,
        },
    );
    lock.plugins.insert(
        "example.com/test/plugin-b".into(),
        LockEntry {
            tracking: TrackingRecord { kind: "default-branch".into(), value: "main".into() },
            commit: commit_b.clone(),
            config_hash: None,
        },
    );

    plugin::install(&cfg, &mut lock, &paths, None, false, &NullReporter).await.unwrap();

    // Force the targeted plugin to be restored so progress events fire.
    let target_dir = paths.plugin_dir("example.com/test/plugin-b");
    std::fs::remove_dir_all(&target_dir).unwrap();

    let capture = CaptureProgress::default();
    plugin::restore(&cfg, &lock, &paths, Some("example.com/test/plugin-b"), &capture)
        .await
        .unwrap();

    assert!(
        capture.all_stage_ids().iter().all(|id| id == "example.com/test/plugin-b"),
        "stage events should only target plugin-b"
    );
    assert!(
        capture.all_finished_ids().iter().all(|id| id == "example.com/test/plugin-b"),
        "finished events should only target plugin-b"
    );
    assert_eq!(capture.stage_ids(), vec!["example.com/test/plugin-b"]);
    assert_eq!(capture.finished_ids(), vec!["example.com/test/plugin-b"]);
}

#[tokio::test]
async fn update_skips_pinned_tag() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let plugin = make_plugin(
        "example.com/test/plugin",
        &format!("file://{}", bare.display()),
        Tracking::Tag("v1.0.0".into()),
        None,
    );
    let cfg = make_config(vec![plugin]);
    let mut lock = LockFile::new();
    lock.plugins.insert(
        "example.com/test/plugin".into(),
        LockEntry {
            tracking: TrackingRecord { kind: "tag".into(), value: "v1.0.0".into() },
            commit: commit.clone(),
            config_hash: None,
        },
    );

    let capture = CaptureProgress::default();
    plugin::update(&cfg, &mut lock, &paths, None, &capture).await.unwrap();

    let outcomes = capture.outcomes_for("example.com/test/plugin");
    assert!(matches!(
        outcomes.as_slice(),
        [PluginOutcome::Skipped { reason: SkipReason::PinnedTag { tag } }] if tag == "v1.0.0"
    ));
}

#[tokio::test]
async fn update_skips_pinned_commit() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let plugin = make_plugin(
        "example.com/test/plugin",
        &format!("file://{}", bare.display()),
        Tracking::Commit(commit.clone()),
        None,
    );
    let cfg = make_config(vec![plugin]);
    let mut lock = LockFile::new();
    lock.plugins.insert(
        "example.com/test/plugin".into(),
        LockEntry {
            tracking: TrackingRecord { kind: "commit".into(), value: commit.clone() },
            commit: commit.clone(),
            config_hash: None,
        },
    );

    let capture = CaptureProgress::default();
    plugin::update(&cfg, &mut lock, &paths, None, &capture).await.unwrap();

    let outcomes = capture.outcomes_for("example.com/test/plugin");
    assert!(matches!(
        outcomes.as_slice(),
        [PluginOutcome::Skipped { reason: SkipReason::PinnedCommit { commit: c } }] if c == &short_hash(&commit)
    ));
}

#[tokio::test]
async fn restore_skips_when_lock_entry_missing() {
    let dir = tempdir().unwrap();
    let (bare, _commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let plugin = make_plugin(
        "example.com/test/plugin",
        &format!("file://{}", bare.display()),
        Tracking::DefaultBranch,
        None,
    );
    let cfg = make_config(vec![plugin]);
    let lock = LockFile::new();

    let capture = CaptureProgress::default();
    plugin::restore(&cfg, &lock, &paths, None, &capture).await.unwrap();

    let outcomes = capture.outcomes_for("example.com/test/plugin");
    assert!(matches!(
        outcomes.as_slice(),
        [PluginOutcome::Skipped { reason: SkipReason::Other(reason) }] if reason == "no lock entry"
    ));
}

#[tokio::test]
async fn init_mode_skips_known_failure_marker() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let build_cmd = "exit 1";
    let plugin = make_plugin(
        "example.com/test/plugin",
        &format!("file://{}", bare.display()),
        Tracking::DefaultBranch,
        Some(build_cmd),
    );
    let cfg = make_config(vec![plugin.clone()]);

    let mut lock = LockFile::new();
    lock.plugins.insert(
        "example.com/test/plugin".into(),
        LockEntry {
            tracking: TrackingRecord { kind: "default-branch".into(), value: "main".into() },
            commit: commit.clone(),
            config_hash: None,
        },
    );

    let marker = state::FailureMarker {
        plugin_id: "example.com/test/plugin".into(),
        commit: commit.clone(),
        build_hash: build_command_hash(build_cmd),
        build_command: build_cmd.to_string(),
        failed_at: "now".into(),
        stderr_summary: "boom".into(),
    };
    state::write_failure_marker(&paths.failures_root, &marker).unwrap();

    let capture = CaptureProgress::default();
    sync::run_and_write(
        &cfg,
        &mut lock,
        &paths,
        None,
        SyncPolicy::init(true),
        SyncMode::Init,
        &capture,
    )
    .await
    .unwrap();

    let outcomes = capture.outcomes_for("example.com/test/plugin");
    assert!(matches!(
        outcomes.as_slice(),
        [PluginOutcome::Skipped { reason: SkipReason::KnownFailure { commit: c } }] if c == &short_hash(&commit)
    ));
}

#[tokio::test]
async fn update_emits_updated_outcome_with_from_and_to_commits() {
    let dir = tempdir().unwrap();
    let (bare, initial_commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let plugin = make_plugin(
        "example.com/test/plugin",
        &format!("file://{}", bare.display()),
        Tracking::DefaultBranch,
        None,
    );
    let cfg = make_config(vec![plugin]);
    let mut lock = LockFile::new();

    plugin::install(&cfg, &mut lock, &paths, None, false, &NullReporter).await.unwrap();
    let next_commit = push_commit(&bare, "next");

    let capture = CaptureProgress::default();
    plugin::update(&cfg, &mut lock, &paths, None, &capture).await.unwrap();

    let outcomes = capture.outcomes_for("example.com/test/plugin");
    assert!(matches!(
        outcomes.as_slice(),
        [PluginOutcome::Updated { from, to }]
            if from == short_hash(&initial_commit) && to == short_hash(&next_commit)
    ));
}
