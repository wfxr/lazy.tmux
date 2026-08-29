# Use tmup commands

This reference explains how each command interacts with configuration, the lock snapshot, managed plugin directories, and the running tmux server. It also documents status output, canonical target IDs, logs, and cleanup boundaries.

Run `tmup --help` or `tmup <command> --help` for the concise CLI syntax.

## Lifecycle model

`tmup.kdl` describes desired plugin state, and `tmup.lock` records the exact remote revisions resolved from it. Mutating commands reconcile the effective configuration into the lock snapshot before follow-up work that depends on remote state.

The common lifecycle is:

1. Run `tmup sync` after changing a remote source, selector, build command, or enabled plugin set. `init` and other mutating commands also perform this reconciliation implicitly.
2. Run `tmup update` when you want unchanged default-branch or named-branch declarations to advance to newer remote commits.
3. Run `tmup restore` when an installed checkout has drifted and you want the revision currently recorded in `tmup.lock`.

Remote preparation and checkout work runs concurrently up to the configured `concurrency`; final publication happens in declaration order. Local plugins don't participate in remote lifecycle work.

## Target one plugin

`install`, `sync`, `update`, and `restore` accept one optional remote plugin ID. The selector is the canonical remote identity, not the source spelling or display name.

Without an ID, these commands reconcile every enabled remote plugin and prune stale lock entries where the command permits it. With an ID, the reconcile and follow-up operation process only that canonical plugin. Targeted commands ignore failures from unrelated remotes and don't prune their stale lock entries. The complete native configuration is still validated before work begins.

Use verbose list output to find the ID:

```sh
tmup list -v
tmup update github.com/tmux-plugins/tmux-sensible
```

GitHub shorthand `tmux-plugins/tmux-sensible` normalizes to `github.com/tmux-plugins/tmux-sensible`. HTTP(S) and scp-style SSH sources use their normalized `host/path` identity. The same ID is used for lock keys, repository caches, installation paths, and targeted selectors.

Local plugins can't be targeted by lifecycle commands. A `name` property is display-only and isn't accepted as a selector.

## `init`

`tmup init` is the startup command for `run-shell "tmup init"` in a tmux configuration. It reconciles managed state, then applies runtime declarations and loads eligible plugins.

An Init Session performs these phases:

1. Wait for the global operation lock, acquire it, and hold it through plugin loading.
2. Read and validate the complete configuration, evaluate enable and load conditions, select runtime branches, and freeze one result snapshot.
3. Reconcile remote declarations into `tmup.lock`. It repairs missing or drifted declared plugins but never advances an unchanged floating selector.
4. Install newly declared remote plugins when `auto-install` is `#true`.
5. Apply environment operations and options, load sorted `*.tmux` scripts, and register explicit bindings for load-eligible plugins.

When `auto-install` is `#false`, `init` doesn't install newly missing plugins. Run `tmup install` explicitly, or change the option and run `init` again.

If reconciliation fails for one plugin, that plugin is omitted from the current load and independent plugins continue. A preparation or staged build failure leaves an older working checkout and lock entry in place. An exact build failure already recorded for the same plugin, commit, and build command isn't retried during `init`; use an explicit `tmup sync [id]` to retry it.

If tmux rejects one plugin's runtime command, tmup skips that plugin's remaining commands in the current and later phases. Other plugins continue. Runtime loading isn't transactional, so commands applied before a later failure remain active in the tmux server.

## `install`

`tmup install [id]` ensures missing declared remote plugins are present. It first reconciles its selected scope using the same rules as `sync`, including selector and build changes, and then installs any remaining missing plugin. Without an ID, that scope contains every enabled remote plugin; with an ID, it contains only that canonical plugin.

```sh
tmup install
tmup install github.com/catppuccin/tmux
```

The command doesn't advance an unchanged floating selector merely because a newer remote commit exists. Use `update` for that operation.

## `sync`

`tmup sync` makes lock metadata and managed checkouts agree with the complete effective configuration. `tmup sync [id]` reconciles only the selected canonical plugin and leaves unrelated lock entries unchanged. Use the matching scope after editing one or more remote declarations.

Within the selected scope, sync handles these changes:

- Add a remote plugin and resolve its selected revision.
- Change a source, `branch`, `tag`, `commit`, or `build` property.
- Repair a missing, broken, or revision-drifted declared checkout.
- Remove lock entries for remote plugins no longer in the effective configuration when the command has no `[id]`.

Sync doesn't advance an unchanged branch or default-branch selector. It also doesn't delete an undeclared checkout from the managed plugin root; run `clean` after reviewing that boundary.

Successful remote revisions are prepared and, when configured, built in staging. A failed preparation or build doesn't replace an existing installation or its lock entry.

## `update`

`tmup update [id]` first performs configuration reconciliation, then fetches and advances remote plugins with floating selectors.

Tracking behavior is:

| Selector | Update behavior |
|----------|-----------------|
| Default branch | Fetch and advance to the latest remote commit |
| `branch="..."` | Fetch and advance the named branch |
| `tag="..."` | Skip with a pinned-tag outcome |
| `commit="..."` | Skip with a pinned-commit outcome |

Each candidate is prepared and built in staging before publication. A preparation or build failure preserves the previously installed and locked revision for that plugin. Other plugins can still update, and any partial failure makes the command exit non-zero.

## `restore`

`tmup restore [id]` first reconciles configuration, then prepares each selected remote plugin at the commit currently recorded in `tmup.lock`. Use it to repair an installed checkout that is missing, broken, or at another revision.

```sh
tmup restore
tmup restore github.com/tmux-plugins/tmux-yank
```

Restore runs the configured remote build command in staging. A failed build doesn't replace an existing working checkout or change its lock entry.

Because restore reconciles configuration first, review `tmup.kdl` and the lock snapshot together when you bring a lock file from another environment. A configuration change can legitimately update lock metadata before the restore phase.

## `clean`

`tmup clean` reconciles removal from the lock snapshot, then deletes undeclared remote checkouts that it still recognizes as tmup-managed under the managed plugin root.

<!-- prettier-ignore -->
> [!CAUTION]
> `clean` is destructive inside the managed plugin root. It recognizes an
> undeclared path as managed when it appears in the canonical directory tree
> and still contains a `.git` entry. Review manual edits, clones, and symlinks
> under that root before running the command.

Clean doesn't delete declared plugins, local plugin source paths, repository caches, or arbitrary files outside the managed plugin root. It isn't a general filesystem repair tool. Out-of-band edits inside the managed root remain outside tmup's supported state contract.

Disabling a previously enabled plugin removes it from the effective configuration. `sync` drops its lock entry, and a later `clean` can remove its managed checkout.

## `list`

`tmup list` is read-only. It validates configuration, evaluates enable and load conditions, reads the current lock and filesystem state, and prints one row for each plugin in the effective specification. It doesn't create a missing config or rewrite a stale lock.

The default columns are exactly `Plugin`, `Kind`, `State`, `Build`, `Load`, and `Lock`:

| Column | Meaning |
|--------|---------|
| `Plugin` | Declared remote source or expanded local path |
| `Kind` | `remote` or `local` |
| `State` | Current availability relative to disk and lock state |
| `Build` | Last known remote build status: `success`, `build-failed`, or `-` |
| `Load` | `yes` or `no` for a future Init Session under the current condition result |
| `Lock` | `synced`, `mismatch`, or `-` for the current and recorded revisions |

The `State` column can contain:

| State | Meaning |
|-------|---------|
| `installed` | A healthy floating remote checkout has no detected lock mismatch |
| `missing` | The declared remote or local plugin isn't present on disk |
| `outdated` | A remote checkout exists, but its HEAD differs from the lock revision |
| `broken` | The path exists but isn't a usable repository or local directory |
| `pinned-tag` | A healthy remote checkout uses a tag selector |
| `pinned-commit` | A healthy remote checkout uses a commit selector |
| `local` | The local plugin directory exists |

`Build` is independent from `State`. It is `success` when a configured remote build has no uncleared failure marker and the checkout is healthy, `build-failed` when a matching failure marker exists, and `-` when no successful build status applies. Local plugins always use `-`.

`Load=no` means the current `cond` result would prevent a future `init` from loading the plugin. It doesn't mean a previous Init Session has undone options, environment values, scripts, or bindings.

If the lock fingerprint doesn't match the effective configuration, `list` prints a warning before the table. Run `tmup sync` with the same `TMUP_CONFIG_MODE` to reconcile it.

### Verbose status

`tmup list -v` replaces the compact view with diagnostic columns. Use it to copy canonical IDs and compare installed and expected commits.

The verbose columns are exactly `Id`, `Name`, `Kind`, `State`, `Build`, `Load`, `Current`, `Expected`, and `Source`. `Current` and `Expected` show shortened commit hashes when available.

## Configuration modes

Commands use native `tmup.kdl` in `pure` mode by default. Set `TMUP_CONFIG_MODE=mixed` to merge supported TPM-style declarations from the discovered tmux configuration.

Use the same mode whenever you inspect and mutate one setup:

```sh
TMUP_CONFIG_MODE=mixed tmup list -v
TMUP_CONFIG_MODE=mixed tmup sync
```

Running `list` in one mode and `sync` in another can produce a legitimate stale lock warning because the effective remote plugin sets differ. See the [mixed-mode configuration reference](configuration.md#mixed-tpm-mode) for discovery and merge rules.

## Exit status and partial failures

Every command returns zero only when the requested operation succeeds. Config, lock, global operation, and path failures stop the command. Per-plugin failures are collected where safe so independent plugins can continue.

For lifecycle commands, successful plugins can be published and written to the lock even when another plugin fails. The final exit status remains non-zero. For `init`, independent plugins can still load after another plugin is omitted. This behavior makes failures visible to scripts without discarding safe work.

Mutating commands use a cross-process operation lock. Standalone `install`, `sync`, `update`, `restore`, and `clean` fail immediately with a non-zero status when another operation holds the lock. `init` waits for the lock, then holds it through runtime loading. Read-only `list` doesn't acquire it.

## Files and directories

tmup follows the XDG base directories and separates configuration, managed data, and state. These are the defaults when the XDG environment variables are unset:

```text
~/.config/tmux/
  tmup.kdl                    # native configuration
  tmup.lock                   # resolved remote revision snapshot

~/.local/share/tmup/
  plugins/                    # managed remote checkouts
    github.com/owner/repo/
  .repos/                     # persistent bare remote caches
    github.com/owner/repo.git/
  .staging/                   # temporary prepared checkouts

~/.local/state/tmup/
  operations.lock            # cross-process mutation lock
  failures/                   # remote build failure markers
  logs/                       # detailed failure logs
```

`XDG_CONFIG_HOME`, `XDG_DATA_HOME`, and `XDG_STATE_HOME` replace the three base directories. `TMUP_CONFIG` replaces only the active config path; `tmup.lock` moves beside that file, while data and state continue to use their XDG roots.

The `.repos/` cache and `plugins/` installation use the same canonical remote ID. `.staging/` contains per-operation checkouts and isn't a backup area.

## Failure logs and markers

`init`, `install`, `sync`, `update`, and `restore` print progress summaries to standard error. When a failure has detailed output, tmup lazily writes a log under `${XDG_STATE_HOME:-$HOME/.local/state}/tmup/logs/` and reports its path in the summary.

Log names contain the Unix timestamp, process ID, and command, for example:

```text
1788000000-12345-update.log
```

The log records canonical plugin IDs, display names, failure stages, summaries, and available Git or build details. Treat logs as diagnostic data; they can include command text, paths, remote URLs, and stderr produced by plugins.

Remote build failures also create markers under `failures/`. A later successful publish clears the plugin's markers. The marker lets `list` show `build-failed` and prevents `init` from repeating an identical known failure on every tmux startup. Explicit lifecycle commands can retry the work.

## Troubleshooting

Start with the command's final summary and any detailed log path it prints. These checks cover the most common configuration and lifecycle failures.

### tmux can't find `tmup`

The tmux server can have an older or narrower `PATH` than your interactive shell. Confirm the binary, then use an absolute startup path if needed:

```sh
command -v tmup
tmup --version
```

```tmux
run-shell "/absolute/path/to/tmup init"
```

Restart the tmux server after changing the environment it inherits.

### tmup reads the wrong configuration

Confirm the file that contains `run-shell`, the XDG configuration home visible to the tmux server, and any `TMUP_CONFIG` override. An explicit override must name an existing file.

For mixed mode, also confirm that `TMUP_CONFIG_MODE=mixed` is present on both startup and diagnostic commands. Use `tmup list -v` in that same mode to see the merged result.

### `list` reports stale lock metadata

The effective remote declarations differ from the lock fingerprint. Run sync with the same environment and mode used by `list`:

```sh
tmup sync
```

```sh
TMUP_CONFIG_MODE=mixed tmup sync
```

Don't delete a lock file to hide a parse or version error. Lock corruption is a hard error because silently resetting it would discard known revision state.

### A build stays `build-failed`

Read the referenced detail log, fix the build command or its dependencies, and retry the canonical plugin ID explicitly:

```sh
tmup sync github.com/example/tmux-clock
tmup list -v
```

An unchanged failure marker intentionally suppresses automatic retries during `init`, but an explicit sync retries it. A successful staged publish clears the marker.

### A condition has an unexpected result

Shell predicates use the tmup process environment and the configuration directory as their working directory. Run the predicate through `/bin/sh` in a matching environment, and remember that a long-lived tmux server can have different variables from a new interactive shell.

`Load=no` predicts the next Init Session. It doesn't remove runtime effects from a previous load. Restart the tmux server or reverse those effects explicitly.

### A command reports several plugin failures

tmup continues independent plugin work where it is safe, then returns non-zero. Review every failed plugin in the summary and detail log; don't assume the first reported failure is the only one. Run `tmup list -v` afterward to distinguish published successes, preserved revisions, missing plugins, and build markers.

## Next steps

See the [configuration reference](configuration.md) for native grammar, conditions, and runtime declarations. The [design invariants](design.md) define the durable consistency and managed-state contract behind these commands.
