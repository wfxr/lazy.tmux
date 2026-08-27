use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;

use crate::lockfile::{
    LockEntry, LockFile, TrackingRecord, config_fingerprint, remote_plugin_config_hash,
    write_lockfile_atomic,
};
use crate::model::{Config, PluginSource, PluginSpec, Tracking};
use crate::progress::{
    PluginOutcome, PluginStage, PluginStageDetail, ProgressEvent, ProgressReporter, SkipReason,
};
use crate::state::{self, FailureMarker, Paths, build_command_hash, timestamp_now};
use crate::{git, planner, plugin, prepare, repo, short_hash};

/// Controls behavioral differences between an interactive sync and tmux init.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// Standard sync triggered by an explicit user command.
    Normal,
    /// Sync triggered automatically during tmux session initialisation.
    Init,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
/// Outcome produced by sync-phase execution.
///
/// `Ok(SyncOutcome)` may still contain per-plugin failures in `plugin_failures`.
/// `Err` is reserved for operation-level failures that make the sync command
/// unsafe or impossible to continue.
pub struct SyncOutcome {
    /// Human-readable failure messages, each formatted as `"<id>: <error>"`.
    pub plugin_failures: Vec<String>,
    /// Canonical remote plugin IDs that must not load after this sync pass.
    load_excluded_plugin_ids: HashSet<String>,
}

impl SyncOutcome {
    /// Returns `true` when no plugin failures were recorded.
    pub fn is_clean(&self) -> bool {
        self.plugin_failures.is_empty()
    }

    /// Return canonical IDs for plugins that must not load after this sync pass.
    pub fn load_excluded_plugin_ids(&self) -> &HashSet<String> {
        &self.load_excluded_plugin_ids
    }

    fn record_plugin_failure(&mut self, id: &str, failure: String) {
        self.load_excluded_plugin_ids.insert(id.to_string());
        self.plugin_failures.push(failure);
    }

    fn exclude_plugin_from_load(&mut self, id: &str) {
        self.load_excluded_plugin_ids.insert(id.to_string());
    }
}

/// Governs which categories of plugin work are permitted during a sync pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncPolicy {
    /// Whether to clone and publish plugins that are not yet installed.
    pub install_new_plugins: bool,
    /// Whether to re-clone or rebuild plugins whose state is out of date.
    pub repair_existing_plugins: bool,
    /// Whether to remove lock entries for plugins no longer in the config.
    pub prune_removed_plugins: bool,
}

impl SyncPolicy {
    /// Full sync: install, repair, and prune.
    pub const SYNC: Self = Self {
        install_new_plugins: true,
        repair_existing_plugins: true,
        prune_removed_plugins: true,
    };
    /// Alias for [`Self::SYNC`] used by the install subcommand.
    pub const INSTALL: Self = Self::SYNC;
    /// Alias for [`Self::SYNC`] used by the update subcommand.
    pub const UPDATE: Self = Self::SYNC;
    /// Alias for [`Self::SYNC`] used by the restore subcommand.
    pub const RESTORE: Self = Self::SYNC;
    /// Prune-only: removes stale plugins without installing or repairing.
    pub const CLEAN: Self = Self {
        install_new_plugins: false,
        repair_existing_plugins: false,
        prune_removed_plugins: true,
    };

    /// Builds the policy used during tmux init, optionally allowing installation.
    pub fn init(auto_install: bool) -> Self {
        Self {
            install_new_plugins: auto_install,
            repair_existing_plugins: true,
            prune_removed_plugins: true,
        }
    }
}

/// Read-only preview: does the given config/lock/policy combination require
/// real plugin work (clone, publish, build)?  Used by `init` to decide whether
/// to pop up a visible UI.
pub struct SyncPreview {
    /// `true` when at least one plugin would require clone, publish, or build work.
    pub needs_work: bool,
}

/// Inspects whether any plugin would need real work under the given policy without executing it.
pub fn preview(
    config: &Config,
    lock: &LockFile,
    target_id: Option<&str>,
    policy: SyncPolicy,
    paths: &Paths,
) -> SyncPreview {
    let desired_hashes = desired_remote_hashes(config);
    for spec in config.plugins.iter().filter(|spec| match spec.remote_id() {
        Some(id) => target_id.is_none_or(|target| target == id),
        None => false,
    }) {
        let id = spec.remote_id().unwrap();
        let Some(desired_hash) = desired_hashes.get(id) else {
            continue;
        };
        if plugin_needs_reconcile(spec, lock, desired_hash, policy, paths) {
            return SyncPreview { needs_work: true };
        }
    }
    SyncPreview { needs_work: false }
}

/// Executes the sync phase and returns a structured outcome.
///
/// Plugin-level failures are collected in [`SyncOutcome::plugin_failures`] and
/// still return `Ok`. `Err` is reserved for operation-level failures that make
/// the sync phase unsafe or impossible to continue.
pub async fn run(
    config: &Config,
    lock: &mut LockFile,
    paths: &Paths,
    target_id: Option<&str>,
    policy: SyncPolicy,
    mode: SyncMode,
    reporter: &dyn ProgressReporter,
) -> Result<SyncOutcome> {
    config.validate_target_id(target_id)?;

    let desired_hashes = desired_remote_hashes(config);
    let mut outcome = SyncOutcome::default();

    // Phase 1: Select candidates that need reconciliation.
    let candidates: Vec<&PluginSpec> = config
        .plugins
        .iter()
        .filter(|spec| match spec.remote_id() {
            Some(id) => target_id.is_none_or(|target| target == id),
            None => false,
        })
        .filter(|spec| {
            let id = spec.remote_id().unwrap();
            desired_hashes
                .get(id)
                .is_some_and(|hash| plugin_needs_reconcile(spec, lock, hash, policy, paths))
        })
        .collect();

    // Phase 2: Parallel prepare — resolve and stage each candidate concurrently.
    let prepare_jobs: Vec<_> = candidates
        .iter()
        .map(|spec| {
            let id = spec.remote_id().unwrap();
            let desired_hash = desired_hashes[id].clone();
            let current_entry = lock.plugins.get(id);
            resolve_desired_plugin(spec, current_entry, desired_hash, paths, reporter)
        })
        .collect();
    let prepare_results = prepare::run_bounded(config.options.concurrency, prepare_jobs).await;

    // Phase 3: Serial reconcile/apply in declaration order.
    for (spec, result) in candidates.iter().zip(prepare_results) {
        let id = spec.remote_id().unwrap();
        let name = spec.name.as_str();
        let PluginSource::Remote { clone_url, .. } = &spec.source else {
            unreachable!("sync only processes remote plugins")
        };
        let tracking = plugin::tracking_selector(&spec.tracking);
        match result {
            Ok(resolved) => {
                let resolved_commit = resolved.entry.commit.clone();
                match reconcile_plugin(spec, lock, paths, mode, resolved, reporter).await {
                    Ok(ReconcileDisposition::ReadyToLoad) => {}
                    Ok(ReconcileDisposition::KnownFailure) => {
                        outcome.exclude_plugin_from_load(id);
                    }
                    Err(err) => {
                        let failure = plugin::report_plugin_failure(
                            reporter,
                            id,
                            name,
                            PluginStage::Applying,
                            &err,
                            vec![
                                ("clone_url", clone_url.clone()),
                                ("tracking", tracking.clone()),
                                ("resolved_commit", resolved_commit),
                                ("target_dir", paths.plugin_dir(id).display().to_string()),
                            ],
                        );
                        outcome.record_plugin_failure(id, failure);
                    }
                }
            }
            Err(err) => {
                let failure = plugin::report_plugin_failure(
                    reporter,
                    id,
                    name,
                    err.stage,
                    &err.err,
                    vec![("clone_url", clone_url.clone()), ("tracking", tracking.clone())],
                );
                outcome.record_plugin_failure(id, failure);
            }
        }
    }

    if target_id.is_none() && policy.prune_removed_plugins {
        let desired_ids: std::collections::HashSet<_> = desired_hashes.keys().cloned().collect();
        lock.plugins.retain(|id, _| desired_ids.contains(id));
    }

    if lock_matches_config(config, lock) {
        lock.config_fingerprint = Some(config_fingerprint(config));
    }

    Ok(outcome)
}

/// Runs sync and then writes the lockfile atomically.
///
/// Plugin-level failures are returned in [`SyncOutcome`] within `Ok`. `Err` is
/// reserved for operation-level failures (for example invalid target id or
/// lockfile write failure).
pub async fn run_and_write(
    config: &Config,
    lock: &mut LockFile,
    paths: &Paths,
    target_id: Option<&str>,
    policy: SyncPolicy,
    mode: SyncMode,
    reporter: &dyn ProgressReporter,
) -> Result<SyncOutcome> {
    let outcome = run(config, lock, paths, target_id, policy, mode, reporter).await?;
    write_lockfile_atomic(&paths.lockfile_path, lock)?;
    Ok(outcome)
}

/// Returns `true` when the lockfile is out of date with respect to the config.
pub fn lock_is_stale(config: &Config, lock: &LockFile) -> bool {
    let has_remote_plugins = config.plugins.iter().any(|spec| spec.remote_id().is_some());
    if !has_remote_plugins {
        return !lock.plugins.is_empty();
    }

    let expected_fingerprint = config_fingerprint(config);
    if lock.config_fingerprint.as_deref() != Some(expected_fingerprint.as_str()) {
        return true;
    }

    !lock_matches_config(config, lock)
}

/// Returns `true` when every plugin's locked config hash matches the current config.
pub fn lock_matches_config(config: &Config, lock: &LockFile) -> bool {
    let desired = desired_remote_hashes(config);
    if lock.plugins.len() != desired.len() {
        return false;
    }

    desired.into_iter().all(|(id, expected_hash)| {
        lock.plugins.get(&id).and_then(|entry| entry.config_hash.as_deref())
            == Some(expected_hash.as_str())
    })
}

fn desired_remote_hashes(config: &Config) -> BTreeMap<String, String> {
    config
        .plugins
        .iter()
        .filter_map(|spec| Some((spec.remote_id()?.to_string(), remote_plugin_config_hash(spec)?)))
        .collect()
}

fn plugin_needs_reconcile(
    spec: &PluginSpec,
    lock: &LockFile,
    desired_hash: &str,
    policy: SyncPolicy,
    paths: &Paths,
) -> bool {
    let Some(id) = spec.remote_id() else {
        return false;
    };
    let current_entry = lock.plugins.get(id);
    let can_reconcile = if current_entry.is_none() {
        policy.install_new_plugins
    } else {
        policy.repair_existing_plugins
    };
    if !can_reconcile {
        return false;
    }
    let Some(current_entry) = current_entry else {
        return true;
    };
    // Different config/build hash always requires a sync write pass, even if the
    // on-disk repo currently points at the same commit.
    if current_entry.config_hash.as_deref() != Some(desired_hash) {
        return true;
    }
    match planner::inspect_plugin_dir(&paths.plugin_dir(id)) {
        planner::RepoHealth::Missing | planner::RepoHealth::Broken => true,
        planner::RepoHealth::Healthy { commit } => commit != current_entry.commit,
    }
}

struct ResolvedPlugin {
    id: String,
    staging_dir: PathBuf,
    entry: LockEntry,
}

enum ReconcileDisposition {
    ReadyToLoad,
    KnownFailure,
}

async fn resolve_desired_plugin(
    spec: &PluginSpec,
    current_entry: Option<&LockEntry>,
    config_hash: String,
    paths: &Paths,
    reporter: &dyn ProgressReporter,
) -> std::result::Result<ResolvedPlugin, plugin::PrepareStageError> {
    let PluginSource::Remote { id, clone_url, .. } = &spec.source else {
        unreachable!("sync only processes remote plugins");
    };

    let name = spec.name.as_str();
    reporter.report(ProgressEvent::PluginStage {
        id,
        name,
        stage: PluginStage::Fetching,
        detail: Some(PluginStageDetail::CloneUrl(clone_url.clone())),
    });

    let prep = async {
        let revision = if let Some(entry) = current_entry
            && tracks_same_revision(spec, &entry.tracking)
        {
            plugin::stage_result(
                PluginStage::Fetching,
                repo::ensure_locked_revision(paths, id, clone_url, &entry.commit).await,
            )?
        } else {
            plugin::resolve_tracking_revision_with_stages(paths, id, clone_url, &spec.tracking)
                .await?
        };
        let tracking = if let Some(entry) = current_entry
            && tracks_same_revision(spec, &entry.tracking)
        {
            entry.tracking.clone()
        } else {
            let tracking = revision.tracking.clone().expect("tracking metadata required");
            reporter.report(ProgressEvent::PluginStage {
                id,
                name,
                stage: PluginStage::Resolving,
                detail: Some(PluginStageDetail::from_tracking(
                    &spec.tracking,
                    &tracking,
                    &revision.commit,
                )),
            });
            tracking
        };
        reporter.report(ProgressEvent::PluginStage {
            id,
            name,
            stage: PluginStage::CheckingOut,
            detail: None,
        });
        let prepared = plugin::stage_result(
            PluginStage::CheckingOut,
            repo::materialize_staging_at_revision(paths, id, clone_url, &revision).await,
        )?;

        let commit = prepared.commit;
        Ok::<_, plugin::PrepareStageError>(LockEntry {
            tracking,
            commit,
            config_hash: Some(config_hash),
        })
        .map(|entry| (prepared.staging_dir, entry))
    }
    .await;

    prep.map(|(staging_dir, entry)| ResolvedPlugin { id: id.clone(), staging_dir, entry })
}

fn tracks_same_revision(spec: &PluginSpec, locked: &TrackingRecord) -> bool {
    match &spec.tracking {
        Tracking::DefaultBranch => locked.kind == "default-branch",
        Tracking::Branch(branch) => locked.kind == "branch" && locked.value == *branch,
        Tracking::Tag(tag) => locked.kind == "tag" && locked.value == *tag,
        Tracking::Commit(commit) => locked.kind == "commit" && locked.value == *commit,
    }
}

fn should_skip_known_failure(
    mode: SyncMode,
    paths: &Paths,
    id: &str,
    commit: &str,
    build: Option<&str>,
) -> Result<bool> {
    if mode != SyncMode::Init {
        return Ok(false);
    }
    let Some(build_cmd) = build else {
        return Ok(false);
    };
    plugin::is_known_failure(paths, id, commit, build_cmd)
}

async fn reconcile_plugin(
    spec: &PluginSpec,
    lock: &mut LockFile,
    paths: &Paths,
    mode: SyncMode,
    resolved: ResolvedPlugin,
    reporter: &dyn ProgressReporter,
) -> Result<ReconcileDisposition> {
    let ResolvedPlugin { id, staging_dir, entry } = resolved;

    let name = spec.name.as_str();
    let target_dir = paths.plugin_dir(&id);
    let current_entry = lock.plugins.get(&id).cloned();
    let health = planner::inspect_plugin_dir(&target_dir);

    let current_commit = match &health {
        planner::RepoHealth::Healthy { commit } => Some(commit.clone()),
        planner::RepoHealth::Missing | planner::RepoHealth::Broken => None,
    };

    if matches!(health, planner::RepoHealth::Broken) {
        let _ = std::fs::remove_dir_all(&target_dir);
    }

    let same_commit = current_commit.as_deref() == Some(entry.commit.as_str());
    let needs_republish =
        same_commit_config_change_requires_republish(current_entry.as_ref(), &entry);
    let needs_publish = current_entry.is_none()
        || matches!(health, planner::RepoHealth::Missing | planner::RepoHealth::Broken)
        || !same_commit
        || needs_republish;

    if !needs_publish {
        let _ = std::fs::remove_dir_all(&staging_dir);
        state::clear_failure_markers(&paths.failures_root, &id)?;
        lock.plugins.insert(id.clone(), entry);
        reporter.report(ProgressEvent::PluginFinished {
            id: &id,
            name,
            outcome: PluginOutcome::Reconciled,
        });
        return Ok(ReconcileDisposition::ReadyToLoad);
    }

    if should_skip_known_failure(mode, paths, &id, &entry.commit, spec.build.as_deref())? {
        reporter.report(ProgressEvent::PluginFinished {
            id: &id,
            name,
            outcome: PluginOutcome::Skipped {
                reason: SkipReason::KnownFailure { commit: short_hash(&entry.commit).to_string() },
            },
        });
        let _ = std::fs::remove_dir_all(&staging_dir);
        return Ok(ReconcileDisposition::KnownFailure);
    }

    reporter.report(ProgressEvent::PluginStage {
        id: &id,
        name,
        stage: PluginStage::Applying,
        detail: spec.build.clone().map(PluginStageDetail::BuildCommand),
    });

    let build = spec.build.as_deref();
    let publish_result = if target_dir.exists() {
        git::publish_replace(&staging_dir, &target_dir, build)
    } else {
        git::publish_fresh_install(&staging_dir, &target_dir, build)
    };

    match publish_result {
        Ok(()) => {
            let synced_commit = short_hash(&entry.commit).to_string();
            state::clear_failure_markers(&paths.failures_root, &id)?;
            lock.plugins.insert(id.clone(), entry);
            reporter.report(ProgressEvent::PluginFinished {
                id: &id,
                name,
                outcome: PluginOutcome::Synced { commit: synced_commit },
            });
            Ok(ReconcileDisposition::ReadyToLoad)
        }
        Err(err) => {
            let _ = std::fs::remove_dir_all(&staging_dir);
            record_failure_marker(paths, &id, &entry.commit, spec, &err)?;
            Err(err)
        }
    }
}

fn same_commit_config_change_requires_republish(
    current_entry: Option<&LockEntry>,
    desired_entry: &LockEntry,
) -> bool {
    let Some(current_entry) = current_entry else {
        return false;
    };
    if current_entry.commit != desired_entry.commit {
        return false;
    }
    if current_entry.config_hash == desired_entry.config_hash {
        return false;
    }
    true
}

fn record_failure_marker(
    paths: &Paths,
    id: &str,
    commit: &str,
    spec: &PluginSpec,
    err: &anyhow::Error,
) -> Result<()> {
    let Some(build_cmd) = &spec.build else {
        return Ok(());
    };

    let marker = FailureMarker {
        plugin_id: id.to_string(),
        commit: commit.to_string(),
        build_hash: build_command_hash(build_cmd),
        build_command: build_cmd.clone(),
        failed_at: timestamp_now(),
        stderr_summary: format!("{err}"),
    };
    state::write_failure_marker(&paths.failures_root, &marker)
}
