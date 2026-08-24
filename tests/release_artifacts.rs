#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;

const RELEASE_TARGETS: [&str; 4] = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];

fn release_script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/release").join(name)
}

fn package_tag() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

fn fake_release_binary(path: &std::path::Path) {
    std::fs::write(path, format!("#!/bin/sh\nprintf 'tmup {}\\n'\n", env!("CARGO_PKG_VERSION")))
        .unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn package_release_archives(root: &std::path::Path) -> PathBuf {
    let downloads = root.join("downloads");
    let binary = root.join("tmup");
    fake_release_binary(&binary);

    for target in RELEASE_TARGETS {
        Command::new(release_script("package.sh"))
            .arg(package_tag())
            .arg(target)
            .arg(&binary)
            .arg(&downloads)
            .assert()
            .success();
    }

    downloads
}

fn git(root: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git").args(args).current_dir(root).output().unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn release_tag_repo(tag_kind: &str) -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "--quiet"]);
    git(repo.path(), &["config", "user.email", "tmup@example.com"]);
    git(repo.path(), &["config", "user.name", "tmup tests"]);
    git(repo.path(), &["config", "commit.gpgSign", "false"]);
    git(repo.path(), &["config", "tag.gpgSign", "false"]);
    git(repo.path(), &["config", "core.hooksPath", ".git/no-hooks"]);
    std::fs::write(repo.path().join("README"), "release\n").unwrap();
    git(repo.path(), &["add", "README"]);
    git(repo.path(), &["commit", "--quiet", "-m", "release"]);
    git(repo.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);

    let tag = package_tag();
    match tag_kind {
        "lightweight" => {
            git(repo.path(), &["tag", &tag]);
        }
        "annotated" => {
            git(repo.path(), &["tag", "--annotate", &tag, "--message", "release"]);
        }
        _ => panic!("unknown tag kind: {tag_kind}"),
    }

    repo
}

fn fake_gh(root: &std::path::Path, initial_state: &str) -> (PathBuf, PathBuf, PathBuf) {
    let bin_dir = root.join("bin");
    let state = root.join("gh-state");
    let log = root.join("gh.log");
    std::fs::create_dir(&bin_dir).unwrap();
    std::fs::write(&state, format!("{initial_state}\n")).unwrap();
    std::fs::write(root.join("gh-prerelease"), "false\n").unwrap();

    let gh = bin_dir.join("gh");
    std::fs::write(
        &gh,
        r###"#!/bin/sh
set -eu

printf '%s\n' "$*" >> "$FAKE_GH_LOG"

case "$1:$2" in
    api:*)
        [ "$(cat "$FAKE_GH_STATE")" != missing ] || exit 1
        query=
        previous=
        for argument in "$@"; do
            if [ "$previous" = --jq ]; then
                query=$argument
                break
            fi
            previous=$argument
        done
        case "$query" in
            .draft)
                if [ "$(cat "$FAKE_GH_STATE")" = draft ]; then
                    echo true
                else
                    echo false
                fi
                ;;
            .prerelease)
                cat "$FAKE_GH_PRERELEASE"
                ;;
            '.assets[].name')
                if [ -f "$FAKE_GH_ASSETS" ]; then
                    cut -f 1 "$FAKE_GH_ASSETS"
                fi
                ;;
            '.assets[] | [.name, .state, .digest] | @tsv')
                if [ -f "$FAKE_GH_ASSETS" ]; then
                    cat "$FAKE_GH_ASSETS"
                fi
                ;;
            *)
                echo "unsupported fake gh api query: $query" >&2
                exit 2
                ;;
        esac
        ;;
    release:create)
        [ "$(cat "$FAKE_GH_STATE")" = missing ] || exit 1
        echo draft > "$FAKE_GH_STATE"
        echo false > "$FAKE_GH_PRERELEASE"
        for argument in "$@"; do
            if [ "$argument" = --prerelease ]; then
                echo true > "$FAKE_GH_PRERELEASE"
            fi
        done
        ;;
    release:delete-asset)
        asset_name=$4
        temporary="$FAKE_GH_ASSETS.tmp"
        awk -F '\t' -v name="$asset_name" '$1 != name' "$FAKE_GH_ASSETS" > "$temporary"
        mv "$temporary" "$FAKE_GH_ASSETS"
        ;;
    release:upload)
        : > "$FAKE_GH_ASSETS"
        for argument in "$@"; do
            if [ -f "$argument" ]; then
                name=$(basename "$argument")
                digest=$(sha256sum "$argument" | awk '{ print $1 }')
                printf '%s\tuploaded\tsha256:%s\n' "$name" "$digest" >> "$FAKE_GH_ASSETS"
            fi
        done
        ;;
    release:edit)
        for argument in "$@"; do
            case "$argument" in
                --prerelease)
                    echo true > "$FAKE_GH_PRERELEASE"
                    ;;
                --prerelease=false)
                    echo false > "$FAKE_GH_PRERELEASE"
                    ;;
                --draft=false)
                    echo public > "$FAKE_GH_STATE"
                    ;;
            esac
        done
        ;;
    *)
        echo "unsupported fake gh command: $*" >&2
        exit 2
        ;;
esac
"###,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&gh).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&gh, permissions).unwrap();

    (bin_dir, state, log)
}

fn publish_command(
    bin_dir: &std::path::Path,
    root: &std::path::Path,
    state: &std::path::Path,
    log: &std::path::Path,
    release: &std::path::Path,
) -> Command {
    let mut command = Command::new(release_script("publish-release.sh"));
    command
        .arg(package_tag())
        .arg(release)
        .env("PATH", format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap()))
        .env("FAKE_GH_STATE", state)
        .env("FAKE_GH_PRERELEASE", root.join("gh-prerelease"))
        .env("FAKE_GH_ASSETS", root.join("gh-assets"))
        .env("FAKE_GH_LOG", log);
    command
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

#[test]
fn release_asset_preparation_generates_the_complete_checksum_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let downloads = package_release_archives(temp.path());
    let release = temp.path().join("release");

    Command::new(release_script("prepare-assets.sh"))
        .arg(package_tag())
        .arg(&downloads)
        .arg(&release)
        .assert()
        .success();

    let mut asset_names = std::fs::read_dir(&release)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    asset_names.sort();

    let version = env!("CARGO_PKG_VERSION");
    let mut expected_names = RELEASE_TARGETS
        .iter()
        .map(|target| format!("tmup-v{version}-{target}.tar.gz"))
        .chain(std::iter::once("SHA256SUMS".to_string()))
        .collect::<Vec<_>>();
    expected_names.sort();
    assert_eq!(asset_names, expected_names);

    let checksum_manifest = std::fs::read_to_string(release.join("SHA256SUMS")).unwrap();
    let checksum_names = checksum_manifest
        .lines()
        .map(|line| line.split_whitespace().nth(1).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        checksum_names,
        RELEASE_TARGETS
            .iter()
            .map(|target| format!("tmup-v{version}-{target}.tar.gz"))
            .collect::<Vec<_>>()
    );

    let verification = std::process::Command::new("sha256sum")
        .arg("--check")
        .arg("SHA256SUMS")
        .current_dir(&release)
        .output()
        .unwrap();
    assert!(
        verification.status.success(),
        "generated checksum manifest should verify: {}",
        String::from_utf8_lossy(&verification.stderr)
    );
}

#[test]
fn release_asset_preparation_rejects_an_incomplete_archive_set() {
    let temp = tempfile::tempdir().unwrap();
    let downloads = package_release_archives(temp.path());
    let missing_name = format!("tmup-v{}-aarch64-apple-darwin.tar.gz", env!("CARGO_PKG_VERSION"));
    std::fs::remove_file(downloads.join(&missing_name)).unwrap();
    let release = temp.path().join("release");

    Command::new(release_script("prepare-assets.sh"))
        .arg(package_tag())
        .arg(&downloads)
        .arg(&release)
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!("missing asset: {missing_name}")));

    assert!(!release.exists(), "failed preparation must not leave a partial asset directory");
}

#[test]
fn release_tag_validation_accepts_lightweight_and_annotated_tags() {
    for tag_kind in ["lightweight", "annotated"] {
        let repo = release_tag_repo(tag_kind);
        let commit = git(repo.path(), &["rev-parse", "HEAD"]);

        Command::new(release_script("validate-release-tag.sh"))
            .arg(package_tag())
            .arg(&commit)
            .current_dir(repo.path())
            .assert()
            .success()
            .stdout(format!("{}\n", env!("CARGO_PKG_VERSION")));
    }
}

#[test]
fn release_tag_validation_rejects_a_commit_outside_origin_main() {
    let repo = release_tag_repo("lightweight");
    let tag = package_tag();
    git(repo.path(), &["tag", "--delete", &tag]);
    std::fs::write(repo.path().join("branch-only"), "unmerged\n").unwrap();
    git(repo.path(), &["add", "branch-only"]);
    git(repo.path(), &["commit", "--quiet", "-m", "unmerged release"]);
    git(repo.path(), &["tag", &tag]);
    let commit = git(repo.path(), &["rev-parse", "HEAD"]);

    Command::new(release_script("validate-release-tag.sh"))
        .arg(&tag)
        .arg(&commit)
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!("{tag} is not reachable from origin/main")));
}

#[test]
fn release_publication_stages_assets_before_making_the_release_public() {
    let temp = tempfile::tempdir().unwrap();
    let downloads = package_release_archives(temp.path());
    let release = temp.path().join("release");
    Command::new(release_script("prepare-assets.sh"))
        .arg(package_tag())
        .arg(&downloads)
        .arg(&release)
        .assert()
        .success();
    let (bin_dir, state, log) = fake_gh(temp.path(), "missing");

    publish_command(&bin_dir, temp.path(), &state, &log, &release).assert().success();

    assert_eq!(std::fs::read_to_string(&state).unwrap(), "public\n");
    let calls = std::fs::read_to_string(&log).unwrap();
    let create = calls.find("release create").expect("release should be created as a draft");
    let upload = calls.find("release upload").expect("release assets should be uploaded");
    let publish =
        calls.rfind("release edit").expect("draft should be published only after verification");
    assert!(create < upload && upload < publish, "unexpected publication order:\n{calls}");
    assert!(calls[create..upload].contains("--draft"));
    assert!(calls[create..upload].contains("--generate-notes"));
    assert!(calls[publish..].contains("--draft=false"));
}

#[test]
fn release_publication_repairs_the_existing_draft() {
    let temp = tempfile::tempdir().unwrap();
    let downloads = package_release_archives(temp.path());
    let release = temp.path().join("release");
    Command::new(release_script("prepare-assets.sh"))
        .arg(package_tag())
        .arg(&downloads)
        .arg(&release)
        .assert()
        .success();
    let (bin_dir, state, log) = fake_gh(temp.path(), "draft");
    std::fs::write(
        temp.path().join("gh-assets"),
        "stale.txt\tuploaded\tsha256:0000000000000000000000000000000000000000000000000000000000000000\n",
    )
    .unwrap();

    publish_command(&bin_dir, temp.path(), &state, &log, &release).assert().success();

    assert_eq!(std::fs::read_to_string(&state).unwrap(), "public\n");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(!calls.contains("release create"), "rerun should reuse the draft:\n{calls}");
    assert!(calls.contains("release delete-asset"));
    assert!(calls.contains("release upload"));
    assert!(calls.contains("--clobber"));
}

#[test]
fn release_publication_refuses_to_mutate_a_public_release() {
    let temp = tempfile::tempdir().unwrap();
    let downloads = package_release_archives(temp.path());
    let release = temp.path().join("release");
    Command::new(release_script("prepare-assets.sh"))
        .arg(package_tag())
        .arg(&downloads)
        .arg(&release)
        .assert()
        .success();
    let (bin_dir, state, log) = fake_gh(temp.path(), "public");

    publish_command(&bin_dir, temp.path(), &state, &log, &release).assert().failure().stderr(
        predicate::str::contains(format!("a public release already exists for {}", package_tag())),
    );

    assert_eq!(std::fs::read_to_string(&state).unwrap(), "public\n");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(!calls.contains("release upload"));
    assert!(!calls.contains("release edit"));
}
