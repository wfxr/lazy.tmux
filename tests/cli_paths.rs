mod utils;
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;
use utils::*;

fn cargo_cmd(
    root: &std::path::Path,
    config_path: &std::path::Path,
    gitconfig: &std::path::Path,
) -> Command {
    let mut cmd = Command::cargo_bin("tmup").unwrap();
    cmd.env("TMUP_CONFIG", config_path)
        .env("XDG_CONFIG_HOME", root.join("xdg-config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", gitconfig)
        .env("HOME", root);
    cmd
}

#[test]
fn cli_paths_list_reads_lockfile_next_to_override_config() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("alt-config");
    let xdg_config_dir = dir.path().join("xdg-config/tmux");
    let gitconfig = write_git_rewrite_config(dir.path());
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&xdg_config_dir).unwrap();

    let config_path = config_dir.join("custom.kdl");
    std::fs::write(&config_path, r#"plug "user/repo""#).unwrap();
    std::fs::write(config_dir.join("tmup.lock"), r#"{"version":2,"plugins":{}}"#).unwrap();
    std::fs::write(xdg_config_dir.join("tmup.lock"), "not-json").unwrap();

    cargo_cmd(dir.path(), &config_path, &gitconfig)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("user/repo"));
}

#[test]
fn cli_paths_sync_writes_lockfile_next_to_override_config() {
    let dir = tempdir().unwrap();
    make_remote_repo(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());
    let config_dir = dir.path().join("alt-config");
    std::fs::create_dir_all(&config_dir).unwrap();

    let config_path = config_dir.join("custom.kdl");
    std::fs::write(&config_path, r#"plug "https://example.com/test/plugin.git""#).unwrap();

    cargo_cmd(dir.path(), &config_path, &gitconfig).arg("sync").assert().success();

    let sibling_lock = config_dir.join("tmup.lock");
    let default_lock = dir.path().join("xdg-config/tmux/tmup.lock");
    assert!(sibling_lock.exists(), "expected sibling lockfile to be written");
    assert!(!default_lock.exists(), "expected default XDG lockfile to remain untouched");

    let content = std::fs::read_to_string(sibling_lock).unwrap();
    assert!(content.contains("example.com/test/plugin"));
}
