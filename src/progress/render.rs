use crate::progress::model::{TrackingResolution, TrackingSelector};
use crate::progress::reducer::{PluginSnapshot, ProgressSnapshot, SnapshotUpdate};
use crate::progress::{OperationStage, PluginOutcome, PluginStage, PluginStageDetail, SkipReason};
use crate::termui::{self, Accent};

#[cfg(test)]
const ACTION_WIDTH: usize = 12;

/// Renderer-neutral display line derived from structured progress state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisplayLine {
    /// Logical visual kind used by renderers.
    pub(crate) kind: LineKind,
    /// Visual accent used to style the line label.
    pub(crate) accent: Accent,
    /// Right-aligned label shown ahead of the message.
    pub(crate) label: String,
    /// Main line content.
    pub(crate) message: String,
}

impl DisplayLine {
    /// Render this display line with styled label/message formatting.
    pub(crate) fn styled(&self, action_width: usize) -> String {
        termui::format_styled_labeled_line(&self.label, action_width, &self.message, self.accent)
    }

    #[cfg(test)]
    fn plain(&self) -> String {
        crate::termui::format_plain_labeled_line(&self.label, ACTION_WIDTH, &self.message)
    }
}

/// Logical kind of rendered display line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineKind {
    /// Operation-level stage information.
    Stage,
    /// Successful completion for a plugin or operation.
    Success,
    /// Warning information.
    Warning,
    /// Failure information.
    Failure,
}

/// Append-only renderer that formats structured progress snapshot changes.
#[derive(Debug, Default)]
pub(crate) struct TranscriptRenderer;

impl TranscriptRenderer {
    /// Construct a new transcript renderer.
    pub(crate) fn new() -> Self {
        Self
    }

    /// Render one structured event against the current snapshot into transcript lines.
    #[cfg(test)]
    pub(crate) fn render_event(
        &self,
        snapshot: &ProgressSnapshot,
        event: &SnapshotUpdate,
    ) -> Vec<String> {
        self.render_lines(snapshot, event).into_iter().map(|line| line.plain()).collect()
    }

    /// Render one structured event against the current snapshot into display lines.
    pub(crate) fn render_lines(
        &self,
        snapshot: &ProgressSnapshot,
        event: &SnapshotUpdate,
    ) -> Vec<DisplayLine> {
        match event {
            SnapshotUpdate::OperationStageChanged { stage } => vec![DisplayLine {
                kind: LineKind::Stage,
                accent: Accent::Info,
                label: title_case(&stage.to_string()),
                message: operation_message(*stage).to_string(),
            }],
            SnapshotUpdate::OperationFailed { .. } => Vec::new(),
            SnapshotUpdate::PluginStageChanged { id, stage, detail } => {
                if matches!(stage, PluginStage::Applying)
                    && !matches!(detail, Some(PluginStageDetail::BuildCommand(_)))
                {
                    return Vec::new();
                }
                let Some(plugin) = snapshot_plugin(snapshot, id) else {
                    return Vec::new();
                };
                vec![DisplayLine {
                    kind: LineKind::Stage,
                    accent: Accent::Info,
                    label: title_case(plugin_stage_label(*stage)),
                    message: plugin_stage_message(&plugin.label, *stage, detail.as_ref()),
                }]
            }
            SnapshotUpdate::PluginFinished { id, outcome } => {
                let Some(plugin) = snapshot_plugin(snapshot, id) else {
                    return Vec::new();
                };
                let (kind, accent, label, message) = plugin_outcome_message(&plugin.label, outcome);
                vec![DisplayLine { kind, accent, label: label.to_string(), message }]
            }
            SnapshotUpdate::PluginFailed { id, summary, .. } => {
                let Some(plugin) = snapshot_plugin(snapshot, id) else {
                    return Vec::new();
                };
                vec![DisplayLine {
                    kind: LineKind::Failure,
                    accent: Accent::Error,
                    label: "Failed".to_string(),
                    message: format!("{} {}", plugin.label, summary),
                }]
            }
        }
    }
}

fn snapshot_plugin<'a>(snapshot: &'a ProgressSnapshot, id: &str) -> Option<&'a PluginSnapshot> {
    match snapshot.plugin(id) {
        Some(plugin) => Some(plugin),
        None => {
            debug_assert!(false, "missing plugin snapshot for id={id}");
            None
        }
    }
}

fn operation_message(stage: OperationStage) -> &'static str {
    match stage {
        OperationStage::WaitingForLock => "lock",
        OperationStage::Syncing => "remote plugins",
        OperationStage::ApplyingWrites => "plugin contents",
        OperationStage::LoadingTmux => "applying load plan",
    }
}

fn plugin_stage_label(stage: PluginStage) -> &'static str {
    match stage {
        PluginStage::Cloning => "cloning",
        PluginStage::Fetching => "fetching",
        PluginStage::Resolving => "resolving",
        PluginStage::CheckingOut => "checking out",
        PluginStage::Applying => "building",
        PluginStage::Loading => "loading",
    }
}

fn plugin_stage_message(
    label: &str,
    stage: PluginStage,
    detail: Option<&PluginStageDetail>,
) -> String {
    match (stage, detail) {
        (PluginStage::Cloning | PluginStage::Fetching, Some(PluginStageDetail::CloneUrl(url))) => {
            format!("{label} {url}")
        }
        (
            PluginStage::Resolving,
            Some(PluginStageDetail::TrackingResolution { selector, resolved, commit }),
        ) => {
            format!("{label} {}", tracking_detail_text(selector, resolved, commit))
        }
        (PluginStage::Applying, Some(PluginStageDetail::BuildCommand(cmd))) => {
            format!(
                "{label} {}",
                crate::progress::sanitize_summary(cmd, crate::progress::SUMMARY_MAX_LEN)
            )
        }
        _ => label.to_string(),
    }
}

fn tracking_selector_text(selector: &TrackingSelector) -> String {
    match selector {
        TrackingSelector::DefaultBranch => "default-branch".to_string(),
        TrackingSelector::Branch(branch) => format!("branch@{branch}"),
        TrackingSelector::Tag(tag) => format!("tag@{tag}"),
        TrackingSelector::Commit(commit) => format!("commit@{commit}"),
    }
}

fn tracking_detail_text(
    selector: &TrackingSelector,
    resolved: &TrackingResolution,
    commit: &str,
) -> String {
    match (selector, resolved) {
        (_, TrackingResolution::Unknown { kind, value }) => {
            unknown_tracking_detail_text(selector, kind, value, commit)
        }
        (TrackingSelector::DefaultBranch, TrackingResolution::DefaultBranch { branch })
        | (TrackingSelector::DefaultBranch, TrackingResolution::Branch { branch }) => {
            format!("default-branch -> branch@{branch} -> commit@{commit}")
        }
        (TrackingSelector::DefaultBranch, TrackingResolution::Tag { tag }) => {
            format!("default-branch -> tag@{tag} -> commit@{commit}")
        }
        (
            TrackingSelector::DefaultBranch,
            TrackingResolution::Commit { commit: resolved_commit },
        ) => {
            format!("default-branch -> commit@{resolved_commit}")
        }
        (
            TrackingSelector::Branch(branch),
            TrackingResolution::DefaultBranch { .. }
            | TrackingResolution::Branch { .. }
            | TrackingResolution::Tag { .. }
            | TrackingResolution::Commit { .. },
        ) => format!("branch@{branch} -> commit@{commit}"),
        (
            TrackingSelector::Tag(tag),
            TrackingResolution::DefaultBranch { .. }
            | TrackingResolution::Branch { .. }
            | TrackingResolution::Tag { .. }
            | TrackingResolution::Commit { .. },
        ) => format!("tag@{tag} -> commit@{commit}"),
        (
            TrackingSelector::Commit(selector_commit),
            TrackingResolution::DefaultBranch { .. }
            | TrackingResolution::Branch { .. }
            | TrackingResolution::Tag { .. }
            | TrackingResolution::Commit { .. },
        ) => format!("commit@{selector_commit}"),
    }
}

fn unknown_tracking_detail_text(
    selector: &TrackingSelector,
    kind: &str,
    value: &str,
    commit: &str,
) -> String {
    format!("{} -> {}@{} -> commit@{}", tracking_selector_text(selector), kind, value, commit)
}

fn plugin_outcome_message(
    label: &str,
    outcome: &PluginOutcome,
) -> (LineKind, Accent, &'static str, String) {
    match outcome {
        PluginOutcome::Installed { commit } => {
            (LineKind::Success, Accent::Success, "Installed", format!("{label} commit@{commit}"))
        }
        PluginOutcome::Updated { from, to } => (
            LineKind::Success,
            Accent::Success,
            "Updated",
            format!("{label} commit@{from} -> commit@{to}"),
        ),
        PluginOutcome::Synced { commit } => {
            (LineKind::Success, Accent::Success, "Synced", format!("{label} commit@{commit}"))
        }
        PluginOutcome::Restored { commit } => {
            (LineKind::Success, Accent::Success, "Restored", format!("{label} commit@{commit}"))
        }
        PluginOutcome::Reconciled => {
            (LineKind::Success, Accent::Success, "Reconciled", label.to_string())
        }
        PluginOutcome::CheckedUpToDate => {
            (LineKind::Success, Accent::Success, "Checked", label.to_string())
        }
        PluginOutcome::AlreadyRestored => {
            (LineKind::Success, Accent::Success, "Checked", label.to_string())
        }
        PluginOutcome::Skipped { reason } => (
            LineKind::Warning,
            Accent::Warning,
            "Skipped",
            format!("{label} {}", skip_reason_text(reason)),
        ),
    }
}

fn skip_reason_text(reason: &SkipReason) -> String {
    match reason {
        SkipReason::PinnedTag { tag } => format!("pinned to tag {tag}"),
        SkipReason::PinnedCommit { commit } => format!("pinned to commit {commit}"),
        SkipReason::KnownFailure { commit } => format!("known build failure at {commit}"),
        SkipReason::Other(reason) => reason.clone(),
    }
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{LineKind, TranscriptRenderer};
    use crate::progress::model::{TrackingResolution, TrackingSelector};
    use crate::progress::reducer::{
        PluginDisplayState, ProgressSnapshot, SnapshotUpdate, apply_event,
    };
    use crate::progress::{
        OperationStage, PluginOutcome, PluginStage, PluginStageDetail, ProgressEvent, SkipReason,
    };

    #[test]
    fn transcript_renderer_formats_structured_plugin_outcomes() {
        let mut snapshot = ProgressSnapshot::new_for_tests([(
            "github.com/tmux-plugins/tmux-sensible",
            "tmux-sensible",
        )]);
        let renderer = TranscriptRenderer::new();

        let operation_event =
            SnapshotUpdate::OperationStageChanged { stage: OperationStage::WaitingForLock };
        apply_event(&mut snapshot, &operation_event);
        assert_eq!(
            renderer.render_event(&snapshot, &operation_event),
            vec!["     Waiting lock".to_string()]
        );

        let fetching_event = SnapshotUpdate::PluginStageChanged {
            id: "github.com/tmux-plugins/tmux-sensible".to_string(),
            stage: PluginStage::Fetching,
            detail: Some(PluginStageDetail::CloneUrl(
                "https://github.com/tmux-plugins/tmux-sensible.git".to_string(),
            )),
        };
        apply_event(&mut snapshot, &fetching_event);
        assert_eq!(
            renderer.render_event(&snapshot, &fetching_event),
            vec![
                "    Fetching tmux-sensible https://github.com/tmux-plugins/tmux-sensible.git"
                    .to_string()
            ]
        );

        let resolving_event = SnapshotUpdate::PluginStageChanged {
            id: "github.com/tmux-plugins/tmux-sensible".to_string(),
            stage: PluginStage::Resolving,
            detail: Some(PluginStageDetail::TrackingResolution {
                selector: TrackingSelector::DefaultBranch,
                resolved: TrackingResolution::DefaultBranch { branch: "main".to_string() },
                commit: "8c1eeec".to_string(),
            }),
        };
        apply_event(&mut snapshot, &resolving_event);
        assert_eq!(
            renderer.render_event(&snapshot, &resolving_event),
            vec![
                "   Resolving tmux-sensible default-branch -> branch@main -> commit@8c1eeec"
                    .to_string()
            ]
        );

        for (outcome, expected) in [
            (
                PluginOutcome::Installed { commit: "8c1eeec".to_string() },
                "   Installed tmux-sensible commit@8c1eeec",
            ),
            (
                PluginOutcome::Updated { from: "8c1eeec".to_string(), to: "def5678".to_string() },
                "     Updated tmux-sensible commit@8c1eeec -> commit@def5678",
            ),
            (
                PluginOutcome::Synced { commit: "8c1eeec".to_string() },
                "      Synced tmux-sensible commit@8c1eeec",
            ),
            (PluginOutcome::Reconciled, "  Reconciled tmux-sensible"),
            (PluginOutcome::CheckedUpToDate, "     Checked tmux-sensible"),
        ] {
            let event = SnapshotUpdate::PluginFinished {
                id: "github.com/tmux-plugins/tmux-sensible".to_string(),
                outcome,
            };
            apply_event(&mut snapshot, &event);
            assert_eq!(renderer.render_event(&snapshot, &event), vec![expected.to_string()]);
        }

        let failed_event = SnapshotUpdate::PluginFailed {
            id: "github.com/tmux-plugins/tmux-sensible".to_string(),
            stage: Some(PluginStage::Applying),
            summary: "build failed".to_string(),
        };
        apply_event(&mut snapshot, &failed_event);
        assert_eq!(
            renderer.render_event(&snapshot, &failed_event),
            vec!["      Failed tmux-sensible build failed".to_string()]
        );

        assert!(matches!(
            snapshot.plugins[0].state,
            PluginDisplayState::Finished(PluginOutcome::Installed { .. })
        ));
    }

    #[test]
    fn transcript_renderer_formats_sync_specific_outcomes() {
        // Compile-time protocol check: runtime progress carries
        // structured completion outcomes for sync.
        let _runtime_reconciled = ProgressEvent::PluginFinished {
            id: "github.com/tmux-plugins/tmux-sensible",
            name: "tmux-sensible",
            outcome: PluginOutcome::Reconciled,
        };
        let _runtime_synced = ProgressEvent::PluginFinished {
            id: "github.com/tmux-plugins/tmux-sensible",
            name: "tmux-sensible",
            outcome: PluginOutcome::Synced { commit: "8c1eeec".to_string() },
        };

        let mut snapshot = ProgressSnapshot::new_for_tests([(
            "github.com/tmux-plugins/tmux-sensible",
            "tmux-sensible",
        )]);
        let renderer = TranscriptRenderer::new();

        for (outcome, expected) in [
            (PluginOutcome::Reconciled, "  Reconciled tmux-sensible"),
            (
                PluginOutcome::Synced { commit: "8c1eeec".to_string() },
                "      Synced tmux-sensible commit@8c1eeec",
            ),
        ] {
            let event = SnapshotUpdate::PluginFinished {
                id: "github.com/tmux-plugins/tmux-sensible".to_string(),
                outcome,
            };
            apply_event(&mut snapshot, &event);
            assert_eq!(renderer.render_event(&snapshot, &event), vec![expected.to_string()]);
        }
    }

    #[test]
    fn transcript_renderer_formats_install_update_restore_outcomes() {
        // Compile-time protocol check: runtime progress carries
        // structured completion outcomes for install/update/restore + skips.
        let _runtime_installed = ProgressEvent::PluginFinished {
            id: "github.com/tmux-plugins/tmux-sensible",
            name: "tmux-sensible",
            outcome: PluginOutcome::Installed { commit: "8c1eeec".to_string() },
        };
        let _runtime_updated = ProgressEvent::PluginFinished {
            id: "github.com/tmux-plugins/tmux-sensible",
            name: "tmux-sensible",
            outcome: PluginOutcome::Updated {
                from: "8c1eeec".to_string(),
                to: "def5678".to_string(),
            },
        };
        let _runtime_restored = ProgressEvent::PluginFinished {
            id: "github.com/tmux-plugins/tmux-sensible",
            name: "tmux-sensible",
            outcome: PluginOutcome::Restored { commit: "8c1eeec".to_string() },
        };
        let _runtime_already_restored = ProgressEvent::PluginFinished {
            id: "github.com/tmux-plugins/tmux-sensible",
            name: "tmux-sensible",
            outcome: PluginOutcome::AlreadyRestored,
        };
        let _runtime_skipped = ProgressEvent::PluginFinished {
            id: "github.com/tmux-plugins/tmux-sensible",
            name: "tmux-sensible",
            outcome: PluginOutcome::Skipped {
                reason: SkipReason::PinnedTag { tag: "v1.0.0".to_string() },
            },
        };

        let mut snapshot = ProgressSnapshot::new_for_tests([(
            "github.com/tmux-plugins/tmux-sensible",
            "tmux-sensible",
        )]);
        let renderer = TranscriptRenderer::new();

        for (outcome, expected) in [
            (
                PluginOutcome::Installed { commit: "8c1eeec".to_string() },
                "   Installed tmux-sensible commit@8c1eeec",
            ),
            (
                PluginOutcome::Updated { from: "8c1eeec".to_string(), to: "def5678".to_string() },
                "     Updated tmux-sensible commit@8c1eeec -> commit@def5678",
            ),
            (
                PluginOutcome::Restored { commit: "8c1eeec".to_string() },
                "    Restored tmux-sensible commit@8c1eeec",
            ),
            (PluginOutcome::AlreadyRestored, "     Checked tmux-sensible"),
            (
                PluginOutcome::Skipped {
                    reason: SkipReason::PinnedTag { tag: "v1.0.0".to_string() },
                },
                "     Skipped tmux-sensible pinned to tag v1.0.0",
            ),
        ] {
            let event = SnapshotUpdate::PluginFinished {
                id: "github.com/tmux-plugins/tmux-sensible".to_string(),
                outcome,
            };
            apply_event(&mut snapshot, &event);
            assert_eq!(renderer.render_event(&snapshot, &event), vec![expected.to_string()]);
        }
    }

    #[test]
    fn transcript_renderer_never_parses_summary_strings() {
        let mut snapshot = ProgressSnapshot::new_for_tests([(
            "github.com/tmux-plugins/tmux-sensible",
            "tmux-sensible",
        )]);
        let renderer = TranscriptRenderer::new();

        for (outcome, expected) in [
            (
                PluginOutcome::Updated { from: "abc1234".to_string(), to: "def5678".to_string() },
                "     Updated tmux-sensible commit@abc1234 -> commit@def5678",
            ),
            (
                PluginOutcome::Updated { from: "updated".to_string(), to: "synced".to_string() },
                "     Updated tmux-sensible commit@updated -> commit@synced",
            ),
            (
                PluginOutcome::Synced { commit: "8c1eeec".to_string() },
                "      Synced tmux-sensible commit@8c1eeec",
            ),
            (PluginOutcome::CheckedUpToDate, "     Checked tmux-sensible"),
            (
                PluginOutcome::Restored { commit: "8c1eeec".to_string() },
                "    Restored tmux-sensible commit@8c1eeec",
            ),
        ] {
            let event = SnapshotUpdate::PluginFinished {
                id: "github.com/tmux-plugins/tmux-sensible".to_string(),
                outcome,
            };
            apply_event(&mut snapshot, &event);
            assert_eq!(renderer.render_event(&snapshot, &event), vec![expected.to_string()]);
        }
    }

    #[test]
    fn transcript_renderer_skips_applying_stage_without_build_command() {
        let mut snapshot = ProgressSnapshot::new_for_tests([(
            "github.com/tmux-plugins/tmux-sensible",
            "tmux-sensible",
        )]);
        let renderer = TranscriptRenderer::new();
        let event = SnapshotUpdate::PluginStageChanged {
            id: "github.com/tmux-plugins/tmux-sensible".to_string(),
            stage: PluginStage::Applying,
            detail: None,
        };
        apply_event(&mut snapshot, &event);
        assert_eq!(renderer.render_event(&snapshot, &event), Vec::<String>::new());
    }

    #[test]
    fn transcript_renderer_sanitizes_build_command_detail() {
        let mut snapshot = ProgressSnapshot::new_for_tests([(
            "github.com/tmux-plugins/tmux-sensible",
            "tmux-sensible",
        )]);
        let renderer = TranscriptRenderer::new();
        let event = SnapshotUpdate::PluginStageChanged {
            id: "github.com/tmux-plugins/tmux-sensible".to_string(),
            stage: PluginStage::Applying,
            detail: Some(PluginStageDetail::BuildCommand(format!(
                "printf 'hello\n{}\tworld{}'",
                '\x1b', '\x1b'
            ))),
        };

        apply_event(&mut snapshot, &event);
        let rendered = renderer.render_event(&snapshot, &event);
        assert_eq!(rendered.len(), 1);
        assert!(rendered[0].contains("Building"));
        assert!(rendered[0].contains("tmux-sensible"));
        assert!(!rendered[0].contains('\n'), "rendered line must stay single-line");
        assert!(
            !rendered[0].contains('\x1b'),
            "rendered line must not leak ANSI escapes: {:?}",
            rendered[0]
        );
    }

    #[test]
    fn transcript_renderer_formats_clone_and_checkout_stages() {
        let mut snapshot = ProgressSnapshot::new_for_tests([(
            "github.com/tmux-plugins/tmux-sensible",
            "tmux-sensible",
        )]);
        let renderer = TranscriptRenderer::new();

        let cloning_event = SnapshotUpdate::PluginStageChanged {
            id: "github.com/tmux-plugins/tmux-sensible".to_string(),
            stage: PluginStage::Cloning,
            detail: Some(PluginStageDetail::CloneUrl(
                "https://github.com/tmux-plugins/tmux-sensible.git".to_string(),
            )),
        };
        apply_event(&mut snapshot, &cloning_event);
        assert_eq!(
            renderer.render_event(&snapshot, &cloning_event),
            vec![
                "     Cloning tmux-sensible https://github.com/tmux-plugins/tmux-sensible.git"
                    .to_string()
            ]
        );

        let checkout_event = SnapshotUpdate::PluginStageChanged {
            id: "github.com/tmux-plugins/tmux-sensible".to_string(),
            stage: PluginStage::CheckingOut,
            detail: None,
        };
        apply_event(&mut snapshot, &checkout_event);
        assert_eq!(
            renderer.render_event(&snapshot, &checkout_event),
            vec!["Checking out tmux-sensible".to_string()]
        );
    }

    #[test]
    fn transcript_renderer_uses_warning_style_for_skipped_outcomes() {
        let mut snapshot = ProgressSnapshot::new_for_tests([(
            "github.com/tmux-plugins/tmux-sensible",
            "tmux-sensible",
        )]);
        let renderer = TranscriptRenderer::new();

        for (reason, expected) in [
            (
                SkipReason::PinnedCommit { commit: "abc1234".to_string() },
                "tmux-sensible pinned to commit abc1234",
            ),
            (
                SkipReason::KnownFailure { commit: "def5678".to_string() },
                "tmux-sensible known build failure at def5678",
            ),
            (SkipReason::Other("not selected".to_string()), "tmux-sensible not selected"),
        ] {
            let event = SnapshotUpdate::PluginFinished {
                id: "github.com/tmux-plugins/tmux-sensible".to_string(),
                outcome: PluginOutcome::Skipped { reason },
            };
            apply_event(&mut snapshot, &event);
            let lines = renderer.render_lines(&snapshot, &event);
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0].kind, LineKind::Warning);
            assert_eq!(lines[0].accent, crate::termui::Accent::Warning);
            assert_eq!(lines[0].label, "Skipped");
            assert_eq!(lines[0].message, expected);
        }
    }

    #[test]
    fn transcript_renderer_preserves_unknown_tracking_kind_in_resolving_output() {
        let mut snapshot = ProgressSnapshot::new_for_tests([(
            "github.com/tmux-plugins/tmux-sensible",
            "tmux-sensible",
        )]);
        let renderer = TranscriptRenderer::new();

        let event = SnapshotUpdate::PluginStageChanged {
            id: "github.com/tmux-plugins/tmux-sensible".to_string(),
            stage: PluginStage::Resolving,
            detail: Some(PluginStageDetail::TrackingResolution {
                selector: TrackingSelector::DefaultBranch,
                resolved: TrackingResolution::Unknown {
                    kind: "unexpected-kind".to_string(),
                    value: "mainline".to_string(),
                },
                commit: "8c1eeec".to_string(),
            }),
        };

        apply_event(&mut snapshot, &event);
        assert_eq!(
            renderer.render_event(&snapshot, &event),
            vec![
                "   Resolving tmux-sensible default-branch -> unexpected-kind@mainline -> commit@8c1eeec"
                    .to_string()
            ]
        );
    }
}
