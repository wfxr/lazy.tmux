<h1 align="center">tmup</h1>

<p align="center">
  A modern tmux plugin manager inspired by <a href="https://github.com/folke/lazy.nvim">lazy.nvim</a>.
</p>

<p align="center">
  <a href="https://github.com/wfxr/tmup/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/wfxr/tmup?sort=semver"></a>
  <a href="https://crates.io/crates/tmup"><img alt="crates.io version" src="https://img.shields.io/crates/v/tmup.svg"></a>
  <a href="https://github.com/wfxr/tmup/actions/workflows/native-artifacts.yml"><img alt="Native artifacts workflow" src="https://github.com/wfxr/tmup/actions/workflows/native-artifacts.yml/badge.svg?branch=main"></a>
  <a href="https://spdx.org/licenses/MIT.html"><img alt="MIT license" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
</p>

tmup removes the manual work from managing tmux plugins. Declare your plugins
once, add `tmup init` to your tmux configuration, and tmup installs anything
missing the first time tmux starts. Installs and updates prepare plugins
concurrently, while `tmup.lock` records the exact revisions selected for your
machine.

When a remote plugin has a build command, tmup runs it in staging before it
touches the installed copy. A failed preparation or build leaves the working
installation and its lock entry in place.

## Why tmup?

[TPM](https://github.com/tmux-plugins/tpm) established the tmux plugin
ecosystem and remains the format that many plugins target. tmup keeps that
common loading contract while adding a config-driven lifecycle:

- Missing plugins install automatically during `init` by default. You don't
  need to trigger a separate install command from tmux.
- Network and checkout work runs concurrently, with configurable concurrency.
- `tmup.lock` captures the resolved commit for each remote plugin.
- Remote revisions are prepared and checked out in staging, where any configured
  build runs before publication. A failed build doesn't replace an existing
  installation.
- Commands report partial plugin failures and return a non-zero exit status.

tmup also provides a mixed mode for using existing TPM-style `@plugin`
declarations while you migrate. Native `tmup.kdl` is the recommended format
for conditions, build commands, local plugins, and other tmup-specific
features.

## Requirements

tmup requires `tmux` and `git`. The remote installer also requires `curl` or
`wget`, `tar`, and either `sha256sum` or `shasum`. Configurations that use shell
predicates, remote build commands, or plugin-scoped bindings require `/bin/sh`.

Pre-built binaries are available for 64-bit Linux and macOS. Linux releases
use MUSL and work across common Linux distributions.

## Installation

Install the latest stable release with the repository-owned installer:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/wfxr/tmup/main/install.sh | sh
```

The installer selects your platform, verifies the archive against the
release's `SHA256SUMS`, and installs `tmup` to `~/.local/bin`. Add that
directory to `PATH` if it isn't already available to your shell and tmux
server.

If you have a Rust toolchain, you can install the crates.io release instead:

```sh
cargo install tmup --locked
```

See the [installation guide](docs/installation.md) for installer options,
supported targets, checksums, and GitHub artifact attestations.

## Quick start

Choose the new setup if you want to declare plugins in KDL. If your current
setup uses TPM, the migration path lets you keep its declarations and removes
the manual install step immediately.

### New setup

Follow these steps to declare and load your first plugins:

1. Create `tmup.kdl` in the tmux configuration directory:

   ```sh
   mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/tmux"
   cat > "${XDG_CONFIG_HOME:-$HOME/.config}/tmux/tmup.kdl" <<'EOF'
   plug "tmux-plugins/tmux-sensible"
   plug "tmux-plugins/tmux-yank"
   EOF
   ```

2. Add this line to the tmux configuration file that your server loads:

   ```tmux
   run-shell "tmup init"
   ```

3. Start a new tmux server, or reload the actual configuration file used by
   your server. For example:

   ```sh
   tmux source-file /path/to/tmux.conf
   ```

4. Confirm that tmup is available and the plugins are installed:

   ```sh
   tmup --version
   tmup list
   ```

The first `init` creates `tmup.lock` next to `tmup.kdl` and installs missing
remote plugins because `auto-install` defaults to `#true`.

### Migrating from TPM

Follow these steps to switch managers without rewriting every declaration at
once:

1. Keep your existing `set -g @plugin ...` declarations and plugin options,
   and replace TPM's final `run` line with:

   ```tmux
   run-shell "TMUP_CONFIG_MODE=mixed tmup init"
   ```

2. Restart tmux or reload the configuration file. On the first run, tmup scans
   the supported TPM declarations, creates a default `tmup.kdl` if needed, and
   installs the missing plugins without waiting for a manual install key
   binding.

3. Check the merged result, including canonical plugin IDs:

   ```sh
   TMUP_CONFIG_MODE=mixed tmup list -v
   ```

4. Move declarations into `tmup.kdl` gradually. If the same remote plugin
   appears in both sources, the KDL declaration wins while keeping the TPM
   position. After the migration, switch the startup line to plain
   `tmup init`.

Mixed mode supports the common TPM contract: global `@option` values,
`TMUX_PLUGIN_MANAGER_PATH`, and plugin `*.tmux` entry scripts. Plugins that
call TPM's internal shell helpers or assume its flat installation layout are
outside the compatibility target.

## Common configuration

tmup uses [KDL v2](https://kdl.dev). The following examples cover the settings
most users need; the [configuration reference](docs/configuration.md) documents
the complete grammar, conditions, environment operations, bindings, runtime
branches, and lock fingerprints.

Declare a GitHub repository by its owner and repository name:

```kdl
plug "tmux-plugins/tmux-sensible"
```

Follow a named branch or pin a release tag. `update` advances branches and
default branches, but skips tags and explicit commits:

```kdl
plug "catppuccin/tmux" branch="main"
plug "tmux-plugins/tmux-yank" tag="v2.3.0"
```

Set plugin options with `opt`. Use `opt-prefix` to avoid repeating the option
namespace:

```kdl
plug "catppuccin/tmux" opt-prefix="catppuccin_" {
    opt "flavor" "mocha"
    opt "window_text" "#W"
}
```

Load a local plugin in place by marking its source as local:

```kdl
plug "~/dev/my-tmux-plugin" local=#true name="my-plugin-dev"
```

Use `enabled` to control whether tmup manages a plugin on the current host, or
use `cond` to keep it managed but control whether `init` loads it:

```kdl
plug "company/workstation-tools" enabled="test \"$(hostname)\" = workstation"
plug "company/remote-tools" cond="[ -n \"$SSH_CLIENT\" ]"
```

## Lock snapshot

`tmup.lock` is the local snapshot of the exact revisions resolved from your
effective configuration. Mutating commands reconcile the configuration and
lock snapshot before follow-up work, while `restore` returns installed plugins
to the recorded commits.

You don't need to commit the lock file for everyday use. Keep, copy, or refer
to it when you need to reproduce a known setup or diagnose and recover a new
environment. A malformed or unsupported lock file is a hard error; tmup never
silently replaces it with an empty snapshot.

## Commands

The lifecycle commands share the same config-first workflow. `sync` applies
declaration and selector changes, `update` then advances unchanged floating
selectors, and `restore` returns plugins to lock-recorded commits.

| Command | Purpose |
|---------|---------|
| `tmup init` | Install missing plugins when enabled, apply runtime configuration, and load plugins |
| `tmup install [id]` | Install missing remote plugins after reconciling configuration |
| `tmup sync [id]` | Reconcile configuration, lock metadata, and declared remote plugins |
| `tmup update [id]` | Reconcile first, then advance default branches and named branches |
| `tmup restore [id]` | Reconcile first, then restore lock-recorded commits |
| `tmup clean` | Remove undeclared remote plugin checkouts recognized as tmup-managed |
| `tmup list [-v]` | Show plugin status; `-v` adds canonical IDs, revisions, and sources |

The optional `[id]` is the canonical remote plugin ID shown by
`tmup list -v`, such as `github.com/tmux-plugins/tmux-sensible`. A display
`name` is never a lifecycle selector.

`tmup clean` has a deliberately narrow but destructive boundary: it removes
undeclared remote repositories under tmup's managed plugin root when they are
still recognizable as Git repositories. It doesn't remove local plugin paths
or act as a general filesystem cleaner. Review the
[managed clean boundary](docs/commands.md#clean) before using it around
manually modified managed directories.

See the [command reference](docs/commands.md) for full command semantics,
status values, paths, logging, failure behavior, and troubleshooting.

## Directory layout

With the default XDG paths, tmup keeps configuration, managed data, and
diagnostics separate:

```text
~/.config/tmux/
  tmup.kdl
  tmup.lock

~/.local/share/tmup/
  plugins/      # installed remote plugins, keyed by canonical ID
  .repos/       # persistent bare repository caches
  .staging/     # checkouts being prepared or built

~/.local/state/tmup/
  operations.lock
  failures/     # remote build failure markers
  logs/         # detailed failure logs
```

Plugin paths include the full `host/owner/repo` identity, which prevents
repositories with the same basename from colliding.

## Troubleshooting

Most startup problems come from the tmux server seeing a different environment
or configuration path than your interactive shell. Start with these checks:

- Run `tmup --version` from a shell and make sure the tmux server can find the
  same executable through `PATH`. Use an absolute path in `run-shell` if needed.
- Confirm which tmux configuration file your server loads and whether
  `TMUP_CONFIG` points to the intended, existing `tmup.kdl`.
- Read the command's summary, then inspect detailed failure logs under
  `${XDG_STATE_HOME:-$HOME/.local/state}/tmup/logs/`.

The [troubleshooting guide](docs/commands.md#troubleshooting) covers stale
locks, failed builds, conditions, mixed mode, and non-zero exit statuses.

## Documentation

The guides separate everyday tasks from the project's precise behavioral
contract:

- [Installation](docs/installation.md) covers release channels, installer
  options, supported systems, checksums, and attestations.
- [Configuration](docs/configuration.md) defines the complete native KDL and
  mixed-mode configuration surface.
- [Commands](docs/commands.md) explains lifecycle workflows, status output,
  storage, failure handling, and cleanup.
- [Design invariants](docs/design.md) records the stable consistency and safety
  contract for maintainers and advanced users.

## Roadmap

Follow planned improvements and future work in
[GitHub Issues](https://github.com/wfxr/tmup/issues).

## License

tmup is available under the [MIT License](https://spdx.org/licenses/MIT.html).
