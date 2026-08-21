#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;

fn release_script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/release").join(name)
}

fn package_tag() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

#[test]
fn version_validation_accepts_the_package_version() {
    Command::new(release_script("validate-version.sh"))
        .arg(package_tag())
        .assert()
        .success()
        .stdout(format!("{}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn version_validation_accepts_a_prerelease_package_version() {
    let temp = tempfile::tempdir().unwrap();
    let manifest = temp.path().join("Cargo.toml");
    std::fs::write(
        &manifest,
        "[package]\nname = \"tmup\"\nversion = \"1.2.3-rc.1\"\nedition = \"2024\"\n",
    )
    .unwrap();

    Command::new(release_script("validate-version.sh"))
        .args(["v1.2.3-rc.1", manifest.to_str().unwrap()])
        .assert()
        .success()
        .stdout("1.2.3-rc.1\n");
}

#[test]
fn version_validation_rejects_malformed_or_build_metadata_tags() {
    for tag in ["0.1.0", "v01.2.3", "v1.2", "v1.2.3-01", "v1.2.3-rc..1", "v0.1.0+build.1"] {
        Command::new(release_script("validate-version.sh"))
            .arg(tag)
            .assert()
            .failure()
            .stderr(predicate::str::contains("v-prefixed SemVer without build metadata"));
    }
}

#[test]
fn version_validation_rejects_a_package_version_mismatch() {
    Command::new(release_script("validate-version.sh")).arg("v999.0.0").assert().failure().stderr(
        predicate::str::contains(format!(
            "does not match Cargo package version v{}",
            env!("CARGO_PKG_VERSION")
        )),
    );
}

#[test]
fn local_packaging_produces_the_single_binary_archive_contract() {
    let temp = tempfile::tempdir().unwrap();
    let output_dir = temp.path().join("dist");
    let target = "x86_64-unknown-linux-musl";
    let package_name = format!("tmup-v{}-{target}", env!("CARGO_PKG_VERSION"));
    let archive = output_dir.join(format!("{package_name}.tar.gz"));

    Command::new(release_script("package.sh"))
        .arg(package_tag())
        .arg(target)
        .arg(assert_cmd::cargo::cargo_bin!("tmup"))
        .arg(&output_dir)
        .assert()
        .success()
        .stdout(format!("{}\n", archive.display()));

    let listing = std::process::Command::new("tar")
        .args(["-tzf", archive.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(listing.status.success());
    assert_eq!(
        String::from_utf8(listing.stdout).unwrap(),
        format!("{package_name}/\n{package_name}/tmup\n")
    );

    let extracted = temp.path().join("extracted");
    std::fs::create_dir(&extracted).unwrap();
    let extraction = std::process::Command::new("tar")
        .args(["-xzf", archive.to_str().unwrap(), "-C", extracted.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(extraction.status.success());

    Command::new(extracted.join(package_name).join("tmup"))
        .arg("--version")
        .assert()
        .success()
        .stdout(format!("tmup {}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn archive_validation_rejects_extra_payload_files() {
    let temp = tempfile::tempdir().unwrap();
    let target = "x86_64-unknown-linux-musl";
    let package_name = format!("tmup-v{}-{target}", env!("CARGO_PKG_VERSION"));
    let package_dir = temp.path().join(&package_name);
    let archive = temp.path().join(format!("{package_name}.tar.gz"));
    std::fs::create_dir(&package_dir).unwrap();
    std::fs::copy(assert_cmd::cargo::cargo_bin!("tmup"), package_dir.join("tmup")).unwrap();
    std::fs::write(package_dir.join("README.md"), "unexpected payload\n").unwrap();

    let packaging = std::process::Command::new("tar")
        .args([
            "-czf",
            archive.to_str().unwrap(),
            "-C",
            temp.path().to_str().unwrap(),
            &package_name,
        ])
        .output()
        .unwrap();
    assert!(packaging.status.success());

    Command::new(release_script("validate-archive.sh"))
        .arg(package_tag())
        .arg(target)
        .arg(&archive)
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!("expected only {package_name}/tmup")));
}

#[test]
fn packaging_rejects_a_binary_with_a_different_version() {
    let temp = tempfile::tempdir().unwrap();
    let output_dir = temp.path().join("dist");
    let binary = temp.path().join("tmup");
    std::fs::write(&binary, "#!/bin/sh\nprintf 'tmup 9.9.9\\n'\n").unwrap();
    let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions).unwrap();

    Command::new(release_script("package.sh"))
        .arg(package_tag())
        .arg("x86_64-unknown-linux-musl")
        .arg(&binary)
        .arg(&output_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!(
            "binary reported 'tmup 9.9.9', expected 'tmup {}'",
            env!("CARGO_PKG_VERSION")
        )));

    assert!(
        !output_dir
            .join(format!("tmup-v{}-x86_64-unknown-linux-musl.tar.gz", env!("CARGO_PKG_VERSION")))
            .exists()
    );
}
