# Configure tmup

This reference defines the native `tmup.kdl` grammar and how tmup turns it into managed plugin state and tmux runtime configuration. Native configuration is strict: tmup validates the complete document before it evaluates predicates or changes any managed or runtime state.

tmup uses [KDL v2](https://kdl.dev). This document describes the supported subset rather than the full KDL language.

## Configuration and lock paths

The default configuration is `tmux/tmup.kdl` under the XDG configuration home. With the default XDG directories, the path is `~/.config/tmux/tmup.kdl`.

Set `TMUP_CONFIG` to use another file:

```sh
TMUP_CONFIG=/path/to/workstation.kdl tmup sync
```

An explicit `TMUP_CONFIG` must point to an existing file. Without the override, mutating commands and `init` create the default config when it doesn't exist; read-only `list` uses an in-memory default and doesn't create the file.

`tmup.lock` always lives next to the active `tmup.kdl`, including when `TMUP_CONFIG` selects another directory.

## Document grammar

The document root accepts at most one `options` node and any number of `plug` nodes, in any order:

```kdl
options {
    auto-install #true
    concurrency 16
}

plug "tmux-plugins/tmux-sensible"
plug "tmux-plugins/tmux-yank"
```

The grammar is closed and fail-fast. Unknown nodes or properties, extra arguments, duplicate scalar declarations, unsupported child blocks, and KDL type annotations are errors. tmup validates declarations in branches that the current run won't select.

KDL comments and slashdash work normally. For example, this declaration is disabled before tmup parses the document structure:

```kdl
/-plug "tmux-plugins/tmux-continuum"
```

## Global options

The `options` node requires a child block and accepts no arguments or properties. Each option can appear once; omitted options keep their defaults.

| Option | Type | Default | Meaning |
|--------|------|---------|---------|
| `auto-install` | bool | `#true` | Install missing remote plugins during `init` |
| `concurrency` | integer | `16` | Maximum concurrent remote preparation jobs; `1` makes preparation serial |

For example, disable startup installation and limit concurrent work:

```kdl
options {
    auto-install #false
    concurrency 4
}
```

## Plugin sources

Every `plug` node takes exactly one non-empty source string. A source is either a remote Git repository or a local directory marked with `local=#true`.

### Remote sources

tmup accepts these remote source forms:

- GitHub shorthand with exactly `owner/repo`, such as `tmux-plugins/tmux-sensible`.
- An HTTP or HTTPS URL, such as `https://gitlab.com/example/tmux-status.git`.
- An scp-style SSH source, such as `git@gitlab.com:example/tmux-status.git`.

tmup derives one canonical remote ID from the source. GitHub shorthand becomes `github.com/owner/repo`; URL schemes, a trailing slash, and a final `.git` don't become part of the identity. The canonical ID is the lock key, managed install path, repository-cache path, and targeted command selector.

For example, these declarations refer to the same canonical remote plugin and can't both appear in one effective configuration:

```kdl
plug "example/tmux-clock"
plug "https://github.com/example/tmux-clock.git"
```

Generic `ssh://`, `git://`, filesystem Git URLs, and remote strings outside the three documented forms aren't part of the accepted syntax.

### Local sources

A local plugin must set `local=#true`. tmup expands `~`, `$NAME`, and `${NAME}` in the source, then requires the result to be an absolute path:

```kdl
plug "~/dev/tmux-clock" local=#true name="tmux-clock-dev"
```

tmup loads a local plugin in place. It doesn't clone, install, update, restore, lock, or run a `build` command for local plugins. Tracking selectors aren't valid on local plugins.

## Plugin properties

Properties describe tracking, display, build, and conditional behavior. Each known property can appear at most once, and unknown properties are errors.

| Property | Type | Default | Meaning |
|----------|------|---------|---------|
| `name` | non-empty string | Source basename | Display name in progress output |
| `opt-prefix` | string | `""` | Prefix added to every `opt` key |
| `branch` | non-empty string | None | Follow a named branch |
| `tag` | non-empty string | None | Pin to a tag |
| `commit` | non-empty string | None | Pin to a commit |
| `local` | bool | `#false` | Interpret the source as a local path |
| `build` | non-empty string | None | Build a remote plugin in staging before publication |
| `enabled` | bool or non-empty string | `#true` | Include the plugin in managed state on this host |
| `cond` | bool or non-empty string | `#true` | Load an enabled plugin during `init` |

`branch`, `tag`, and `commit` are mutually exclusive. A plugin without one of these selectors follows its remote's default branch. `update` advances default and named branches, but skips tags and commits.

`name` changes presentation only. It isn't a lock identity, install path, or valid targeted command selector.

## Remote build commands

The `build` property defines a shell command for a remote plugin. tmup runs it from the staged plugin checkout before publishing that checkout into the managed plugin root:

```kdl
plug "example/tmux-clock" build="make release"
```

Remote reconciliation can run the build during `init`, `install`, `sync`, `update`, and `restore`. Changing only the build string makes the plugin eligible for reconciliation even when its selected commit hasn't changed. `list` and `clean` never build plugins, and local plugin builds never run.

The command requires `/bin/sh` and inherits the tmup process environment. If the command fails, tmup removes the staging checkout and preserves an existing installed revision and lock entry. It records the failed plugin, commit, and build-command hash so `list` can report `build-failed`. During `init`, an exact known failure isn't retried and that plugin is omitted from the current load; changing the commit or command makes the build eligible again.

## Runtime child nodes

A `plug` child block describes tmux configuration associated with that plugin. Remote and local plugins support the same runtime nodes.

| Child | Arguments | Meaning |
|-------|-----------|---------|
| `opt` | Key string, value string | Set `@{opt-prefix}{key}` as a tmux global option |
| `env` | Non-empty name string, value string | Set a tmux global environment value |
| `unset-env` | Non-empty name string | Remove a tmux global environment value |
| `bind` | Non-empty key string, child block | Register a plugin-scoped shell binding |
| `if` | Bool or non-empty predicate, child block | Select runtime declarations during `init` |
| `else` | Child block | Provide the fallback immediately after an `if` node |

`build` is a property, not a child node. Runtime declarations are replayed only by `init`; `sync` and `update` don't change the running tmux server.

An Init Session resolves eligible declarations first, then uses these global phases:

1. Apply environment operations and options for all eligible plugins in declaration order.
2. Run each eligible plugin's sorted `*.tmux` scripts in plugin order.
3. Register explicit bindings in plugin and node declaration order.

If one plugin's tmux command fails, tmup skips its remaining commands in the current and later phases. Independent plugins continue, and the final command status is non-zero. tmup doesn't reverse runtime effects that completed before a later failure.

## Plugin options

An `opt` node sets one tmux global option. `opt-prefix` is prepended to the key, so this configuration sets `@catppuccin_flavor` and `@catppuccin_window_text`:

```kdl
plug "catppuccin/tmux" opt-prefix="catppuccin_" {
    opt "flavor" "mocha"
    opt "window_text" "#W"
}
```

Keys must contain a non-whitespace character. Values and `opt-prefix` can be empty strings. Options are applied before plugin scripts run.

## Environment operations

`env` and `unset-env` edit the tmux server's global environment before plugin scripts run. They retain declaration order, so repeated names follow tmux's normal last-write-wins behavior.

```kdl
plug "wfxr/tmux-fzf" {
    env "TMUX_FZF_LAUNCH_KEY" "C-f"
    env "TMUX_FZF_OPTIONS" "-p -w 62% -h 38%"
    unset-env "OLD_TMUX_FZF_OPTION"
}
```

Environment values are literal. tmup doesn't expand shell syntax, `~`, process environment variables, or plugin-directory placeholders in them. No name is reserved, so a plugin can overwrite or unset `TMUX_PLUGIN_MANAGER_PATH` and affect plugins loaded after it. Environment names must be non-empty; values can be empty.

Removing an `env` declaration doesn't remove a value set by an earlier `init`. Add `unset-env` or restart the tmux server when you need to clear it.

## Plugin-scoped bindings

Each `bind` requires exactly one `shell` child and accepts at most one `options` child. The following example passes three separate option tokens to `tmux bind-key` and runs the action in the background:

```kdl
plug "wfxr/tmux-fzf" {
    bind "C-w" {
        options "-n" "-r" "-T" "root"
        shell "scripts/session.sh attach" background=#true
    }
}
```

Each `options` string becomes one unchanged `bind-key` argument before the key. tmup doesn't split strings or validate them against the installed tmux version. Use separate strings for separate tokens, such as `options "-T" "root"`. When present, the node requires at least one non-empty option string. Binding keys and shell commands must also be non-empty, and `background` must be a bool.

The `shell` action runs through `/bin/sh` from the resolved plugin directory when you press the key. Remote plugins use their canonical managed path; local plugins use the expanded local path. Shell operators, quotes, and variables are preserved until tmux runs the action. `background` defaults to `#false`; set it to `#true` to give the nested `run-shell` action tmux's `-b` option.

tmup doesn't detect duplicate bindings or automatically unbind a removed declaration. Tmux's normal last-write-wins behavior applies. Restart the tmux server or clean up the binding explicitly when a new `init` no longer declares it.

## Enable and load conditions

Conditions separate whether tmup manages a plugin from whether an Init Session loads it. Both `enabled` and `cond` accept a KDL bool or a non-empty shell predicate string and default to true.

```kdl
// Exclude this plugin from managed state on other hosts.
plug "company/workstation-tools" enabled=#"test "$(hostname)" = workstation"#

// Keep this plugin managed, but load it only in SSH environments.
plug "company/remote-tools" cond=#"[ -n "$SSH_CLIENT" ]"#
```

When `enabled` is false, the plugin doesn't participate in lifecycle commands, `list`, targeted selectors, or the lock snapshot. If a previously enabled remote plugin becomes disabled, `sync` removes its lock entry; a later `clean` can remove its managed checkout.

When `cond` is false, tmup still installs, builds, updates, restores, and locks the plugin. `init` skips its environment operations, options, scripts, and bindings. Changing `cond` to false doesn't undo effects from an earlier load.

String predicates run through `/bin/sh -c`, inherit the tmup process environment, use the configuration directory as their working directory, and time out after five seconds. Exit status zero is true; any other normal exit status is false. tmup discards predicate output. A failure to start the shell, a signal, or a timeout is a hard error.

Every command evaluates `enabled`. Only `init` and `list` evaluate `cond`; `list` reports whether a future Init Session would load the plugin. Each command freezes one condition result snapshot before it changes managed state.

Use fast, side-effect-free predicates based on stable host properties. A long-lived tmux server can inherit different values, such as `SSH_CLIENT`, from your current interactive shell.

## Runtime configuration branches

Runtime branches select options, environment operations, and bindings for the current tmux server without changing managed or locked plugin state. A plugin can mix unconditional declarations with multiple `if` nodes, and branches can nest.

```kdl
plug "wfxr/tmux-fzf" {
    env "TMUX_FZF_OPTIONS" "-p -w 62% -h 38%"

    if #"[ -n "$SSH_CLIENT" ] || [ -f /.dockerenv ]"# {
        bind "M-w" {
            shell "scripts/session.sh attach" background=#true
        }
    }
    else {
        bind "C-w" {
            shell "scripts/session.sh attach" background=#true
        }
    }
}
```

An `else` must immediately follow its `if` node. A branch can contain only `opt`, `env`, `unset-env`, `bind`, and nested `if` nodes. Empty branches are valid. Unknown nodes, plugin declarations, build commands, and condition properties inside a branch are errors.

Only `init` evaluates branch predicates, after it has evaluated `enabled` and `cond`. It evaluates branches only for load-eligible plugins and freezes the selected declarations before managed-state mutation. Plugin `env` declarations are applied later and can't affect predicate selection.

Branch predicates have the same shell, working directory, timeout, and error behavior as plugin conditions. A later `init` that selects another branch doesn't undo earlier options, environment values, or bindings; clean them up explicitly or restart the tmux server.

## Lock fingerprints

The lock fingerprint detects changes to remote managed state without treating runtime-only edits as lifecycle changes. It covers the effective set of enabled remote plugins by canonical ID, tracking selector, and `build` string.

These changes affect the fingerprint and can trigger reconciliation:

- Adding or removing an enabled remote plugin.
- Changing its canonical remote source.
- Changing `branch`, `tag`, `commit`, or the default-branch selector.
- Changing its `build` command.
- Changing an `enabled` result so that the plugin enters or leaves the effective specification.

These changes don't affect the fingerprint:

- Comments, formatting, or declaration order.
- Equivalent remote spellings, such as a final `.git` versus no `.git`.
- `name`, `opt-prefix`, `opt`, `env`, `unset-env`, or `bind` changes.
- Local plugin declarations or changes.
- `cond` values, predicate text, current load eligibility, or runtime branch predicates and contents.
- Changing an `enabled` predicate while its result remains true.

`tmup list` warns when the lock fingerprint is stale but doesn't rewrite the lock file. Run `tmup sync` in the same configuration mode to reconcile it.

## Mixed TPM mode

Mixed mode is a supported migration bridge for configurations that still use TPM-style declarations. Enable it for any command that reads configuration:

```sh
TMUP_CONFIG_MODE=mixed tmup list
TMUP_CONFIG_MODE=mixed tmup sync
```

The only accepted values are `pure` and `mixed`; `pure` is the default. In mixed mode, tmup discovers a tmux configuration in this order:

1. `$XDG_CONFIG_HOME/tmux/tmux.conf`, when `XDG_CONFIG_HOME` is absolute.
2. `~/.config/tmux/tmux.conf`.
3. `~/.tmux.conf`.

The scanner recognizes TPM's `set -g @plugin ...` and `set-option -g @plugin ...` forms in the main tmux file and directly sourced files. It scans the main file first, appends direct `source` and `source-file` matches, and doesn't recursively scan files sourced by those files. It ignores unrelated tmux syntax. TPM declarations are unconditional remote plugins; the TPM `#branch` suffix becomes a branch selector.

Merged plugin order starts with TPM-discovered declarations, followed by KDL-only declarations in `tmup.kdl` order. If both sources declare the same canonical remote ID, the KDL declaration wins while keeping the TPM position. This lets a KDL `enabled=#false` suppress a matching TPM declaration.

Native KDL remains strict in mixed mode. The TPM scanner stays permissive because a tmux configuration contains syntax unrelated to tmup.

## Complete example

This example combines remote and local sources, tracking, a build command, runtime configuration, and host conditions. Use only the parts your setup needs.

```kdl
options {
    auto-install #true
    concurrency 16
}

plug "tmux-plugins/tmux-sensible"
plug "tmux-plugins/tmux-yank" tag="v2.3.0"
plug "example/tmux-clock" branch="main" build="make release"

plug "catppuccin/tmux" opt-prefix="catppuccin_" {
    opt "flavor" "mocha"
    opt "window_text" "#W"
}

plug "wfxr/tmux-fzf" cond=#"[ -n "$TMUX" ]"# {
    env "TMUX_FZF_LAUNCH_KEY" "C-f"
    bind "C-w" {
        options "-T" "root"
        shell "scripts/session.sh attach" background=#true
    }
}

plug "~/dev/my-tmux-plugin" local=#true name="my-plugin-dev"
```

## Next steps

Use the [command reference](commands.md) to choose between `sync`, `update`, and `restore`, and to understand `list` output. The [design invariants](design.md) are the source of truth for durable consistency, failure, and compatibility guarantees.
