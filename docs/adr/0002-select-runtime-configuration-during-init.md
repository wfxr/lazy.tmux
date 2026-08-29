---
status: accepted
---

# Select runtime configuration during init

tmup can conditionally include or load a plugin as a whole, but an enabled and load-eligible plugin also needs to select different runtime options, environment operations, or bindings for different Execution Hosts. Runtime Configuration Branches keep a plugin's source and configuration together while selecting runtime declarations through mutually exclusive branches during an Init Session.

## Decision

A remote or local plugin may contain `if` nodes with optional, immediately following `else` nodes. Each `if` accepts exactly one boolean or non-empty `/bin/sh` predicate string and accepts no properties or KDL type annotations. An `else` accepts no arguments or properties. Comments may appear between the paired nodes, but any orphaned, repeated, or non-adjacent `else` is invalid.

```kdl
plug "wfxr/tmux-fzf" {
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

Plugin declarations may mix unconditional runtime declarations with multiple independent Runtime Configuration Branches. Branches may nest, with nesting serving the role of `else if`; no separate `else if` syntax is introduced. Empty branches are valid. Branches form a closed configuration language containing only `opt`, `env`, `unset-env`, `bind`, and nested `if` nodes. Managed-state declarations such as `build`, plugin-level conditions, plugin declarations, and unknown nodes are hard errors inside a branch. Applying KDL slashdash to an `if` does not remove its paired `else`; users must slashdash both nodes or leave a valid orphan-free structure.

Every command that reads `tmup.kdl` parses and statically validates the complete branch tree, including unselected branches. Only `init` evaluates branch predicates. It evaluates Enable Conditions first, then Load Conditions for enabled plugins, then Runtime Configuration Branches for plugins with Load Eligibility. Independent and reachable predicates run sequentially in declaration order, while nested branches short-circuit. One Init Session freezes all selected branches before managed-state mutation. A predicate must not depend on plugin files that the same Init Session may install or change.

Runtime Configuration Branch predicates reuse the existing shell condition contract from [ADR 0001](0001-separate-plugin-inclusion-from-loading.md): they inherit the tmup process environment, use the active configuration directory as their working directory, discard ordinary output, and time out after five seconds. Exit status zero selects the `if` branch and any normal non-zero status selects `else`. Failure to start or monitor `/bin/sh`, signal termination, or timeout aborts `init` before managed-state mutation. Plugin `env` declarations affect the tmux server environment later in loading and cannot affect predicate evaluation.

After selection, tmup flattens unconditional and selected runtime declarations at their source positions. The result follows the existing global loading phases: environment operations and options for all eligible plugins, then plugin scripts, then explicit bindings. Declaration order remains authoritative within each phase, and tmux last-write-wins behavior continues to resolve repeated options or bindings. A plugin-attributable runtime command failure retains the existing behavior of skipping that plugin's remaining commands, continuing independent plugins, and producing a non-zero final outcome.

Runtime Configuration Branches affect neither the Effective Plugin Specification nor Load Eligibility. Their predicates and contents do not affect managed state, lock membership, or lock fingerprints. In mixed TPM mode, branches belong only to the selected KDL declaration after the existing merge. `list` does not evaluate or display branch selection.

Branch selection is server-global and scoped to the Execution Host, not to an attached client, session, or pane. Reapplying configuration is not runtime reconciliation: when a later Init Session selects a different branch, tmup does not undo options, environment effects, or bindings applied by an earlier branch. Users must explicitly clean up those effects or restart the tmux server. Environment values such as `SSH_CLIENT` may be stale in a long-lived tmux server and must not be interpreted as the identity of the currently attaching client.

## Considered options

The alternatives either failed to provide true branch semantics or crossed tmup's existing state boundaries.

- Conditions on individual runtime nodes would require users to duplicate and invert predicates. Separate evaluations could select both alternatives or neither, so they do not model `if` and `else`.
- tmux `if-shell` would defer selection to tmux while options and bindings remain server-global, incorrectly suggesting client-specific isolation or unloading guarantees.
- Reusing the `cond { ... }` child form would overload Load Condition terminology and conflate whole-plugin Load Eligibility with runtime declaration selection. The form remains reserved for a future structured condition language.
- Automatic cleanup would require tmup to persist ownership of runtime tmux state and safely distinguish its effects from later overrides. That is a separate runtime reconciliation feature.
- Conditional `build` declarations or arbitrary tmux commands would mix managed-state decisions or an open-ended execution language into a runtime-only branch.

## Consequences

The configuration parser retains an unresolved tree of runtime declarations until `init` resolves conditions, then flattens the selected declarations into the existing plugin specification and load plan. The loader and tmux command layer do not need conditional commands or `tmux if-shell` support.

Runtime Configuration Branches are planned for their first stable release in tmup 0.2.0. Earlier versions may warn and ignore `if` and `else` plugin children, loading the plugin without their runtime declarations.
