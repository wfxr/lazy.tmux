use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::config_mode::{ConfigMode, TpmConfigPolicy};

/// Represents a tmux command to be executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxCommand {
    /// Set a global tmux environment variable.
    SetEnvironment {
        /// Environment variable name.
        key: String,
        /// Environment variable value.
        value: String,
    },
    /// Set a global tmux option (prefixed with `@`).
    SetOption {
        /// Option name.
        key: String,
        /// Option value.
        value: String,
    },
    /// Run an external shell script via `tmux run-shell`.
    RunShell {
        /// Path to the shell script.
        script: PathBuf,
    },
}

/// Parsed representation of a tmux version string (e.g. `3.3a`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TmuxVersion {
    /// Major version component.
    pub major: u16,
    /// Minor version component.
    pub minor: u16,
    /// Optional alphabetic suffix (e.g. `'a'` in `3.3a`).
    pub suffix: Option<char>,
}

/// UI mode used to display the init progress interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitUiMode {
    /// tmux `display-popup` is available; carries whether popup titles are supported.
    Popup {
        /// Whether the popup command supports the `-T` title flag.
        supports_title: bool,
    },
    /// tmux `split-window` is available but popup is not.
    Split,
    /// No suitable tmux UI is available; fall back to inline terminal output.
    Inline,
}

impl TmuxCommand {
    /// Convert to tmux CLI arguments.
    pub fn to_args(&self) -> Vec<String> {
        match self {
            Self::SetEnvironment { key, value } => {
                vec!["set-environment".into(), "-g".into(), key.clone(), value.clone()]
            }
            Self::SetOption { key, value } => {
                vec!["set".into(), "-g".into(), format!("@{key}"), value.clone()]
            }
            Self::RunShell { script } => {
                vec!["run-shell".into(), shell_quote(&script.to_string_lossy())]
            }
        }
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

fn shell_env_assignment(key: &str, value: &str) -> String {
    format!("{key}={}", shell_quote(value))
}

/// Parse a raw `tmux -V` output string into a `TmuxVersion`, returning `None` on failure.
pub fn parse_tmux_version(raw: &str) -> Option<TmuxVersion> {
    let raw = raw.trim();
    let start = raw.find(|ch: char| ch.is_ascii_digit())?;
    let version = &raw[start..];

    let dot = version.find('.')?;
    let major: u16 = version[..dot].parse().ok()?;

    let rest = &version[dot + 1..];
    let end = rest.find(|ch: char| !ch.is_ascii_digit()).unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let minor: u16 = rest[..end].parse().ok()?;
    let suffix = rest[end..].chars().next().filter(|ch| ch.is_ascii_alphabetic());

    Some(TmuxVersion { major, minor, suffix })
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

fn tmux_supports_popup_title(version: TmuxVersion) -> bool {
    (version.major, version.minor) >= (3, 3)
}

fn tmux_supports_popup(version: TmuxVersion) -> bool {
    (version.major, version.minor) >= (3, 2)
}

fn tmux_supports_split_ui(version: TmuxVersion) -> bool {
    (version.major, version.minor) >= (2, 0)
}

/// Detect the best available tmux UI mode for the running tmux version.
pub fn init_ui_mode() -> InitUiMode {
    let Some(version) = read_tmux_version() else {
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

/// Execute a single tmux command.
pub fn execute(cmd: &TmuxCommand) -> Result<()> {
    let args = cmd.to_args();
    let output = std::process::Command::new("tmux")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("tmux {} failed: {stderr}", args.first().map_or("?", |s| s.as_str()));
    }
    Ok(())
}

/// Execute a sequence of tmux commands.
pub fn execute_plan(plan: &[TmuxCommand]) -> Result<()> {
    for cmd in plan {
        execute(cmd)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Init UI child helpers
// ---------------------------------------------------------------------------

/// Identifies the tmux client and pane that will host the init UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitUiTarget {
    /// tmux client name (e.g. `/dev/pts/0`).
    pub client: String,
    /// tmux pane ID (e.g. `%1`).
    pub pane: String,
}

/// Parameters for scheduling the deferred init bootstrap command.
pub struct InitBootstrapSpec {
    /// Path to the tmup executable.
    pub exe: PathBuf,
    /// Absolute path to the immutable bootstrap continuation record.
    pub resume_path: PathBuf,
}

impl InitBootstrapSpec {
    fn build_shell_command(&self) -> String {
        let args = vec![
            self.exe.to_string_lossy().into_owned(),
            "init".into(),
            "--resume".into(),
            self.resume_path.to_string_lossy().into_owned(),
        ];
        shell_join(args)
    }
}

/// Parameters for spawning an init child process in a tmux popup or split.
pub struct InitUiChildSpec {
    /// Path to the tmup executable.
    pub exe: PathBuf,
    /// Path to the tmup configuration file.
    pub config_path: PathBuf,
    /// Resolved TPM config loading policy for mixed-mode init.
    pub tpm_config_policy: TpmConfigPolicy,
    /// Root directory for persistent data.
    pub data_root: PathBuf,
    /// Root directory for runtime state.
    pub state_root: PathBuf,
    /// tmux `wait-for` channel name used to signal completion.
    pub wait_channel: String,
    /// Configuration loading mode for the child process.
    pub config_mode: ConfigMode,
}

impl InitUiChildSpec {
    /// Build a shell wrapper that:
    /// - runs the tmup child process
    /// - writes exit code to a result file
    /// - uses `trap` to guarantee `wait-for -S` fires on any exit path
    ///
    /// Important: does NOT use `exec`, so the trap fires after the child exits.
    fn build_shell_wrapper(&self, result_file: &Path, keep_failed_pane: bool) -> String {
        let remain_on_exit = if keep_failed_pane {
            "tmux set-option -p remain-on-exit failed >/dev/null 2>&1 || true\n"
        } else {
            ""
        };
        let tpm_config_args = match &self.tpm_config_policy {
            TpmConfigPolicy::Resolved(Some(path)) => {
                format!(" --tpm-config-path {}", shell_quote(&path.to_string_lossy()))
            }
            TpmConfigPolicy::Resolved(None) => " --no-tpm-config".into(),
            // Parent init resolves discovery before constructing child specs.
            TpmConfigPolicy::Disabled | TpmConfigPolicy::Discover => String::new(),
        };
        let config_mode_env =
            shell_env_assignment("TMUP_CONFIG_MODE", &self.config_mode.to_string());
        format!(
            r#"channel={ch}
result_file={rf}
tty_state=
cleanup() {{ tmux wait-for -S "$channel"; }}
restore_tty() {{ [ -n "$tty_state" ] && stty "$tty_state" >/dev/null 2>&1 || true; }}
trap 'restore_tty; cleanup' EXIT INT TERM HUP
{roe}{cme} {exe} init --ui-child --wait-channel {ch} --config-path {cp}{tp} --data-root {dr} --state-root {sr}
rc=$?
printf '{{"exit_code":%d}}\n' "$rc" > "$result_file"
if [ -t 0 ]; then
  tty_state=$(stty -g 2>/dev/null || true)
  stty -icanon -echo min 1 time 0 >/dev/null 2>&1 || true
  while :; do
    key=$(dd bs=1 count=1 2>/dev/null)
    [ "$key" = 'q' ] && break
  done
fi
exit 0"#,
            ch = shell_quote(&self.wait_channel),
            rf = shell_quote(&result_file.to_string_lossy()),
            roe = remain_on_exit,
            exe = shell_quote(&self.exe.to_string_lossy()),
            cp = shell_quote(&self.config_path.to_string_lossy()),
            tp = tpm_config_args,
            dr = shell_quote(&self.data_root.to_string_lossy()),
            sr = shell_quote(&self.state_root.to_string_lossy()),
            cme = config_mode_env,
        )
    }
}

fn display_message_format(format: &str) -> Result<String> {
    let output = std::process::Command::new("tmux")
        .args(["display-message", "-p", format])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("display-message failed: {stderr}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn read_init_ui_target_once() -> Option<InitUiTarget> {
    let client = display_message_format("#{client_name}").ok()?;
    let pane = display_message_format("#{pane_id}").ok()?;
    if client.is_empty() || pane.is_empty() {
        return None;
    }
    Some(InitUiTarget { client, pane })
}

/// Schedule the deferred init bootstrap via `tmux run-shell -b ...`.
pub fn spawn_init_bootstrap(spec: &InitBootstrapSpec) -> Result<()> {
    let command = spec.build_shell_command();
    let output = std::process::Command::new("tmux")
        .args(["run-shell", "-b", &command])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("run-shell failed: {stderr}");
    }
    Ok(())
}

/// Check whether the current tmux command context already has a usable UI target.
pub fn current_init_ui_target() -> Option<InitUiTarget> {
    read_init_ui_target_once()
}

/// Probe tmux for a usable client and pane target after startup.
pub fn probe_init_ui_target() -> Option<InitUiTarget> {
    const INITIAL_BACKOFF_MS: u64 = 20;
    const MAX_DELAY_MS: u64 = 1_000;

    let mut next_delay_ms = 0;
    loop {
        let delay_ms = next_delay_ms;
        if delay_ms != 0 {
            std::thread::sleep(Duration::from_millis(delay_ms));
        }
        // Try immediately first: bootstrap may already be running after tmux has
        // finished attaching a usable client/pane target, so only back off on miss.
        if let Some(target) = read_init_ui_target_once() {
            return Some(target);
        }
        if delay_ms >= MAX_DELAY_MS {
            break;
        }
        next_delay_ms =
            if next_delay_ms == 0 { INITIAL_BACKOFF_MS } else { next_delay_ms.saturating_mul(2) };
    }

    None
}

/// Spawn an init child in a tmux popup (`display-popup`).
///
/// `display-popup` **blocks** the calling tmux client until the popup is
/// dismissed, so by the time this function returns the child has already
/// finished and signaled `wait-for -S`.  The caller must NOT call
/// `wait_for` afterwards — just read the result file directly.
pub fn spawn_init_popup(
    spec: &InitUiChildSpec,
    target: &InitUiTarget,
    result_file: &Path,
    supports_title: bool,
) -> Result<()> {
    let wrapper = spec.build_shell_wrapper(result_file, false);
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
    // tmux runs the shell-command via `/bin/sh -c <command>`, so we pass the
    // wrapper as a single positional argument — no extra `sh -c` prefix.
    let output = std::process::Command::new("tmux")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("display-popup failed: {stderr}");
    }
    Ok(())
}

/// Spawn an init child in a tmux split-window (fallback when popup is unavailable).
///
/// Unlike `display-popup`, `split-window` returns **immediately** — the
/// command runs asynchronously in the new pane.  The caller MUST call
/// `wait_for` to block until the child signals completion.
pub fn spawn_init_split(
    spec: &InitUiChildSpec,
    target: &InitUiTarget,
    result_file: &Path,
) -> Result<()> {
    let wrapper = spec.build_shell_wrapper(result_file, true);
    let output = std::process::Command::new("tmux")
        .args(["split-window", "-v", "-l", "50%", "-t", &target.pane, "--", &wrapper])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("split-window failed: {stderr}");
    }
    Ok(())
}

/// Block until the named channel is signaled via `tmux wait-for -S <channel>`.
pub fn wait_for(channel: &str) -> Result<()> {
    let status = std::process::Command::new("tmux").args(["wait-for", channel]).status()?;
    if !status.success() {
        bail!("tmux wait-for failed");
    }
    Ok(())
}

/// Display a transient status-bar message.
pub fn display_message(msg: &str) -> Result<()> {
    let output = std::process::Command::new("tmux")
        .args(["display-message", msg])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("display-message failed: {stderr}");
    }
    Ok(())
}
