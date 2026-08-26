use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn cli_version_reports_cargo_package_version() {
    Command::cargo_bin("tmup")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(format!("tmup {}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn cli_help_lists_core_commands() {
    Command::cargo_bin("tmup")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--tpm").not())
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("install"))
        .stdout(predicate::str::contains("sync"))
        .stdout(predicate::str::contains("update"))
        .stdout(predicate::str::contains("restore"))
        .stdout(predicate::str::contains("clean"))
        .stdout(predicate::str::contains("list"));
}

#[test]
fn init_help_omits_removed_tpm_flag() {
    Command::cargo_bin("tmup")
        .unwrap()
        .args(["init", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--tpm").not());
}

#[test]
fn init_rejects_retired_internal_transport_flags() {
    for flag in [
        "--bootstrap",
        "--ui-child",
        "--wait-channel",
        "--config-path",
        "--tpm-config-path",
        "--no-tpm-config",
        "--data-root",
        "--state-root",
    ] {
        Command::cargo_bin("tmup")
            .unwrap()
            .args(["init", flag])
            .assert()
            .failure()
            .stderr(predicate::str::contains("unexpected argument"));
    }
}
