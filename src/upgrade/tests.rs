use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::process::Command;

use super::*;

#[test]
fn version_policy_covers_force_explicit_downgrades_and_metadata() {
    let current = Version::parse("1.2.3+custom").unwrap();
    for (selected, explicit, force, skip) in [
        ("1.2.4", false, false, false),
        ("1.2.3", false, false, true),
        ("1.2.3", false, true, false),
        ("1.2.2", false, true, true),
        ("1.2.2", true, false, false),
        ("1.2.3-rc.1", false, true, true),
        ("1.3.0-rc.1", false, false, false),
    ] {
        let options = Options { version: explicit.then(|| selected.into()), force, pre: false };
        assert_eq!(
            skip_reason(&current, &Version::parse(selected).unwrap(), &options).is_some(),
            skip,
            "{selected} explicit={explicit} force={force}"
        );
    }
    for invalid in ["../1.0.0", "https://a", "1.0", "1.0.0+x", "01.0.0", "1.0.0-01"] {
        assert!(release_version(invalid).is_err(), "{invalid}");
    }
    assert_eq!(release_version("v1.2.3").unwrap().to_string(), "1.2.3");
    assert!(validate_options(&Options::default(), false).is_err());
    assert!(validate_options(&Options::default(), true).is_ok());
    assert!(target("windows", "x86_64").is_err());
    assert!(target("linux", "x86_64").unwrap().ends_with("musl"));
    assert!(target("macos", "aarch64").unwrap().ends_with("darwin"));
}

#[test]
fn destination_changes_and_symlink_candidates_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("renamed");
    fs::write(&path, "original").unwrap();
    let destination = install::Destination::capture(path.clone()).unwrap();
    let alias = dir.path().join("alias");
    symlink(&path, &alias).unwrap();
    assert!(destination.copy_candidate(&alias).is_err());
    let replacement = dir.path().join("replacement");
    fs::write(&replacement, "changed").unwrap();
    fs::rename(&replacement, &path).unwrap();
    assert!(destination.check_unchanged().is_err());
    fs::remove_file(&path).unwrap();
    symlink(&alias, &path).unwrap();
    assert!(destination.check_unchanged().is_err());
}

#[test]
fn candidate_smoke_timeout_and_failure_preserve_original_and_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tmup");
    fs::write(&path, "original").unwrap();
    let destination = install::Destination::capture(path.clone()).unwrap();
    let prepared = dir.path().join("prepared");
    fs::write(&prepared, "#!/bin/sh\nsleep 10\n").unwrap();
    fs::set_permissions(&prepared, fs::Permissions::from_mode(0o755)).unwrap();
    let candidate = destination.copy_candidate(&prepared).unwrap();
    let error = destination
        .verify_candidate(&candidate, &Version::new(1, 2, 3), dir.path(), Duration::from_millis(50))
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    let candidate_path = candidate.to_path_buf();
    candidate.close().unwrap();
    assert!(!candidate_path.exists());
    assert_eq!(fs::read_to_string(path).unwrap(), "original");
}

#[test]
fn process_timeout_stops_descendants_before_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let late = dir.path().join("late");
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg("(sleep 0.2; printf leaked > \"$1\") & wait").arg("sh").arg(&late);
    let error = process::run(&mut command, dir.path(), Duration::from_millis(50), "test query")
        .err()
        .unwrap();
    assert!(error.to_string().contains("timed out"));
    std::thread::sleep(Duration::from_millis(250));
    assert!(!late.exists());
}

#[test]
fn exited_helper_cannot_leave_background_writers() {
    let dir = tempfile::tempdir().unwrap();
    let late = dir.path().join("late");
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg("(sleep 0.2; printf leaked > \"$1\") & exit 0").arg("sh").arg(&late);
    process::run(&mut command, dir.path(), Duration::from_secs(1), "test").unwrap();
    std::thread::sleep(Duration::from_millis(250));
    assert!(!late.exists());
}

#[test]
fn cleanup_error_preserves_original_error_and_reports_commit_state() {
    let err = finish_cleanup(
        Err(anyhow::anyhow!("invalid candidate")),
        vec!["/tmp/leftover: denied".into()],
        false,
        std::path::Path::new("/bin/tmup"),
    )
    .unwrap_err();
    let text = format!("{err:#}");
    assert!(
        text.contains("invalid candidate")
            && text.contains("/tmp/leftover")
            && text.contains("not replaced")
    );
    let err = finish_cleanup(
        Ok(Outcome { message: "done".into() }),
        vec!["/tmp/leftover: denied".into()],
        true,
        std::path::Path::new("/bin/tmup"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("already replaced at /bin/tmup"));
}

#[test]
fn target_mapping_matches_release_manifest() {
    let output = Command::new("/bin/sh")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/release/release-targets.sh"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut actual: Vec<_> =
        [("linux", "x86_64"), ("linux", "aarch64"), ("macos", "x86_64"), ("macos", "aarch64")]
            .into_iter()
            .map(|(os, arch)| target(os, arch).unwrap())
            .collect();
    let text = String::from_utf8(output.stdout).unwrap();
    let mut expected: Vec<_> = text.lines().collect();
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected);
}

#[test]
fn installer_query_and_preparation_deadlines_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("helper");
    fs::write(&script, "sleep 10\n").unwrap();
    let timeout = Duration::from_millis(30);
    let query = installer::resolve(&script, dir.path(), &Options::default(), timeout).unwrap_err();
    assert!(query.to_string().contains("version query timed out"));
    let prepare =
        installer::prepare(&script, dir.path(), &Version::new(1, 2, 3), "target", timeout)
            .unwrap_err();
    assert!(prepare.to_string().contains("preparation timed out"));
}
