use std::io::IsTerminal;

use crate::model::Config;
use crate::state::Paths;

/// Stable plugin display catalog for progress views.
pub(crate) mod catalog;
/// Live fixed-row progress renderer for TTY output.
pub(crate) mod live;
/// Shared failure-detail logging primitives.
pub(crate) mod log;
/// Structured progress event/value types.
pub(crate) mod model;
/// Reducer and snapshot state for structured progress.
pub(crate) mod reducer;
/// Shared progress line rendering from structured snapshot state.
pub(crate) mod render;
/// Reducer-driven runtime reporter core.
pub(crate) mod reporter;

pub use model::{OperationStage, PluginOutcome, PluginStage, PluginStageDetail, SkipReason};

pub(crate) const SUMMARY_MAX_LEN: usize = 80;
pub(crate) const ACTION_WIDTH: usize = 12;

#[cfg(test)]
pub(crate) mod test_support {
    use crate::model::{PluginSource, PluginSpec, Tracking};

    pub(crate) fn remote_plugin(raw: &str, id: &str, name: &str) -> PluginSpec {
        PluginSpec {
            source: PluginSource::Remote {
                raw: raw.to_string(),
                id: id.to_string(),
                clone_url: format!("https://{id}.git"),
            },
            name: name.to_string(),
            opt_prefix: "@plugin".to_string(),
            tracking: Tracking::DefaultBranch,
            build: None,
            opts: Vec::new(),
            environment: Vec::new(),
            bindings: Vec::new(),
        }
    }
}

/// Progress events emitted during command execution.
pub enum ProgressEvent<'a> {
    /// The named command has started.
    OperationStart {
        /// The subcommand name (e.g. `"init"`, `"update"`).
        command: &'static str,
    },
    /// The overall operation has advanced to a new stage.
    OperationStage {
        /// The new operation stage.
        stage: OperationStage,
    },
    /// A plugin has advanced to a new processing stage.
    PluginStage {
        /// Stable remote identifier for the plugin.
        id: &'a str,
        /// Human-readable plugin name.
        name: &'a str,
        /// The new plugin stage.
        stage: PluginStage,
        /// Optional stage-specific structured detail.
        detail: Option<PluginStageDetail>,
    },
    /// A plugin reached a terminal outcome.
    PluginFinished {
        /// Stable remote identifier for the plugin.
        id: &'a str,
        /// Human-readable plugin name.
        name: &'a str,
        /// Structured completion outcome.
        outcome: PluginOutcome,
    },
    /// A plugin failed with a short summary and full diagnostic detail.
    PluginFailed {
        /// Stable remote identifier for the plugin.
        id: &'a str,
        /// Human-readable plugin name.
        name: &'a str,
        /// The processing stage at which the failure occurred, when known.
        ///
        /// Runtime command paths usually emit `Some(stage)`, but `None` remains
        /// valid for callers that only have a terminal failure summary.
        stage: Option<PluginStage>,
        /// One-line failure summary suitable for terminal output.
        summary: String,
        /// Full diagnostic text written to the log file.
        detail: String,
        /// Extra key-value context written to the log file.
        context: Vec<(&'static str, String)>,
    },
    /// The overall operation failed with a short summary and full diagnostic detail.
    OperationFailed {
        /// One-line failure summary suitable for terminal output.
        summary: String,
        /// Full diagnostic text written to the log file.
        detail: String,
    },
    /// The named command has completed.
    OperationEnd {
        /// The subcommand name (e.g. `"init"`, `"update"`).
        command: &'static str,
        /// Whether the operation completed successfully.
        success: bool,
    },
}

/// Trait for receiving progress events.
pub trait ProgressReporter: Send + Sync {
    /// Deliver a single progress event to this reporter.
    fn report(&self, event: ProgressEvent<'_>);
}

/// An error that carries a user-facing progress message.
#[derive(Debug)]
pub struct ProgressFailure {
    message: String,
}

impl ProgressFailure {
    /// Create a new `ProgressFailure` with the given message.
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

impl std::fmt::Display for ProgressFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ProgressFailure {}

/// A sentinel error indicating failure was already reported via progress output.
#[derive(Debug)]
pub struct ReportedError;

impl std::fmt::Display for ReportedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "operation failed (see progress output)")
    }
}

impl std::error::Error for ReportedError {}

/// Wrap a message into a `ProgressFailure` error.
pub fn progress_failure(message: impl Into<String>) -> anyhow::Error {
    ProgressFailure::new(message).into()
}

/// Construct a `ReportedError` as an `anyhow::Error`.
pub fn reported_error() -> anyhow::Error {
    ReportedError.into()
}

/// Return `true` if any cause in the error chain is a `ProgressFailure`.
pub fn is_progress_failure(err: &anyhow::Error) -> bool {
    err.chain().any(|e| e.downcast_ref::<ProgressFailure>().is_some())
}

/// Return `true` if any cause in the error chain is a `ReportedError`.
pub fn is_reported_error(err: &anyhow::Error) -> bool {
    err.chain().any(|e| e.downcast_ref::<ReportedError>().is_some())
}

/// Create a reporter for human-readable progress output.
pub fn create_reporter(
    paths: &Paths,
    command: &'static str,
    config: &Config,
    target_id: Option<&str>,
) -> Box<dyn ProgressReporter> {
    let catalog = catalog::DisplayCatalog::from_config(config, target_id);
    if std::io::stderr().is_terminal() {
        Box::new(reporter::ReducerReporter::new_live(&paths.logs_root, command, catalog))
    } else {
        Box::new(reporter::ReducerReporter::new(&paths.logs_root, command, catalog))
    }
}

/// Strip ANSI escape sequences and non-printable control characters.
///
/// This helper intentionally handles the sequences that tmup emits/consumes in
/// progress output (`CSI` and `OSC`) rather than implementing full terminal
/// control-sequence parsing.
pub(crate) fn strip_ansi(s: &str) -> String {
    #[derive(Clone, Copy)]
    enum StripState {
        Normal,
        Escape,
        Csi,
        Osc,
        OscEscape,
    }

    let mut result = String::with_capacity(s.len());
    let mut state = StripState::Normal;

    for ch in s.chars() {
        state = match state {
            StripState::Normal => {
                if ch == '\x1b' {
                    StripState::Escape
                } else {
                    if !ch.is_control() {
                        result.push(ch);
                    }
                    StripState::Normal
                }
            }
            StripState::Escape => match ch {
                '[' => StripState::Csi,
                ']' => StripState::Osc,
                _ => StripState::Normal,
            },
            StripState::Csi => {
                if ch.is_ascii() && matches!(ch as u8, 0x40..=0x7e) {
                    StripState::Normal
                } else {
                    StripState::Csi
                }
            }
            StripState::Osc => {
                if ch == '\x07' {
                    // OSC terminated by BEL.
                    StripState::Normal
                } else if ch == '\x1b' {
                    // Potential ST terminator (ESC \).
                    StripState::OscEscape
                } else {
                    StripState::Osc
                }
            }
            StripState::OscEscape => {
                if ch == '\\' {
                    StripState::Normal
                } else if ch == '\x1b' {
                    // Stay pending in case multiple ESC bytes are emitted.
                    StripState::OscEscape
                } else {
                    StripState::Osc
                }
            }
        }
    }

    result
}

/// Sanitize a string: strip ANSI and collapse whitespace to a single line.
fn sanitize(s: &str) -> String {
    strip_ansi(s).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Sanitize and truncate to `max_chars` by character count.
///
/// This summary budget is character-based (not display-width-based) because the
/// same summary string is reused in logs and non-terminal contexts. Terminal
/// renderers clamp display width separately when drawing lines.
pub(crate) fn sanitize_summary(s: &str, max_chars: usize) -> String {
    let clean = sanitize(s);
    if clean.chars().count() <= max_chars {
        return clean;
    }
    let truncated: String = clean.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}\u{2026}")
}

/// Extract a one-line summary and full detail from an anyhow error.
pub fn summarize_error(err: &anyhow::Error) -> (String, String) {
    let detail = format!("{err:?}");
    let summary = sanitize_summary(&format!("{err}"), SUMMARY_MAX_LEN);
    (summary, detail)
}

/// A no-op reporter that silently discards all progress events.
pub struct NullReporter;

impl ProgressReporter for NullReporter {
    fn report(&self, _event: ProgressEvent<'_>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_csi_sequences() {
        assert_eq!(strip_ansi("\x1b[31mhello\x1b[0m"), "hello");
        assert_eq!(strip_ansi("\x1b[1;33;40mworld\x1b[0m"), "world");
    }

    #[test]
    fn strip_ansi_removes_osc_sequences() {
        assert_eq!(strip_ansi("\x1b]8;;https://example.com\x07link\x1b]8;;\x07"), "link");
        assert_eq!(strip_ansi("\x1b]0;title\x1b\\hello"), "hello");
    }

    #[test]
    fn strip_ansi_removes_control_chars() {
        assert_eq!(strip_ansi("hello\x07world\tok"), "helloworldok");
    }

    #[test]
    fn sanitize_collapses_whitespace() {
        assert_eq!(sanitize("  hello   world\n  foo  "), "hello world foo");
    }

    #[test]
    fn sanitize_summary_truncates() {
        let long = "a".repeat(100);
        let result = sanitize_summary(&long, 10);
        assert_eq!(result.chars().count(), 10);
        assert!(result.ends_with('\u{2026}'));
    }

    #[test]
    fn sanitize_summary_uses_character_budget_for_wide_chars() {
        let result = sanitize_summary("你好世界", 3);
        assert_eq!(result, "你好…");
        assert_eq!(result.chars().count(), 3);
    }

    #[test]
    fn summarize_error_produces_single_line() {
        let err = anyhow::anyhow!("line1\nline2\nline3");
        let (summary, detail) = summarize_error(&err);
        assert!(!summary.contains('\n'));
        assert!(detail.contains("line1"));
    }

    #[test]
    fn null_reporter_accepts_events() {
        let reporter = NullReporter;
        reporter.report(ProgressEvent::OperationStart { command: "test" });
        reporter.report(ProgressEvent::OperationEnd { command: "test", success: true });
    }
}
