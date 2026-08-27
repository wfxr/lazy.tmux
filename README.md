<h1 align="center">tmup</h1>

<p align="center">
  A modern, config-driven tmux plugin manager — inspired by <a href="https://github.com/folke/lazy.nvim">lazy.nvim</a>.
</p>

<p align="center">
  <a href="#features">Features</a> &bull;
  <a href="#installation">Installation</a> &bull;
  <a href="#quick-start">Quick Start</a> &bull;
  <a href="#configuration">Configuration</a> &bull;
  <a href="#commands">Commands</a> &bull;
  <a href="#tpm-compatibility">TPM Compatibility</a>
</p>

---

## Why tmup?

[TPM](https://github.com/tmux-plugins/tpm) has been the de-facto tmux plugin
manager for years, but it is largely unmaintained and carries several structural
limitations: pure bash implementation, weak error handling, serial
install/update, no lock file, and no reproducible state management.

tmup is a ground-up rewrite in Rust that brings the convenience of
lazy.nvim's design philosophy to tmux:

- **Declarative config** — a single `tmup.kdl` file describes everything.
- **Resolved lock snapshot** — `tmup.lock` records the commits selected from config.
- **Concurrent operations** — installs and updates run in parallel.
- **Safe publish protocol** — staging + atomic rename + rollback on build failure.
- **Script-friendly CLI** — clear exit codes, partial-failure reporting, predictable semantics.

## Features

- **Config-driven sync** — `tmup.kdl` is the desired state for remote plugins;
  `tmup.lock` is the resolved snapshot that mutating commands reconcile first.
- **Safe publish** — every revision change goes through a staging directory
  first. Build failures trigger automatic rollback to the previous version.
- **Safe init** — `init` holds the global lock from start through plugin
  loading, preventing concurrent writers from modifying state mid-init.
- **Incremental reconcile** — changing one remote plugin's source, selector, or
  `build` only syncs that plugin.
- **Build failure memory** — failed builds are recorded as
  `(plugin, commit, build-command-hash)` tuples. Those markers are surfaced in
  `list`, and exact-tuple suppression currently applies in the install path
  after commit resolution; because `init` starts with implicit `sync`, startup
  may still re-surface the same failure.
- **Partial failure reporting** — commands like `install` and `update` publish
  successful plugins and write the lock, but return a non-zero exit code if
  any plugin fails.
- **TPM-compatible** — plugins that use `@option` + `*.tmux` entry scripts work
  out of the box.
- **Conditional inclusion and loading** — reuse one config across hosts while
  choosing independently whether tmup manages or loads each plugin.

## Installation

Install tmup from a pre-built binary or build it locally from source.

### From source

Build and install tmup with your local Rust toolchain:

```bash
cargo install --path .
```

### Pre-built binaries

Pre-built binaries are available for 64-bit Linux and macOS. Linux releases use
MUSL so the same target works across common Linux distributions.

#### Remote installer

Run the repository-owned installer over HTTPS:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/wfxr/tmup/main/install.sh | sh
```

The installer selects the latest stable release and installs `tmup` to
`~/.local/bin` by default. It verifies the downloaded archive against the
release's `SHA256SUMS` before it changes the destination; verification is
mandatory and can't be disabled.

#### Installer options

The installer supports deterministic overrides and help behavior. When you use
the pipe command, pass options after `sh -s --`.

| Option | Behavior |
|--------|----------|
| `--version <VERSION>` | Install an explicit stable or prerelease version, with or without a leading `v`; omit it to select the latest stable release |
| `--include-prerelease`, `--pre` | Install the latest published release, whether stable or prerelease; can't be combined with `--version` |
| `--to <DIRECTORY>` | Install to a specific directory instead of `~/.local/bin` |
| `--force` | Replace an existing `tmup` binary; without this option, the installer refuses to overwrite it |
| `--target <TARGET>` | Override host detection with one of the supported target triples |
| `--help` | Print usage information and exit without installing |

GitHub's latest-release selection excludes prereleases. To install the latest
published release whether it is stable or prerelease, use `--pre`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/wfxr/tmup/main/install.sh |
  sh -s -- --pre
```

For a reproducible install, request an exact version with `--version`, for
example, `--version 0.1.0-rc.3`.

#### Supported targets

The installer supports only these operating system and architecture pairs:

| Host | Release target |
|------|----------------|
| Linux x86-64 | `x86_64-unknown-linux-musl` |
| Linux ARM64 | `aarch64-unknown-linux-musl` |
| macOS Intel | `x86_64-apple-darwin` |
| macOS Apple Silicon | `aarch64-apple-darwin` |

Every other platform is a hard failure. The installer lists the supported
targets and does not fall back to a source build.

#### Manual installation

You can download and verify the release assets directly instead of running the
remote installer. The following example installs version `0.1.0` for Linux
x86-64; replace `version` and `target` with the release and supported target you
need.

1. Select the versioned target archive:

   ```sh
   version=0.1.0
   target=x86_64-unknown-linux-musl
   archive_dir="tmup-v${version}-${target}"
   archive="${archive_dir}.tar.gz"
   release_url="https://github.com/wfxr/tmup/releases/download/v${version}"
   ```

2. Download the archive and its checksum manifest:

   ```sh
   curl --fail --location --remote-name "${release_url}/${archive}"
   curl --fail --location --remote-name "${release_url}/SHA256SUMS"
   grep -F "  ${archive}" SHA256SUMS > "${archive}.sha256"
   ```

3. Verify the archive with the SHA-256 tool for your platform. On Linux, run:

   ```sh
   sha256sum --check "${archive}.sha256"
   ```

   On macOS, run:

   ```sh
   shasum -a 256 --check "${archive}.sha256"
   ```

4. Verify the GitHub artifact attestations for both downloaded assets:

   ```sh
   gh attestation verify "${archive}" --repo wfxr/tmup
   gh attestation verify SHA256SUMS --repo wfxr/tmup
   ```

   The checksums and attestations identify the official bytes produced by the
   release workflow. They do not claim that separate builds are bit-for-bit
   reproducible.

5. Extract the versioned directory and install its single `tmup` executable:

   ```sh
   tar -xzf "${archive}"
   mkdir -p "${HOME}/.local/bin"
   install -m 755 "${archive_dir}/tmup" "${HOME}/.local/bin/tmup"
   ```

## Quick Start

**1. Create a config file**

```bash
mkdir -p ~/.config/tmux
cat > ~/.config/tmux/tmup.kdl << 'EOF'
options {
    auto-install #true
    concurrency 16
}

plugin "tmux-plugins/tmux-sensible"
plugin "tmux-plugins/tmux-yank"
plugin "catppuccin/tmux" opt-prefix="catppuccin_" {
    opt "flavor" "mocha"
}
EOF
```

**2. Add to `.tmux.conf`**

```tmux
run-shell "tmup init"
```

**3. Reload tmux**

```bash
tmux source-file ~/.tmux.conf
```

tmup will auto-install missing plugins on the first `init` and generate
`tmup.lock`. The lock snapshot records the resolved plugin revisions so an
existing environment can be reproduced when needed.

## Configuration

tmup uses [KDL v2](https://kdl.dev) syntax. Config file search order:

1. `$TMUP_CONFIG`
2. `$XDG_CONFIG_HOME/tmux/tmup.kdl`
3. `~/.config/tmux/tmup.kdl`

Mutating commands and `init` create the default discovered `tmup.kdl` when it
does not exist yet. Read-only commands such as `list` do not create it. When
`TMUP_CONFIG` is set explicitly, it must point to an existing file.

`tmup.lock` lives next to the active `tmup.kdl`.

tmup supports two internal config loading modes:

- `pure` — load only `tmup.kdl`
- `mixed` — load `tmup.kdl` and TPM-style tmux plugin declarations together

Set `TMUP_CONFIG_MODE=mixed` for a command when you want to combine `tmup.kdl`
with existing `set -g @plugin ...` lines from your tmux config.

In mixed mode, plugin order starts from the TPM-compatible declarations
discovered by tmup's TPM scan. KDL-only entries, including local plugins, are
appended afterward in `tmup.kdl` order, and if both sources declare the same
remote plugin ID, tmup keeps the TPM position but uses the `tmup.kdl` entry.

### Full example

```kdl
options {
    auto-install #true
    concurrency 16
}

// GitHub shorthand — track default branch
plugin "tmux-plugins/tmux-sensible"

// Pin to a tag — update skips pinned plugins
plugin "tmux-plugins/tmux-yank" tag="v2.3"

// Branch + build command + options
plugin "tmux-plugins/tmux-resurrect" branch="master" build="make install" {
    opt "resurrect-strategy-vim" "session"
    opt "resurrect-save-bash-history" "on"
}

// opt-prefix avoids repetition: opt "flavor" → @catppuccin_flavor
plugin "catppuccin/tmux" opt-prefix="catppuccin_" {
    opt "flavor" "mocha"
    opt "window_text" "#W"
}

// Non-GitHub source
plugin "https://gitlab.com/user/my-plugin.git"

// Local plugin — loaded in-place, not in the lock snapshot
plugin "~/dev/my-tmux-plugin" local=#true name="my-plugin-dev"

// Manage this plugin only on one stable execution host
plugin "company/workstation-tools" enabled=#"test "$(hostname)" = workstation"#

// Keep this plugin managed, but load it only in SSH environments
plugin "company/remote-tools" cond=#"[ -n "$SSH_CLIENT" ]"#

// Disable with KDL slashdash
/-plugin "tmux-plugins/tmux-continuum"
```

### Options reference

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `auto-install` | bool | `#true` | Install missing plugins during `init` |
| `concurrency` | integer | `16` | Max concurrent remote prepare jobs; `1` forces serial prepare |


### Plugin properties

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| 1st arg | string | — | GitHub `user/repo`, full git URL, or local path |
| `name` | string | basename of id | Display name for logs |
| `opt-prefix` | string | `""` | Prefix prepended to all `opt` keys |
| `branch` | string | — | Track a specific branch |
| `tag` | string | — | Pin to a tag (update skips) |
| `commit` | string | — | Pin to a commit (update skips) |
| `local` | bool | `#false` | Treat source as a local path; after expansion it must be absolute |
| `build` | string | — | Shell command to run after sync/update/restore publishes a revision |
| `enabled` | bool or string | `#true` | Include the plugin in this host's Effective Plugin Specification |
| `cond` | bool or string | `#true` | Give the enabled plugin Load Eligibility for `init` |

> `branch`, `tag`, and `commit` are mutually exclusive.
>
> Local paths support `~`, `$VAR`, and `${VAR}` expansion. After expansion, the
> path must be absolute.
>
> Sync hashes the canonical remote identity, tracking selector, and `build`.
> Equivalent remote spellings such as `.git` vs no `.git` do not trigger sync.
> Comments, formatting, `name`, `opt`, `opt-prefix`, and local-plugin-only
> changes do not trigger sync. The configuration fingerprint covers remote
> plugins in the resulting Effective Plugin Specification. Changing an
> `enabled` predicate while it continues to evaluate true does not affect the
> fingerprint. If `enabled` becomes false for a remote plugin, that plugin
> leaves the specification and lock snapshot, which changes the fingerprint.
> `cond` values, predicate text, and Load Eligibility do not affect lock
> fingerprints.

### Conditional plugins

Conditional plugin declarations separate whether tmup manages a plugin from
whether an Init Session loads it. This design applies to both remote and local
plugins.

Both properties default to true and accept either a KDL bool or a non-empty
shell predicate string:

```kdl
// Exclude this plugin from the effective spec on other hosts.
plugin "company/workstation-tools" enabled=#"test "$(hostname)" = workstation"#

// Keep this plugin managed, but load only in an SSH environment.
plugin "company/remote-tools" cond=#"[ -n "$SSH_CLIENT" ]"#
```

`enabled=#false` excludes the plugin from the Effective Plugin Specification.
The plugin does not participate in lifecycle commands, list output, targeted
selectors, or the lock snapshot. If a previously enabled remote plugin becomes
disabled, `sync` drops its lock entry and a later `clean` may remove its managed
checkout.

`cond=#false` keeps the plugin managed, installed, built, updated, and locked.
During `init`, tmup skips both its options and its `*.tmux` scripts. Installation
or build failures still make `init` return a non-zero status. Changing `cond`
from true to false does not undo effects from an earlier load; restart the tmux
server when you need to clear those effects.

String predicates run with `/bin/sh -c`, inherit the tmup process environment,
use the configuration directory as their working directory, and time out after
five seconds. Exit status zero means true and every non-zero status means false.
tmup discards ordinary predicate output. Failure to start the shell, signal
termination, and timeout are hard errors. Predicates must be fast,
side-effect-free, and independent of plugin state that the current command may
change.

Every command evaluates `enabled` from its own environment. Only `init` and
`list` evaluate `cond`; `list` reports the current result as `LOAD=yes` or
`LOAD=no`. This value describes whether a future Init Session would load the
plugin, not whether a previous load still affects the tmux server.

tmup validates all declarations and merges mixed-mode sources before running
predicates. It evaluates Enable Conditions sequentially in effective
declaration order, then evaluates Load Conditions only for enabled plugins.
Each command freezes one result snapshot. An Init Session may evaluate an
advisory preview, but its final execution rereads the inherited config and
resolves one authoritative snapshot before managed-state mutation.

Use stable host properties for `enabled`. Environment values such as
`SSH_CLIENT` may differ between direct shell commands and commands launched
by a long-lived tmux server. tmup does not infer which environment is correct.

In mixed TPM mode, tmup merges declarations before evaluating conditions.
TPM-only declarations are unconditional. A matching KDL declaration with
`enabled=#false` suppresses the plugin instead of falling back to the TPM
declaration.

Unknown plugin parameters produce warnings and are otherwise ignored. Invalid
values for `enabled` or `cond`, empty predicate strings, duplicate known scalar
properties, and reserved condition child or type-annotation syntax are errors.
Older tmup versions may ignore condition properties and load affected plugins
unconditionally, so conditional configs require a version that documents these
properties.

### Option mechanism

Each `opt "key" "value"` child becomes:

```
tmux set -g @{opt-prefix}{key} "{value}"
```

## Commands

```
tmup init               # Startup path: install missing, apply opts, load plugins
tmup sync [id]          # Reconcile config into tmup.lock and plugin dirs
tmup install [id]       # Install missing remote plugins
tmup update [id]        # Advance unchanged floating selectors after sync
tmup restore [id]       # Restore to lock-recorded commits
tmup clean              # Remove undeclared managed remote repos
tmup list [-v]          # Print plugin status table (`-v` for diagnostic columns)
```

Set `TMUP_CONFIG_MODE=mixed` to enable mixed loading with TPM-style
declarations for any command that reads config.

### `init` — startup path

Designed for `run-shell "tmup init"` in `.tmux.conf`.

1. **Acquire global lock** — hold it through final condition resolution, sync,
   and plugin loading so concurrent writers cannot modify state mid-init.
2. **Resolve conditions** — reread the inherited config and freeze one
   authoritative Effective Plugin Specification and Load Eligibility snapshot.
3. **Implicit sync** — reconcile `tmup.kdl` into `tmup.lock` before any
   mutating work. Existing declared plugins may be repaired; removed plugins
   drop lock entries immediately.
4. **Respect init policy** — newly declared remote plugins are installed only
   when `auto-install=true`. Use `tmup clean` to remove undeclared plugin
   directories.
5. **Load tmux state** — for plugins with Load Eligibility, set options and
   source `*.tmux` files after sync.

`init` never advances floating selectors beyond what config declares. Known
build failures are still recorded and surfaced, but because startup begins with
implicit `sync`, `init` may re-surface the same failure before the later
install-path suppression check runs.

### `sync` — reconcile config into the lock snapshot

`sync [id]` resolves remote plugins from `tmup.kdl`, updates `tmup.lock`,
and applies only the changed plugin directories.

- Changing `branch`, `tag`, `commit`, source URL, or `build` is handled by `sync`.
- Removed remote plugins drop their lock entries immediately.
- `sync` does not delete undeclared plugin directories; `clean`
  only removes undeclared remote directories that still look like
  tmup-managed git repos.
- Mutating commands run this same sync engine first and abort if it fails.

### `update` — advance floating selectors

`update` runs after implicit sync, so selector and build changes are already
applied. Its job is only to advance unchanged floating selectors.

| Tracking | Behavior |
|----------|----------|
| branch / default | Fetch and advance to latest remote commit |
| `tag="..."` | Skip, report `pinned-tag` |
| `commit="..."` | Skip, report `pinned-commit` |

### `list` — status overview

Outputs a table with separated **state** and **last-result** columns:

| State | Meaning |
|-------|---------|
| `installed` | Plugin present and matches lock |
| `missing` | Declared but not on disk |
| `outdated` | Installed but HEAD differs from lock |
| `broken` | Directory exists but is not a valid git repo or HEAD is unreadable |
| `pinned-tag` | Installed, pinned to a tag |
| `pinned-commit` | Installed, pinned to a commit |

| Last Result | Meaning |
|-------------|---------|
| `ok` | Last operation succeeded |
| `build-failed` | Build command failed (marker recorded) |
| `none` | No operation attempted yet |

The `Load` column reports `yes` or `no` independently from the state, build,
and lock columns. `no` means the plugin's current Load Condition prevents a
future Init Session from loading it; it does not mean earlier tmux effects have
been unloaded.

If the lock snapshot is stale relative to the effective configuration for the
selected mode, `list` prints a warning before the table without mutating
`tmup.lock` or plugin state.

## Directory Layout

Default layout when using `~/.config/tmux/tmup.kdl`:

```
~/.config/tmux/
  ├── tmup.kdl                          # configuration
  └── tmup.lock                         # resolved snapshot

~/.local/share/tmup/
  ├── plugins/                          # installed plugins
  │   ├── github.com/tmux-plugins/tmux-sensible/
  │   ├── github.com/catppuccin/tmux/
  │   └── gitlab.com/user/plugin/
  ├── .staging/                         # in-progress installs
  └── .backup/                          # rollback during publish

~/.local/state/tmup/
  ├── operations.lock                   # cross-process mutex
  └── failures/                         # build failure markers
```

Managed scope note: tmup only reconciles and cleans remote plugin
directories it manages under `~/.local/share/tmup/plugins/`. Cleanup is
defined only for undeclared remote directories that it still recognizes as
managed git repos (currently, paths in that tree that still contain a `.git`
directory). Manually cloned repos, ad-hoc edits inside that tree, and
symlink-based layouts there are outside the current support contract.

Plugin directories use the full `host/owner/repo` path (like Go modules) to
avoid basename collisions between `user1/tmux-foo` and `user2/tmux-foo`.

## TPM Compatibility

tmup is compatible with the majority of TPM plugins — specifically those
that work through:

- `tmux set -g @...` options
- `*.tmux` entry scripts
- `TMUX_PLUGIN_MANAGER_PATH` environment variable

### Not compatible

Plugins that depend on TPM internals will **not** work:

- Assuming `TMUX_PLUGIN_MANAGER_PATH` has a flat `plugin-name/` layout
- Calling TPM's internal shell helpers
- Detecting the TPM repo at `~/.tmux/plugins/tpm/`

This boundary is intentional, not an oversight.

## Migrating from TPM

1. Replace the TPM `run` line in `.tmux.conf` with
   `run-shell "TMUP_CONFIG_MODE=mixed tmup init"`.
2. Restart tmux. tmup will read both `tmup.kdl` and existing TPM-style
   `set -g @plugin` declarations, then generate `tmup.lock`.
3. Move plugins gradually from `.tmux.conf` into `~/.config/tmux/tmup.kdl`.
4. Once migration is complete, switch back to plain `run-shell "tmup init"`.
5. Commit `tmup.kdl` to your dotfiles repo.
6. Remove the old `~/.tmux/plugins/` directory when satisfied.

## Roadmap

- [x] **Concurrent operations** — parallel git clone/fetch across plugins (`concurrency` config option)
- [x] **Conditional plugins** — host-specific managed state and independent Load Eligibility
- [ ] **Incremental fetch** — reuse healthy local repos (fetch + resolve) instead of fresh clone on every sync/install

## License

MIT
