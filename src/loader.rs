use std::path::Path;

use crate::config_mode::LoadEligibility;
use crate::model::{EnvironmentOperation, PluginSource, PluginSpec};
use crate::tmux::TmuxCommand;

/// Build the full load plan: set the manager path, configure plugins, run `*.tmux` files, then bind keys.
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

    // 2. Configure each eligible plugin in declaration order.
    for (spec, load_eligible) in load_eligibility.plugins() {
        if !load_eligible {
            continue;
        }
        for operation in &spec.environment {
            match operation {
                EnvironmentOperation::Set { name, value } => {
                    plan.push(TmuxCommand::SetEnvironment {
                        key: name.clone(),
                        value: value.clone(),
                    });
                }
                EnvironmentOperation::Unset { name } => {
                    plan.push(TmuxCommand::UnsetEnvironment { key: name.clone() });
                }
            }
        }

        // Apply opt settings
        for (key, value) in &spec.opts {
            plan.push(TmuxCommand::SetOption {
                key: format!("{}{}", spec.opt_prefix, key),
                value: value.clone(),
            });
        }
    }

    // 3. Load scripts only after every eligible plugin's environment and options are configured.
    for (spec, load_eligible) in load_eligibility.plugins() {
        if !load_eligible {
            continue;
        }
        let plugin_dir = resolved_plugin_dir(spec, plugin_root);

        // Find and sort *.tmux files
        let tmux_scripts = find_tmux_scripts(&plugin_dir);
        for script in tmux_scripts {
            plan.push(TmuxCommand::RunShell { script });
        }
    }

    // 4. Register explicit bindings after all plugin scripts so declarations can override defaults.
    for (spec, load_eligible) in load_eligibility.plugins() {
        if !load_eligible {
            continue;
        }
        let plugin_dir = resolved_plugin_dir(spec, plugin_root);
        for binding in &spec.bindings {
            plan.push(TmuxCommand::BindKey {
                options: binding.options.clone(),
                key: binding.key.clone(),
                plugin_dir: plugin_dir.clone(),
                shell: binding.shell.clone(),
                background: binding.background,
            });
        }
    }

    plan
}

fn resolved_plugin_dir(spec: &PluginSpec, plugin_root: &Path) -> std::path::PathBuf {
    match &spec.source {
        PluginSource::Remote { id, .. } => plugin_root.join(id),
        PluginSource::Local { path } => std::path::PathBuf::from(path),
    }
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
