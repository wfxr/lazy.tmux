use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

#[test]
fn list_prints_state_and_build_status_columns() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(data_dir.join("plugins/github.com/user/repo/.git")).unwrap();

    // Write config
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();

    // Write lockfile
    let lock_path = config_dir.join("tmup.lock");
    std::fs::write(
        &lock_path,
        r#"{"version":2,"plugins":{"github.com/user/repo":{"source":"user/repo","tracking":{"type":"branch","value":"main"},"commit":"abc1234"}}}"#,
    ).unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_DATA_HOME", dir.path().join("data").parent().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("State"))
        .stdout(predicate::str::contains("Build"));
}

#[test]
fn list_uses_human_readable_default_columns() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();

    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG", &config_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Plugin"))
        .stdout(predicate::str::contains("Kind"))
        .stdout(predicate::str::contains("State"))
        .stdout(predicate::str::contains("Build"))
        .stdout(predicate::str::contains("Lock"))
        .stdout(predicate::str::contains("user/repo"))
        .stdout(predicate::str::contains("Current").not())
        .stdout(
            predicate::str::contains("Id                                            Name").not(),
        )
        .stdout(predicate::str::contains("Source\n").not());
}

#[test]
fn list_verbose_shows_diagnostic_columns() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();

    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo" name="repo-name""#).unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .args(["list", "-v"])
        .env("TMUP_CONFIG", &config_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Id"))
        .stdout(predicate::str::contains("Name"))
        .stdout(predicate::str::contains("Current"))
        .stdout(predicate::str::contains("Expected"))
        .stdout(predicate::str::contains("Source"))
        .stdout(predicate::str::contains("github.com/user/repo"))
        .stdout(predicate::str::contains("repo-name"))
        .stdout(predicate::str::contains("user/repo"));
}

#[test]
fn list_shows_plugin_entries() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();

    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(
        &config_path,
        r#"
plugin "tmux-plugins/tmux-sensible"
plugin "catppuccin/tmux"
    "#,
    )
    .unwrap();

    // Write empty lockfile
    let lock_path = config_dir.join("tmup.lock");
    std::fs::write(&lock_path, r#"{"version":2,"plugins":{}}"#).unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG", &config_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("tmux-sensible"))
        .stdout(predicate::str::contains("tmux"));
}

#[test]
fn list_warns_before_table_when_lock_metadata_is_stale() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    let data_home = dir.path().join("data");
    let state_home = dir.path().join("state");
    std::fs::create_dir_all(&config_dir).unwrap();

    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo" build="make install""#).unwrap();

    let lock_path = config_dir.join("tmup.lock");
    let stale_lock = r#"{"version":2,"plugins":{"github.com/user/repo":{"source":"user/repo","tracking":{"type":"branch","value":"main"},"commit":"abc1234"}}}"#;
    std::fs::write(&lock_path, stale_lock).unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_STATE_HOME", &state_home)
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "warning: lock metadata is stale relative to config; run `tmup sync`",
        ))
        .stdout(predicate::str::contains("Plugin"))
        .stdout(predicate::str::contains("State"))
        .stdout(predicate::str::contains("user/repo"));
}

#[test]
fn list_mixed_stale_lock_hint_preserves_config_mode() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    std::fs::create_dir_all(&config_dir).unwrap();

    std::fs::write(&config_dir.join("tmup.kdl"), "").unwrap();
    std::fs::write(&config_dir.join("tmux.conf"), "set -g @plugin 'tmux-plugins/tmux-sensible'\n")
        .unwrap();
    std::fs::write(&config_dir.join("tmup.lock"), r#"{"version":2,"plugins":{}}"#).unwrap();

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
            "warning: lock metadata is stale relative to config; run `tmup sync`",
        ));
}

#[test]
fn list_does_not_mutate_stale_lockfile() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    std::fs::create_dir_all(&config_dir).unwrap();

    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();

    let lock_path = config_dir.join("tmup.lock");
    let original = r#"{"version":2,"plugins":{"github.com/user/repo":{"source":"user/repo","tracking":{"type":"branch","value":"main"},"commit":"abc1234"}}}"#;
    std::fs::write(&lock_path, original).unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .assert()
        .success();

    assert_eq!(std::fs::read_to_string(&lock_path).unwrap(), original);
}

#[test]
fn list_marks_missing_local_plugin_as_missing() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    let data_home = dir.path().join("data");
    let state_home = dir.path().join("state");
    std::fs::create_dir_all(&config_dir).unwrap();

    let missing_local = dir.path().join("missing-plugin");
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, format!(r#"plugin "{}" local=#true"#, missing_local.display()))
        .unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_STATE_HOME", &state_home)
        .assert()
        .success()
        .stdout(predicate::str::contains(missing_local.to_string_lossy().as_ref()))
        .stdout(predicate::str::contains("missing"));
}

#[test]
fn list_mixed_preserves_tpm_branch_suffix_in_source_display() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    std::fs::create_dir_all(&config_dir).unwrap();

    std::fs::write(&config_dir.join("tmup.kdl"), "").unwrap();
    std::fs::write(
        config_dir.join("tmux.conf"),
        "set -g @plugin 'tmux-plugins/tmux-resurrect#feature'\n",
    )
    .unwrap();

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
        .stdout(predicate::str::contains("tmux-plugins/tmux-resurrect#feature"));
}

#[test]
fn list_uses_effective_plugin_specification_and_keeps_false_predicates_quiet() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let local = dir.path().join("local-disabled");
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(
        &config_path,
        format!(
            r#"
plugin "user/enabled" enabled="test \"$TMUP_TEST_HOST\" = workstation"
plugin "user/disabled" enabled="echo predicate-stdout; echo predicate-stderr >&2; exit 23"
plugin "{}" local=#true enabled=#false
"#,
            local.display(),
        ),
    )
    .unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG", &config_path)
        .env("TMUP_TEST_HOST", "workstation")
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .assert()
        .success()
        .stdout(predicate::str::contains("user/enabled"))
        .stdout(predicate::str::contains("user/disabled").not())
        .stdout(predicate::str::contains("local-disabled").not())
        .stdout(predicate::str::contains("predicate-stdout").not())
        .stderr(predicate::str::contains("predicate-stderr").not());
}

#[test]
fn list_emits_unknown_plugin_parameter_warnings_without_failing() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(
        &config_path,
        r#"plugin "user/repo" "future" future-property=#true { future-child #true }"#,
    )
    .unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("list")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .assert()
        .success()
        .stdout(predicate::str::contains("user/repo"))
        .stderr(predicate::str::contains("warning:"))
        .stderr(predicate::str::contains("future-property"))
        .stderr(predicate::str::contains("future-child"))
        .stderr(predicate::str::contains("positional"));
}
