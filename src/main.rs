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
use tmup::state::{OperationLock, Paths};
use tmup::sync::{self, SyncMode, SyncPolicy};
use tmup::{lockfile, plugin, termui};

mod init_session;

#[derive(Debug, Parser)]
#[command(name = "tmup", about = "Modern tmux plugin manager", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// tmux startup: install missing plugins, apply options, load plugins
    Init(init_session::ProductionInitArgs),
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

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match resolve_requested_config_mode() {
        Ok(requested_config_mode) => match cli.command {
            Commands::Init(invocation) => invocation.execute(requested_config_mode).await,
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

fn default_tpm_config_policy(mode: ConfigMode) -> TpmConfigPolicy {
    match mode {
        ConfigMode::Pure => TpmConfigPolicy::Disabled,
        ConfigMode::Mixed => TpmConfigPolicy::Discover,
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
        explicit_tpm_policy.unwrap_or_else(|| default_tpm_config_policy(mode)),
    );
    let loaded = config_mode::load_with_request(paths, request)?;
    Ok(AppliedConfig { paths: loaded.paths, config: loaded.config, warnings: loaded.warnings })
}

fn load_lockfile(paths: &Paths) -> Result<lockfile::LockFile> {
    if paths.lockfile_path.exists() {
        lockfile::read_lockfile(&paths.lockfile_path)
    } else {
        Ok(lockfile::LockFile::new())
    }
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
