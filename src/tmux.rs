use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Result, bail};

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
    /// Unset a global tmux environment variable.
    UnsetEnvironment {
        /// Environment variable name.
        key: String,
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
    /// Register a key that runs a shell command from a plugin directory.
    BindKey {
        /// Ordered `bind-key` option tokens.
        options: Vec<String>,
        /// Key passed to `bind-key`.
        key: String,
        /// Plugin installation directory used as the command working directory.
        plugin_dir: PathBuf,
        /// User shell text evaluated when the key is pressed.
        shell: String,
        /// Whether the nested `run-shell` action runs in the background.
        background: bool,
    },
}

impl TmuxCommand {
    /// Convert to tmux CLI arguments.
    pub fn to_args(&self) -> Vec<String> {
        match self {
            Self::SetEnvironment { key, value } => {
                vec!["set-environment".into(), "-g".into(), key.clone(), value.clone()]
            }
            Self::UnsetEnvironment { key } => {
                vec!["set-environment".into(), "-gu".into(), key.clone()]
            }
            Self::SetOption { key, value } => {
                vec!["set".into(), "-g".into(), format!("@{key}"), value.clone()]
            }
            Self::RunShell { script } => {
                vec!["run-shell".into(), shell_quote(&script.to_string_lossy())]
            }
            Self::BindKey { options, key, plugin_dir, shell, background } => {
                let mut args = Vec::with_capacity(options.len() + 5);
                args.push("bind-key".into());
                args.extend(options.iter().cloned());
                args.push(key.clone());
                args.push("run-shell".into());
                if *background {
                    args.push("-b".into());
                }
                args.push(format!(
                    "cd {} && exec /bin/sh -c {}",
                    shell_quote(&plugin_dir.to_string_lossy()),
                    shell_quote(shell)
                ));
                args
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
