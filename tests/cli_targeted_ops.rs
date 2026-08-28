mod utils;
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;
use utils::*;

fn write_config(root: &std::path::Path) -> std::path::PathBuf {
    let config_dir = root.join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(
        &config_path,
        concat!(
            "plugin \"https://example.com/test/plugin.git\"\n",
            "plugin \"https://example.com/bad/plugin.git\"\n",
        ),
    )
    .unwrap();
    config_path
}

fn cargo_cmd(
    root: &std::path::Path,
    config_path: &std::path::Path,
    gitconfig: &std::path::Path,
) -> Command {
    let mut cmd = Command::cargo_bin("tmup").unwrap();
    cmd.env("TMUP_CONFIG", config_path)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", gitconfig)
        .env("HOME", root);
    cmd
}

#[test]
fn install_target_ignores_unrelated_sync_failures() {
    let dir = tempdir().unwrap();
    make_remote_repo(dir.path());
    let config_path = write_config(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());

    cargo_cmd(dir.path(), &config_path, &gitconfig)
        .args(["install", "example.com/test/plugin"])
        .assert()
        .success();

    assert!(dir.path().join("data/tmup/plugins/example.com/test/plugin/init.tmux").exists());

    let lock = std::fs::read_to_string(dir.path().join("config/tmux/tmup.lock")).unwrap();
    assert!(lock.contains("example.com/test/plugin"));
    assert!(!lock.contains("example.com/bad/plugin"));
}

#[test]
fn update_target_ignores_unrelated_sync_failures() {
    let dir = tempdir().unwrap();
    make_remote_repo(dir.path());
    let config_path = write_config(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());

    cargo_cmd(dir.path(), &config_path, &gitconfig)
        .args(["sync", "example.com/test/plugin"])
        .assert()
        .success();

    cargo_cmd(dir.path(), &config_path, &gitconfig)
        .args(["update", "example.com/test/plugin"])
        .assert()
        .success();

    let lock = std::fs::read_to_string(dir.path().join("config/tmux/tmup.lock")).unwrap();
    assert!(lock.contains("example.com/test/plugin"));
    assert!(!lock.contains("example.com/bad/plugin"));
}

#[test]
fn restore_target_ignores_unrelated_sync_failures() {
    let dir = tempdir().unwrap();
    make_remote_repo(dir.path());
    let config_path = write_config(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());

    cargo_cmd(dir.path(), &config_path, &gitconfig)
        .args(["sync", "example.com/test/plugin"])
        .assert()
        .success();

    cargo_cmd(dir.path(), &config_path, &gitconfig)
        .args(["restore", "example.com/test/plugin"])
        .assert()
        .success();

    let lock = std::fs::read_to_string(dir.path().join("config/tmux/tmup.lock")).unwrap();
    assert!(lock.contains("example.com/test/plugin"));
    assert!(!lock.contains("example.com/bad/plugin"));
}

#[test]
fn targeted_lifecycle_commands_reject_disabled_plugins_as_unknown() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config/tmux/tmup.kdl");
    write_file(&config_path, r#"plugin "https://example.com/test/plugin.git" enabled=#false"#);
    let gitconfig = write_git_rewrite_config(dir.path());

    for command in ["sync", "install", "update", "restore"] {
        cargo_cmd(dir.path(), &config_path, &gitconfig)
            .args([command, "example.com/test/plugin"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("unknown plugin id"));
    }
}

#[test]
fn standalone_lifecycle_commands_do_not_evaluate_load_conditions() {
    let dir = tempdir().unwrap();
    make_remote_repo(dir.path());
    let config_path = dir.path().join("config/tmux/tmup.kdl");
    write_file(
        &config_path,
        r#"
plugin "https://example.com/test/plugin.git" cond="kill -TERM $$" {
    if "kill -TERM $$" {
        bind "unreachable" { shell "./unreachable" }
    }
}
"#,
    );
    let gitconfig = write_git_rewrite_config(dir.path());

    for command in ["sync", "install", "update", "restore", "clean"] {
        cargo_cmd(dir.path(), &config_path, &gitconfig)
            .arg(command)
            .assert()
            .success()
            .stderr(predicate::str::contains("if shell predicate").not())
            .stderr(predicate::str::contains("unknown child").not());
    }
}
