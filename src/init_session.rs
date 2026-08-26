use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use tmup::config_mode::{self, ConfigMode, TpmConfigPolicy};
use tmup::lockfile::{self, LockFile};
use tmup::progress::{NullReporter, OperationStage, ProgressEvent, ProgressReporter};
use tmup::state::{OperationLock, Paths};
use tmup::sync::{self, SyncMode, SyncPolicy};
use tmup::tmux::TmuxCommand;
use tmup::{loader, progress};

mod record;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolvedTpmIdentity {
    Disabled,
    Path(PathBuf),
    Absent,
}

impl ResolvedTpmIdentity {
    fn policy(&self) -> TpmConfigPolicy {
        match self {
            Self::Disabled => TpmConfigPolicy::Disabled,
            Self::Path(path) => TpmConfigPolicy::Resolved(Some(path.clone())),
            Self::Absent => TpmConfigPolicy::Resolved(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InvocationContext {
    config_mode: ConfigMode,
    config_path: PathBuf,
    tpm_identity: ResolvedTpmIdentity,
    data_root: PathBuf,
    state_root: PathBuf,
}

impl InvocationContext {
    pub(crate) fn new(
        config_mode: ConfigMode,
        config_path: PathBuf,
        tpm_identity: ResolvedTpmIdentity,
        data_root: PathBuf,
        state_root: PathBuf,
    ) -> Self {
        Self { config_mode, config_path, tpm_identity, data_root, state_root }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Continuation {
    DeferredBootstrap(InvocationContext),
    HostedChild(InvocationContext),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Completed,
    Deferred,
    CompletedWithPluginFailures(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PublicInvocation {
    config_mode: ConfigMode,
}

impl PublicInvocation {
    pub(crate) fn new(config_mode: ConfigMode) -> Self {
        Self { config_mode }
    }
}

trait TmuxAdapter {
    fn ui_available(&mut self) -> bool;
    fn current_host_available(&mut self) -> bool;
    fn wait_for_host(&mut self) -> bool;
    fn defer(&mut self, resume_path: &Path) -> Result<()>;
    fn host_child(&mut self, continuation: Continuation) -> Result<()>;
    fn display_fallback(&mut self, message: &str);
    fn display_waiting(&mut self);
    fn execute_load_plan(&mut self, plan: &[TmuxCommand]) -> Result<()>;
}

async fn resume_with_adapter(
    continuation: Continuation,
    tmux: &mut impl TmuxAdapter,
) -> Result<Outcome> {
    match continuation {
        Continuation::DeferredBootstrap(context) => {
            let loaded = load_context(&context)?;
            if !needs_sync_work(&loaded)? {
                return execute_inline(&context, &loaded.warnings, LockWaitMessage::Announce, tmux)
                    .await;
            }
            if !tmux.ui_available() {
                return execute_inline(&context, &loaded.warnings, LockWaitMessage::Silent, tmux)
                    .await;
            }
            if !tmux.wait_for_host() {
                tmux.display_fallback("tmup: unable to create progress UI, running inline");
                return execute_inline(&context, &loaded.warnings, LockWaitMessage::Silent, tmux)
                    .await;
            }
            tmux.host_child(Continuation::HostedChild(context))?;
            Ok(Outcome::Completed)
        }
        Continuation::HostedChild(context) => execute_hosted(&context, tmux).await,
    }
}

pub(crate) async fn run(invocation: PublicInvocation) -> Result<Outcome> {
    let (context, loaded) = resolve_normal_invocation(invocation)?;
    let mut tmux = ProductionTmux::new();
    run_loaded_with_adapter(context, loaded, &mut tmux).await
}

pub(crate) async fn resume(continuation: Continuation) -> Result<Outcome> {
    let mut tmux = ProductionTmux::new();
    resume_with_adapter(continuation, &mut tmux).await
}

async fn resume_bootstrap_record(path: &Path) -> Result<Outcome> {
    let claimed = record::claim_bootstrap(path)?;
    let (context, owner) = claimed.into_parts();
    match resume(Continuation::DeferredBootstrap(context)).await {
        Ok(outcome) => {
            owner.cleanup()?;
            Ok(outcome)
        }
        Err(error) => {
            let _ = owner.cleanup();
            Err(error)
        }
    }
}

fn finish(outcome: Outcome, failures_already_reported: bool) -> Result<()> {
    match outcome {
        Outcome::Completed | Outcome::Deferred => Ok(()),
        Outcome::CompletedWithPluginFailures(_) if failures_already_reported => {
            Err(progress::reported_error())
        }
        Outcome::CompletedWithPluginFailures(failures) => {
            anyhow::bail!(
                "init encountered {} failure(s):\n  {}",
                failures.len(),
                failures.join("\n  ")
            )
        }
    }
}

fn resolve_normal_invocation(
    invocation: PublicInvocation,
) -> Result<(InvocationContext, LoadedContext)> {
    let config_mode = invocation.config_mode;
    let paths = super::resolve_runtime_paths()?;
    paths.ensure_dirs()?;
    let tpm_policy = match config_mode {
        ConfigMode::Pure => TpmConfigPolicy::Disabled,
        ConfigMode::Mixed => TpmConfigPolicy::Discover,
    };
    let request = config_mode::LoadRequest::from_command(config_mode, false, tpm_policy);
    let loaded = config_mode::load_with_request(&paths, request)?;
    let context = InvocationContext::new(
        config_mode,
        loaded.paths.config_path.clone(),
        resolved_tpm_identity(loaded.tpm_policy.clone())?,
        loaded.paths.data_root().to_path_buf(),
        loaded.paths.state_root().to_path_buf(),
    );
    let loaded =
        LoadedContext { paths: loaded.paths, config: loaded.config, warnings: loaded.warnings };
    Ok((context, loaded))
}

fn resolved_tpm_identity(policy: TpmConfigPolicy) -> Result<ResolvedTpmIdentity> {
    match policy {
        TpmConfigPolicy::Disabled => Ok(ResolvedTpmIdentity::Disabled),
        TpmConfigPolicy::Resolved(Some(path)) => Ok(ResolvedTpmIdentity::Path(path)),
        TpmConfigPolicy::Resolved(None) => Ok(ResolvedTpmIdentity::Absent),
        TpmConfigPolicy::Discover => {
            anyhow::bail!("Init Session context requires a resolved TPM config identity")
        }
    }
}

#[cfg(test)]
async fn run_with_adapter(
    context: InvocationContext,
    tmux: &mut impl TmuxAdapter,
) -> Result<Outcome> {
    let loaded = load_context(&context)?;
    run_loaded_with_adapter(context, loaded, tmux).await
}

async fn run_loaded_with_adapter(
    context: InvocationContext,
    loaded: LoadedContext,
    tmux: &mut impl TmuxAdapter,
) -> Result<Outcome> {
    if !needs_sync_work(&loaded)? {
        return execute_inline(&context, &loaded.warnings, LockWaitMessage::Announce, tmux).await;
    }
    if !tmux.ui_available() {
        return execute_inline(&context, &loaded.warnings, LockWaitMessage::Silent, tmux).await;
    }
    if tmux.current_host_available() {
        tmux.host_child(Continuation::HostedChild(context))?;
        return Ok(Outcome::Completed);
    }
    let published = record::publish_bootstrap(&context)?;
    if tmux.defer(published.record_path()).is_ok() {
        return Ok(Outcome::Deferred);
    }
    published.cleanup()?;
    tmux.display_fallback("tmup: unable to schedule background bootstrap, running inline");
    execute_inline(&context, &loaded.warnings, LockWaitMessage::Silent, tmux).await
}

struct LoadedContext {
    paths: Paths,
    config: tmup::model::Config,
    warnings: Vec<String>,
}

fn load_context(context: &InvocationContext) -> Result<LoadedContext> {
    let paths = Paths::from_runtime_roots(
        context.data_root.clone(),
        context.state_root.clone(),
        context.config_path.clone(),
    )?;
    paths.ensure_dirs()?;
    let tpm_policy = context.tpm_identity.policy();
    let request = config_mode::LoadRequest::from_command(context.config_mode, false, tpm_policy);
    let loaded = config_mode::load_with_request(&paths, request)?;
    Ok(LoadedContext { paths: loaded.paths, config: loaded.config, warnings: loaded.warnings })
}

fn load_lockfile(paths: &Paths) -> Result<LockFile> {
    if paths.lockfile_path.exists() {
        lockfile::read_lockfile(&paths.lockfile_path)
    } else {
        Ok(LockFile::new())
    }
}

fn needs_sync_work(loaded: &LoadedContext) -> Result<bool> {
    let lock = load_lockfile(&loaded.paths)?;
    Ok(sync::preview(
        &loaded.config,
        &lock,
        None,
        SyncPolicy::init(loaded.config.options.auto_install),
        &loaded.paths,
    )
    .needs_work)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockWaitMessage {
    Announce,
    Silent,
}

async fn execute_inline(
    context: &InvocationContext,
    preview_warnings: &[String],
    lock_wait_message: LockWaitMessage,
    tmux: &mut impl TmuxAdapter,
) -> Result<Outcome> {
    let loaded = load_context(context)?;
    emit_warnings(preview_warnings, &loaded.warnings);
    let _guard = match OperationLock::try_acquire(&loaded.paths.lock_path)? {
        Some(guard) => guard,
        None => {
            if matches!(lock_wait_message, LockWaitMessage::Announce) {
                tmux.display_waiting();
            }
            OperationLock::acquire(&loaded.paths.lock_path)?
        }
    };
    execute_core(&loaded, tmux, &NullReporter).await
}

async fn execute_hosted(
    context: &InvocationContext,
    tmux: &mut impl TmuxAdapter,
) -> Result<Outcome> {
    let loaded = load_context(context)?;
    emit_warnings(&[], &loaded.warnings);
    let reporter = progress::create_reporter(&loaded.paths, "init", &loaded.config, None);
    reporter.report(ProgressEvent::OperationStart { command: "init" });
    reporter.report(ProgressEvent::OperationStage { stage: OperationStage::WaitingForLock });

    let result = {
        let _guard = OperationLock::acquire(&loaded.paths.lock_path)?;
        execute_core(&loaded, tmux, &*reporter).await
    };
    match result {
        Ok(Outcome::Completed) => {
            reporter.report(ProgressEvent::OperationEnd { command: "init", success: true });
            Ok(Outcome::Completed)
        }
        Ok(Outcome::CompletedWithPluginFailures(failures)) => {
            reporter.report(ProgressEvent::OperationEnd { command: "init", success: false });
            Ok(Outcome::CompletedWithPluginFailures(failures))
        }
        Ok(Outcome::Deferred) => unreachable!("locked execution cannot defer"),
        Err(error) => {
            if !progress::is_progress_failure(&error) {
                let (summary, detail) = progress::summarize_error(&error);
                reporter.report(ProgressEvent::OperationFailed { summary, detail });
            }
            reporter.report(ProgressEvent::OperationEnd { command: "init", success: false });
            Err(progress::reported_error())
        }
    }
}

fn emit_warnings(preview_warnings: &[String], execution_warnings: &[String]) {
    let mut emitted = HashSet::new();
    for warning in preview_warnings.iter().chain(execution_warnings) {
        if emitted.insert(warning) {
            eprintln!("warning: {warning}");
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct ProductionInitArgs {
    #[arg(
        hide = true,
        long,
        value_name = "PATH",
        conflicts_with_all = [
            "ui_child",
            "wait_channel",
            "config_path",
            "tpm_config_path",
            "no_tpm_config",
            "data_root",
            "state_root"
        ]
    )]
    resume: Option<PathBuf>,
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
}

impl ProductionInitArgs {
    pub(crate) async fn execute(self) -> Result<()> {
        if let Some(resume_path) = self.resume.as_deref() {
            return finish(resume_bootstrap_record(resume_path).await?, false);
        }
        let config_mode = super::resolve_requested_config_mode()?;
        match self.into_continuation(config_mode)? {
            Some(continuation) => {
                let is_hosted_child = matches!(continuation, Continuation::HostedChild(_));
                finish(resume(continuation).await?, is_hosted_child)
            }
            None => finish(run(PublicInvocation::new(config_mode)).await?, false),
        }
    }

    fn into_continuation(self, config_mode: ConfigMode) -> Result<Option<Continuation>> {
        if self.ui_child {
            self.wait_channel.as_ref().context("--ui-child requires --wait-channel")?;
            return Ok(Some(Continuation::HostedChild(self.context(config_mode, "--ui-child")?)));
        }
        Ok(None)
    }

    fn context(&self, config_mode: ConfigMode, role: &str) -> Result<InvocationContext> {
        let tpm_identity = match config_mode {
            ConfigMode::Pure => ResolvedTpmIdentity::Disabled,
            ConfigMode::Mixed if self.no_tpm_config => ResolvedTpmIdentity::Absent,
            ConfigMode::Mixed => self
                .tpm_config_path
                .clone()
                .map(ResolvedTpmIdentity::Path)
                .with_context(|| format!("{role} requires a resolved TPM config identity"))?,
        };
        Ok(InvocationContext::new(
            config_mode,
            self.config_path.clone().with_context(|| format!("{role} requires --config-path"))?,
            tpm_identity,
            self.data_root.clone().with_context(|| format!("{role} requires --data-root"))?,
            self.state_root.clone().with_context(|| format!("{role} requires --state-root"))?,
        ))
    }
}

struct ProductionTmux {
    ui_mode: Option<tmup::tmux::InitUiMode>,
    target: Option<tmup::tmux::InitUiTarget>,
}

impl ProductionTmux {
    fn new() -> Self {
        Self { ui_mode: None, target: None }
    }

    fn executable() -> Result<PathBuf> {
        std::env::current_exe().context("failed to determine current executable")
    }

    fn bootstrap_spec(resume_path: &Path) -> Result<tmup::tmux::InitBootstrapSpec> {
        Ok(tmup::tmux::InitBootstrapSpec {
            exe: Self::executable()?,
            resume_path: resume_path.to_path_buf(),
        })
    }

    fn child_spec(
        context: InvocationContext,
        wait_channel: String,
    ) -> Result<tmup::tmux::InitUiChildSpec> {
        Ok(tmup::tmux::InitUiChildSpec {
            exe: Self::executable()?,
            config_path: context.config_path,
            tpm_config_policy: context.tpm_identity.policy(),
            data_root: context.data_root,
            state_root: context.state_root,
            wait_channel,
            config_mode: context.config_mode,
        })
    }
}

impl TmuxAdapter for ProductionTmux {
    fn ui_available(&mut self) -> bool {
        let mode = tmup::tmux::init_ui_mode();
        let available = !matches!(mode, tmup::tmux::InitUiMode::Inline);
        self.ui_mode = Some(mode);
        available
    }

    fn current_host_available(&mut self) -> bool {
        self.target = tmup::tmux::current_init_ui_target();
        self.target.is_some()
    }

    fn wait_for_host(&mut self) -> bool {
        self.target = tmup::tmux::probe_init_ui_target();
        self.target.is_some()
    }

    fn defer(&mut self, resume_path: &Path) -> Result<()> {
        tmup::tmux::spawn_init_bootstrap(&Self::bootstrap_spec(resume_path)?)
    }

    fn host_child(&mut self, continuation: Continuation) -> Result<()> {
        let Continuation::HostedChild(context) = continuation else {
            anyhow::bail!("only hosted-child continuations can be hosted")
        };
        let target = self.target.as_ref().context("tmux host target is unavailable")?;
        let wait_channel = format!("tmup-init-{}-{}", std::process::id(), epoch_millis());
        let paths = Paths::from_runtime_roots(
            context.data_root.clone(),
            context.state_root.clone(),
            context.config_path.clone(),
        )?;
        let result_file = paths.init_result_path(&wait_channel);
        let _ = std::fs::remove_file(&result_file);
        let spec = Self::child_spec(context, wait_channel)?;
        let result = match self.ui_mode.context("tmux UI availability was not inspected")? {
            tmup::tmux::InitUiMode::Popup { supports_title } => {
                tmup::tmux::spawn_init_popup(&spec, target, &result_file, supports_title)?;
                read_and_cleanup_init_result(&result_file).context("reading popup init result")
            }
            tmup::tmux::InitUiMode::Split => {
                tmup::tmux::spawn_init_split(&spec, target, &result_file)?;
                tmup::tmux::wait_for(&spec.wait_channel)?;
                read_and_cleanup_init_result(&result_file).context("reading split init result")
            }
            tmup::tmux::InitUiMode::Inline => {
                unreachable!("inline mode cannot host an Init Session child")
            }
        }?;
        // The legacy result record contains only an exit code, so it cannot safely
        // distinguish plugin failures from operation failures. Preserve that transport
        // until the continuation-record migration and surface either as a reported error.
        if result == 0 { Ok(()) } else { Err(progress::reported_error()) }
    }

    fn display_fallback(&mut self, message: &str) {
        let _ = tmup::tmux::display_message(message);
    }

    fn display_waiting(&mut self) {
        let _ = tmup::tmux::display_message("tmup: waiting for another operation...");
    }

    fn execute_load_plan(&mut self, plan: &[TmuxCommand]) -> Result<()> {
        tmup::tmux::execute_plan(plan)
    }
}

fn read_and_cleanup_init_result(path: &Path) -> Result<i32> {
    let result = read_init_result(path);
    let _ = std::fs::remove_file(path);
    result
}

fn read_init_result(path: &Path) -> Result<i32> {
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

async fn execute_core(
    loaded: &LoadedContext,
    tmux: &mut impl TmuxAdapter,
    reporter: &dyn ProgressReporter,
) -> Result<Outcome> {
    config_mode::ensure_tmup_config_exists(&loaded.paths)?;
    let mut lock = load_lockfile(&loaded.paths)?;
    reporter.report(ProgressEvent::OperationStage { stage: OperationStage::Syncing });
    let sync_outcome = sync::run_and_write(
        &loaded.config,
        &mut lock,
        &loaded.paths,
        None,
        SyncPolicy::init(loaded.config.options.auto_install),
        SyncMode::Init,
        reporter,
    )
    .await?;

    reporter.report(ProgressEvent::OperationStage { stage: OperationStage::LoadingTmux });
    let load_plan = loader::build_load_plan(&loaded.config, &loaded.paths.plugin_root);
    tmux.execute_load_plan(&load_plan)?;

    if sync_outcome.is_clean() {
        Ok(Outcome::Completed)
    } else {
        Ok(Outcome::CompletedWithPluginFailures(sync_outcome.plugin_failures))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum Request {
        InspectUi,
        InspectCurrentHost,
        WaitForHost,
        Defer(PathBuf),
        Host(Continuation),
        Fallback(String),
        Waiting,
        Load(Vec<TmuxCommand>),
    }

    struct MockTmux {
        requests: Vec<Request>,
        ui_available: bool,
        current_host_available: bool,
        waited_host_available: bool,
        expected_lock_path: Option<PathBuf>,
        load_while_locked: bool,
        config_replacement_on_ui_inspection: Option<(PathBuf, String)>,
        config_replacement_on_defer: Option<(PathBuf, String)>,
        defer_fails: bool,
    }

    impl MockTmux {
        fn hosted() -> Self {
            Self {
                requests: Vec::new(),
                ui_available: true,
                current_host_available: false,
                waited_host_available: true,
                expected_lock_path: None,
                load_while_locked: false,
                config_replacement_on_ui_inspection: None,
                config_replacement_on_defer: None,
                defer_fails: false,
            }
        }
    }

    impl TmuxAdapter for MockTmux {
        fn ui_available(&mut self) -> bool {
            self.requests.push(Request::InspectUi);
            if let Some((path, contents)) = self.config_replacement_on_ui_inspection.take() {
                std::fs::write(path, contents).unwrap();
            }
            self.ui_available
        }

        fn current_host_available(&mut self) -> bool {
            self.requests.push(Request::InspectCurrentHost);
            self.current_host_available
        }

        fn wait_for_host(&mut self) -> bool {
            self.requests.push(Request::WaitForHost);
            self.waited_host_available
        }

        fn defer(&mut self, resume_path: &Path) -> Result<()> {
            self.requests.push(Request::Defer(resume_path.to_path_buf()));
            if let Some((path, contents)) = self.config_replacement_on_defer.take() {
                std::fs::write(path, contents).unwrap();
            }
            if self.defer_fails {
                anyhow::bail!("unable to schedule deferred bootstrap")
            }
            Ok(())
        }

        fn host_child(&mut self, continuation: Continuation) -> Result<()> {
            self.requests.push(Request::Host(continuation));
            Ok(())
        }

        fn display_fallback(&mut self, message: &str) {
            self.requests.push(Request::Fallback(message.to_string()));
        }

        fn display_waiting(&mut self) {
            self.requests.push(Request::Waiting);
        }

        fn execute_load_plan(&mut self, plan: &[TmuxCommand]) -> Result<()> {
            if let Some(lock_path) = &self.expected_lock_path {
                self.load_while_locked =
                    tmup::state::OperationLock::try_acquire(lock_path)?.is_none();
            }
            self.requests.push(Request::Load(plan.to_vec()));
            Ok(())
        }
    }

    fn context(root: &Path) -> InvocationContext {
        InvocationContext::new(
            ConfigMode::Pure,
            root.join("config/tmux/tmup.kdl"),
            ResolvedTpmIdentity::Disabled,
            root.join("data/tmup"),
            root.join("state/tmup"),
        )
    }

    fn write_config(context: &InvocationContext, contents: &str) {
        std::fs::create_dir_all(context.config_path.parent().unwrap()).unwrap();
        std::fs::write(&context.config_path, contents).unwrap();
    }

    #[tokio::test]
    async fn deferred_session_hosts_child_when_ui_becomes_available() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plugin "https://example.com/test/plugin.git""#);
        let continuation = Continuation::DeferredBootstrap(context.clone());
        let mut tmux = MockTmux::hosted();

        let outcome = resume_with_adapter(continuation, &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert_eq!(
            tmux.requests,
            vec![
                Request::InspectUi,
                Request::WaitForHost,
                Request::Host(Continuation::HostedChild(context)),
            ]
        );
    }

    #[tokio::test]
    async fn normal_session_defers_when_work_needs_ui_but_no_host_exists() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plugin "https://example.com/test/plugin.git""#);
        let mut tmux = MockTmux::hosted();
        tmux.waited_host_available = false;

        let outcome = run_with_adapter(context.clone(), &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Deferred);
        assert!(matches!(
            tmux.requests.as_slice(),
            [Request::InspectUi, Request::InspectCurrentHost, Request::Defer(_)]
        ));
    }

    #[tokio::test]
    async fn no_work_session_loads_inline_while_operation_lock_is_held() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, "");
        let mut tmux = MockTmux::hosted();
        let lock_path = context.state_root.join("operations.lock");
        tmux.expected_lock_path = Some(lock_path.clone());

        let outcome = run_with_adapter(context.clone(), &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert!(tmux.load_while_locked, "tmux loading must remain inside the operation lock");
        assert!(matches!(tmux.requests.as_slice(), [Request::Load(_)]));
        assert!(
            OperationLock::try_acquire(&lock_path).unwrap().is_some(),
            "the operation lock must be released after plugin loading"
        );
        assert!(
            !context.state_root.join("init-sessions").exists(),
            "the no-work fast path must not create continuation records"
        );
    }

    #[tokio::test]
    async fn no_work_fast_path_announces_lock_contention() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, "");
        let lock_path = context.state_root.join("operations.lock");
        let held_lock = OperationLock::acquire(&lock_path).unwrap();
        let release_lock = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(held_lock);
        });
        let mut tmux = MockTmux::hosted();

        let outcome = run_with_adapter(context.clone(), &mut tmux).await.unwrap();
        release_lock.join().unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert!(matches!(tmux.requests.as_slice(), [Request::Waiting, Request::Load(_)]));
    }

    #[tokio::test]
    async fn hosted_child_reloads_the_inherited_config_source() {
        let dir = tempdir().unwrap();
        let mut context = context(dir.path());
        context.config_path = dir.path().join("explicit/source.kdl");
        let inherited_plugin = dir.path().join("inherited-plugin");
        std::fs::create_dir_all(&inherited_plugin).unwrap();
        let inherited_script = inherited_plugin.join("inherited.tmux");
        std::fs::write(&inherited_script, "#!/bin/sh\n").unwrap();
        write_config(&context, &format!(r#"plugin "{}" local=#true"#, inherited_plugin.display()));
        let default_config = context.state_root.join("tmup.kdl");
        std::fs::create_dir_all(default_config.parent().unwrap()).unwrap();
        std::fs::write(&default_config, "").unwrap();
        let mut tmux = MockTmux::hosted();

        let outcome =
            resume_with_adapter(Continuation::HostedChild(context), &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        let Request::Load(plan) = tmux.requests.last().unwrap() else {
            panic!("hosted child must execute a tmux load plan");
        };
        assert!(plan.contains(&TmuxCommand::RunShell { script: inherited_script }));
    }

    #[tokio::test]
    async fn deferred_session_falls_back_inline_and_rereads_config() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plugin "https://example.com/test/plugin.git""#);
        let mut tmux = MockTmux::hosted();
        tmux.waited_host_available = false;
        tmux.config_replacement_on_ui_inspection =
            Some((context.config_path.clone(), String::new()));

        let outcome =
            resume_with_adapter(Continuation::DeferredBootstrap(context), &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert!(matches!(
            tmux.requests.as_slice(),
            [Request::InspectUi, Request::WaitForHost, Request::Fallback(_), Request::Load(_),]
        ));
    }

    #[tokio::test]
    async fn deferred_session_uses_inline_mode_without_a_fallback_message() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plugin "https://example.com/test/plugin.git""#);
        let mut tmux = MockTmux::hosted();
        tmux.ui_available = false;
        tmux.config_replacement_on_ui_inspection =
            Some((context.config_path.clone(), String::new()));

        let outcome =
            resume_with_adapter(Continuation::DeferredBootstrap(context), &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert!(matches!(tmux.requests.as_slice(), [Request::InspectUi, Request::Load(_)]));
    }

    #[tokio::test]
    async fn visible_work_inline_fallback_waits_for_the_lock_silently() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plugin "https://example.com/test/plugin.git""#);
        let lock_path = context.state_root.join("operations.lock");
        let held_lock = OperationLock::acquire(&lock_path).unwrap();
        let release_lock = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(held_lock);
        });
        let mut tmux = MockTmux::hosted();
        tmux.ui_available = false;
        tmux.config_replacement_on_ui_inspection =
            Some((context.config_path.clone(), String::new()));

        let outcome = run_with_adapter(context.clone(), &mut tmux).await.unwrap();
        release_lock.join().unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert!(matches!(tmux.requests.as_slice(), [Request::InspectUi, Request::Load(_)]));
        assert!(
            !context.state_root.join("init-sessions").exists(),
            "the inline fast path must not create continuation records"
        );
    }

    #[tokio::test]
    async fn resolved_tpm_absence_stays_distinct_from_an_explicit_source() {
        let dir = tempdir().unwrap();
        let mut absent_context = context(dir.path());
        absent_context.config_mode = ConfigMode::Mixed;
        absent_context.tpm_identity = ResolvedTpmIdentity::Absent;
        write_config(&absent_context, "options { auto-install #false }");

        let tpm_config = dir.path().join("config/tmux/tmux.conf");
        std::fs::write(&tpm_config, "set -g @plugin 'user/plugin'\n").unwrap();
        let plugin_dir = absent_context.data_root.join("plugins/github.com/user/plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let tpm_script = plugin_dir.join("from-tpm.tmux");
        std::fs::write(&tpm_script, "#!/bin/sh\n").unwrap();

        let mut absent_tmux = MockTmux::hosted();
        resume_with_adapter(Continuation::HostedChild(absent_context.clone()), &mut absent_tmux)
            .await
            .unwrap();
        let Request::Load(absent_plan) = absent_tmux.requests.last().unwrap() else {
            panic!("hosted child must execute a tmux load plan");
        };
        assert!(!absent_plan.iter().any(|command| matches!(command, TmuxCommand::RunShell { .. })));

        let mut explicit_context = absent_context;
        explicit_context.tpm_identity = ResolvedTpmIdentity::Path(tpm_config);
        let mut explicit_tmux = MockTmux::hosted();
        resume_with_adapter(Continuation::HostedChild(explicit_context), &mut explicit_tmux)
            .await
            .unwrap();
        let Request::Load(explicit_plan) = explicit_tmux.requests.last().unwrap() else {
            panic!("hosted child must execute a tmux load plan");
        };
        assert!(explicit_plan.contains(&TmuxCommand::RunShell { script: tpm_script }));
    }

    #[tokio::test]
    async fn normal_session_falls_back_inline_when_deferral_fails() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plugin "https://example.com/test/plugin.git""#);
        let mut tmux = MockTmux::hosted();
        tmux.defer_fails = true;
        tmux.config_replacement_on_defer = Some((context.config_path.clone(), String::new()));

        let outcome = run_with_adapter(context, &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert!(matches!(tmux.requests.first(), Some(Request::InspectUi)));
        assert!(matches!(tmux.requests.get(1), Some(Request::InspectCurrentHost)));
        assert!(matches!(tmux.requests.get(2), Some(Request::Defer(_))));
        assert_eq!(
            tmux.requests.get(3),
            Some(&Request::Fallback(
                "tmup: unable to schedule background bootstrap, running inline".into(),
            )),
        );
        assert!(matches!(tmux.requests.get(4), Some(Request::Load(_))));
        let Request::Defer(record_path) = tmux.requests.get(2).unwrap() else { unreachable!() };
        assert!(
            !record_path.parent().unwrap().exists(),
            "a failed deferred spawn must clean the session it created"
        );
    }

    #[tokio::test]
    async fn normal_session_hosts_a_child_when_a_current_target_is_available() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plugin "https://example.com/test/plugin.git""#);
        let mut tmux = MockTmux::hosted();
        tmux.current_host_available = true;

        let outcome = run_with_adapter(context.clone(), &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert_eq!(
            tmux.requests,
            vec![
                Request::InspectUi,
                Request::InspectCurrentHost,
                Request::Host(Continuation::HostedChild(context)),
            ]
        );
    }

    #[tokio::test]
    async fn plugin_sync_failure_loads_usable_plugins_and_returns_named_failures() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        let usable_plugin = dir.path().join("usable-plugin");
        std::fs::create_dir_all(&usable_plugin).unwrap();
        let usable_script = usable_plugin.join("usable.tmux");
        std::fs::write(&usable_script, "#!/bin/sh\n").unwrap();
        write_config(
            &context,
            &format!(
                concat!(
                    "plugin \"http://127.0.0.1:1/test/plugin.git\"\n",
                    "plugin \"{}\" local=#true\n",
                ),
                usable_plugin.display(),
            ),
        );
        let mut tmux = MockTmux::hosted();

        let outcome =
            resume_with_adapter(Continuation::HostedChild(context), &mut tmux).await.unwrap();

        let Outcome::CompletedWithPluginFailures(failures) = outcome else {
            panic!("plugin sync failure must produce a named failure outcome");
        };
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("127.0.0.1:1/test/plugin"));
        let Request::Load(plan) = tmux.requests.last().unwrap() else {
            panic!("plugin-level failure must continue to tmux loading");
        };
        assert!(plan.contains(&TmuxCommand::RunShell { script: usable_script }));
    }

    #[tokio::test]
    async fn operation_failure_stops_before_tmux_loading() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, "");
        std::fs::write(context.config_path.parent().unwrap().join("tmup.lock"), "not json")
            .unwrap();
        let mut tmux = MockTmux::hosted();

        let result = resume_with_adapter(Continuation::HostedChild(context), &mut tmux).await;

        assert!(result.is_err());
        assert!(!tmux.requests.iter().any(|request| matches!(request, Request::Load(_))));
    }

    #[tokio::test]
    async fn resumed_session_cleans_its_directory_after_an_operation_error() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, "");
        std::fs::write(context.config_path.parent().unwrap().join("tmup.lock"), "not json")
            .unwrap();
        let published = record::publish_bootstrap(&context).unwrap();
        let record_path = published.record_path().to_path_buf();
        let session_dir = record_path.parent().unwrap().to_path_buf();

        let result = resume_bootstrap_record(&record_path).await;

        assert!(result.is_err());
        assert!(
            !session_dir.exists(),
            "the resumed owner must clean its session after a handled error"
        );
    }
}
