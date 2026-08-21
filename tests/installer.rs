use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{env, fs};

use tempfile::TempDir;

const TARGET: &str = "x86_64-unknown-linux-musl";

struct InstallerTest {
    root: TempDir,
    bin_dir: PathBuf,
    fixtures_dir: PathBuf,
    tmp_dir: PathBuf,
    download_log: PathBuf,
    checksum_log: PathBuf,
}

impl InstallerTest {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin_dir = root.path().join("bin");
        let fixtures_dir = root.path().join("fixtures");
        let tmp_dir = root.path().join("tmp");
        fs::create_dir(&bin_dir).unwrap();
        fs::create_dir(&fixtures_dir).unwrap();
        fs::create_dir(&tmp_dir).unwrap();

        let test = Self {
            download_log: root.path().join("downloads.log"),
            checksum_log: root.path().join("checksums.log"),
            root,
            bin_dir,
            fixtures_dir,
            tmp_dir,
        };
        for command in [
            "awk", "cat", "chmod", "cp", "grep", "gzip", "ln", "mkdir", "mktemp", "mv", "rm", "tar",
        ] {
            test.link_command(command);
        }
        test.write_fake_curl();
        test.write_fake_wget();
        test.write_checksum_tools();
        test
    }

    fn link_command(&self, command: &str) {
        symlink(find_command(command), self.bin_dir.join(command)).unwrap();
    }

    fn write_fake_curl(&self) {
        self.write_executable(
            &self.bin_dir.join("curl"),
            r#"#!/bin/sh
output=
url=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output)
            output=$2
            shift 2
            ;;
        http://*|https://*)
            url=$1
            shift
            ;;
        *)
            shift
            ;;
    esac
done
printf 'curl %s\n' "$url" >> "$TMUP_TEST_DOWNLOAD_LOG"
cp "$TMUP_TEST_FIXTURES/${url##*/}" "$output"
"#,
        );
    }

    fn write_fake_wget(&self) {
        self.write_executable(
            &self.bin_dir.join("wget"),
            r#"#!/bin/sh
output=
url=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output-document)
            output=$2
            shift 2
            ;;
        http://*|https://*)
            url=$1
            shift
            ;;
        *)
            shift
            ;;
    esac
done
printf 'wget %s\n' "$url" >> "$TMUP_TEST_DOWNLOAD_LOG"
cp "$TMUP_TEST_FIXTURES/${url##*/}" "$output"
"#,
        );
    }

    fn write_checksum_tools(&self) {
        if let Some(sha256sum) = find_optional_command("sha256sum") {
            self.write_executable(
                &self.bin_dir.join("sha256sum"),
                &format!(
                    "#!/bin/sh\nprintf 'sha256sum\\n' >> \"$TMUP_TEST_CHECKSUM_LOG\"\nexec '{}' \"$@\"\n",
                    sha256sum.display()
                ),
            );
            self.write_executable(
                &self.bin_dir.join("shasum"),
                &format!(
                    "#!/bin/sh\nprintf 'shasum\\n' >> \"$TMUP_TEST_CHECKSUM_LOG\"\nif [ \"$1\" = -a ] && [ \"$2\" = 256 ]; then shift 2; fi\nexec '{}' \"$@\"\n",
                    sha256sum.display()
                ),
            );
        } else {
            let shasum = find_command("shasum");
            self.write_executable(
                &self.bin_dir.join("sha256sum"),
                &format!(
                    "#!/bin/sh\nprintf 'sha256sum\\n' >> \"$TMUP_TEST_CHECKSUM_LOG\"\nexec '{}' -a 256 \"$@\"\n",
                    shasum.display()
                ),
            );
            self.write_executable(
                &self.bin_dir.join("shasum"),
                &format!(
                    "#!/bin/sh\nprintf 'shasum\\n' >> \"$TMUP_TEST_CHECKSUM_LOG\"\nexec '{}' \"$@\"\n",
                    shasum.display()
                ),
            );
        }
    }

    fn remove_tool(&self, tool: &str) {
        fs::remove_file(self.bin_dir.join(tool)).unwrap();
    }

    fn fail_archive_extraction(&self) {
        let tar = find_command("tar");
        self.remove_tool("tar");
        self.write_executable(
            &self.bin_dir.join("tar"),
            &format!(
                "#!/bin/sh\nif [ \"$1\" = -xzf ]; then exit 1; fi\nexec '{}' \"$@\"\n",
                tar.display()
            ),
        );
    }

    fn assert_temporary_storage_is_empty(&self) {
        assert_eq!(fs::read_dir(&self.tmp_dir).unwrap().count(), 0);
    }

    fn write_executable(&self, path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn add_release(&self, version: &str, target: &str, binary: &str) {
        let archive_dir = format!("tmup-v{version}-{target}");
        let payload_root = self.root.path().join("payload");
        let payload_dir = payload_root.join(&archive_dir);
        fs::create_dir_all(&payload_dir).unwrap();
        self.write_executable(&payload_dir.join("tmup"), binary);

        self.package_payload(version, target, &archive_dir);
    }

    fn package_payload(&self, version: &str, target: &str, payload_member: &str) {
        let archive_name = format!("tmup-v{version}-{target}.tar.gz");
        let archive_path = self.fixtures_dir.join(&archive_name);
        let status = Command::new(find_command("tar"))
            .args(["-czf"])
            .arg(&archive_path)
            .arg("-C")
            .arg(self.root.path().join("payload"))
            .arg(payload_member)
            .status()
            .unwrap();
        assert!(status.success());

        self.write_checksum(&archive_name);
    }

    fn write_checksum(&self, archive_name: &str) {
        let checksum = checksum_output(&self.fixtures_dir.join(archive_name));
        fs::write(self.fixtures_dir.join("SHA256SUMS"), format!("{checksum}  {archive_name}\n"))
            .unwrap();
    }

    fn archive_path(&self, version: &str, target: &str) -> PathBuf {
        self.fixtures_dir.join(format!("tmup-v{version}-{target}.tar.gz"))
    }

    fn payload_dir(&self, version: &str, target: &str) -> PathBuf {
        self.root.path().join("payload").join(format!("tmup-v{version}-{target}"))
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new("/bin/sh")
            .arg(installer_path())
            .args(args)
            .env("PATH", &self.bin_dir)
            .env("TMPDIR", &self.tmp_dir)
            .env("TMUP_TEST_FIXTURES", &self.fixtures_dir)
            .env("TMUP_TEST_DOWNLOAD_LOG", &self.download_log)
            .env("TMUP_TEST_CHECKSUM_LOG", &self.checksum_log)
            .output()
            .unwrap()
    }

    fn install(&self, version: &str, target: &str, destination: &Path) -> Output {
        self.run_install(version, target, destination, false)
    }

    fn force_install(&self, version: &str, target: &str, destination: &Path) -> Output {
        self.run_install(version, target, destination, true)
    }

    fn run_install(&self, version: &str, target: &str, destination: &Path, force: bool) -> Output {
        let mut args =
            vec!["--version", version, "--target", target, "--to", destination.to_str().unwrap()];
        if force {
            args.push("--force");
        }
        self.run(&args)
    }
}

fn installer_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh")
}

fn find_command(name: &str) -> PathBuf {
    find_optional_command(name)
        .unwrap_or_else(|| panic!("required test command `{name}` was not found"))
}

fn find_optional_command(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH").unwrap())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}

fn checksum_output(path: &Path) -> String {
    let output = if let Some(sha256sum) = env::split_paths(&env::var_os("PATH").unwrap())
        .map(|directory| directory.join("sha256sum"))
        .find(|path| path.is_file())
    {
        Command::new(sha256sum).arg(path).output().unwrap()
    } else {
        Command::new(find_command("shasum")).args(["-a", "256"]).arg(path).output().unwrap()
    };
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().split_whitespace().next().unwrap().to_owned()
}

#[test]
fn installs_an_explicit_verified_release() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let installed = destination.join("tmup");
    assert_eq!(fs::read_to_string(&installed).unwrap(), "new tmup\n");
    assert_ne!(fs::metadata(installed).unwrap().permissions().mode() & 0o111, 0);
    assert_eq!(
        fs::read_to_string(&test.download_log).unwrap(),
        concat!(
            "curl https://github.com/wfxr/tmup/releases/download/v1.2.3/",
            "tmup-v1.2.3-x86_64-unknown-linux-musl.tar.gz\n",
            "curl https://github.com/wfxr/tmup/releases/download/v1.2.3/SHA256SUMS\n",
        )
    );
    assert_eq!(fs::read_to_string(&test.checksum_log).unwrap(), "sha256sum\n");
}

#[test]
fn help_describes_the_explicit_install_interface() {
    let test = InstallerTest::new();

    let output = test.run(&["--help"]);

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for option in ["--version", "--target", "--to", "--force", "--help"] {
        assert!(stdout.contains(option), "help omitted {option}: {stdout}");
    }
}

#[test]
fn explicit_version_target_and_destination_are_required() {
    let test = InstallerTest::new();
    let destination = test.root.path().join("destination");
    let destination = destination.to_str().unwrap();
    let cases = [
        (vec!["--target", TARGET, "--to", destination], "--version is required"),
        (vec!["--version", "1.2.3", "--to", destination], "--target is required"),
        (vec!["--version", "1.2.3", "--target", TARGET], "--to is required"),
    ];

    for (args, expected_error) in cases {
        let output = test.run(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr).unwrap().contains(expected_error));
    }
    assert!(!test.download_log.exists());
}

#[test]
fn verification_cannot_be_bypassed() {
    let test = InstallerTest::new();
    let destination = test.root.path().join("destination");

    let output = test.run(&[
        "--version",
        "1.2.3",
        "--target",
        TARGET,
        "--to",
        destination.to_str().unwrap(),
        "--no-verify",
    ]);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("unknown argument"));
    assert!(!test.download_log.exists());
    assert!(!destination.exists());
}

#[test]
fn normalizes_a_leading_v_on_prerelease_versions() {
    let test = InstallerTest::new();
    test.add_release("2.0.0-rc.1", TARGET, "prerelease tmup\n");
    let destination = test.root.path().join("destination");

    let output = test.install("v2.0.0-rc.1", TARGET, &destination);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "prerelease tmup\n");
    assert!(
        fs::read_to_string(&test.download_log)
            .unwrap()
            .contains("/download/v2.0.0-rc.1/tmup-v2.0.0-rc.1-")
    );
}

#[test]
fn rejects_versions_outside_the_release_semver_contract() {
    for version in [
        "1.2",
        "01.2.3",
        "1.02.3",
        "1.2.03",
        "1.2.3-",
        "1.2.3-alpha..1",
        "1.2.3-01",
        "1.2.3+build.1",
        "1.2.3\njunk",
        "junk\n1.2.3",
        "v",
        "vv1.2.3",
    ] {
        let test = InstallerTest::new();
        let destination = test.root.path().join("destination");

        let output = test.install(version, TARGET, &destination);

        assert!(!output.status.success(), "invalid version {version} was accepted");
        assert!(!test.download_log.exists(), "invalid version {version} reached the downloader");
        assert!(!destination.exists());
    }
}

#[test]
fn rejects_unsupported_explicit_targets_with_the_supported_list() {
    let test = InstallerTest::new();
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", "x86_64-unknown-linux-gnu", &destination);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    for target in [
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    ] {
        assert!(stderr.contains(target), "error omitted {target}: {stderr}");
    }
    assert!(!test.download_log.exists());
    assert!(!destination.exists());
}

#[test]
fn accepts_each_supported_explicit_target() {
    for target in [
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    ] {
        let test = InstallerTest::new();
        test.add_release("1.2.3", target, target);
        let destination = test.root.path().join("destination");

        let output = test.install("1.2.3", target, &destination);

        assert!(
            output.status.success(),
            "target {target} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), target);
    }
}

#[test]
fn falls_back_to_wget_and_shasum() {
    let test = InstallerTest::new();
    test.remove_tool("curl");
    test.remove_tool("sha256sum");
    test.add_release("1.2.3", TARGET, "fallback tmup\n");
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read_to_string(&test.download_log)
            .unwrap()
            .lines()
            .all(|line| line.starts_with("wget "))
    );
    assert_eq!(fs::read_to_string(&test.checksum_log).unwrap(), "shasum\n");
}

#[test]
fn fails_when_no_supported_downloader_is_available() {
    let test = InstallerTest::new();
    test.remove_tool("curl");
    test.remove_tool("wget");
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("curl or wget"));
    assert!(!destination.exists());
    test.assert_temporary_storage_is_empty();
}

#[test]
fn fails_when_no_supported_checksum_tool_is_available() {
    let test = InstallerTest::new();
    test.remove_tool("sha256sum");
    test.remove_tool("shasum");
    test.add_release("1.2.3", TARGET, "unchecked tmup\n");
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("sha256sum or shasum"));
    assert!(!destination.exists());
    test.assert_temporary_storage_is_empty();
}

#[test]
fn refuses_to_replace_an_existing_binary_without_force() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    let destination = test.root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    test.write_executable(&destination.join("tmup"), "existing tmup\n");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("--force"));
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "existing tmup\n");
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
    test.assert_temporary_storage_is_empty();
}

#[test]
fn force_replaces_an_existing_binary() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "replacement tmup\n");
    let destination = test.root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    test.write_executable(&destination.join("tmup"), "existing tmup\n");

    let output = test.force_install("1.2.3", TARGET, &destination);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "replacement tmup\n");
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
    test.assert_temporary_storage_is_empty();
}

#[test]
fn force_replaces_a_symlink_to_a_directory() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "replacement tmup\n");
    let destination = test.root.path().join("destination");
    let symlink_target = test.root.path().join("symlink-target");
    fs::create_dir(&destination).unwrap();
    fs::create_dir(&symlink_target).unwrap();
    symlink(&symlink_target, destination.join("tmup")).unwrap();

    let output = test.force_install("1.2.3", TARGET, &destination);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let installed = destination.join("tmup");
    assert!(!fs::symlink_metadata(&installed).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(installed).unwrap(), "replacement tmup\n");
    assert_eq!(fs::read_dir(&symlink_target).unwrap().count(), 0);
    test.assert_temporary_storage_is_empty();
}

#[test]
fn archive_download_failure_preserves_an_existing_binary() {
    let test = InstallerTest::new();
    let destination = test.root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    test.write_executable(&destination.join("tmup"), "existing tmup\n");

    let output = test.force_install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("failed to download"));
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "existing tmup\n");
    test.assert_temporary_storage_is_empty();
}

#[test]
fn checksum_download_failure_preserves_an_existing_binary() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    fs::remove_file(test.fixtures_dir.join("SHA256SUMS")).unwrap();
    let destination = test.root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    test.write_executable(&destination.join("tmup"), "existing tmup\n");

    let output = test.force_install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("failed to download SHA256SUMS"));
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "existing tmup\n");
    test.assert_temporary_storage_is_empty();
}

#[test]
fn checksum_mismatch_preserves_an_existing_binary() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    fs::write(test.archive_path("1.2.3", TARGET), "tampered archive\n").unwrap();
    let destination = test.root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    test.write_executable(&destination.join("tmup"), "existing tmup\n");

    let output = test.force_install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("checksum verification failed"));
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "existing tmup\n");
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
    test.assert_temporary_storage_is_empty();
}

#[test]
fn missing_checksum_entry_preserves_an_existing_binary() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    fs::write(
        test.fixtures_dir.join("SHA256SUMS"),
        "0000000000000000000000000000000000000000000000000000000000000000  other.tar.gz\n",
    )
    .unwrap();
    let destination = test.root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    test.write_executable(&destination.join("tmup"), "existing tmup\n");

    let output = test.force_install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("no entry"));
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "existing tmup\n");
    test.assert_temporary_storage_is_empty();
}

#[test]
fn malformed_archive_preserves_an_existing_binary() {
    let test = InstallerTest::new();
    let archive_name = format!("tmup-v1.2.3-{TARGET}.tar.gz");
    fs::write(test.fixtures_dir.join(&archive_name), "not a tar archive\n").unwrap();
    test.write_checksum(&archive_name);
    let destination = test.root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    test.write_executable(&destination.join("tmup"), "existing tmup\n");

    let output = test.force_install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "existing tmup\n");
    test.assert_temporary_storage_is_empty();
}

#[test]
fn extraction_failure_preserves_an_existing_binary() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    test.fail_archive_extraction();
    let destination = test.root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    test.write_executable(&destination.join("tmup"), "existing tmup\n");

    let output = test.force_install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("could not extract"));
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "existing tmup\n");
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
    test.assert_temporary_storage_is_empty();
}

#[test]
fn archive_with_extra_payload_is_rejected() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    fs::write(test.payload_dir("1.2.3", TARGET).join("README"), "extra\n").unwrap();
    test.package_payload("1.2.3", TARGET, &format!("tmup-v1.2.3-{TARGET}"));
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(!destination.exists());
    test.assert_temporary_storage_is_empty();
}

#[test]
fn archive_with_a_symlink_instead_of_the_binary_is_rejected() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    let binary = test.payload_dir("1.2.3", TARGET).join("tmup");
    fs::remove_file(&binary).unwrap();
    symlink("/bin/sh", &binary).unwrap();
    test.package_payload("1.2.3", TARGET, &format!("tmup-v1.2.3-{TARGET}"));
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(!destination.exists());
    test.assert_temporary_storage_is_empty();
}

#[test]
fn archive_with_a_non_executable_binary_is_rejected() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    let binary = test.payload_dir("1.2.3", TARGET).join("tmup");
    let mut permissions = fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(&binary, permissions).unwrap();
    test.package_payload("1.2.3", TARGET, &format!("tmup-v1.2.3-{TARGET}"));
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(!destination.exists());
    test.assert_temporary_storage_is_empty();
}

#[test]
fn archive_with_the_binary_outside_the_versioned_directory_is_rejected() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    let wrong_dir = test.root.path().join("payload/wrong-directory");
    fs::rename(test.payload_dir("1.2.3", TARGET), &wrong_dir).unwrap();
    test.package_payload("1.2.3", TARGET, "wrong-directory");
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(!destination.exists());
    test.assert_temporary_storage_is_empty();
}
