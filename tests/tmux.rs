use std::path::PathBuf;

use tmup::tmux::TmuxCommand;

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
