mod utils;
use std::sync::Mutex;

use tempfile::tempdir;
use tmup::lockfile::{
    LockEntry, LockFile, config_fingerprint, read_lockfile, remote_plugin_config_hash,
};
use tmup::model::{Config, Options, PluginSource, PluginSpec, Tracking};
use tmup::progress::{NullReporter, PluginStage, ProgressEvent, ProgressReporter};
use tmup::state::{FailureMarker, Paths, build_command_hash};
use tmup::sync::{self, SyncMode, SyncPolicy};
use utils::*;

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

fn plugin_head(paths: &Paths, id: &str) -> String {
    git(&["rev-parse", "HEAD"], &paths.plugin_dir(id))
}

#[derive(Default)]
struct CaptureFailureStages {
    stages: Mutex<Vec<Option<PluginStage>>>,
}

impl CaptureFailureStages {
    fn take(&self) -> Vec<Option<PluginStage>> {
        std::mem::take(&mut *self.stages.lock().unwrap())
    }
}

impl ProgressReporter for CaptureFailureStages {
    fn report(&self, event: ProgressEvent<'_>) {
        if let ProgressEvent::PluginFailed { stage, .. } = event {
            self.stages.lock().unwrap().push(stage);
        }
    }
}

#[tokio::test]
async fn sync_run_and_write_does_not_create_lockfile_for_unknown_target() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    let cfg = make_config(vec![make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        "file:///tmp/unused.git",
        Tracking::DefaultBranch,
        None,
    )]);
    let mut lock = LockFile::new();

    let err = sync::run_and_write(
        &cfg,
        &mut lock,
        &paths,
        Some("example.com/test/other"),
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("unknown plugin id"), "unexpected error: {err}");
    assert!(!paths.lockfile_path.exists(), "unknown target should not create a lockfile");
}

#[tokio::test]
async fn init_mode_skips_known_failed_publish_without_retrying_build() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let marker_path = dir.path().join("out-of-band-build-marker");
    let build_cmd = format!(": > \"{}\"; exit 1", marker_path.display());
    let plugin = make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        &clone_url,
        Tracking::DefaultBranch,
        Some(&build_cmd),
    );
    let old_plugin = make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        &clone_url,
        Tracking::DefaultBranch,
        Some("touch old-build.marker"),
    );
    let cfg = make_config(vec![plugin.clone()]);
    let mut lock = LockFile::new();
    let mut entry = LockEntry::default_branch("main", &commit);
    entry.config_hash = remote_plugin_config_hash(&old_plugin);
    lock.plugins.insert("example.com/test/plugin".into(), entry);

    let marker = FailureMarker {
        plugin_id: "example.com/test/plugin".into(),
        commit: commit.clone(),
        build_hash: build_command_hash(&build_cmd),
        build_command: build_cmd.clone(),
        failed_at: "now".into(),
        stderr_summary: "boom".into(),
    };
    tmup::state::write_failure_marker(&paths.failures_root, &marker).unwrap();

    let outcome = sync::run_and_write(
        &cfg,
        &mut lock,
        &paths,
        None,
        SyncPolicy::init(true),
        SyncMode::Init,
        &NullReporter,
    )
    .await
    .unwrap();

    assert!(outcome.plugin_failures.is_empty());
    assert!(
        !marker_path.exists(),
        "build should not be retried in init mode when a matching known-failure marker exists"
    );
}

#[tokio::test]
async fn init_mode_retries_publish_when_build_command_changes() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let marker_path = dir.path().join("build-retried.marker");
    let previous_build = "make install";
    let new_build = format!(": > \"{}\"; exit 1", marker_path.display());
    let plugin = make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        &clone_url,
        Tracking::DefaultBranch,
        Some(&new_build),
    );
    let old_plugin = make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        &clone_url,
        Tracking::DefaultBranch,
        Some(previous_build),
    );
    let cfg = make_config(vec![plugin]);

    let mut lock = LockFile::new();
    let mut entry = LockEntry::default_branch("main", &commit);
    entry.config_hash = remote_plugin_config_hash(&old_plugin);
    lock.plugins.insert("example.com/test/plugin".into(), entry);

    let marker = FailureMarker {
        plugin_id: "example.com/test/plugin".into(),
        commit: commit.clone(),
        build_hash: build_command_hash(previous_build),
        build_command: previous_build.into(),
        failed_at: "now".into(),
        stderr_summary: "boom".into(),
    };
    tmup::state::write_failure_marker(&paths.failures_root, &marker).unwrap();

    let outcome = sync::run_and_write(
        &cfg,
        &mut lock,
        &paths,
        None,
        SyncPolicy::init(true),
        SyncMode::Init,
        &NullReporter,
    )
    .await
    .unwrap();

    assert_eq!(outcome.plugin_failures.len(), 1);
    assert!(marker_path.exists(), "changed build command should retry publish/build in init mode");
}

#[tokio::test]
async fn sync_installs_new_remote_plugin_and_persists_metadata() {
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
        Some("touch built.marker"),
    );
    let cfg = make_config(vec![plugin.clone()]);
    let mut lock = LockFile::new();

    sync::run(&cfg, &mut lock, &paths, None, SyncPolicy::SYNC, SyncMode::Normal, &NullReporter)
        .await
        .unwrap();

    let entry = lock.plugins.get("example.com/test/plugin").unwrap();
    let expected_config_hash = remote_plugin_config_hash(&plugin).unwrap();
    let expected_fingerprint = config_fingerprint(&cfg);
    assert_eq!(entry.commit, commit);
    assert_eq!(entry.tracking.kind, "default-branch");
    assert_eq!(entry.tracking.value, "main");
    assert_eq!(entry.config_hash.as_deref(), Some(expected_config_hash.as_str()));
    assert_eq!(lock.config_fingerprint.as_deref(), Some(expected_fingerprint.as_str()));
    assert_eq!(plugin_head(&paths, "example.com/test/plugin"), commit);
    assert!(paths.plugin_dir("example.com/test/plugin").join("built.marker").exists());
}

#[tokio::test]
async fn sync_creates_repo_cache_for_remote_plugin() {
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

    sync::run(&cfg, &mut lock, &paths, None, SyncPolicy::SYNC, SyncMode::Normal, &NullReporter)
        .await
        .unwrap();

    assert!(paths.repo_cache_dir("example.com/test/plugin").exists());
    assert_eq!(plugin_head(&paths, "example.com/test/plugin"), commit);
}

#[tokio::test]
async fn sync_prepare_checkout_failure_reports_checking_out_stage() {
    let dir = tempdir().unwrap();
    let (bare, _commit) = make_bare_repo(&dir.path().join("repo"));
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
    lock.plugins.insert(
        "example.com/test/plugin".into(),
        LockEntry {
            tracking: tmup::lockfile::TrackingRecord {
                kind: "default-branch".into(),
                value: "main".into(),
            },
            // Deliberately absent from remote refs so checkout fails in staging.
            commit: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into(),
            config_hash: Some("placeholder-hash".into()),
        },
    );

    let reporter = CaptureFailureStages::default();
    let outcome =
        sync::run(&cfg, &mut lock, &paths, None, SyncPolicy::SYNC, SyncMode::Normal, &reporter)
            .await
            .unwrap();

    assert_eq!(outcome.plugin_failures.len(), 1);
    let stages = reporter.take();
    assert_eq!(stages.len(), 1);
    assert!(matches!(stages[0], Some(PluginStage::CheckingOut)));
}

#[tokio::test]
async fn sync_failed_rebuild_preserves_existing_target_and_lock_snapshot() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let plugin_id = "example.com/test/plugin";
    let old_plugin = make_plugin(
        "test/plugin",
        plugin_id,
        &clone_url,
        Tracking::DefaultBranch,
        Some("touch built-v1"),
    );
    let new_plugin = make_plugin(
        "test/plugin",
        plugin_id,
        &clone_url,
        Tracking::DefaultBranch,
        Some(": > staged-only.marker; exit 1"),
    );

    let cfg_old = make_config(vec![old_plugin.clone()]);
    let cfg_new = make_config(vec![new_plugin]);
    let mut lock = LockFile::new();

    sync::run_and_write(
        &cfg_old,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    let target = paths.plugin_dir(plugin_id);
    let previous_hash = lock.plugins[plugin_id].config_hash.clone();
    assert_eq!(plugin_head(&paths, plugin_id), commit);
    assert!(target.join("built-v1").exists(), "initial sync should publish built artifacts");

    let outcome = sync::run_and_write(
        &cfg_new,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    assert_eq!(
        outcome.plugin_failures.len(),
        1,
        "rebuild failure should surface as plugin failure"
    );
    assert_eq!(
        plugin_head(&paths, plugin_id),
        commit,
        "failed staged build must leave the previously installed commit untouched"
    );
    assert!(
        target.join("built-v1").exists(),
        "failed staged build must not replace the existing target directory"
    );
    assert!(
        !target.join("staged-only.marker").exists(),
        "staging-only build outputs must not leak into the installed target"
    );
    assert_eq!(
        lock.plugins[plugin_id].config_hash, previous_hash,
        "failed rebuild must not advance the in-memory lock entry"
    );
    assert!(
        !paths.staging_dir(plugin_id).exists(),
        "failed sync should clean up the temporary staging checkout"
    );

    let persisted = read_lockfile(&paths.lockfile_path).unwrap();
    assert_eq!(persisted.plugins[plugin_id].commit, commit);
    assert_eq!(persisted.plugins[plugin_id].config_hash, previous_hash);
}

#[tokio::test]
async fn init_mode_reconciles_missing_repo_even_when_lock_hash_is_aligned() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let plugin_id = "example.com/test/plugin";
    let cfg = make_config(vec![make_plugin(
        "test/plugin",
        plugin_id,
        &clone_url,
        Tracking::DefaultBranch,
        None,
    )]);

    let mut lock = LockFile::new();
    sync::run(&cfg, &mut lock, &paths, None, SyncPolicy::SYNC, SyncMode::Normal, &NullReporter)
        .await
        .unwrap();
    assert_eq!(plugin_head(&paths, plugin_id), commit);

    std::fs::remove_dir_all(paths.plugin_dir(plugin_id)).unwrap();
    assert!(!paths.plugin_dir(plugin_id).exists());

    let outcome = sync::run(
        &cfg,
        &mut lock,
        &paths,
        None,
        SyncPolicy::init(true),
        SyncMode::Init,
        &NullReporter,
    )
    .await
    .unwrap();

    assert!(
        outcome.is_clean(),
        "missing repo should be reconciled as a normal sync action, not reported as plugin failure"
    );
    assert!(
        paths.plugin_dir(plugin_id).exists(),
        "init-mode sync must recreate missing plugin repo even when lock/config hashes are aligned"
    );
    assert_eq!(plugin_head(&paths, plugin_id), commit);
}

#[tokio::test]
async fn init_mode_reconciles_drifted_head_even_when_lock_hash_is_aligned() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let plugin_id = "example.com/test/plugin";
    let cfg = make_config(vec![make_plugin(
        "test/plugin",
        plugin_id,
        &clone_url,
        Tracking::DefaultBranch,
        None,
    )]);

    let mut lock = LockFile::new();
    sync::run(&cfg, &mut lock, &paths, None, SyncPolicy::SYNC, SyncMode::Normal, &NullReporter)
        .await
        .unwrap();
    assert_eq!(plugin_head(&paths, plugin_id), commit);

    let plugin_dir = paths.plugin_dir(plugin_id);
    std::fs::write(plugin_dir.join("drift.txt"), "drift\n").unwrap();
    git(&["add", "."], &plugin_dir);
    git(&["commit", "-m", "drift"], &plugin_dir);
    let drifted = plugin_head(&paths, plugin_id);
    assert_ne!(drifted, commit);

    let outcome = sync::run(
        &cfg,
        &mut lock,
        &paths,
        None,
        SyncPolicy::init(true),
        SyncMode::Init,
        &NullReporter,
    )
    .await
    .unwrap();

    assert!(
        outcome.is_clean(),
        "drifted repo head should be reconciled as normal sync work, not reported as plugin failure"
    );
    assert_eq!(
        plugin_head(&paths, plugin_id),
        commit,
        "init-mode sync must restore drifted repo head to the locked commit when hashes are aligned"
    );
}

#[tokio::test]
async fn sync_reconciles_branch_to_tag_and_commit_transitions() {
    let dir = tempdir().unwrap();
    let (bare, commit_a) = make_bare_repo(&dir.path().join("repo"));
    let commit_b = push_commit(&bare, "second");
    push_tag(&bare, "v1.0.0", &commit_a);

    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let mut lock = LockFile::new();

    let branch_cfg = make_config(vec![make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        &clone_url,
        Tracking::Branch("main".into()),
        None,
    )]);
    sync::run(
        &branch_cfg,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();
    assert_eq!(plugin_head(&paths, "example.com/test/plugin"), commit_b);

    let tag_cfg = make_config(vec![make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        &clone_url,
        Tracking::Tag("v1.0.0".into()),
        None,
    )]);
    sync::run(&tag_cfg, &mut lock, &paths, None, SyncPolicy::SYNC, SyncMode::Normal, &NullReporter)
        .await
        .unwrap();
    let entry = lock.plugins.get("example.com/test/plugin").unwrap();
    assert_eq!(entry.tracking.kind, "tag");
    assert_eq!(entry.tracking.value, "v1.0.0");
    assert_eq!(entry.commit, commit_a);
    assert_eq!(plugin_head(&paths, "example.com/test/plugin"), commit_a);

    let commit_cfg = make_config(vec![make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        &clone_url,
        Tracking::Commit(commit_b.clone()),
        None,
    )]);
    sync::run(
        &commit_cfg,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();
    let entry = lock.plugins.get("example.com/test/plugin").unwrap();
    assert_eq!(entry.tracking.kind, "commit");
    assert_eq!(entry.tracking.value, commit_b);
    assert_eq!(entry.commit, entry.tracking.value);
    assert_eq!(plugin_head(&paths, "example.com/test/plugin"), entry.commit);
}

#[tokio::test]
async fn sync_prefers_tag_ref_when_branch_and_tag_names_conflict() {
    let dir = tempdir().unwrap();
    let (bare, tagged_commit) = make_bare_repo(&dir.path().join("repo"));
    push_tag(&bare, "same", &tagged_commit);
    let branch_commit = push_branch_commit(&bare, "same", "branch-head");

    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let cfg = make_config(vec![make_plugin(
        "test/plugin",
        "example.com/test/plugin",
        &clone_url,
        Tracking::Tag("same".into()),
        None,
    )]);
    let mut lock = LockFile::new();

    sync::run(&cfg, &mut lock, &paths, None, SyncPolicy::SYNC, SyncMode::Normal, &NullReporter)
        .await
        .unwrap();

    let entry = lock.plugins.get("example.com/test/plugin").unwrap();
    assert_eq!(entry.tracking.kind, "tag");
    assert_eq!(entry.tracking.value, "same");
    assert_eq!(entry.commit, tagged_commit);
    assert_ne!(entry.commit, branch_commit);
    assert_eq!(plugin_head(&paths, "example.com/test/plugin"), tagged_commit);
}

#[tokio::test]
async fn sync_updates_only_the_targeted_plugin_id() {
    let dir = tempdir().unwrap();
    let (bare_a, commit_a) = make_bare_repo(&dir.path().join("repo-a"));
    let (bare_b, commit_b_main) = make_bare_repo(&dir.path().join("repo-b"));
    let commit_b_feature = push_branch_commit(&bare_b, "feature", "feature");

    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_a = format!("file://{}", bare_a.display());
    let clone_b = format!("file://{}", bare_b.display());

    let cfg_initial = make_config(vec![
        make_plugin(
            "test/plugin-a",
            "example.com/test/plugin-a",
            &clone_a,
            Tracking::Branch("main".into()),
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
    sync::run(
        &cfg_initial,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    assert_eq!(plugin_head(&paths, "example.com/test/plugin-a"), commit_a);
    assert_eq!(plugin_head(&paths, "example.com/test/plugin-b"), commit_b_main);

    let original_a_hash = lock.plugins["example.com/test/plugin-a"].config_hash.clone();

    let cfg_changed = make_config(vec![
        make_plugin(
            "test/plugin-a",
            "example.com/test/plugin-a",
            &clone_a,
            Tracking::Branch("feature".into()),
            None,
        ),
        make_plugin(
            "test/plugin-b",
            "example.com/test/plugin-b",
            &clone_b,
            Tracking::Branch("feature".into()),
            None,
        ),
    ]);

    sync::run(
        &cfg_changed,
        &mut lock,
        &paths,
        Some("example.com/test/plugin-b"),
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    assert_eq!(plugin_head(&paths, "example.com/test/plugin-a"), commit_a);
    assert_eq!(plugin_head(&paths, "example.com/test/plugin-b"), commit_b_feature);
    assert_eq!(lock.plugins["example.com/test/plugin-a"].config_hash, original_a_hash);
    assert_eq!(lock.plugins["example.com/test/plugin-a"].commit, commit_a);
    assert_eq!(lock.plugins["example.com/test/plugin-b"].commit, commit_b_feature);
}

#[tokio::test]
async fn sync_prunes_removed_lock_entries_without_deleting_repo_dirs() {
    let dir = tempdir().unwrap();
    let (bare_a, _commit_a) = make_bare_repo(&dir.path().join("repo-a"));
    let (bare_b, _commit_b) = make_bare_repo(&dir.path().join("repo-b"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_a = format!("file://{}", bare_a.display());
    let clone_b = format!("file://{}", bare_b.display());
    let mut lock = LockFile::new();

    let cfg_initial = make_config(vec![
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
    sync::run(
        &cfg_initial,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    let removed_dir = paths.plugin_dir("example.com/test/plugin-b");
    assert!(removed_dir.exists());

    let cfg_removed = make_config(vec![make_plugin(
        "test/plugin-a",
        "example.com/test/plugin-a",
        &clone_a,
        Tracking::DefaultBranch,
        None,
    )]);
    sync::run(
        &cfg_removed,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    assert!(lock.plugins.contains_key("example.com/test/plugin-a"));
    assert!(!lock.plugins.contains_key("example.com/test/plugin-b"));
    assert!(removed_dir.exists(), "sync should not delete repo directories");
}

#[tokio::test]
async fn sync_rebuilds_same_commit_when_only_build_changes_and_rewrites_markers() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let plugin_id = "example.com/test/plugin";
    let cfg_initial = make_config(vec![make_plugin(
        "test/plugin",
        plugin_id,
        &clone_url,
        Tracking::DefaultBranch,
        Some("touch built-v1.marker"),
    )]);
    let mut lock = LockFile::new();
    sync::run(
        &cfg_initial,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    let target = paths.plugin_dir(plugin_id);
    assert!(target.join("built-v1.marker").exists());

    let cfg_fail = make_config(vec![make_plugin(
        "test/plugin",
        plugin_id,
        &clone_url,
        Tracking::DefaultBranch,
        Some("touch built-v2.marker; exit 1"),
    )]);
    let result = sync::run(
        &cfg_fail,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await;
    let outcome = result.expect("failing rebuild should return SyncOutcome");
    assert!(!outcome.is_clean(), "failing rebuild should be recorded as plugin failure");
    assert_eq!(outcome.plugin_failures.len(), 1);
    assert!(
        outcome.plugin_failures[0].contains(plugin_id),
        "plugin failure should include plugin id"
    );
    assert!(
        outcome.plugin_failures[0].starts_with(&format!("{plugin_id}:")),
        "plugin failure should keep aggregate message shape `<plugin_id>: <error>`"
    );
    assert_eq!(plugin_head(&paths, plugin_id), commit);
    assert!(target.join("built-v1.marker").exists());
    assert!(!target.join("built-v2.marker").exists());

    let fail_hash = build_command_hash("touch built-v2.marker; exit 1");
    let markers = tmup::state::read_failure_markers(&paths.failures_root).unwrap();
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].plugin_id, plugin_id);
    assert_eq!(markers[0].commit, commit);
    assert_eq!(markers[0].build_hash, fail_hash);

    let previous_hash = lock.plugins[plugin_id].config_hash.clone();

    let cfg_success = make_config(vec![make_plugin(
        "test/plugin",
        plugin_id,
        &clone_url,
        Tracking::DefaultBranch,
        Some("touch built-v2.marker"),
    )]);
    sync::run(
        &cfg_success,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    assert_eq!(plugin_head(&paths, plugin_id), commit);
    assert!(!target.join("built-v1.marker").exists());
    assert!(target.join("built-v2.marker").exists());
    assert!(tmup::state::read_failure_markers(&paths.failures_root).unwrap().is_empty());
    assert_ne!(lock.plugins[plugin_id].config_hash, previous_hash);
}

#[tokio::test]
async fn sync_does_not_advance_floating_commit_when_only_build_changes() {
    let dir = tempdir().unwrap();
    let (bare, commit_a) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let plugin_id = "example.com/test/plugin";

    let cfg_initial = make_config(vec![make_plugin(
        "test/plugin",
        plugin_id,
        &clone_url,
        Tracking::DefaultBranch,
        Some("touch built-v1.marker"),
    )]);
    let mut lock = LockFile::new();
    sync::run(
        &cfg_initial,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    let commit_b = push_commit(&bare, "second");
    assert_ne!(commit_a, commit_b);

    let cfg_changed = make_config(vec![make_plugin(
        "test/plugin",
        plugin_id,
        &clone_url,
        Tracking::DefaultBranch,
        Some("touch built-v2.marker"),
    )]);
    sync::run(
        &cfg_changed,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    let entry = lock.plugins.get(plugin_id).unwrap();
    let target = paths.plugin_dir(plugin_id);
    assert_eq!(entry.commit, commit_a);
    assert_eq!(plugin_head(&paths, plugin_id), commit_a);
    assert!(!target.join("built-v1.marker").exists());
    assert!(target.join("built-v2.marker").exists());
}

#[tokio::test]
async fn sync_republishes_clean_tree_when_build_is_removed_at_same_commit() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let plugin_id = "example.com/test/plugin";

    let cfg_with_build = make_config(vec![make_plugin(
        "test/plugin",
        plugin_id,
        &clone_url,
        Tracking::DefaultBranch,
        Some("touch built-v1.marker"),
    )]);
    let mut lock = LockFile::new();
    sync::run(
        &cfg_with_build,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    let target = paths.plugin_dir(plugin_id);
    assert_eq!(plugin_head(&paths, plugin_id), commit);
    assert!(target.join("built-v1.marker").exists());

    let cfg_without_build = make_config(vec![make_plugin(
        "test/plugin",
        plugin_id,
        &clone_url,
        Tracking::DefaultBranch,
        None,
    )]);
    sync::run(
        &cfg_without_build,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    let entry = lock.plugins.get(plugin_id).unwrap();
    assert_eq!(entry.commit, commit);
    assert!(!target.join("built-v1.marker").exists());
}

#[tokio::test]
async fn sync_rebuilds_when_build_changes_and_tracking_changes_to_same_commit() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let plugin_id = "example.com/test/plugin";

    let cfg_initial = make_config(vec![make_plugin(
        "test/plugin",
        plugin_id,
        &clone_url,
        Tracking::DefaultBranch,
        Some("touch built-v1.marker"),
    )]);
    let mut lock = LockFile::new();
    sync::run(
        &cfg_initial,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    let target = paths.plugin_dir(plugin_id);
    assert_eq!(plugin_head(&paths, plugin_id), commit);
    assert!(target.join("built-v1.marker").exists());

    let cfg_changed = make_config(vec![make_plugin(
        "test/plugin",
        plugin_id,
        &clone_url,
        Tracking::Branch("main".into()),
        Some("touch built-v2.marker"),
    )]);
    sync::run(
        &cfg_changed,
        &mut lock,
        &paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();

    let entry = lock.plugins.get(plugin_id).unwrap();
    assert_eq!(entry.commit, commit);
    assert_eq!(entry.tracking.kind, "branch");
    assert_eq!(entry.tracking.value, "main");
    assert!(!target.join("built-v1.marker").exists());
    assert!(target.join("built-v2.marker").exists());
}

#[tokio::test]
async fn concurrent_sync_installs_multiple_plugins() {
    let dir = tempdir().unwrap();
    let (bare_a, commit_a) = make_bare_repo(&dir.path().join("repo-a"));
    let (bare_b, commit_b) = make_bare_repo(&dir.path().join("repo-b"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let url_a = format!("file://{}", bare_a.display());
    let url_b = format!("file://{}", bare_b.display());

    let mut cfg = make_config(vec![
        make_plugin("test/a", "example.com/test/a", &url_a, Tracking::DefaultBranch, None),
        make_plugin("test/b", "example.com/test/b", &url_b, Tracking::DefaultBranch, None),
    ]);
    cfg.options.concurrency = 2;

    let mut lock = LockFile::new();
    let outcome =
        sync::run(&cfg, &mut lock, &paths, None, SyncPolicy::SYNC, SyncMode::Normal, &NullReporter)
            .await
            .unwrap();

    assert!(outcome.is_clean(), "unexpected failures: {:?}", outcome.plugin_failures);
    assert_eq!(lock.plugins["example.com/test/a"].commit, commit_a);
    assert_eq!(lock.plugins["example.com/test/b"].commit, commit_b);
}

#[tokio::test]
async fn concurrent_sync_partial_failure_preserves_successes() {
    let dir = tempdir().unwrap();
    let (bare_a, commit_a) = make_bare_repo(&dir.path().join("repo-a"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let url_a = format!("file://{}", bare_a.display());
    let url_bad = "file:///nonexistent/repo.git".to_string();

    let mut cfg = make_config(vec![
        make_plugin("test/a", "example.com/test/a", &url_a, Tracking::DefaultBranch, None),
        make_plugin("test/bad", "example.com/test/bad", &url_bad, Tracking::DefaultBranch, None),
    ]);
    cfg.options.concurrency = 2;

    let mut lock = LockFile::new();
    let outcome =
        sync::run(&cfg, &mut lock, &paths, None, SyncPolicy::SYNC, SyncMode::Normal, &NullReporter)
            .await
            .unwrap();

    assert!(!outcome.is_clean());
    assert_eq!(outcome.plugin_failures.len(), 1);
    assert!(outcome.plugin_failures[0].contains("example.com/test/bad"));
    assert_eq!(lock.plugins["example.com/test/a"].commit, commit_a);
    assert!(!lock.plugins.contains_key("example.com/test/bad"));
}

#[tokio::test]
async fn sync_lockfile_is_identical_for_serial_and_parallel_prepare() {
    let dir = tempdir().unwrap();
    let (bare_a, _commit_a) = make_bare_repo(&dir.path().join("repo-a"));
    let (bare_b, _commit_b) = make_bare_repo(&dir.path().join("repo-b"));

    let url_a = format!("file://{}", bare_a.display());
    let url_b = format!("file://{}", bare_b.display());
    let base_cfg = make_config(vec![
        make_plugin("test/a", "example.com/test/a", &url_a, Tracking::DefaultBranch, None),
        make_plugin("test/b", "example.com/test/b", &url_b, Tracking::DefaultBranch, None),
    ]);

    let serial_paths =
        Paths::for_test(dir.path().join("data-serial"), dir.path().join("state-serial"));
    serial_paths.ensure_dirs().unwrap();
    let mut serial_cfg = base_cfg.clone();
    serial_cfg.options.concurrency = 1;
    let mut serial_lock_state = LockFile::new();
    let serial_outcome = sync::run_and_write(
        &serial_cfg,
        &mut serial_lock_state,
        &serial_paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();
    assert!(
        serial_outcome.is_clean(),
        "unexpected serial sync failures: {:?}",
        serial_outcome.plugin_failures
    );
    let serial_lock = read_lockfile(&serial_paths.lockfile_path).unwrap();

    let parallel_paths =
        Paths::for_test(dir.path().join("data-parallel"), dir.path().join("state-parallel"));
    parallel_paths.ensure_dirs().unwrap();
    let mut parallel_cfg = base_cfg.clone();
    parallel_cfg.options.concurrency = 2;
    let mut parallel_lock_state = LockFile::new();
    let parallel_outcome = sync::run_and_write(
        &parallel_cfg,
        &mut parallel_lock_state,
        &parallel_paths,
        None,
        SyncPolicy::SYNC,
        SyncMode::Normal,
        &NullReporter,
    )
    .await
    .unwrap();
    assert!(
        parallel_outcome.is_clean(),
        "unexpected parallel sync failures: {:?}",
        parallel_outcome.plugin_failures
    );
    let parallel_lock = read_lockfile(&parallel_paths.lockfile_path).unwrap();
    assert_eq!(serial_lock, parallel_lock, "lockfile contents must be stable across concurrency");
}

#[tokio::test]
async fn concurrent_sync_all_prepare_failures_report_all_and_write_no_entries() {
    let dir = tempdir().unwrap();
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let mut cfg = make_config(vec![
        make_plugin(
            "test/missing-a",
            "example.com/test/missing-a",
            "file:///definitely/missing/repo-a.git",
            Tracking::DefaultBranch,
            None,
        ),
        make_plugin(
            "test/missing-b",
            "example.com/test/missing-b",
            "file:///definitely/missing/repo-b.git",
            Tracking::DefaultBranch,
            None,
        ),
    ]);
    cfg.options.concurrency = 2;

    let mut lock = LockFile::new();
    let outcome =
        sync::run(&cfg, &mut lock, &paths, None, SyncPolicy::SYNC, SyncMode::Normal, &NullReporter)
            .await
            .unwrap();

    assert!(!outcome.is_clean(), "expected aggregated plugin failures");
    assert_eq!(outcome.plugin_failures.len(), 2);
    assert!(outcome.plugin_failures.iter().any(|f| f.contains("example.com/test/missing-a")));
    assert!(outcome.plugin_failures.iter().any(|f| f.contains("example.com/test/missing-b")));
    assert!(
        lock.plugins.is_empty(),
        "sync must not write successful lock entries when every plugin prepare fails"
    );
}
