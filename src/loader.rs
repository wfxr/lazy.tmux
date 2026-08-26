use std::path::Path;

use crate::config_mode::LoadEligibility;
use crate::model::PluginSource;
use crate::tmux::TmuxCommand;

/// Build the full load plan: set env, then for each plugin set opts + run *.tmux files.
pub fn build_load_plan(
    load_eligibility: LoadEligibility<'_>,
    plugin_root: &Path,
) -> Vec<TmuxCommand> {
    let mut plan = Vec::new();

    // 1. Set TMUX_PLUGIN_MANAGER_PATH with trailing slash
    let root_str = format!("{}/", plugin_root.display());
    plan.push(TmuxCommand::SetEnvironment {
        key: "TMUX_PLUGIN_MANAGER_PATH".into(),
        value: root_str,
    });

    // 2. For each plugin in declaration order: apply opts, then run *.tmux
    for (spec, load_eligible) in load_eligibility.plugins() {
        if !load_eligible {
            continue;
        }
        // Apply opt settings
        for (key, value) in &spec.opts {
            plan.push(TmuxCommand::SetOption {
                key: format!("{}{}", spec.opt_prefix, key),
                value: value.clone(),
            });
        }

        // Determine plugin directory
        let plugin_dir = match &spec.source {
            PluginSource::Remote { id, .. } => plugin_root.join(id),
            PluginSource::Local { path } => std::path::PathBuf::from(path),
        };

        // Find and sort *.tmux files
        let tmux_scripts = find_tmux_scripts(&plugin_dir);
        for script in tmux_scripts {
            plan.push(TmuxCommand::RunShell { script });
        }
    }

    plan
}

/// Find all *.tmux files in a directory, sorted by filename.
pub fn find_tmux_scripts(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut scripts = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Some(ext) = path.extension()
                && ext == "tmux"
            {
                scripts.push(path);
            }
        }
    }
    scripts.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    scripts
}
