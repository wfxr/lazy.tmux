# tmup

`tmup` reconciles declared tmux plugins with a reproducible lock snapshot and loads the resulting managed plugin state into tmux.

## Language

**Init Session**:
The lifecycle of one `tmup init` request, from deciding whether managed plugin state needs reconciliation through loading declared plugins into tmux.
_Avoid_: Init flow, init process
