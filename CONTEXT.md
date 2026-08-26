# tmup

`tmup` reconciles declared tmux plugins with a reproducible lock snapshot and loads the resulting managed plugin state into tmux.

## Language

**Init Session**:
The lifecycle of one `tmup init` request, from deciding whether managed plugin state needs reconciliation through loading declared plugins into tmux.
_Avoid_: Init flow, init process

**Execution Host**:
The machine that runs a tmux server and the Init Sessions that manage it. Conditions may differ across Execution Hosts, but not across clients attached to the same tmux server.
_Avoid_: Runtime host, remote plugin host

**Enable Condition**:
The `enabled` predicate that determines whether a plugin declaration belongs to an Execution Host's effective plugin specification.
_Avoid_: Load condition

**Effective Plugin Specification**:
The ordered plugin declarations whose Enable Conditions are true on an Execution Host. It defines that host's desired managed state and command-visible plugins.
_Avoid_: Active plugins, raw configuration

**Load Condition**:
The `cond` predicate that determines whether an enabled plugin's options and scripts are loaded during an Init Session without changing its managed state.
_Avoid_: Enable condition

**Load Eligibility**:
Whether an enabled plugin's Load Condition currently permits a future Init Session to load it. It does not describe or undo effects from an earlier load.
_Avoid_: Active state, loaded state
