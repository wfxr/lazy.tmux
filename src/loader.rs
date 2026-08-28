use std::path::Path;

use crate::config_mode::{LoadEligibility, RuntimeSetup};
use crate::model::{EnvironmentOperation, PluginSource, PluginSpec};
use crate::tmux::TmuxCommand;

/// One tmux command attributed to the plugin that declared or supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLoadCommand {
    /// Canonical remote plugin ID, or the expanded path for a local plugin.
    pub plugin_id: String,
    /// Human-readable plugin name used in diagnostics.
    pub plugin_name: String,
    /// Tmux command to execute for the plugin.
    pub command: TmuxCommand,
}

/// Globally phased tmux commands for one Init Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadPlan {
    /// Initial tmup-owned setup command that cannot be attributed to a plugin.
    pub global_setup: TmuxCommand,
    /// Plugin-attributable commands in global phase order.
    pub plugin_commands: Vec<PluginLoadCommand>,
}

impl LoadPlan {
    /// Iterate over every command in execution order.
    pub fn iter(&self) -> impl Iterator<Item = &TmuxCommand> {
        std::iter::once(&self.global_setup)
            .chain(self.plugin_commands.iter().map(|entry| &entry.command))
    }
}

/// Build the full load plan: set the manager path, configure plugins, run `*.tmux` files, then bind keys.
pub fn build_load_plan(load_eligibility: LoadEligibility<'_>, plugin_root: &Path) -> LoadPlan {
    // 1. Set TMUX_PLUGIN_MANAGER_PATH with trailing slash
    let root_str = format!("{}/", plugin_root.display());
    let global_setup =
        TmuxCommand::SetEnvironment { key: "TMUX_PLUGIN_MANAGER_PATH".into(), value: root_str };
    let mut plugin_commands = Vec::new();

    // 2. Configure each eligible plugin in declaration order.
    for (spec, load_eligible, runtime_setup) in load_eligibility.plugins_with_runtime_setup() {
        if !load_eligible {
            continue;
        }
        for declaration in runtime_setup {
            match declaration {
                RuntimeSetup::Environment(EnvironmentOperation::Set { name, value }) => {
                    push_plugin_command(
                        &mut plugin_commands,
                        spec,
                        TmuxCommand::SetEnvironment { key: name.clone(), value: value.clone() },
                    );
                }
                RuntimeSetup::Environment(EnvironmentOperation::Unset { name }) => {
                    push_plugin_command(
                        &mut plugin_commands,
                        spec,
                        TmuxCommand::UnsetEnvironment { key: name.clone() },
                    );
                }
                RuntimeSetup::Option { key, value } => {
                    push_plugin_command(
                        &mut plugin_commands,
                        spec,
                        TmuxCommand::SetOption {
                            key: format!("{}{}", spec.opt_prefix, key),
                            value: value.clone(),
                        },
                    );
                }
            }
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
            push_plugin_command(&mut plugin_commands, spec, TmuxCommand::RunShell { script });
        }
    }

    // 4. Register explicit bindings after all plugin scripts so declarations can override defaults.
    for (spec, load_eligible) in load_eligibility.plugins() {
        if !load_eligible {
            continue;
        }
        let plugin_dir = resolved_plugin_dir(spec, plugin_root);
        for binding in &spec.bindings {
            push_plugin_command(
                &mut plugin_commands,
                spec,
                TmuxCommand::BindKey {
                    options: binding.options.clone(),
                    key: binding.key.clone(),
                    plugin_dir: plugin_dir.clone(),
                    shell: binding.shell.clone(),
                    background: binding.background,
                },
            );
        }
    }

    LoadPlan { global_setup, plugin_commands }
}

fn push_plugin_command(
    commands: &mut Vec<PluginLoadCommand>,
    spec: &PluginSpec,
    command: TmuxCommand,
) {
    let plugin_id = match &spec.source {
        PluginSource::Remote { id, .. } => id.clone(),
        PluginSource::Local { path } => path.clone(),
    };
    commands.push(PluginLoadCommand { plugin_id, plugin_name: spec.name.clone(), command });
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
