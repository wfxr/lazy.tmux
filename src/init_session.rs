use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use tmup::config_mode::{self, ConfigMode, ResolutionIntent, TpmConfigPolicy};
use tmup::loader::{LoadPlan, PluginLoadCommand};
use tmup::lockfile::{self, LockFile};
use tmup::progress::{NullReporter, OperationStage, PluginStage, ProgressEvent, ProgressReporter};
use tmup::state::{OperationLock, Paths};
use tmup::sync::{self, SyncMode, SyncPolicy};
#[cfg(test)]
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

struct ChildHandoff<'session> {
    context: InvocationContext,
    source: record::UiChildSource,
    session: Option<&'session mut record::SessionOwner>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChildDisposition {
    Completed,
    CompletedWithPluginFailures(Vec<String>),
}

impl From<ChildDisposition> for Outcome {
    fn from(disposition: ChildDisposition) -> Self {
        match disposition {
            ChildDisposition::Completed => Self::Completed,
            ChildDisposition::CompletedWithPluginFailures(failures) => {
                Self::CompletedWithPluginFailures(failures)
            }
        }
    }
}

trait TmuxAdapter {
    fn ui_available(&mut self) -> bool;
    fn current_host_available(&mut self) -> bool;
    fn wait_for_host(&mut self) -> bool;
    fn defer(&mut self, resume_path: &Path) -> Result<()>;
    fn host_child(&mut self, handoff: ChildHandoff<'_>) -> Result<ChildDisposition>;
    fn display_fallback(&mut self, message: &str);
    fn display_waiting(&mut self);
    fn execute_load_plan(
        &mut self,
        plan: &LoadPlan,
        excluded_plugin_ids: &HashSet<String>,
    ) -> Result<Vec<PluginLoadFailure>>;
}

#[derive(Debug)]
struct PluginLoadFailure {
    entry: PluginLoadCommand,
    error: anyhow::Error,
}

async fn resume_with_adapter(
    continuation: Continuation,
    tmux: &mut impl TmuxAdapter,
) -> Result<Outcome> {
    resume_with_adapter_in_session(continuation, None, tmux).await
}

async fn resume_with_adapter_in_session(
    continuation: Continuation,
    session: Option<&mut record::SessionOwner>,
    tmux: &mut impl TmuxAdapter,
) -> Result<Outcome> {
    match continuation {
        Continuation::DeferredBootstrap(context) => {
            let loaded = load_preview_context(&context)?;
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
            let disposition = tmux.host_child(ChildHandoff {
                context,
                source: record::UiChildSource::DeferredBootstrap,
                session,
            })?;
            Ok(disposition.into())
        }
        Continuation::HostedChild(context) => execute_hosted(&context, tmux).await,
    }
}

pub(crate) async fn run(invocation: PublicInvocation) -> Result<Outcome> {
    let (context, loaded) = resolve_normal_invocation(invocation)?;
    let mut tmux = ProductionTmux::new();
    run_loaded_with_adapter(context, loaded, &mut tmux).await
}

async fn resume_record_with_adapter(path: &Path, tmux: &mut impl TmuxAdapter) -> Result<Outcome> {
    match record::claim(path)? {
        record::ClaimedContinuation::Bootstrap(claimed) => {
            let (context, mut owner) = claimed.into_parts();
            match resume_with_adapter_in_session(
                Continuation::DeferredBootstrap(context),
                Some(&mut owner),
                tmux,
            )
            .await
            {
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
        record::ClaimedContinuation::UiChild(claimed) => {
            let execution =
                resume_with_adapter(Continuation::HostedChild(claimed.context().clone()), tmux)
                    .await;
            let child_result = match &execution {
                Ok(Outcome::Completed) => record::ChildResult::Completed,
                Ok(Outcome::CompletedWithPluginFailures(failures)) => {
                    record::ChildResult::CompletedWithPluginFailures { failures: failures.clone() }
                }
                Ok(Outcome::Deferred) => unreachable!("hosted child cannot defer"),
                Err(_) => record::ChildResult::OperationFailed,
            };
            claimed.publish_result(&child_result)?;
            match execution {
                Ok(Outcome::CompletedWithPluginFailures(_)) => Err(progress::reported_error()),
                other => other,
            }
        }
    }
}

async fn resume_record(path: &Path) -> Result<Outcome> {
    let mut tmux = ProductionTmux::new();
    resume_record_with_adapter(path, &mut tmux).await
}

fn finish(outcome: Outcome) -> Result<()> {
    match outcome {
        Outcome::Completed | Outcome::Deferred => Ok(()),
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
    let tpm_policy = match config_mode {
        ConfigMode::Pure => TpmConfigPolicy::Disabled,
        ConfigMode::Mixed => TpmConfigPolicy::Discover,
    };
    let request = config_mode::LoadRequest::from_command(
        config_mode,
        false,
        tpm_policy,
        ResolutionIntent::LoadEligibility,
    );
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
    let loaded = load_preview_context(&context)?;
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
        let disposition = tmux.host_child(ChildHandoff {
            context,
            source: record::UiChildSource::Direct,
            session: None,
        })?;
        return Ok(disposition.into());
    }
    let published = record::publish_bootstrap(&context)?;
    if tmux.defer(published.record_path()).is_ok() {
        return Ok(Outcome::Deferred);
    }
    let _ = published.cleanup();
    tmux.display_fallback("tmup: unable to schedule background bootstrap, running inline");
    execute_inline(&context, &loaded.warnings, LockWaitMessage::Silent, tmux).await
}

struct LoadedContext {
    paths: Paths,
    config: config_mode::ResolvedConfig,
    warnings: Vec<String>,
}

fn load_preview_context(context: &InvocationContext) -> Result<LoadedContext> {
    load_context_with_intent(context, ResolutionIntent::LoadEligibility)
}

fn load_runtime_context(context: &InvocationContext) -> Result<LoadedContext> {
    load_context_with_intent(context, ResolutionIntent::RuntimeConfiguration)
}

fn load_context_with_intent(
    context: &InvocationContext,
    intent: ResolutionIntent,
) -> Result<LoadedContext> {
    let paths = context_paths(context)?;
    let tpm_policy = context.tpm_identity.policy();
    let request =
        config_mode::LoadRequest::from_command(context.config_mode, false, tpm_policy, intent);
    let loaded = config_mode::load_with_request(&paths, request)?;
    Ok(LoadedContext { paths: loaded.paths, config: loaded.config, warnings: loaded.warnings })
}

fn context_paths(context: &InvocationContext) -> Result<Paths> {
    Paths::from_runtime_roots(
        context.data_root.clone(),
        context.state_root.clone(),
        context.config_path.clone(),
    )
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
    let paths = context_paths(context)?;
    let guard = match OperationLock::try_acquire(&paths.lock_path)? {
        Some(guard) => guard,
        None => {
            if matches!(lock_wait_message, LockWaitMessage::Announce) {
                tmux.display_waiting();
            }
            OperationLock::acquire(&paths.lock_path)?
        }
    };
    let loaded = load_runtime_context(context)?;
    emit_warnings(preview_warnings, &loaded.warnings);
    let outcome = execute_core(&loaded, tmux, &NullReporter).await;
    drop(guard);
    outcome
}

async fn execute_hosted(
    context: &InvocationContext,
    tmux: &mut impl TmuxAdapter,
) -> Result<Outcome> {
    let preview = load_preview_context(context)?;
    let reporter = progress::create_reporter(&preview.paths, "init", &preview.config, None);
    reporter.report(ProgressEvent::OperationStart { command: "init" });
    reporter.report(ProgressEvent::OperationStage { stage: OperationStage::WaitingForLock });

    let result = {
        let _guard = OperationLock::acquire(&preview.paths.lock_path)?;
        let loaded = load_runtime_context(context)?;
        emit_warnings(&preview.warnings, &loaded.warnings);
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
    #[arg(hide = true, long, value_name = "PATH")]
    resume: Option<PathBuf>,
}

impl ProductionInitArgs {
    pub(crate) async fn execute(self) -> Result<()> {
        if let Some(resume_path) = self.resume.as_deref() {
            return finish(resume_record(resume_path).await?);
        }
        let config_mode = super::resolve_requested_config_mode()?;
        finish(run(PublicInvocation::new(config_mode)).await?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TmuxVersion {
    major: u16,
    minor: u16,
    suffix: Option<char>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitUiMode {
    Popup { supports_title: bool },
    Split,
    Inline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InitUiTarget {
    client: String,
    pane: String,
}

struct InitResumeSpec {
    exe: PathBuf,
    resume_path: PathBuf,
}

impl InitResumeSpec {
    fn build_shell_command(&self) -> String {
        shell_join([
            self.exe.to_string_lossy().into_owned(),
            "init".into(),
            "--resume".into(),
            self.resume_path.to_string_lossy().into_owned(),
        ])
    }

    fn build_shell_wrapper(&self, wait_channel: Option<&str>, keep_failed_pane: bool) -> String {
        let remain_on_exit = if keep_failed_pane {
            "tmux set-option -p remain-on-exit failed >/dev/null 2>&1 || true\n"
        } else {
            ""
        };
        let (channel, cleanup, trap) = match wait_channel {
            Some(channel) => (
                format!("channel={}\n", shell_quote(channel)),
                "cleanup() { tmux wait-for -S \"$channel\"; }\n",
                "trap 'restore_tty; cleanup' EXIT INT TERM HUP",
            ),
            None => (String::new(), "", "trap 'restore_tty' EXIT INT TERM HUP"),
        };
        let command = self.build_shell_command();
        format!(
            r#"{channel}tty_state=
{cleanup}
restore_tty() {{ [ -n "$tty_state" ] && stty "$tty_state" >/dev/null 2>&1 || true; }}
{trap}
{remain_on_exit}{command}
if [ -t 0 ]; then
  tty_state=$(stty -g 2>/dev/null || true)
  stty -icanon -echo min 1 time 0 >/dev/null 2>&1 || true
  while :; do
    key=$(dd bs=1 count=1 2>/dev/null)
    [ "$key" = 'q' ] && break
  done
fi
exit 0"#,
        )
    }
}

fn shell_quote(value: &str) -> String {
    let mut quoted = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\"'\"'");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

fn shell_join(args: impl IntoIterator<Item = String>) -> String {
    args.into_iter().map(|arg| shell_quote(&arg)).collect::<Vec<_>>().join(" ")
}

fn parse_tmux_version(raw: &str) -> Option<TmuxVersion> {
    let raw = raw.trim();
    let start = raw.find(|ch: char| ch.is_ascii_digit())?;
    let version = &raw[start..];
    let dot = version.find('.')?;
    let major = version[..dot].parse().ok()?;
    let rest = &version[dot + 1..];
    let end = rest.find(|ch: char| !ch.is_ascii_digit()).unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let minor = rest[..end].parse().ok()?;
    let suffix = rest[end..].chars().next().filter(|ch| ch.is_ascii_alphabetic());
    Some(TmuxVersion { major, minor, suffix })
}

fn tmux_supports_popup_title(version: TmuxVersion) -> bool {
    (version.major, version.minor) >= (3, 3)
}

fn tmux_supports_popup(version: TmuxVersion) -> bool {
    (version.major, version.minor) >= (3, 2)
}

fn tmux_supports_split_ui(version: TmuxVersion) -> bool {
    (version.major, version.minor) >= (2, 0)
}

struct ProductionTmux {
    ui_mode: Option<InitUiMode>,
    target: Option<InitUiTarget>,
}

impl ProductionTmux {
    fn new() -> Self {
        Self { ui_mode: None, target: None }
    }

    fn executable() -> Result<PathBuf> {
        std::env::current_exe().context("failed to determine current executable")
    }

    fn resume_spec(resume_path: &Path) -> Result<InitResumeSpec> {
        Ok(InitResumeSpec { exe: Self::executable()?, resume_path: resume_path.to_path_buf() })
    }

    fn read_tmux_version() -> Option<TmuxVersion> {
        let output = std::process::Command::new("tmux")
            .arg("-V")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        parse_tmux_version(&String::from_utf8_lossy(&output.stdout))
    }

    fn init_ui_mode() -> InitUiMode {
        let Some(version) = Self::read_tmux_version() else {
            return InitUiMode::Inline;
        };
        if tmux_supports_popup(version) {
            return InitUiMode::Popup { supports_title: tmux_supports_popup_title(version) };
        }
        if tmux_supports_split_ui(version) {
            return InitUiMode::Split;
        }
        InitUiMode::Inline
    }

    fn display_message_format(format: &str) -> Result<String> {
        let output = std::process::Command::new("tmux")
            .args(["display-message", "-p", format])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("display-message failed: {stderr}");
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn read_init_ui_target_once() -> Option<InitUiTarget> {
        let client = Self::display_message_format("#{client_name}").ok()?;
        let pane = Self::display_message_format("#{pane_id}").ok()?;
        if client.is_empty() || pane.is_empty() {
            return None;
        }
        Some(InitUiTarget { client, pane })
    }

    fn probe_init_ui_target() -> Option<InitUiTarget> {
        const INITIAL_BACKOFF_MS: u64 = 20;
        const MAX_DELAY_MS: u64 = 1_000;

        let mut next_delay_ms = 0;
        loop {
            let delay_ms = next_delay_ms;
            if delay_ms != 0 {
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
            if let Some(target) = Self::read_init_ui_target_once() {
                return Some(target);
            }
            if delay_ms >= MAX_DELAY_MS {
                break;
            }
            next_delay_ms = if next_delay_ms == 0 {
                INITIAL_BACKOFF_MS
            } else {
                next_delay_ms.saturating_mul(2)
            };
        }
        None
    }

    fn spawn_bootstrap(spec: &InitResumeSpec) -> Result<()> {
        let command = spec.build_shell_command();
        let output = std::process::Command::new("tmux")
            .args(["run-shell", "-b", &command])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("run-shell failed: {stderr}");
        }
        Ok(())
    }

    fn spawn_popup(
        spec: &InitResumeSpec,
        target: &InitUiTarget,
        supports_title: bool,
    ) -> Result<()> {
        let wrapper = spec.build_shell_wrapper(None, false);
        let mut args = vec![
            "display-popup".to_string(),
            "-E".to_string(),
            "-w".to_string(),
            "80%".to_string(),
            "-h".to_string(),
            "80%".to_string(),
            "-c".to_string(),
            target.client.clone(),
        ];
        if supports_title {
            args.push("-T".to_string());
            args.push(" tmup init (press #[bold,fg=red]q#[default] to exit) ".to_string());
        }
        args.push("--".to_string());
        args.push(wrapper);
        let output = std::process::Command::new("tmux")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("display-popup failed: {stderr}");
        }
        Ok(())
    }

    fn spawn_split(spec: &InitResumeSpec, target: &InitUiTarget, wait_channel: &str) -> Result<()> {
        let wrapper = spec.build_shell_wrapper(Some(wait_channel), true);
        let output = std::process::Command::new("tmux")
            .args(["split-window", "-v", "-l", "50%", "-t", &target.pane, "--", &wrapper])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("split-window failed: {stderr}");
        }
        Ok(())
    }

    fn wait_for(wait_channel: &str) -> Result<()> {
        let status =
            std::process::Command::new("tmux").args(["wait-for", wait_channel]).status()?;
        if !status.success() {
            anyhow::bail!("tmux wait-for failed");
        }
        Ok(())
    }
}

impl TmuxAdapter for ProductionTmux {
    fn ui_available(&mut self) -> bool {
        let mode = Self::init_ui_mode();
        let available = !matches!(mode, InitUiMode::Inline);
        self.ui_mode = Some(mode);
        available
    }

    fn current_host_available(&mut self) -> bool {
        self.target = Self::read_init_ui_target_once();
        self.target.is_some()
    }

    fn wait_for_host(&mut self) -> bool {
        self.target = Self::probe_init_ui_target();
        self.target.is_some()
    }

    fn defer(&mut self, resume_path: &Path) -> Result<()> {
        Self::spawn_bootstrap(&Self::resume_spec(resume_path)?)
    }

    fn host_child(&mut self, handoff: ChildHandoff<'_>) -> Result<ChildDisposition> {
        let ChildHandoff { context, source, session } = handoff;
        let target = self.target.as_ref().context("tmux host target is unavailable")?;
        let mode = self.ui_mode.context("tmux UI availability was not inspected")?;
        let wait_channel = matches!(mode, InitUiMode::Split)
            .then(|| format!("tmup-init-{}-{}", std::process::id(), epoch_millis()));
        let host = match &wait_channel {
            Some(wait_channel) => record::UiHost::Split { wait_channel: wait_channel.clone() },
            None => record::UiHost::Popup,
        };
        let mut published = record::publish_ui_child(&context, source, host, session)?;
        let hosted = (|| {
            let spec = Self::resume_spec(published.record_path())?;
            match mode {
                InitUiMode::Popup { supports_title } => {
                    Self::spawn_popup(&spec, target, supports_title)?;
                    published.terminal_completion_confirmed();
                    record::consume_child_result(published.result_path())
                        .context("reading popup init result")
                }
                InitUiMode::Split => {
                    let wait_channel = wait_channel.as_deref().unwrap();
                    Self::spawn_split(&spec, target, wait_channel)?;
                    published.child_launched();
                    Self::wait_for(wait_channel)?;
                    published.terminal_completion_confirmed();
                    record::consume_child_result(published.result_path())
                        .context("reading split init result")
                }
                InitUiMode::Inline => {
                    unreachable!("inline mode cannot host an Init Session child")
                }
            }
        })();
        let cleanup = published.cleanup();
        let result = match hosted {
            Ok(result) => {
                cleanup?;
                result
            }
            Err(error) => {
                let _ = cleanup;
                return Err(error);
            }
        };
        match result {
            record::ChildResult::Completed => Ok(ChildDisposition::Completed),
            record::ChildResult::CompletedWithPluginFailures { failures } => {
                Ok(ChildDisposition::CompletedWithPluginFailures(failures))
            }
            record::ChildResult::OperationFailed => Err(progress::reported_error()),
        }
    }

    fn display_fallback(&mut self, message: &str) {
        let _ = tmup::tmux::display_message(message);
    }

    fn display_waiting(&mut self) {
        let _ = tmup::tmux::display_message("tmup: waiting for another operation...");
    }

    fn execute_load_plan(
        &mut self,
        plan: &LoadPlan,
        excluded_plugin_ids: &HashSet<String>,
    ) -> Result<Vec<PluginLoadFailure>> {
        tmup::tmux::execute(&plan.global_setup)?;

        let mut failed_plugin_ids = excluded_plugin_ids.clone();
        let mut failures = Vec::new();
        for entry in &plan.plugin_commands {
            if failed_plugin_ids.contains(&entry.plugin_id) {
                continue;
            }
            if let Err(error) = tmup::tmux::execute(&entry.command) {
                failed_plugin_ids.insert(entry.plugin_id.clone());
                failures.push(PluginLoadFailure { entry: entry.clone(), error });
            }
        }
        Ok(failures)
    }
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
    let runtime_configuration = loaded
        .config
        .runtime_configuration()
        .context("Init Session configuration did not resolve Runtime Configuration")?;
    let load_plan = loader::build_load_plan(runtime_configuration, &loaded.paths.plugin_root);
    let runtime_failures =
        tmux.execute_load_plan(&load_plan, sync_outcome.load_excluded_plugin_ids())?;
    let mut plugin_failures = sync_outcome.plugin_failures;
    for failure in runtime_failures {
        let PluginLoadFailure { entry, error } = failure;
        let PluginLoadCommand { plugin_id, plugin_name, command } = entry;
        let (summary, detail) = progress::summarize_error(&error);
        let command_name = command.to_args().into_iter().next().unwrap_or_else(|| "tmux".into());
        reporter.report(ProgressEvent::PluginFailed {
            id: &plugin_id,
            name: &plugin_name,
            stage: Some(PluginStage::Loading),
            summary,
            detail,
            context: vec![("tmux_command", command_name)],
        });
        plugin_failures.push(format!("{plugin_id}: {error}"));
    }

    if plugin_failures.is_empty() {
        Ok(Outcome::Completed)
    } else {
        Ok(Outcome::CompletedWithPluginFailures(plugin_failures))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum Request {
        InspectUi,
        InspectCurrentHost,
        WaitForHost,
        Defer(PathBuf),
        HostChild {
            context: InvocationContext,
            source: record::UiChildSource,
            reuses_session: bool,
        },
        Fallback(String),
        Waiting,
        Load(Vec<TmuxCommand>),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum HostFailure {
        CompletionUnknown,
        CompletionConfirmed,
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
        host_disposition: ChildDisposition,
        host_failure: Option<HostFailure>,
        #[cfg(unix)]
        block_record_cleanup_on_defer: bool,
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
                host_disposition: ChildDisposition::Completed,
                host_failure: None,
                #[cfg(unix)]
                block_record_cleanup_on_defer: false,
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
            #[cfg(unix)]
            if self.block_record_cleanup_on_defer {
                let sessions_root = resume_path.parent().unwrap().parent().unwrap();
                std::fs::set_permissions(sessions_root, std::fs::Permissions::from_mode(0o500))
                    .unwrap();
            }
            if self.defer_fails {
                anyhow::bail!("unable to schedule deferred bootstrap")
            }
            Ok(())
        }

        fn host_child(&mut self, handoff: ChildHandoff<'_>) -> Result<ChildDisposition> {
            let ChildHandoff { context, source, mut session } = handoff;
            self.requests.push(Request::HostChild {
                context,
                source,
                reuses_session: session.is_some(),
            });
            match self.host_failure {
                Some(HostFailure::CompletionUnknown) => {
                    if let Some(owner) = session.as_deref_mut() {
                        owner.child_launched();
                    }
                    anyhow::bail!("hosted child completion is unknown")
                }
                Some(HostFailure::CompletionConfirmed) => {
                    if let Some(owner) = session.as_deref_mut() {
                        owner.child_launched();
                        owner.terminal_completion_confirmed();
                    }
                    Err(progress::reported_error())
                }
                None => Ok(self.host_disposition.clone()),
            }
        }

        fn display_fallback(&mut self, message: &str) {
            self.requests.push(Request::Fallback(message.to_string()));
        }

        fn display_waiting(&mut self) {
            self.requests.push(Request::Waiting);
        }

        fn execute_load_plan(
            &mut self,
            plan: &LoadPlan,
            _excluded_plugin_ids: &HashSet<String>,
        ) -> Result<Vec<PluginLoadFailure>> {
            if let Some(lock_path) = &self.expected_lock_path {
                self.load_while_locked =
                    tmup::state::OperationLock::try_acquire(lock_path)?.is_none();
            }
            self.requests.push(Request::Load(plan.iter().cloned().collect()));
            Ok(Vec::new())
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

    #[test]
    fn production_adapter_parses_supported_tmux_versions() {
        let cases = [
            ("tmux 3.2", Some((3, 2, None))),
            ("tmux 3.3a", Some((3, 3, Some('a')))),
            ("tmux next-3.4", Some((3, 4, None))),
            ("tmux master", None),
        ];

        for (input, expected) in cases {
            let parsed = parse_tmux_version(input).map(|v| (v.major, v.minor, v.suffix));
            assert_eq!(parsed, expected, "{input} should parse as {expected:?}");
        }
    }

    #[test]
    fn production_adapter_quotes_the_single_resume_transport() {
        let spec = InitResumeSpec {
            exe: PathBuf::from("/tmp/tmup it's"),
            resume_path: PathBuf::from("/tmp/session it's/ui-child.json"),
        };

        assert_eq!(
            spec.build_shell_command(),
            "'/tmp/tmup it'\"'\"'s' 'init' '--resume' '/tmp/session it'\"'\"'s/ui-child.json'"
        );
        let popup = spec.build_shell_wrapper(None, false);
        assert!(!popup.contains("wait-for"));
        assert!(!popup.contains("TMUP_CONFIG"));
        assert!(!popup.contains("--ui-child"));
        assert!(!popup.contains("exit_code"));
    }

    #[tokio::test]
    async fn deferred_session_hosts_child_when_ui_becomes_available() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plug "https://example.com/test/plugin.git""#);
        let continuation = Continuation::DeferredBootstrap(context.clone());
        let mut tmux = MockTmux::hosted();

        let outcome = resume_with_adapter(continuation, &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert_eq!(
            tmux.requests,
            vec![
                Request::InspectUi,
                Request::WaitForHost,
                Request::HostChild {
                    context,
                    source: record::UiChildSource::DeferredBootstrap,
                    reuses_session: false,
                },
            ]
        );
    }

    #[tokio::test]
    async fn resumed_bootstrap_reuses_its_session_for_the_ui_child_handoff() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plug "https://example.com/test/plugin.git""#);
        let published = record::publish_bootstrap(&context).unwrap();
        let record_path = published.record_path().to_path_buf();
        let session_dir = record_path.parent().unwrap().to_path_buf();
        let mut tmux = MockTmux::hosted();

        let outcome = resume_record_with_adapter(&record_path, &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert!(matches!(
            tmux.requests.as_slice(),
            [
                Request::InspectUi,
                Request::WaitForHost,
                Request::HostChild {
                    source: record::UiChildSource::DeferredBootstrap,
                    reuses_session: true,
                    ..
                },
            ]
        ));
        assert!(!session_dir.exists(), "the bootstrap owner must clean its Init Session");
    }

    #[tokio::test]
    async fn resumed_ui_child_publishes_completion_after_loading() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, "");
        let published = record::publish_ui_child(
            &context,
            record::UiChildSource::Direct,
            record::UiHost::Popup,
            None,
        )
        .unwrap();
        let record_path = published.record_path().to_path_buf();
        let result_path = published.result_path().to_path_buf();
        let session_dir = record_path.parent().unwrap().to_path_buf();
        let mut tmux = MockTmux::hosted();

        let outcome = resume_record_with_adapter(&record_path, &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert!(matches!(tmux.requests.as_slice(), [Request::Load(_)]));
        assert_eq!(
            record::consume_child_result(&result_path).unwrap(),
            record::ChildResult::Completed
        );
        assert!(session_dir.exists(), "the hosting parent retains Init Session ownership");
        published.cleanup().unwrap();
    }

    #[tokio::test]
    async fn resumed_ui_child_publishes_operation_failure_and_releases_the_lock() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, "");
        std::fs::write(context.config_path.parent().unwrap().join("tmup.lock"), "not json")
            .unwrap();
        let lock_path = context.state_root.join("operations.lock");
        let published = record::publish_ui_child(
            &context,
            record::UiChildSource::Direct,
            record::UiHost::Popup,
            None,
        )
        .unwrap();
        let record_path = published.record_path().to_path_buf();
        let result_path = published.result_path().to_path_buf();
        let mut tmux = MockTmux::hosted();

        let result = resume_record_with_adapter(&record_path, &mut tmux).await;

        assert!(result.is_err());
        assert_eq!(
            record::consume_child_result(&result_path).unwrap(),
            record::ChildResult::OperationFailed
        );
        assert!(!tmux.requests.iter().any(|request| matches!(request, Request::Load(_))));
        assert!(
            OperationLock::try_acquire(&lock_path).unwrap().is_some(),
            "the UI child must publish its result after releasing the operation lock"
        );
        published.cleanup().unwrap();
    }

    #[tokio::test]
    async fn normal_session_defers_when_work_needs_ui_but_no_host_exists() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plug "https://example.com/test/plugin.git""#);
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
    async fn disabled_plugin_is_absent_from_init_managed_and_load_snapshots() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        let plugin_dir = dir.path().join("local-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let script = plugin_dir.join("disabled.tmux");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        write_config(
            &context,
            &format!(
                r#"plug "{}" local=#true enabled=#false {{ opt "disabled" "yes" }}"#,
                plugin_dir.display()
            ),
        );
        let mut tmux = MockTmux::hosted();

        let outcome = run_with_adapter(context, &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        let Request::Load(plan) = tmux.requests.last().unwrap() else {
            panic!("init must execute its global tmux load plan");
        };
        assert!(
            plan.iter().all(|command| matches!(command, TmuxCommand::SetEnvironment { .. })),
            "disabled plugin options and scripts must be absent from init: {plan:?}"
        );
    }

    #[tokio::test]
    async fn false_load_condition_skips_plugin_options_and_scripts_but_preserves_neighbors() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        let skipped_dir = dir.path().join("skipped-plugin");
        let loaded_dir = dir.path().join("loaded-plugin");
        std::fs::create_dir_all(&skipped_dir).unwrap();
        std::fs::create_dir_all(&loaded_dir).unwrap();
        let skipped_script = skipped_dir.join("skipped.tmux");
        let loaded_script = loaded_dir.join("loaded.tmux");
        std::fs::write(&skipped_script, "#!/bin/sh\n").unwrap();
        std::fs::write(&loaded_script, "#!/bin/sh\n").unwrap();
        write_config(
            &context,
            &format!(
                concat!(
                    "plug \"{}\" local=#true cond=#false {{ opt \"skipped\" \"yes\" }}\n",
                    "plug \"{}\" local=#true {{ opt \"loaded\" \"yes\" }}\n",
                ),
                skipped_dir.display(),
                loaded_dir.display(),
            ),
        );
        let mut tmux = MockTmux::hosted();

        let outcome = run_with_adapter(context, &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        let Request::Load(plan) = tmux.requests.last().unwrap() else {
            panic!("init must execute its tmux load plan");
        };
        assert!(
            !plan.contains(&TmuxCommand::SetOption { key: "skipped".into(), value: "yes".into() })
        );
        assert!(!plan.contains(&TmuxCommand::RunShell { script: skipped_script }));
        assert!(
            plan.contains(&TmuxCommand::SetOption { key: "loaded".into(), value: "yes".into() })
        );
        assert!(plan.contains(&TmuxCommand::RunShell { script: loaded_script }));
    }

    #[tokio::test]
    async fn init_runtime_configuration_respects_enable_and_load_gates() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        let disabled_dir = dir.path().join("disabled-plugin");
        let skipped_dir = dir.path().join("skipped-plugin");
        let loaded_dir = dir.path().join("loaded-plugin");
        for plugin_dir in [&disabled_dir, &skipped_dir, &loaded_dir] {
            std::fs::create_dir_all(plugin_dir).unwrap();
        }
        write_config(
            &context,
            &format!(
                r#"
plug "{}" local=#true enabled=#false {{
    if "kill -TERM $$" {{
        bind "disabled" {{ shell "./disabled" }}
    }}
}}
plug "{}" local=#true cond=#false {{
    if "kill -TERM $$" {{
        bind "skipped" {{ shell "./skipped" }}
    }}
}}
plug "{}" local=#true {{
    if #false {{
        bind "then" {{ shell "./then" }}
    }}
    else {{
        bind "otherwise" {{ shell "./otherwise" }}
    }}
}}
"#,
                disabled_dir.display(),
                skipped_dir.display(),
                loaded_dir.display(),
            ),
        );
        let mut tmux = MockTmux::hosted();

        let outcome = run_with_adapter(context.clone(), &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert_eq!(
            tmux.requests,
            vec![Request::Load(vec![
                TmuxCommand::SetEnvironment {
                    key: "TMUX_PLUGIN_MANAGER_PATH".into(),
                    value: format!("{}/", context.data_root.join("plugins").display()),
                },
                TmuxCommand::BindKey {
                    options: vec![],
                    key: "otherwise".into(),
                    plugin_dir: loaded_dir,
                    shell: "./otherwise".into(),
                    background: false,
                },
            ])]
        );
    }

    #[tokio::test]
    async fn init_preview_is_advisory_and_execution_uses_one_frozen_snapshot() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(
            &context,
            r#"plug "user/repo" enabled="n=$(cat evaluations 2>/dev/null || printf 0); n=$((n+1)); printf %s $n > evaluations; test $n -ne 2""#,
        );
        let lock_path = context.config_path.parent().unwrap().join("tmup.lock");
        let mut lock = LockFile::new();
        lock.plugins.insert(
            "github.com/user/repo".into(),
            tmup::lockfile::LockEntry::default_branch("main", "abc1234"),
        );
        lockfile::write_lockfile_atomic(&lock_path, &lock).unwrap();
        let mut tmux = MockTmux::hosted();
        tmux.ui_available = false;

        let outcome = run_with_adapter(context.clone(), &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert_eq!(
            std::fs::read_to_string(context.config_path.parent().unwrap().join("evaluations"))
                .unwrap(),
            "2",
            "init may evaluate once for preview and once for authoritative execution only"
        );
        let lock = lockfile::read_lockfile(&lock_path).unwrap();
        assert!(
            lock.plugins.is_empty(),
            "the authoritative false snapshot must drive managed-state reconciliation"
        );
        let Request::Load(plan) = tmux.requests.last().unwrap() else {
            panic!("init must execute its tmux load plan");
        };
        assert!(
            plan.iter().all(|command| matches!(command, TmuxCommand::SetEnvironment { .. })),
            "the same authoritative false snapshot must drive loading: {plan:?}"
        );
    }

    #[tokio::test]
    async fn runtime_configuration_is_resolved_once_and_frozen_for_loading() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        let plugin_dir = dir.path().join("local-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        write_config(
            &context,
            &format!(
                r#"
plug "{}" local=#true {{
    if "n=$(cat branch-evaluations 2>/dev/null || printf 0); n=$((n+1)); printf %s $n > branch-evaluations; test $n -eq 1" {{
        bind "selected" {{ shell "./selected" }}
    }}
    else {{
        bind "otherwise" {{ shell "./otherwise" }}
    }}
}}
"#,
                plugin_dir.display(),
            ),
        );
        let mut tmux = MockTmux::hosted();

        let outcome = run_with_adapter(context.clone(), &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert_eq!(
            std::fs::read_to_string(
                context.config_path.parent().unwrap().join("branch-evaluations")
            )
            .unwrap(),
            "1",
            "one Init Session must evaluate each reachable runtime branch exactly once"
        );
        let Request::Load(plan) = tmux.requests.last().unwrap() else {
            panic!("init must execute its tmux load plan");
        };
        assert_eq!(
            plan,
            &vec![
                TmuxCommand::SetEnvironment {
                    key: "TMUX_PLUGIN_MANAGER_PATH".into(),
                    value: format!("{}/", context.data_root.join("plugins").display()),
                },
                TmuxCommand::BindKey {
                    options: vec![],
                    key: "selected".into(),
                    plugin_dir,
                    shell: "./selected".into(),
                    background: false,
                },
            ],
            "loading must reuse the frozen runtime branch selection"
        );
    }

    #[test]
    fn authoritative_condition_snapshot_is_resolved_after_acquiring_the_operation_lock() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        let plugin_dir = dir.path().join("local-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let plugin_script = plugin_dir.join("load.tmux");
        std::fs::write(&plugin_script, "#!/bin/sh\n").unwrap();
        write_config(
            &context,
            &format!(
                r#"plug "{}" local=#true cond="n=$(cat evaluations 2>/dev/null || printf 0); n=$((n+1)); printf %s $n > evaluations; test $n -ne 2""#,
                plugin_dir.display()
            ),
        );
        let lock_path = context.state_root.join("operations.lock");
        let held_lock = OperationLock::acquire(&lock_path).unwrap();
        let evaluation_path = context.config_path.parent().unwrap().join("evaluations");
        let thread_context = context.clone();
        let handle = std::thread::spawn(move || {
            let runtime =
                tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let mut tmux = MockTmux::hosted();
            let outcome = runtime.block_on(run_with_adapter(thread_context, &mut tmux));
            (outcome, tmux)
        });

        for _ in 0..100 {
            if evaluation_path.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(
            std::fs::read_to_string(&evaluation_path).unwrap(),
            "1",
            "the final condition snapshot must wait for the operation lock"
        );

        drop(held_lock);
        let (outcome, tmux) = handle.join().unwrap();
        assert_eq!(outcome.unwrap(), Outcome::Completed);
        assert_eq!(std::fs::read_to_string(evaluation_path).unwrap(), "2");
        let [Request::Waiting, Request::Load(plan)] = tmux.requests.as_slice() else {
            panic!("inline execution must wait, then load from the authoritative snapshot");
        };
        assert!(
            !plan.contains(&TmuxCommand::RunShell { script: plugin_script }),
            "the authoritative false result must drive the loader"
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
        write_config(&context, &format!(r#"plug "{}" local=#true"#, inherited_plugin.display()));
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
        write_config(&context, r#"plug "https://example.com/test/plugin.git""#);
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
        write_config(&context, r#"plug "https://example.com/test/plugin.git""#);
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
        write_config(&context, r#"plug "https://example.com/test/plugin.git""#);
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
        write_config(&context, r#"plug "https://example.com/test/plugin.git""#);
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

    #[cfg(unix)]
    #[tokio::test]
    async fn record_cleanup_failure_does_not_cancel_inline_fallback() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plug "https://example.com/test/plugin.git""#);
        let sessions_root = context.state_root.join("init-sessions");
        let mut tmux = MockTmux::hosted();
        tmux.defer_fails = true;
        tmux.block_record_cleanup_on_defer = true;
        tmux.config_replacement_on_defer = Some((context.config_path.clone(), String::new()));

        let result = run_with_adapter(context, &mut tmux).await;
        std::fs::set_permissions(&sessions_root, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(result.unwrap(), Outcome::Completed);
        assert!(tmux.requests.iter().any(|request| matches!(request, Request::Fallback(_))));
        assert!(tmux.requests.iter().any(|request| matches!(request, Request::Load(_))));
    }

    #[tokio::test]
    async fn normal_session_hosts_a_child_when_a_current_target_is_available() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plug "https://example.com/test/plugin.git""#);
        let mut tmux = MockTmux::hosted();
        tmux.current_host_available = true;

        let outcome = run_with_adapter(context.clone(), &mut tmux).await.unwrap();

        assert_eq!(outcome, Outcome::Completed);
        assert_eq!(
            tmux.requests,
            vec![
                Request::InspectUi,
                Request::InspectCurrentHost,
                Request::HostChild {
                    context,
                    source: record::UiChildSource::Direct,
                    reuses_session: false,
                },
            ]
        );
    }

    #[tokio::test]
    async fn direct_parent_preserves_named_hosted_plugin_failures() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plug "https://example.com/test/plugin.git""#);
        let mut tmux = MockTmux::hosted();
        tmux.current_host_available = true;
        tmux.host_disposition = ChildDisposition::CompletedWithPluginFailures(vec![
            "example.com/test/plugin: build failed".into(),
        ]);

        let outcome = run_with_adapter(context, &mut tmux).await.unwrap();

        assert_eq!(
            outcome,
            Outcome::CompletedWithPluginFailures(vec![
                "example.com/test/plugin: build failed".into()
            ])
        );
    }

    #[tokio::test]
    async fn deferred_parent_preserves_named_hosted_plugin_failures() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plug "https://example.com/test/plugin.git""#);
        let continuation = Continuation::DeferredBootstrap(context);
        let mut tmux = MockTmux::hosted();
        tmux.host_disposition = ChildDisposition::CompletedWithPluginFailures(vec![
            "example.com/test/plugin: build failed".into(),
        ]);

        let outcome = resume_with_adapter(continuation, &mut tmux).await.unwrap();

        assert_eq!(
            outcome,
            Outcome::CompletedWithPluginFailures(vec![
                "example.com/test/plugin: build failed".into()
            ])
        );
    }

    #[tokio::test]
    async fn resumed_bootstrap_preserves_session_when_child_completion_is_unknown() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plug "https://example.com/test/plugin.git""#);
        let published = record::publish_bootstrap(&context).unwrap();
        let record_path = published.record_path().to_path_buf();
        let session_dir = record_path.parent().unwrap().to_path_buf();
        let mut tmux = MockTmux::hosted();
        tmux.host_failure = Some(HostFailure::CompletionUnknown);

        let result = resume_record_with_adapter(&record_path, &mut tmux).await;

        assert!(result.is_err());
        assert!(
            session_dir.exists(),
            "the outer bootstrap owner must preserve a session whose child completion is unknown"
        );
    }

    #[tokio::test]
    async fn resumed_bootstrap_cleans_session_after_confirmed_child_termination() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        write_config(&context, r#"plug "https://example.com/test/plugin.git""#);
        let published = record::publish_bootstrap(&context).unwrap();
        let record_path = published.record_path().to_path_buf();
        let session_dir = record_path.parent().unwrap().to_path_buf();
        let mut tmux = MockTmux::hosted();
        tmux.host_failure = Some(HostFailure::CompletionConfirmed);

        let result = resume_record_with_adapter(&record_path, &mut tmux).await;

        assert!(result.is_err());
        assert!(
            !session_dir.exists(),
            "the outer bootstrap owner must clean a session after confirmed child termination"
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
                    "plug \"http://127.0.0.1:1/test/plugin.git\" cond=#false\n",
                    "plug \"{}\" local=#true\n",
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

        let result = resume_record(&record_path).await;

        assert!(result.is_err());
        assert!(
            !session_dir.exists(),
            "the resumed owner must clean its session after a handled error"
        );
    }
}
