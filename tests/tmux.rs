use std::path::PathBuf;

use tmup::tmux::TmuxCommand;

#[test]
fn environment_commands_use_direct_global_tmux_arguments() {
    assert_eq!(
        TmuxCommand::SetEnvironment { key: "EMPTY".into(), value: "".into() }.to_args(),
        vec!["set-environment", "-g", "EMPTY", ""]
    );
    assert_eq!(
        TmuxCommand::UnsetEnvironment { key: "STALE".into() }.to_args(),
        vec!["set-environment", "-gu", "STALE"]
    );
}

#[test]
fn bind_key_preserves_argv_tokens_and_quotes_the_guarded_shell_action() {
    let command = TmuxCommand::BindKey {
        options: vec!["-n".into(), "-r".into()],
        key: "C-w".into(),
        plugin_dir: PathBuf::from("/tmp/plugin's dir"),
        shell: r#"printf '%s\n' "$MODE" | tee out > result"#.into(),
        background: false,
    };

    assert_eq!(
        command.to_args(),
        vec![
            "bind-key",
            "-n",
            "-r",
            "C-w",
            "run-shell",
            r#"cd '/tmp/plugin'"'"'s dir' && exec /bin/sh -c 'printf '"'"'%s\n'"'"' "$MODE" | tee out > result'"#,
        ]
    );
}

#[test]
fn bind_key_places_background_flag_on_the_nested_run_shell_action() {
    let command = TmuxCommand::BindKey {
        options: vec![],
        key: "x".into(),
        plugin_dir: PathBuf::from("/tmp/plugin"),
        shell: "./launch".into(),
        background: true,
    };

    assert_eq!(
        command.to_args(),
        vec!["bind-key", "x", "run-shell", "-b", "cd '/tmp/plugin' && exec /bin/sh -c './launch'",]
    );
}

#[test]
fn run_shell_quotes_paths_with_spaces() {
    let cmd = TmuxCommand::RunShell { script: PathBuf::from("/tmp/with space/plugin.tmux") };

    assert_eq!(
        cmd.to_args(),
        vec!["run-shell".to_string(), "'/tmp/with space/plugin.tmux'".to_string(),]
    );
}

#[test]
fn run_shell_escapes_single_quotes() {
    let cmd = TmuxCommand::RunShell { script: PathBuf::from("/tmp/it's/plugin.tmux") };

    assert_eq!(
        cmd.to_args(),
        vec!["run-shell".to_string(), "'/tmp/it'\"'\"'s/plugin.tmux'".to_string(),]
    );
}
