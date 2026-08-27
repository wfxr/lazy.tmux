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
