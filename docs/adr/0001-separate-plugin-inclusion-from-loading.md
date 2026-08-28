---
status: accepted
---

# Separate plugin inclusion from loading

tmup needs one configuration to adapt to different Execution Hosts without making tmux server-global plugin effects client-specific. We separate whether a plugin belongs to an Effective Plugin Specification from whether an Init Session may load it, preserving a clear lock-backed lifecycle while allowing host-specific activation.

> **Decision update:** [ADR 0003](0003-reject-unsupported-native-configuration-syntax.md) supersedes this decision's forward-compatibility warning for unknown native plugin parameters. Unsupported native KDL is now a hard configuration error.

## Decision

Plugin declarations have two independent, optional conditions. Both default to true, apply to remote and local plugins as a whole, and accept either a boolean or a non-empty `/bin/sh` predicate string.

- The Enable Condition, exposed as `enabled`, determines whether a declaration belongs to the current Execution Host's Effective Plugin Specification. A false result excludes the plugin from lifecycle commands, command-visible selectors, list output, and the lock snapshot. A later `clean` may remove a previously managed remote checkout that is no longer enabled.
- The Load Condition, exposed as `cond`, affects only Load Eligibility. A false result keeps the plugin managed, installed, built, updated, locked, and visible, while an Init Session skips its tmux options and `*.tmux` scripts.

Every command evaluates Enable Conditions from its own process environment. `init` and `list` also evaluate Load Conditions. An Init Session evaluates conditions before managed-state mutation, freezes one authoritative result snapshot for sync and loading, and does not reevaluate during that execution. Preview work may evaluate separately.

String predicates run sequentially in declaration order, inherit the tmup process environment, use the active configuration directory as their working directory, and have a five-second timeout. Exit status zero means true and every non-zero exit status means false. Failure to start `/bin/sh`, signal termination, or timeout is a hard command error; normal predicate output is discarded. Predicates must be fast, side-effect-free, and independent of managed state that the command may change.

Configuration structure is validated before conditions run. Duplicate canonical remote IDs remain invalid even when their Enable Conditions are mutually exclusive. Unknown plugin parameters produce warnings for forward compatibility, while duplicate known scalar fields and unsupported reserved condition forms are errors.

In mixed TPM mode, tmup merges TPM and KDL declarations before evaluating conditions. TPM-only declarations are unconditional. A KDL declaration still replaces the matching TPM declaration, and `enabled=#false` suppresses the merged plugin rather than falling back to TPM.

Condition values use scalar properties in the first release. The `enabled { ... }` and `cond { ... }` child forms and KDL type annotations are reserved for a future structured condition language and must not silently degrade to unconditional behavior.

## Considered options

The design keeps the useful distinction inspired by lazy.nvim without copying its lifecycle exactly. Treating both conditions as removal from the active specification would leave tmup without a way to suppress loading while retaining normal lock-backed management. A single condition would conflate those two intents.

Evaluating conditions through tmux `if-shell` was rejected because plugin options, key bindings, and hooks are normally server-global. Client-specific evaluation would imply isolation and unloading guarantees that tmup cannot provide.

Built-in predicates such as `not-ssh`, host matching, and OS matching are deferred. Shell predicates cover the initial use cases, while reserved child syntax leaves room for a validated, composable condition language if repeated patterns justify one.

## Consequences

Conditions are scoped to the Execution Host, not to individual clients attached to one tmux server. A Load Condition that changes from true to false prevents future loads but does not undo existing plugin effects; fully applying that transition requires restarting the tmux server.

An unstable Enable Condition may project different Effective Plugin Specifications for `init`, `sync`, and `clean` when those commands inherit different environments. Users must base Enable Conditions on stable host properties. Session-sensitive values such as `SSH_CONNECTION` are better suited to Load Conditions, but a long-lived tmux server may still expose stale environment values.

A plugin whose Load Condition is false may still be installed, built, or reported as a plugin failure. Users who want to exclude that lifecycle work must use the Enable Condition.

The lock fingerprint covers the resulting Effective Plugin Specification and its existing lock-affecting fields. Condition expression text and Load Conditions do not enter the fingerprint. Lock snapshots remain available for reproducing an existing environment, but tmup does not assume that they are shared across Execution Hosts.

Older tmup versions may ignore condition properties and load affected plugins unconditionally. Conditional configurations therefore require a tmup version that explicitly supports this decision.
