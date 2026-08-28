use tempfile::tempdir;
use tmup::config_mode::{
    ConfigMode, ResolutionIntent, ResolvedConfig, load_from_sources_with_intent,
};
use tmup::loader::{PluginLoadCommand, build_load_plan};
use tmup::model::RuntimeConfiguration;
use tmup::tmux::TmuxCommand;

fn resolve_config(root: &std::path::Path, input: &str) -> ResolvedConfig {
    let path = root.join("tmup.kdl");
    std::fs::write(&path, input).unwrap();
    load_from_sources_with_intent(
        ConfigMode::Pure,
        Some(&path),
        None,
        ResolutionIntent::RuntimeConfiguration,
    )
    .unwrap()
    .config
}

#[test]
fn loader_applies_setup_then_runs_tmux_files_in_order() {
    let dir = tempdir().unwrap();
    let plugin_root = dir.path().join("plugins");

    // Create a fake plugin with two .tmux files
    let plugin_dir = plugin_root.join("github.com/user/plugin-a");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("10-second.tmux"), "#!/bin/sh").unwrap();
    std::fs::write(plugin_dir.join("00-first.tmux"), "#!/bin/sh").unwrap();

    let config = resolve_config(
        dir.path(),
        r##"
plugin "user/plugin-a" opt-prefix="pa_" {
    opt "theme" "dark"
}
    "##,
    );

    let plan = build_load_plan(config.runtime_configuration().unwrap(), &plugin_root);
    let commands: Vec<_> = plan.iter().collect();

    // 1. First command should be SetEnvironment
    assert!(
        matches!(commands[0], TmuxCommand::SetEnvironment { key, .. } if key == "TMUX_PLUGIN_MANAGER_PATH")
    );

    // 2. Second should be the opt
    assert_eq!(
        commands[1],
        &TmuxCommand::SetOption { key: "pa_theme".into(), value: "dark".into() }
    );

    // 3. *.tmux files in sorted order
    match commands[2] {
        TmuxCommand::RunShell { script } => {
            assert!(script.file_name().unwrap().to_str().unwrap().starts_with("00-"));
        }
        other => panic!("expected RunShell, got {other:?}"),
    }
    match commands[3] {
        TmuxCommand::RunShell { script } => {
            assert!(script.file_name().unwrap().to_str().unwrap().starts_with("10-"));
        }
        other => panic!("expected RunShell, got {other:?}"),
    }

    assert_eq!(commands.len(), 4);
}

#[test]
fn loader_preserves_plugin_declaration_order() {
    let dir = tempdir().unwrap();
    let plugin_root = dir.path().join("plugins");

    let plugin_a = plugin_root.join("github.com/user/plugin-a");
    let plugin_b = plugin_root.join("github.com/user/plugin-b");
    std::fs::create_dir_all(&plugin_a).unwrap();
    std::fs::create_dir_all(&plugin_b).unwrap();
    std::fs::write(plugin_a.join("a.tmux"), "#!/bin/sh").unwrap();
    std::fs::write(plugin_b.join("b.tmux"), "#!/bin/sh").unwrap();

    let config = resolve_config(
        dir.path(),
        r#"
plugin "user/plugin-a"
plugin "user/plugin-b"
    "#,
    );

    let plan = build_load_plan(config.runtime_configuration().unwrap(), &plugin_root);

    // After env setup, plugin-a runs before plugin-b
    let run_shells: Vec<_> = plan
        .iter()
        .filter_map(|cmd| {
            if let TmuxCommand::RunShell { script } = cmd {
                Some(script.file_name().unwrap().to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect();

    assert_eq!(run_shells, vec!["a.tmux", "b.tmux"]);
}

#[test]
fn loader_attributes_each_plugin_command_to_its_plugin() {
    let dir = tempdir().unwrap();
    let plugin_root = dir.path().join("plugins");
    for name in ["plugin-a", "plugin-b"] {
        let plugin_dir = plugin_root.join(format!("github.com/user/{name}"));
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join(format!("{name}.tmux")), "#!/bin/sh").unwrap();
    }
    let config = resolve_config(
        dir.path(),
        r#"
plugin "user/plugin-a" { env "PLUGIN_A" "yes" }
plugin "user/plugin-b" { env "PLUGIN_B" "yes" }
"#,
    );

    let plan = build_load_plan(config.runtime_configuration().unwrap(), &plugin_root);

    assert!(matches!(
        plan.plugin_commands.first(),
        Some(PluginLoadCommand {
            plugin_id,
            plugin_name,
            command: TmuxCommand::SetEnvironment { key, .. },
        }) if plugin_id == "github.com/user/plugin-a"
            && plugin_name == "plugin-a"
            && key == "PLUGIN_A"
    ));
    assert!(
        plan.plugin_commands
            .iter()
            .filter(|entry| entry.plugin_id == "github.com/user/plugin-a")
            .all(|entry| entry.plugin_name == "plugin-a")
    );
    assert!(
        plan.plugin_commands
            .iter()
            .filter(|entry| entry.plugin_id == "github.com/user/plugin-b")
            .all(|entry| entry.plugin_name == "plugin-b")
    );
}

#[test]
fn loader_applies_plugin_setup_before_loading_any_scripts() {
    let dir = tempdir().unwrap();
    let plugin_root = dir.path().join("plugins");
    for name in ["plugin-a", "plugin-b"] {
        let plugin_dir = plugin_root.join(format!("github.com/user/{name}"));
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join(format!("{name}.tmux")), "#!/bin/sh").unwrap();
    }
    let config = resolve_config(
        dir.path(),
        r##"
plugin "user/plugin-a" opt-prefix="a_" {
    env "SHARED" "first"
    unset-env "LEGACY"
    opt "mode" "one"
    bind "x" {
        options "-n" "-r"
        shell "./first"
    }
    bind "x" {
        shell "./override"
    }
}
plugin "user/plugin-b" opt-prefix="b_" {
    env "SHARED" "second"
    opt "mode" "two"
    bind "M-b" {
        shell "./background" background=#true
    }
}
"##,
    );

    let plan = build_load_plan(config.runtime_configuration().unwrap(), &plugin_root);
    let commands: Vec<_> = plan.iter().cloned().collect();

    assert_eq!(
        commands,
        vec![
            TmuxCommand::SetEnvironment {
                key: "TMUX_PLUGIN_MANAGER_PATH".into(),
                value: format!("{}/", plugin_root.display()),
            },
            TmuxCommand::SetEnvironment { key: "SHARED".into(), value: "first".into() },
            TmuxCommand::UnsetEnvironment { key: "LEGACY".into() },
            TmuxCommand::SetOption { key: "a_mode".into(), value: "one".into() },
            TmuxCommand::SetEnvironment { key: "SHARED".into(), value: "second".into() },
            TmuxCommand::SetOption { key: "b_mode".into(), value: "two".into() },
            TmuxCommand::RunShell {
                script: plugin_root.join("github.com/user/plugin-a/plugin-a.tmux")
            },
            TmuxCommand::RunShell {
                script: plugin_root.join("github.com/user/plugin-b/plugin-b.tmux")
            },
            TmuxCommand::BindKey {
                options: vec!["-n".into(), "-r".into()],
                key: "x".into(),
                plugin_dir: plugin_root.join("github.com/user/plugin-a"),
                shell: "./first".into(),
                background: false,
            },
            TmuxCommand::BindKey {
                options: vec![],
                key: "x".into(),
                plugin_dir: plugin_root.join("github.com/user/plugin-a"),
                shell: "./override".into(),
                background: false,
            },
            TmuxCommand::BindKey {
                options: vec![],
                key: "M-b".into(),
                plugin_dir: plugin_root.join("github.com/user/plugin-b"),
                shell: "./background".into(),
                background: true,
            },
        ]
    );
}

#[test]
fn loader_phases_selected_runtime_configuration_in_source_order() {
    let dir = tempdir().unwrap();
    let plugin_root = dir.path().join("plugins");
    let plugin_dir = plugin_root.join("github.com/user/plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.tmux"), "#!/bin/sh").unwrap();
    let kdl = dir.path().join("tmup.kdl");
    std::fs::write(
        &kdl,
        r#"
plugin "user/plugin" opt-prefix="p_" {
    env "OUTSIDE_BEFORE" "one"
    opt "outside_before" "one"
    bind "outside-before" { shell "./outside-before" }
    if #true {
        env "BRANCH_SET" "two"
        unset-env "BRANCH_UNSET"
        opt "branch_first" "two"
        opt "branch_second" "three"
        bind "branch-first" { shell "./branch-first" }
        bind "branch-second" { shell "./branch-second" }
    }
    env "OUTSIDE_AFTER" "three"
    unset-env "OUTSIDE_UNSET"
    opt "outside_after" "four"
    bind "outside-after" { shell "./outside-after" }
}
"#,
    )
    .unwrap();
    let config = load_from_sources_with_intent(
        ConfigMode::Pure,
        Some(&kdl),
        None,
        ResolutionIntent::RuntimeConfiguration,
    )
    .unwrap()
    .config;

    let plan = build_load_plan(config.runtime_configuration().unwrap(), &plugin_root);
    let commands: Vec<_> = plan.iter().cloned().collect();

    assert_eq!(
        commands,
        vec![
            TmuxCommand::SetEnvironment {
                key: "TMUX_PLUGIN_MANAGER_PATH".into(),
                value: format!("{}/", plugin_root.display()),
            },
            TmuxCommand::SetEnvironment { key: "OUTSIDE_BEFORE".into(), value: "one".into() },
            TmuxCommand::SetOption { key: "p_outside_before".into(), value: "one".into() },
            TmuxCommand::SetEnvironment { key: "BRANCH_SET".into(), value: "two".into() },
            TmuxCommand::UnsetEnvironment { key: "BRANCH_UNSET".into() },
            TmuxCommand::SetOption { key: "p_branch_first".into(), value: "two".into() },
            TmuxCommand::SetOption { key: "p_branch_second".into(), value: "three".into() },
            TmuxCommand::SetEnvironment { key: "OUTSIDE_AFTER".into(), value: "three".into() },
            TmuxCommand::UnsetEnvironment { key: "OUTSIDE_UNSET".into() },
            TmuxCommand::SetOption { key: "p_outside_after".into(), value: "four".into() },
            TmuxCommand::RunShell { script: plugin_dir.join("plugin.tmux") },
            TmuxCommand::BindKey {
                options: vec![],
                key: "outside-before".into(),
                plugin_dir: plugin_dir.clone(),
                shell: "./outside-before".into(),
                background: false,
            },
            TmuxCommand::BindKey {
                options: vec![],
                key: "branch-first".into(),
                plugin_dir: plugin_dir.clone(),
                shell: "./branch-first".into(),
                background: false,
            },
            TmuxCommand::BindKey {
                options: vec![],
                key: "branch-second".into(),
                plugin_dir: plugin_dir.clone(),
                shell: "./branch-second".into(),
                background: false,
            },
            TmuxCommand::BindKey {
                options: vec![],
                key: "outside-after".into(),
                plugin_dir,
                shell: "./outside-after".into(),
                background: false,
            },
        ]
    );
}

#[test]
fn loader_handles_missing_plugin_dir() {
    let dir = tempdir().unwrap();
    let plugin_root = dir.path().join("plugins");
    // Don't create any plugin directories

    let config = resolve_config(dir.path(), r#"plugin "user/missing""#);
    let plan = build_load_plan(config.runtime_configuration().unwrap(), &plugin_root);

    assert!(plan.plugin_commands.is_empty());
    assert!(matches!(plan.global_setup, TmuxCommand::SetEnvironment { .. }));
}

#[test]
fn loader_applies_opt_prefix() {
    let dir = tempdir().unwrap();
    let plugin_root = dir.path().join("plugins");
    std::fs::create_dir_all(plugin_root.join("github.com/catppuccin/tmux")).unwrap();

    let config = resolve_config(
        dir.path(),
        r##"
plugin "catppuccin/tmux" opt-prefix="catppuccin_" {
    opt "flavor" "mocha"
    opt "window_text" "#W"
}
    "##,
    );

    let plan = build_load_plan(config.runtime_configuration().unwrap(), &plugin_root);

    let opts: Vec<_> = plan
        .iter()
        .filter_map(|cmd| {
            if let TmuxCommand::SetOption { key, value } = cmd {
                Some((key.clone(), value.clone()))
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        opts,
        vec![
            ("catppuccin_flavor".into(), "mocha".into()),
            ("catppuccin_window_text".into(), "#W".into()),
        ]
    );
}

#[test]
fn loader_skips_options_and_scripts_for_plugins_without_load_eligibility() {
    let dir = tempdir().unwrap();
    let plugin_root = dir.path().join("plugins");
    for name in ["load-first", "skip", "load-last"] {
        let plugin_dir = plugin_root.join(format!("github.com/user/{name}"));
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join(format!("{name}.tmux")), "#!/bin/sh").unwrap();
    }
    let config = resolve_config(
        dir.path(),
        r#"
plugin "user/load-first" opt-prefix="first_" { opt "mode" "yes" }
plugin "user/skip" cond=#false opt-prefix="skip_" {
    env "SKIP_ENV" "no"
    unset-env "SKIP_UNSET"
    opt "mode" "no"
    bind "skip" { shell "./skip" }
}
plugin "user/load-last" opt-prefix="last_" { opt "mode" "yes" }
"#,
    );

    let load_eligibility: Vec<_> = config.load_eligibility().unwrap().values().collect();
    assert_eq!(load_eligibility, [true, false, true]);

    let plan = build_load_plan(config.runtime_configuration().unwrap(), &plugin_root);

    assert!(matches!(config.plugins[0].runtime, RuntimeConfiguration::Selected(_)));
    assert!(matches!(config.plugins[1].runtime, RuntimeConfiguration::Unresolved));
    assert!(matches!(config.plugins[2].runtime, RuntimeConfiguration::Selected(_)));

    assert!(matches!(plan.global_setup, TmuxCommand::SetEnvironment { .. }));
    assert!(!plan.iter().any(|command| match command {
        TmuxCommand::SetOption { key, .. } => key.starts_with("skip_"),
        TmuxCommand::RunShell { script } => script.ends_with("skip.tmux"),
        TmuxCommand::SetEnvironment { key, .. } => key == "SKIP_ENV",
        TmuxCommand::UnsetEnvironment { key } => key == "SKIP_UNSET",
        TmuxCommand::BindKey { key, .. } => key == "skip",
    }));
    let scripts: Vec<_> = plan
        .iter()
        .filter_map(|command| match command {
            TmuxCommand::RunShell { script } => script.file_name().map(|name| name.to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(
        scripts,
        [std::ffi::OsString::from("load-first.tmux"), std::ffi::OsString::from("load-last.tmux")]
    );
}
