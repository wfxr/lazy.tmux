mod utils;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;
use utils::{make_remote_repo, write_file, write_git_rewrite_config};

fn configured_command(
    root: &std::path::Path,
    config_path: &std::path::Path,
    gitconfig: &std::path::Path,
) -> Command {
    let mut command = Command::cargo_bin("tmup").unwrap();
    command
        .env("TMUP_CONFIG", config_path)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", gitconfig)
        .env("HOME", root);
    command
}

#[test]
fn sync_errors_on_unknown_plugin_id() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .args(["sync", "github.com/user/other"])
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown plugin id"));
}

#[test]
fn sync_errors_on_local_plugin_target() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "/tmp/local-plugin" local=#true name="local-plugin""#)
        .unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .args(["sync", "/tmp/local-plugin"])
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown plugin id"));
}

#[test]
fn disabling_plugin_prunes_lock_but_checkout_waits_for_clean() {
    let dir = tempdir().unwrap();
    make_remote_repo(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());
    let config_path = dir.path().join("config/tmux/tmup.kdl");
    write_file(&config_path, r#"plugin "https://example.com/test/plugin.git""#);

    configured_command(dir.path(), &config_path, &gitconfig).arg("install").assert().success();

    let plugin_dir = dir.path().join("data/tmup/plugins/example.com/test/plugin");
    let lock_path = dir.path().join("config/tmux/tmup.lock");
    assert!(plugin_dir.exists());
    assert!(std::fs::read_to_string(&lock_path).unwrap().contains("example.com/test/plugin"));

    write_file(&config_path, r#"plugin "https://example.com/test/plugin.git" enabled=#false"#);
    configured_command(dir.path(), &config_path, &gitconfig).arg("sync").assert().success();

    assert!(plugin_dir.exists(), "sync must not delete a newly disabled checkout");
    assert!(!std::fs::read_to_string(&lock_path).unwrap().contains("example.com/test/plugin"));

    configured_command(dir.path(), &config_path, &gitconfig).arg("clean").assert().success();

    assert!(!plugin_dir.exists(), "clean should remove the undeclared managed checkout");
}

#[test]
fn hard_enable_condition_errors_precede_lock_and_plugin_mutation() {
    let dir = tempdir().unwrap();
    let gitconfig = write_git_rewrite_config(dir.path());
    let config_path = dir.path().join("config/tmux/tmup.kdl");
    let lock_path = dir.path().join("config/tmux/tmup.lock");
    let original_lock = r#"{"version":2,"plugins":{}}"#;
    write_file(&lock_path, original_lock);
    write_file(&config_path, r#"plugin "user/repo" enabled="kill -TERM $$""#);

    configured_command(dir.path(), &config_path, &gitconfig)
        .arg("sync")
        .assert()
        .failure()
        .stderr(predicate::str::contains("terminated by a signal"));

    assert_eq!(std::fs::read_to_string(&lock_path).unwrap(), original_lock);
    assert!(!dir.path().join("data/tmup/plugins").exists());
}

#[test]
fn enable_condition_timeout_is_a_hard_error_before_mutation() {
    let dir = tempdir().unwrap();
    let gitconfig = write_git_rewrite_config(dir.path());
    let config_path = dir.path().join("config/tmux/tmup.kdl");
    let lock_path = dir.path().join("config/tmux/tmup.lock");
    let original_lock = r#"{"version":2,"plugins":{}}"#;
    write_file(&lock_path, original_lock);
    write_file(&config_path, r#"plugin "user/repo" enabled="sleep 30""#);

    configured_command(dir.path(), &config_path, &gitconfig)
        .arg("sync")
        .assert()
        .failure()
        .stderr(predicate::str::contains("timed out after 5 seconds"));

    assert_eq!(std::fs::read_to_string(&lock_path).unwrap(), original_lock);
    assert!(!dir.path().join("data/tmup/plugins").exists());
}
