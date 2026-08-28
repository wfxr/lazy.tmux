#![allow(dead_code)]

use std::path::Path;

pub const MALFORMED_ENVIRONMENT_NODES: &[(&str, &str)] = &[
    (r#"plugin "user/repo" { env "NAME" }"#, "exactly 2 string arguments"),
    (r#"plugin "user/repo" { env "NAME" "value" "extra" }"#, "exactly 2 string arguments"),
    (r#"plugin "user/repo" { env 42 "value" }"#, "arguments must be strings"),
    (r#"plugin "user/repo" { env "NAME" #true }"#, "arguments must be strings"),
    (r#"plugin "user/repo" { env "" "value" }"#, "env name must not be empty"),
    (r#"plugin "user/repo" { unset-env }"#, "exactly 1 string argument"),
    (r#"plugin "user/repo" { unset-env "" }"#, "unset-env name must not be empty"),
    (r#"plugin "user/repo" { unset-env 42 }"#, "arguments must be strings"),
    (r#"plugin "user/repo" { env "NAME" "value" future=#true }"#, "must not have properties"),
    (
        r#"plugin "user/repo" { env (future)"NAME" "value" }"#,
        "does not support KDL type annotations",
    ),
    ("plugin \"user/repo\" { unset-env \"NAME\" { future #true } }", "must not have child nodes"),
];

pub const MALFORMED_BINDING_NODES: &[(&str, &str)] = &[
    (r#"plugin "user/repo" { bind { shell "true" } }"#, "exactly 1 key string argument"),
    (
        r#"plugin "user/repo" { bind "x" "extra" { shell "true" } }"#,
        "exactly 1 key string argument",
    ),
    (r#"plugin "user/repo" { bind 42 { shell "true" } }"#, "key must be a string"),
    (r#"plugin "user/repo" { bind "" { shell "true" } }"#, "key must not be empty"),
    (
        r#"plugin "user/repo" { bind "x" future=#true { shell "true" } }"#,
        "bind must not have properties",
    ),
    (
        r#"plugin "user/repo" { bind (future)"x" { shell "true" } }"#,
        "bind does not support KDL type annotations",
    ),
    (r#"plugin "user/repo" { bind "x" }"#, "bind requires a child block"),
    (r#"plugin "user/repo" { bind "x" { options; shell "true" } }"#, "at least 1"),
    (
        r#"plugin "user/repo" { bind "x" { options ""; shell "true" } }"#,
        "option strings must not be empty",
    ),
    (
        r#"plugin "user/repo" { bind "x" { options "-n" 42; shell "true" } }"#,
        "options must be strings",
    ),
    (
        r#"plugin "user/repo" { bind "x" { options "-n"; options "-r"; shell "true" } }"#,
        "options child may only be specified once",
    ),
    (
        r#"plugin "user/repo" { bind "x" { options "-n" future=#true; shell "true" } }"#,
        "options must not have properties",
    ),
    (
        r#"plugin "user/repo" { bind "x" { options (future)"-n"; shell "true" } }"#,
        "options does not support KDL type annotations",
    ),
    (
        "plugin \"user/repo\" { bind \"x\" { options \"-n\" { future #true }; shell \"true\" } }",
        "options must not have child nodes",
    ),
    (r#"plugin "user/repo" { bind "x" { options "-n" } }"#, "exactly one shell child"),
    (
        r#"plugin "user/repo" { bind "x" { shell "true"; shell "false" } }"#,
        "exactly one shell child",
    ),
    (
        r#"plugin "user/repo" { bind "x" { shell "true" "extra" } }"#,
        "shell requires exactly 1 command string",
    ),
    (r#"plugin "user/repo" { bind "x" { shell 42 } }"#, "shell command must be a string"),
    (r#"plugin "user/repo" { bind "x" { shell "   " } }"#, "empty or whitespace-only"),
    (r#"plugin "user/repo" { bind "x" { shell "true" future=#true } }"#, "unknown shell property"),
    (
        r#"plugin "user/repo" { bind "x" { shell "true" background="yes" } }"#,
        "background must be a bool",
    ),
    (
        r#"plugin "user/repo" { bind "x" { shell "true" background=#true background=#false } }"#,
        "background may only be specified once",
    ),
    (
        r#"plugin "user/repo" { bind "x" { shell "true" background=(future)#true } }"#,
        "background does not support KDL type annotations",
    ),
    (
        r#"plugin "user/repo" { bind "x" { shell (future)"true" } }"#,
        "shell does not support KDL type annotations",
    ),
    (
        "plugin \"user/repo\" { bind \"x\" { shell \"true\" { future #true } } }",
        "shell must not have child nodes",
    ),
    (r#"plugin "user/repo" { bind "x" { future "value"; shell "true" } }"#, "unknown bind child"),
];

pub fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// Run a hermetic git command in the given directory.
pub fn git(args: &[&str], dir: &Path) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("HOME", dir)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Create a bare repo under `remotes/example.com/test/{name}.git`.
pub fn make_remote_repo_named(root: &Path, name: &str) -> std::path::PathBuf {
    let work = root.join(format!("work-{name}"));
    std::fs::create_dir_all(&work).unwrap();

    git(&["init", "-b", "main"], &work);
    std::fs::write(work.join("init.tmux"), "#!/bin/sh\n").unwrap();
    git(&["add", "."], &work);
    git(&["commit", "-m", "init"], &work);

    let bare_parent = root.join("remotes/example.com/test");
    std::fs::create_dir_all(&bare_parent).unwrap();
    let bare = bare_parent.join(format!("{name}.git"));
    git(&["clone", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()], root);
    bare
}

/// Create the default bare repo at `remotes/example.com/test/plugin.git`.
pub fn make_remote_repo(root: &Path) -> std::path::PathBuf {
    make_remote_repo_named(root, "plugin")
}

/// Write a git config that rewrites `https://example.com/` to the local remotes dir.
pub fn write_git_rewrite_config(root: &Path) -> std::path::PathBuf {
    let gitconfig = root.join("gitconfig");
    let rewritten_base = format!("file://{}/", root.join("remotes/example.com").display());
    std::fs::write(
        &gitconfig,
        format!("[url \"{rewritten_base}\"]\n    insteadOf = https://example.com/\n"),
    )
    .unwrap();
    gitconfig
}

/// Create a bare repo with one commit and return (bare_path, commit_hash).
pub fn make_bare_repo(root: &Path) -> (std::path::PathBuf, String) {
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();

    git(&["init", "-b", "main"], &work);
    std::fs::write(work.join("init.tmux"), "#!/bin/sh\n").unwrap();
    git(&["add", "."], &work);
    git(&["commit", "-m", "init"], &work);

    let commit = git(&["rev-parse", "HEAD"], &work);

    let bare = root.join("bare.git");
    git(&["clone", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()], root);

    (bare, commit)
}

/// Add a commit to the default branch of a bare repo and push it.
pub fn push_commit(bare: &Path, message: &str) -> String {
    let tmp = bare.parent().unwrap().join(format!("_push_{message}_tmp"));
    let _ = std::fs::remove_dir_all(&tmp);
    git(&["clone", bare.to_str().unwrap(), tmp.to_str().unwrap()], bare.parent().unwrap());
    std::fs::write(tmp.join(format!("{message}.txt")), message).unwrap();
    git(&["add", "."], &tmp);
    git(&["commit", "-m", message], &tmp);
    git(&["push"], &tmp);
    let hash = git(&["rev-parse", "HEAD"], &tmp);
    std::fs::remove_dir_all(&tmp).unwrap();
    hash
}

/// Create a new branch on a bare repo, push a commit, and return its hash.
pub fn push_branch_commit(bare: &Path, branch: &str, message: &str) -> String {
    let tmp = bare.parent().unwrap().join(format!("_branch_{branch}_tmp"));
    let _ = std::fs::remove_dir_all(&tmp);
    git(&["clone", bare.to_str().unwrap(), tmp.to_str().unwrap()], bare.parent().unwrap());
    git(&["checkout", "-b", branch], &tmp);
    std::fs::write(tmp.join(format!("{message}.txt")), message).unwrap();
    git(&["add", "."], &tmp);
    git(&["commit", "-m", message], &tmp);
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    git(&["push", "-u", "origin", &refspec], &tmp);
    let hash = git(&["rev-parse", "HEAD"], &tmp);
    std::fs::remove_dir_all(&tmp).unwrap();
    hash
}

/// Tag a commit in a bare repo.
pub fn push_tag(bare: &Path, tag: &str, commit: &str) {
    let tmp = bare.parent().unwrap().join("_tag_tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    git(&["clone", bare.to_str().unwrap(), tmp.to_str().unwrap()], bare.parent().unwrap());
    git(&["tag", tag, commit], &tmp);
    git(&["push", "origin", tag], &tmp);
    std::fs::remove_dir_all(&tmp).unwrap();
}

/// Clone a bare repo into a target directory (simulating an installed plugin).
pub fn clone_to_target(source: &Path, target: &Path) {
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    git(&["clone", source.to_str().unwrap(), target.to_str().unwrap()], target.parent().unwrap());
}
