use tempfile::tempdir;
use tmup::config_mode::{
    ConfigMode, ResolutionIntent, ResolvedConfig, load_from_sources_with_intent,
};
use tmup::loader::build_load_plan;
use tmup::tmux::TmuxCommand;

fn resolve_config(root: &std::path::Path, input: &str) -> ResolvedConfig {
    let path = root.join("tmup.kdl");
    std::fs::write(&path, input).unwrap();
    load_from_sources_with_intent(
        ConfigMode::Pure,
        Some(&path),
        None,
        ResolutionIntent::LoadEligibility,
    )
    .unwrap()
    .config
}

#[test]
fn loader_sets_env_then_opts_then_runs_tmux_files_in_order() {
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

    let plan = build_load_plan(config.load_eligibility().unwrap(), &plugin_root);

    // 1. First command should be SetEnvironment
    assert!(
        matches!(&plan[0], TmuxCommand::SetEnvironment { key, .. } if key == "TMUX_PLUGIN_MANAGER_PATH")
    );

    // 2. Second should be the opt
    assert_eq!(plan[1], TmuxCommand::SetOption { key: "pa_theme".into(), value: "dark".into() });

    // 3. *.tmux files in sorted order
    match &plan[2] {
        TmuxCommand::RunShell { script } => {
            assert!(script.file_name().unwrap().to_str().unwrap().starts_with("00-"));
        }
        other => panic!("expected RunShell, got {other:?}"),
    }
    match &plan[3] {
        TmuxCommand::RunShell { script } => {
            assert!(script.file_name().unwrap().to_str().unwrap().starts_with("10-"));
        }
        other => panic!("expected RunShell, got {other:?}"),
    }

    assert_eq!(plan.len(), 4);
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

    let plan = build_load_plan(config.load_eligibility().unwrap(), &plugin_root);

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
fn loader_handles_missing_plugin_dir() {
    let dir = tempdir().unwrap();
    let plugin_root = dir.path().join("plugins");
    // Don't create any plugin directories

    let config = resolve_config(dir.path(), r#"plugin "user/missing""#);
    let plan = build_load_plan(config.load_eligibility().unwrap(), &plugin_root);

    // Should have env setup but no RunShell (plugin dir doesn't exist)
    assert_eq!(plan.len(), 1); // just SetEnvironment
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

    let plan = build_load_plan(config.load_eligibility().unwrap(), &plugin_root);

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
plugin "user/skip" cond=#false opt-prefix="skip_" { opt "mode" "no" }
plugin "user/load-last" opt-prefix="last_" { opt "mode" "yes" }
"#,
    );

    let plan = build_load_plan(config.load_eligibility().unwrap(), &plugin_root);

    assert!(matches!(plan.first(), Some(TmuxCommand::SetEnvironment { .. })));
    assert!(!plan.iter().any(|command| match command {
        TmuxCommand::SetOption { key, .. } => key.starts_with("skip_"),
        TmuxCommand::RunShell { script } => script.ends_with("skip.tmux"),
        TmuxCommand::SetEnvironment { .. } => false,
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
