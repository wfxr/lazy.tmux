mod utils;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;
use utils::{make_remote_repo, push_branch_commit, write_file, write_git_rewrite_config};

#[test]
fn config_mode_cli_list_mixed_reads_tpm_config_without_scaffolding_tmup_kdl() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    write_file(&config_dir.join("tmux.conf"), "set -g @plugin 'tmux-plugins/tmux-sensible'\n");

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG_MODE", "mixed")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("HOME", dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("tmux-plugins/tmux-sensible"));

    assert!(
        !config_dir.join("tmup.kdl").exists(),
        "list should stay read-only and avoid scaffolding a missing tmup.kdl"
    );
}

#[test]
fn config_mode_cli_list_mixed_reads_tilde_sourced_tpm_file() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    let sourced = dir.path().join("plugins.conf");
    write_file(&config_dir.join("tmux.conf"), "source-file ~/plugins.conf\n");
    write_file(&sourced, "set -g @plugin 'tmux-plugins/tmux-sensible'\n");

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG_MODE", "mixed")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("HOME", dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("tmux-plugins/tmux-sensible"));
}

#[test]
fn config_mode_cli_pure_list_does_not_auto_create_missing_kdl() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    std::fs::create_dir_all(&config_dir).unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("HOME", dir.path())
        .assert()
        .success();

    assert!(!config_dir.join("tmup.kdl").exists(), "list should not create a default tmup.kdl");
}

#[test]
fn config_mode_cli_list_mixed_rejects_missing_tmup_config_override() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    write_file(&config_dir.join("tmux.conf"), "set -g @plugin 'tmux-plugins/tmux-sensible'\n");
    let override_dir = dir.path().join("override");
    let override_kdl = override_dir.join("missing.kdl");

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG_MODE", "mixed")
        .env("TMUP_CONFIG", &override_kdl)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("HOME", dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("TMUP_CONFIG"))
        .stderr(predicate::str::contains("existing file"));

    assert!(!override_kdl.exists(), "list should not create a missing TMUP_CONFIG path");
    assert!(!override_dir.join("tmup.lock").exists(), "list should not create a lockfile");
}

#[test]
fn config_mode_cli_sync_rejects_missing_tmup_config_override() {
    let dir = tempdir().unwrap();
    let override_dir = dir.path().join("override");
    let override_kdl = override_dir.join("missing.kdl");

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("sync")
        .env("TMUP_CONFIG", &override_kdl)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("HOME", dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("TMUP_CONFIG"))
        .stderr(predicate::str::contains("existing file"));

    assert!(!override_kdl.exists(), "sync should not create a missing TMUP_CONFIG path");
    assert!(!override_dir.join("tmup.lock").exists(), "sync should not create a sibling lockfile");
}

#[test]
fn config_mode_cli_mixed_works_with_absolute_xdg_without_home() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    write_file(&config_dir.join("tmux.conf"), "set -g @plugin 'tmux-plugins/tmux-sensible'\n");

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG_MODE", "mixed")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env_remove("HOME")
        .assert()
        .success()
        .stdout(predicate::str::contains("tmux-plugins/tmux-sensible"));
}

#[test]
fn config_mode_cli_mixed_without_tpm_config_still_works_without_home() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    std::fs::create_dir_all(config_home.join("tmux")).unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG_MODE", "mixed")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env_remove("HOME")
        .assert()
        .success()
        .stdout(predicate::str::contains("Plugin"));
}

#[test]
fn config_mode_cli_mixed_warns_when_home_is_unavailable_for_tpm_discovery() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("alt-config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    write_file(&config_path, "");

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG_MODE", "mixed")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "warning: HOME is unavailable; skipping default TPM config discovery",
        ));
}

#[test]
fn config_mode_cli_list_mixed_warns_and_prefers_kdl() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    write_file(
        &config_dir.join("tmup.kdl"),
        r#"plug "tmux-plugins/tmux-sensible" branch="feature""#,
    );
    write_file(
        &config_dir.join("tmux.conf"),
        concat!(
            "set -g @plugin 'tmux-plugins/tmux-sensible'\n",
            "set -g @plugin 'tmux-plugins/tmux-yank'\n",
        ),
    );
    write_file(&config_dir.join("tmup.lock"), r#"{"version":2,"plugins":{}}"#);

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG_MODE", "mixed")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("HOME", dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "warning: plugin \"github.com/tmux-plugins/tmux-sensible\" declared in both tmup.kdl and TPM config; using tmup.kdl entry",
        ))
        .stdout(predicate::str::contains("tmux-plugins/tmux-sensible"))
        .stdout(predicate::str::contains("tmux-plugins/tmux-yank"));
}

#[test]
fn config_mode_cli_sync_mixed_writes_lockfile_next_to_kdl_with_kdl_precedence() {
    let dir = tempdir().unwrap();
    let bare = make_remote_repo(dir.path());
    push_branch_commit(&bare, "feature", "feature");
    let gitconfig = write_git_rewrite_config(dir.path());
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    write_file(
        &config_dir.join("tmup.kdl"),
        r#"plug "https://example.com/test/plugin.git" branch="feature""#,
    );
    write_file(
        &config_dir.join("tmux.conf"),
        "set -g @plugin 'https://example.com/test/plugin.git'\n",
    );

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("sync")
        .env("TMUP_CONFIG_MODE", "mixed")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .env("HOME", dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "warning: plugin \"example.com/test/plugin\" declared in both tmup.kdl and TPM config; using tmup.kdl entry",
        ));

    let lock = std::fs::read_to_string(config_dir.join("tmup.lock")).unwrap();
    assert!(lock.contains(r#""type": "branch""#), "{lock}");
    assert!(lock.contains(r#""value": "feature""#), "{lock}");
}

#[test]
fn config_mode_cli_sync_mixed_scaffolds_tmup_kdl_when_only_tpm_config_exists() {
    let dir = tempdir().unwrap();
    let bare = make_remote_repo(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    write_file(
        &config_dir.join("tmux.conf"),
        "set -g @plugin 'https://example.com/test/plugin.git'\n",
    );

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("sync")
        .env("TMUP_CONFIG_MODE", "mixed")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .env("HOME", dir.path())
        .assert()
        .success();

    assert!(bare.exists(), "remote repo should remain available for the sync");
    assert!(
        config_dir.join("tmup.kdl").exists(),
        "mixed sync should scaffold tmup.kdl for the migration write path"
    );

    let lock = std::fs::read_to_string(config_dir.join("tmup.lock")).unwrap();
    assert!(lock.contains(r#""example.com/test/plugin""#), "{lock}");
}

#[test]
fn config_mode_cli_rejects_invalid_env_value() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    std::fs::create_dir_all(config_home.join("tmux")).unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG_MODE", "bogus")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("HOME", dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("TMUP_CONFIG_MODE"))
        .stderr(predicate::str::contains("pure"))
        .stderr(predicate::str::contains("mixed"));
}
