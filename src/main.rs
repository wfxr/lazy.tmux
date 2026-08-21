use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use owo_colors::OwoColorize;
use tabled::builder::Builder;
use tabled::settings::object::Segment;
use tabled::settings::{Alignment, Modify, Style};
use tmup::config_mode::{self, ConfigMode, TpmConfigPolicy};
use tmup::planner::{BuildStatus, PluginState, PluginStatus};
use tmup::progress::{self, NullReporter, OperationStage, ProgressEvent, ProgressReporter};
use tmup::state::{OperationLock, OperationLockGuard, Paths};
use tmup::sync::{self, SyncMode, SyncPolicy};
use tmup::{loader, lockfile, plugin, termui, tmux};

#[derive(Debug, Parser)]
#[command(name = "tmup", about = "Modern tmux plugin manager", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// tmux startup: install missing plugins, apply options, load plugins
    Init {
        #[arg(hide = true, long)]
        bootstrap: bool,
        #[arg(hide = true, long)]
        ui_child: bool,
        #[arg(hide = true, long)]
        wait_channel: Option<String>,
        #[arg(hide = true, long)]
        config_path: Option<PathBuf>,
        #[arg(hide = true, long)]
        tpm_config_path: Option<PathBuf>,
        #[arg(hide = true, long, conflicts_with = "tpm_config_path")]
        no_tpm_config: bool,
        #[arg(hide = true, long)]
        data_root: Option<PathBuf>,
        #[arg(hide = true, long)]
        state_root: Option<PathBuf>,
    },
    /// Install missing remote plugins
    Install {
        /// Plugin id to install (all if omitted)
        id: Option<String>,
    },
    /// Reconcile lock metadata and declared remote plugins with config
    Sync {
        /// Plugin id to sync (all if omitted)
        id: Option<String>,
    },
    /// Update remote plugins (the only command that advances lock)
    Update {
        /// Plugin id to update (all if omitted)
        id: Option<String>,
    },
    /// Restore plugins to lock-recorded commits
    Restore {
        /// Plugin id to restore (all if omitted)
        id: Option<String>,
    },
    /// Remove undeclared managed remote plugins
    Clean,
    /// List plugin status
    List {
        /// Show diagnostic columns including canonical id and source details
        #[arg(short, long)]
        verbose: bool,
    },
}

struct InitInvocation {
    bootstrap: bool,
    ui_child: bool,
    wait_channel: Option<String>,
    config_path: Option<PathBuf>,
    tpm_config_path: Option<PathBuf>,
    no_tpm_config: bool,
    data_root: Option<PathBuf>,
    state_root: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match resolve_requested_config_mode() {
        Ok(requested_config_mode) => match cli.command {
            Commands::Init {
                bootstrap,
                ui_child,
                wait_channel,
                config_path,
                tpm_config_path,
                no_tpm_config,
                data_root,
                state_root,
            } => {
                run_init(
                    InitInvocation {
                        bootstrap,
                        ui_child,
                        wait_channel,
                        config_path,
                        tpm_config_path,
                        no_tpm_config,
                        data_root,
                        state_root,
                    },
                    requested_config_mode,
                )
                .await
            }
            Commands::Install { id } => run_install(id, requested_config_mode).await,
            Commands::Sync { id } => run_sync(id, requested_config_mode).await,
            Commands::Update { id } => run_update(id, requested_config_mode).await,
            Commands::Restore { id } => run_restore(id, requested_config_mode).await,
            Commands::Clean => run_clean(requested_config_mode).await,
            Commands::List { verbose } => run_list(verbose, requested_config_mode),
        },
        Err(e) => Err(e),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Errors already shown by the progress reporter are suppressed here.
            if !progress::is_reported_error(&e) {
                eprintln!("tmup: {e:#}");
            }
            ExitCode::FAILURE
        }
    }
}

fn resolve_requested_config_mode() -> Result<ConfigMode> {
    match std::env::var("TMUP_CONFIG_MODE") {
        Ok(value) => parse_config_mode_env(&value),
        Err(std::env::VarError::NotPresent) => Ok(ConfigMode::Pure),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("TMUP_CONFIG_MODE contains invalid UTF-8")
        }
    }
}

fn parse_config_mode_env(value: &str) -> Result<ConfigMode> {
    let normalized = value.trim();
    match normalized {
        "pure" => Ok(ConfigMode::Pure),
        "mixed" => Ok(ConfigMode::Mixed),
        _ => {
            anyhow::bail!("invalid TMUP_CONFIG_MODE={value:?}: expected 'pure' or 'mixed'")
        }
    }
}

fn resolve_runtime_paths() -> Result<Paths> {
    if let Ok(path) = std::env::var("TMUP_CONFIG") {
        let path = resolve_explicit_config_path(PathBuf::from(path))?;
        anyhow::ensure!(
            path.is_file(),
            "TMUP_CONFIG={} must point to an existing file",
            path.display()
        );
        return Paths::resolve_with_config_path(Some(path));
    }
    Paths::resolve()
}

fn resolve_explicit_config_path(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()
            .context("failed to resolve current directory for TMUP_CONFIG")?
            .join(path))
    }
}

struct AppliedConfig {
    paths: Paths,
    config: tmup::model::Config,
    warnings: Vec<String>,
    tpm_config_policy: TpmConfigPolicy,
}

fn emit_config_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}

fn apply_config(paths: &Paths, mode: ConfigMode, create_missing: bool) -> Result<AppliedConfig> {
    let applied = apply_config_with_tpm_policy(paths, mode, create_missing, None)?;
    emit_config_warnings(&applied.warnings);
    Ok(applied)
}

fn init_tpm_config_policy(
    mode: ConfigMode,
    explicit_tpm_config_path: Option<PathBuf>,
    no_tpm_config: bool,
) -> TpmConfigPolicy {
    match mode {
        ConfigMode::Pure => TpmConfigPolicy::Disabled,
        ConfigMode::Mixed => {
            if no_tpm_config {
                TpmConfigPolicy::Resolved(None)
            } else if let Some(path) = explicit_tpm_config_path {
                TpmConfigPolicy::Resolved(Some(path))
            } else {
                TpmConfigPolicy::Discover
            }
        }
    }
}

fn apply_config_with_tpm_policy(
    paths: &Paths,
    mode: ConfigMode,
    create_missing: bool,
    explicit_tpm_policy: Option<TpmConfigPolicy>,
) -> Result<AppliedConfig> {
    let request = config_mode::LoadRequest::from_command(
        mode,
        create_missing,
        explicit_tpm_policy.unwrap_or_else(|| init_tpm_config_policy(mode, None, false)),
    );
    let loaded = config_mode::load_with_request(paths, request)?;
    Ok(AppliedConfig {
        paths: loaded.paths,
        config: loaded.config,
        warnings: loaded.warnings,
        tpm_config_policy: loaded.tpm_policy,
    })
}

fn load_lockfile(paths: &Paths) -> Result<lockfile::LockFile> {
    if paths.lockfile_path.exists() {
        lockfile::read_lockfile(&paths.lockfile_path)
    } else {
        Ok(lockfile::LockFile::new())
    }
}

// ---------------------------------------------------------------------------
// Init
// ---------------------------------------------------------------------------

async fn run_init(init: InitInvocation, config_mode: ConfigMode) -> Result<()> {
    if init.ui_child {
        return run_init_child(
            init.wait_channel.context("--ui-child requires --wait-channel")?,
            init.config_path.context("--ui-child requires --config-path")?,
            init_tpm_config_policy(config_mode, init.tpm_config_path, init.no_tpm_config),
            init.data_root.context("--ui-child requires --data-root")?,
            init.state_root.context("--ui-child requires --state-root")?,
            config_mode,
        )
        .await;
    }
    if init.bootstrap {
        return run_init_bootstrap(
            init.config_path.context("--bootstrap requires --config-path")?,
            init_tpm_config_policy(config_mode, init.tpm_config_path, init.no_tpm_config),
            init.data_root.context("--bootstrap requires --data-root")?,
            init.state_root.context("--bootstrap requires --state-root")?,
            config_mode,
        )
        .await;
    }
    run_init_parent(config_mode).await
}

fn build_init_bootstrap_spec(
    paths: &Paths,
    config_mode: ConfigMode,
    tpm_config_policy: &TpmConfigPolicy,
) -> Result<tmux::InitBootstrapSpec> {
    let exe = std::env::current_exe().context("failed to determine current executable")?;
    Ok(tmux::InitBootstrapSpec {
        exe,
        config_path: paths.config_path.clone(),
        tpm_config_policy: tpm_config_policy.clone(),
        data_root: paths.data_root().to_path_buf(),
        state_root: paths.state_root().to_path_buf(),
        config_mode,
    })
}

fn build_init_ui_child_spec(
    paths: &Paths,
    wait_channel: String,
    config_mode: ConfigMode,
    tpm_config_policy: &TpmConfigPolicy,
) -> Result<tmux::InitUiChildSpec> {
    let exe = std::env::current_exe().context("failed to determine current executable")?;
    Ok(tmux::InitUiChildSpec {
        exe,
        config_path: paths.config_path.clone(),
        tpm_config_policy: tpm_config_policy.clone(),
        data_root: paths.data_root().to_path_buf(),
        state_root: paths.state_root().to_path_buf(),
        wait_channel,
        config_mode,
    })
}

fn read_and_cleanup_init_result(path: &std::path::Path) -> Result<i32> {
    let result = read_init_result(path);
    let _ = std::fs::remove_file(path);
    result
}

async fn run_init_inline_fast_path(paths: &Paths, cfg: &tmup::model::Config) -> Result<()> {
    match OperationLock::try_acquire(&paths.lock_path)? {
        Some(guard) => run_init_inline(paths, cfg, guard).await,
        None => {
            let _ = tmux::display_message("tmup: waiting for another operation...");
            let guard = OperationLock::acquire(&paths.lock_path)?;
            run_init_inline(paths, cfg, guard).await
        }
    }
}

async fn run_init_with_ui_mode(
    paths: &Paths,
    target: &tmux::InitUiTarget,
    mode: tmux::InitUiMode,
    config_mode: ConfigMode,
    tpm_config_policy: &TpmConfigPolicy,
) -> Result<i32> {
    let wait_channel = format!("tmup-init-{}-{}", std::process::id(), epoch_millis());
    let result_file = paths.init_result_path(&wait_channel);
    let _ = std::fs::remove_file(&result_file);
    let spec = build_init_ui_child_spec(paths, wait_channel, config_mode, tpm_config_policy)?;

    match mode {
        tmux::InitUiMode::Popup { supports_title } => {
            tmux::spawn_init_popup(&spec, target, &result_file, supports_title)?;
            read_and_cleanup_init_result(&result_file).context("reading popup init result")
        }
        tmux::InitUiMode::Split => {
            tmux::spawn_init_split(&spec, target, &result_file)?;
            tmux::wait_for(&spec.wait_channel)?;
            read_and_cleanup_init_result(&result_file).context("reading split init result")
        }
        tmux::InitUiMode::Inline => {
            unreachable!("inline mode should bypass tmux UI spawning")
        }
    }
}

/// Parent init flow: preview whether work is needed, then either run inline,
/// launch popup/split immediately when a usable tmux target already exists,
/// or schedule a deferred bootstrap for cold startup.
async fn run_init_parent(config_mode: ConfigMode) -> Result<()> {
    let paths = resolve_runtime_paths()?;
    paths.ensure_dirs()?;
    let applied = apply_config_with_tpm_policy(&paths, config_mode, false, None)?;
    let paths = applied.paths;
    let cfg = applied.config;
    let warnings = applied.warnings;
    let tpm_config_policy = applied.tpm_config_policy;

    // Lock-free preview: does this init need visible work?
    let lock = load_lockfile(&paths)?;
    let sync_preview =
        sync::preview(&cfg, &lock, None, SyncPolicy::init(cfg.options.auto_install), &paths);
    let needs_ui = sync_preview.needs_work;

    if !needs_ui {
        emit_config_warnings(&warnings);
        return run_init_inline_fast_path(&paths, &cfg).await;
    }

    let ui_mode = tmux::init_ui_mode();
    if matches!(ui_mode, tmux::InitUiMode::Inline) {
        emit_config_warnings(&warnings);
        let guard = OperationLock::acquire(&paths.lock_path)?;
        return run_init_inline(&paths, &cfg, guard).await;
    }

    if let Some(target) = tmux::current_init_ui_target() {
        let exit_code =
            run_init_with_ui_mode(&paths, &target, ui_mode, config_mode, &tpm_config_policy)
                .await?;
        return if exit_code == 0 { Ok(()) } else { Err(progress::reported_error()) };
    }

    let spec = build_init_bootstrap_spec(&paths, config_mode, &tpm_config_policy)?;
    if tmux::spawn_init_bootstrap(&spec).is_ok() {
        return Ok(());
    }

    let _ = tmux::display_message("tmup: unable to schedule background bootstrap, running inline");
    emit_config_warnings(&warnings);
    let guard = OperationLock::acquire(&paths.lock_path)?;
    run_init_inline(&paths, &cfg, guard).await
}

async fn run_init_bootstrap(
    config_path: PathBuf,
    tpm_config_policy: TpmConfigPolicy,
    data_root: PathBuf,
    state_root: PathBuf,
    config_mode: ConfigMode,
) -> Result<()> {
    let paths = Paths::from_runtime_roots(data_root, state_root, config_path)?;
    paths.ensure_dirs()?;
    let applied =
        apply_config_with_tpm_policy(&paths, config_mode, false, Some(tpm_config_policy))?;
    let paths = applied.paths;
    let cfg = applied.config;
    let warnings = applied.warnings;
    let tpm_config_policy = applied.tpm_config_policy;

    let lock = load_lockfile(&paths)?;
    let sync_preview =
        sync::preview(&cfg, &lock, None, SyncPolicy::init(cfg.options.auto_install), &paths);
    if !sync_preview.needs_work {
        emit_config_warnings(&warnings);
        return run_init_inline_fast_path(&paths, &cfg).await;
    }

    let ui_mode = tmux::init_ui_mode();
    if matches!(ui_mode, tmux::InitUiMode::Inline) {
        emit_config_warnings(&warnings);
        let guard = OperationLock::acquire(&paths.lock_path)?;
        return run_init_inline(&paths, &cfg, guard).await;
    }

    if let Some(target) = tmux::probe_init_ui_target() {
        let exit_code =
            run_init_with_ui_mode(&paths, &target, ui_mode, config_mode, &tpm_config_policy)
                .await?;
        return if exit_code == 0 { Ok(()) } else { Err(progress::reported_error()) };
    }

    // Falling through here is intentional: no UI target became available for
    // the chosen tmux UI mode within the probe window.
    let _ = tmux::display_message("tmup: unable to create progress UI, running inline");
    emit_config_warnings(&warnings);
    let guard = OperationLock::acquire(&paths.lock_path)?;
    run_init_inline(&paths, &cfg, guard).await
}

enum InitCoreResult {
    Success,
    WriteFailures(Vec<String>),
}

/// Child init flow: runs inside a tmux popup/split-window with a live reporter.
/// The shell wrapper handles wait-for signaling and exit code forwarding.
async fn run_init_child(
    _wait_channel: String, // signaled by the shell wrapper, not by Rust
    config_path: PathBuf,
    tpm_config_policy: TpmConfigPolicy,
    data_root: PathBuf,
    state_root: PathBuf,
    config_mode: ConfigMode,
) -> Result<()> {
    let paths = Paths::from_runtime_roots(data_root, state_root, config_path)?;
    paths.ensure_dirs()?;
    let applied =
        apply_config_with_tpm_policy(&paths, config_mode, false, Some(tpm_config_policy))?;
    let paths = applied.paths;
    let cfg = applied.config;
    emit_config_warnings(&applied.warnings);

    let reporter = progress::create_reporter(&paths, "init", &cfg, None);
    reporter.report(ProgressEvent::OperationStart { command: "init" });
    reporter.report(ProgressEvent::OperationStage { stage: OperationStage::WaitingForLock });

    let _guard = OperationLock::acquire(&paths.lock_path)?;
    match run_init_core(&cfg, &paths, &*reporter).await {
        Ok(InitCoreResult::Success) => {
            reporter.report(ProgressEvent::OperationEnd { command: "init", success: true });
            Ok(())
        }
        Ok(InitCoreResult::WriteFailures(_)) => {
            reporter.report(ProgressEvent::OperationEnd { command: "init", success: false });
            Err(progress::reported_error())
        }
        Err(e) => {
            if !progress::is_progress_failure(&e) {
                let (summary, detail) = progress::summarize_error(&e);
                reporter.report(ProgressEvent::OperationFailed { summary, detail });
            }
            reporter.report(ProgressEvent::OperationEnd { command: "init", success: false });
            Err(progress::reported_error())
        }
    }
}

/// Inline init: no popup/split, just execute directly. Used when no visible
/// work is expected or when tmux UI creation fails.
async fn run_init_inline(
    paths: &Paths,
    cfg: &tmup::model::Config,
    _guard: OperationLockGuard,
) -> Result<()> {
    match run_init_core(cfg, paths, &NullReporter).await? {
        InitCoreResult::Success => Ok(()),
        InitCoreResult::WriteFailures(write_failures) => {
            anyhow::bail!(
                "init encountered {} failure(s):\n  {}",
                write_failures.len(),
                write_failures.join("\n  ")
            );
        }
    }
}

async fn run_init_core(
    cfg: &tmup::model::Config,
    paths: &Paths,
    reporter: &dyn ProgressReporter,
) -> Result<InitCoreResult> {
    config_mode::ensure_tmup_config_exists(paths)?;
    let mut lock = load_lockfile(paths)?;
    reporter.report(ProgressEvent::OperationStage { stage: OperationStage::Syncing });
    let outcome = sync::run_and_write(
        cfg,
        &mut lock,
        paths,
        None,
        SyncPolicy::init(cfg.options.auto_install),
        SyncMode::Init,
        reporter,
    )
    .await?;

    reporter.report(ProgressEvent::OperationStage { stage: OperationStage::LoadingTmux });
    let load_plan = loader::build_load_plan(cfg, &paths.plugin_root);
    tmux::execute_plan(&load_plan)?;

    if outcome.is_clean() {
        Ok(InitCoreResult::Success)
    } else {
        Ok(InitCoreResult::WriteFailures(outcome.plugin_failures))
    }
}

fn read_init_result(path: &std::path::Path) -> Result<i32> {
    #[derive(serde::Deserialize)]
    struct InitResult {
        exit_code: i32,
    }
    let invalid = || format!("init child exited without a valid result record: {}", path.display());
    let content = std::fs::read_to_string(path).with_context(invalid)?;
    let result = serde_json::from_str::<InitResult>(&content).with_context(invalid)?;
    Ok(result.exit_code)
}

fn epoch_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

// ---------------------------------------------------------------------------
// Progress-enabled commands
// ---------------------------------------------------------------------------

async fn run_install(id: Option<String>, config_mode: ConfigMode) -> Result<()> {
    let paths = resolve_runtime_paths()?;
    let _guard = OperationLock::try_acquire(&paths.lock_path)?
        .context("another tmup operation is in progress")?;
    let applied = apply_config(&paths, config_mode, true)?;
    let paths = applied.paths;
    let cfg = applied.config;
    cfg.validate_target_id(id.as_deref())?;
    let mut lock = load_lockfile(&paths)?;
    paths.ensure_dirs()?;

    let reporter = progress::create_reporter(&paths, "install", &cfg, id.as_deref());
    reporter.report(ProgressEvent::OperationStart { command: "install" });

    let result = async {
        reporter.report(ProgressEvent::OperationStage { stage: OperationStage::Syncing });
        sync::run_and_write(
            &cfg,
            &mut lock,
            &paths,
            id.as_deref(),
            SyncPolicy::INSTALL,
            SyncMode::Normal,
            &*reporter,
        )
        .await
        .and_then(ensure_sync_phase_clean)?;
        reporter.report(ProgressEvent::OperationStage { stage: OperationStage::ApplyingWrites });
        plugin::install(&cfg, &mut lock, &paths, id.as_deref(), false, &*reporter).await
    }
    .await;
    finish_visible_operation(&*reporter, "install", result)
}

async fn run_sync(id: Option<String>, config_mode: ConfigMode) -> Result<()> {
    let paths = resolve_runtime_paths()?;
    let _guard = OperationLock::try_acquire(&paths.lock_path)?
        .context("another tmup operation is in progress")?;
    let applied = apply_config(&paths, config_mode, true)?;
    let paths = applied.paths;
    let cfg = applied.config;
    cfg.validate_target_id(id.as_deref())?;
    let mut lock = load_lockfile(&paths)?;
    paths.ensure_dirs()?;

    let reporter = progress::create_reporter(&paths, "sync", &cfg, id.as_deref());
    reporter.report(ProgressEvent::OperationStart { command: "sync" });

    let result = async {
        reporter.report(ProgressEvent::OperationStage { stage: OperationStage::Syncing });
        sync::run_and_write(
            &cfg,
            &mut lock,
            &paths,
            id.as_deref(),
            SyncPolicy::SYNC,
            SyncMode::Normal,
            &*reporter,
        )
        .await
        .and_then(ensure_sync_phase_clean)
    }
    .await;
    finish_visible_operation(&*reporter, "sync", result)
}

async fn run_update(id: Option<String>, config_mode: ConfigMode) -> Result<()> {
    let paths = resolve_runtime_paths()?;
    let _guard = OperationLock::try_acquire(&paths.lock_path)?
        .context("another tmup operation is in progress")?;
    let applied = apply_config(&paths, config_mode, true)?;
    let paths = applied.paths;
    let cfg = applied.config;
    cfg.validate_target_id(id.as_deref())?;
    let mut lock = load_lockfile(&paths)?;
    paths.ensure_dirs()?;

    let reporter = progress::create_reporter(&paths, "update", &cfg, id.as_deref());
    reporter.report(ProgressEvent::OperationStart { command: "update" });

    let result = async {
        reporter.report(ProgressEvent::OperationStage { stage: OperationStage::Syncing });
        let sync_outcome = sync::run_and_write(
            &cfg,
            &mut lock,
            &paths,
            id.as_deref(),
            SyncPolicy::UPDATE,
            SyncMode::Normal,
            &*reporter,
        )
        .await?;
        ensure_sync_phase_clean(sync_outcome)?;
        reporter.report(ProgressEvent::OperationStage { stage: OperationStage::ApplyingWrites });
        plugin::update(&cfg, &mut lock, &paths, id.as_deref(), &*reporter).await
    }
    .await;
    finish_visible_operation(&*reporter, "update", result)
}

async fn run_restore(id: Option<String>, config_mode: ConfigMode) -> Result<()> {
    let paths = resolve_runtime_paths()?;
    let _guard = OperationLock::try_acquire(&paths.lock_path)?
        .context("another tmup operation is in progress")?;
    let applied = apply_config(&paths, config_mode, true)?;
    let paths = applied.paths;
    let cfg = applied.config;
    cfg.validate_target_id(id.as_deref())?;
    let mut lock = load_lockfile(&paths)?;
    paths.ensure_dirs()?;

    let reporter = progress::create_reporter(&paths, "restore", &cfg, id.as_deref());
    reporter.report(ProgressEvent::OperationStart { command: "restore" });

    let result = async {
        reporter.report(ProgressEvent::OperationStage { stage: OperationStage::Syncing });
        sync::run_and_write(
            &cfg,
            &mut lock,
            &paths,
            id.as_deref(),
            SyncPolicy::RESTORE,
            SyncMode::Normal,
            &*reporter,
        )
        .await
        .and_then(ensure_sync_phase_clean)?;
        reporter.report(ProgressEvent::OperationStage { stage: OperationStage::ApplyingWrites });
        plugin::restore(&cfg, &lock, &paths, id.as_deref(), &*reporter).await
    }
    .await;
    finish_visible_operation(&*reporter, "restore", result)
}

// ---------------------------------------------------------------------------
// Non-progress commands
// ---------------------------------------------------------------------------

async fn run_clean(config_mode: ConfigMode) -> Result<()> {
    let paths = resolve_runtime_paths()?;
    let _guard = OperationLock::try_acquire(&paths.lock_path)?
        .context("another tmup operation is in progress")?;
    let applied = apply_config(&paths, config_mode, true)?;
    let paths = applied.paths;
    let cfg = applied.config;
    let mut lock = load_lockfile(&paths)?;
    let sync_outcome = sync::run_and_write(
        &cfg,
        &mut lock,
        &paths,
        None,
        SyncPolicy::CLEAN,
        SyncMode::Normal,
        &NullReporter,
    )
    .await?;
    ensure_sync_phase_clean(sync_outcome)?;
    plugin::clean(&cfg, &paths)
}

fn run_list(verbose: bool, config_mode: ConfigMode) -> Result<()> {
    let paths = resolve_runtime_paths()?;
    let applied = apply_config(&paths, config_mode, false)?;
    let paths = applied.paths;
    let cfg = applied.config;
    let lock = load_lockfile(&paths)?;
    let statuses = plugin::list(&cfg, &lock, &paths)?;

    if sync::lock_is_stale(&cfg, &lock) {
        eprintln!("warning: lock metadata is stale relative to config; run `tmup sync`");
    }

    if verbose {
        print_verbose_statuses(&statuses)?;
    } else {
        print_default_statuses(&statuses)?;
    }

    Ok(())
}

fn print_default_statuses(statuses: &[PluginStatus]) -> Result<()> {
    let rows = statuses.iter().map(|s| {
        vec![
            s.source.clone(),
            s.kind.clone(),
            style_state(s.state),
            style_build_status(s.build_status),
            style_lock_status(s.current_commit.as_deref(), s.lock_commit.as_deref()),
        ]
    });
    write_table(&render_table(["Plugin", "Kind", "State", "Build", "Lock"], rows))
}

fn print_verbose_statuses(statuses: &[PluginStatus]) -> Result<()> {
    let rows = statuses.iter().map(|s| {
        vec![
            s.id.clone(),
            s.name.clone(),
            s.kind.clone(),
            style_state(s.state),
            style_build_status(s.build_status),
            style_commit(s.current_commit.as_deref()),
            style_commit(s.lock_commit.as_deref()),
            s.source.clone(),
        ]
    });
    write_table(&render_table(
        ["Id", "Name", "Kind", "State", "Build", "Current", "Expected", "Source"],
        rows,
    ))
}

fn style_state(state: PluginState) -> String {
    match state {
        PluginState::Installed | PluginState::Local => format!("{}", state.green()),
        PluginState::Missing | PluginState::Broken => format!("{}", state.red()),
        PluginState::Outdated => format!("{}", state.yellow()),
        PluginState::PinnedTag | PluginState::PinnedCommit => format!("{}", state.cyan()),
    }
}

fn style_build_status(status: BuildStatus) -> String {
    match status {
        BuildStatus::Ok => format!("{}", "success".green()),
        BuildStatus::BuildFailed => format!("{}", status.red()),
        BuildStatus::None => format!("{}", "-".dimmed()),
    }
}

fn style_lock_status(current: Option<&str>, lock: Option<&str>) -> String {
    match (current, lock) {
        (Some(c), Some(l)) if c == l => format!("{}", "synced".green()),
        (Some(_), Some(_)) | (None, Some(_)) => format!("{}", "mismatch".yellow()),
        _ => format!("{}", "-".dimmed()),
    }
}

fn style_commit(hash: Option<&str>) -> String {
    format!("{}", short_commit(hash).dimmed())
}

fn render_table<const N: usize>(
    headers: [&str; N],
    rows: impl IntoIterator<Item = Vec<String>>,
) -> String {
    let mut builder = Builder::default();
    builder.push_record(headers.map(termui::bold));
    for row in rows {
        builder.push_record(row);
    }

    let mut table = builder.build();
    table.with(Style::blank());
    table.with(Modify::new(Segment::all()).with(Alignment::left()));

    table.to_string()
}

fn write_table(table: &str) -> Result<()> {
    let mut stdout = anstream::stdout();
    writeln!(stdout, "{table}")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn short_commit(hash: Option<&str>) -> &str {
    hash.map(tmup::short_hash).unwrap_or("-")
}

fn ensure_sync_phase_clean(outcome: sync::SyncOutcome) -> Result<()> {
    if outcome.is_clean() {
        return Ok(());
    }
    Err(progress::progress_failure(format!(
        "{} plugin(s) failed to sync:\n  {}",
        outcome.plugin_failures.len(),
        outcome.plugin_failures.join("\n  ")
    )))
}

fn finish_visible_operation(
    reporter: &dyn ProgressReporter,
    command: &'static str,
    result: Result<()>,
) -> Result<()> {
    match result {
        Ok(()) => {
            reporter.report(ProgressEvent::OperationEnd { command, success: true });
            Ok(())
        }
        Err(e) if progress::is_progress_failure(&e) => {
            reporter.report(ProgressEvent::OperationEnd { command, success: false });
            Err(progress::reported_error())
        }
        Err(e) => {
            let (summary, detail) = progress::summarize_error(&e);
            reporter.report(ProgressEvent::OperationFailed { summary, detail });
            reporter.report(ProgressEvent::OperationEnd { command, success: false });
            Err(progress::reported_error())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use anstream::{AutoStream, ColorChoice};
    use clap::Parser;
    use tempfile::tempdir;
    use tmup::config_mode::{ConfigMode, TpmConfigPolicy};
    use tmup::state::Paths;

    use super::{Cli, Commands, apply_config_with_tpm_policy, parse_config_mode_env, render_table};

    fn adapt(text: &str, choice: ColorChoice) -> String {
        let mut stream = AutoStream::new(Vec::new(), choice);
        write!(stream, "{text}").unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    #[test]
    fn render_table_styles_header_text() {
        let output =
            render_table(["Plugin", "State"], [vec!["user/repo".into(), "missing".into()]]);
        assert!(output.contains("\u{1b}[1mPlugin"));
        assert!(output.contains("user/repo"));
    }

    #[test]
    fn anstream_strips_table_ansi_when_disabled() {
        let table = render_table(["Plugin", "State"], [vec!["user/repo".into(), "missing".into()]]);
        let output = adapt(&table, ColorChoice::Never);
        assert!(!output.contains("\u{1b}[1m"));
        assert!(output.contains("Plugin"));
        assert!(output.contains("user/repo"));
    }

    #[test]
    fn anstream_keeps_table_ansi_when_enabled() {
        let table = render_table(["Plugin", "State"], [vec!["user/repo".into(), "missing".into()]]);
        let output = adapt(&table, ColorChoice::AlwaysAnsi);
        assert!(output.contains("\u{1b}[1mPlugin"));
    }

    #[test]
    fn cli_parses_list_subcommand_without_public_mode_flags() {
        let cli = Cli::try_parse_from(["tmup", "list"]).unwrap();
        assert!(matches!(cli.command, Commands::List { .. }));
    }

    #[test]
    fn parse_config_mode_env_accepts_supported_values() {
        assert_eq!(parse_config_mode_env("pure").unwrap(), ConfigMode::Pure);
        assert_eq!(parse_config_mode_env("mixed").unwrap(), ConfigMode::Mixed);
        assert_eq!(parse_config_mode_env(" mixed ").unwrap(), ConfigMode::Mixed);
    }

    #[test]
    fn parse_config_mode_env_rejects_invalid_values() {
        let err = parse_config_mode_env("bogus").unwrap_err();
        assert!(err.to_string().contains("TMUP_CONFIG_MODE"));
        assert!(err.to_string().contains("pure"));
        assert!(err.to_string().contains("mixed"));
    }

    #[test]
    fn apply_config_with_tpm_policy_returns_warnings_for_init_callers() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config/tmux");
        std::fs::create_dir_all(&config_dir).unwrap();
        let tmup_config = config_dir.join("tmup.kdl");
        let tpm_config = config_dir.join("tmux.conf");
        std::fs::write(&tmup_config, r#"plugin "tmux-plugins/tmux-sensible" branch="feature""#)
            .unwrap();
        std::fs::write(
            &tpm_config,
            "set -g @plugin 'tmux-plugins/tmux-sensible'\nset -g @plugin 'tmux-plugins/tmux-yank'\n",
        )
        .unwrap();

        let paths = Paths::from_runtime_roots(
            dir.path().join("data"),
            dir.path().join("state"),
            tmup_config,
        )
        .unwrap();

        let applied = apply_config_with_tpm_policy(
            &paths,
            ConfigMode::Mixed,
            false,
            Some(TpmConfigPolicy::Resolved(Some(tpm_config))),
        )
        .unwrap();

        assert_eq!(applied.warnings.len(), 1);
        assert!(applied.warnings[0].contains("github.com/tmux-plugins/tmux-sensible"));
    }
}
