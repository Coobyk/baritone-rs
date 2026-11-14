# Baritone for Horizon

A walking-only [Baritone](https://github.com/cabaletta/baritone)-style
pathing mod for [Horizon](https://github.com/horizon-rust/horizon), as a
native mixin bundle.

## Commands

Typed in chat as client commands (`/` commands; the chat box completes and
colours `b` and `baritone`):

- `/b goto <x> <z>` — walk to the column's surface nearest your height
- `/b goto <x> <y> <z>` — walk to an exact block
- `/b stop` (`cancel`, `pause`) — stop walking
- `/b path` (`repath`) — recompute from where you stand
- `/b sprint [on|off]` — sprint while walking (default on, persisted)

Feedback (`walking to …`, `arrived`, `stuck, giving up`) prints as client
chat lines.

## What it does

- A* over the block grid (`pathfind.rs`): steps up one block, drops up to
  four, cuts diagonals only between open corners. Floors are `full_collision`
  blocks; the body needs two open cells. Unloaded chunks are not walked into.
- Follows the path in `native_tick` (`glue.rs`): faces each node's centre,
  drives forward through `native_movement_input`, jumps onto step-ups, and
  re-plans once when stuck.
- It does **not** break or place blocks, bridge, pillar, climb ladders, mine,
  or swim — vanilla Baritone features that need those are future work.

## Layout

A mixin bundle (`mixins/baritone/`, `horizon-mixin.toml`):

- `pathfind.rs` — the A* and the block-space rules.
- `runtime.rs` — the pathing state statics.
- `glue.rs` — `native_command`/`native_commands`, the tick driver,
  `native_movement_input`, the mod listing and the buttons page.
- `hooks/` — one thin wrapper per injection.

## Building

```sh
horizon-mixins plan --workspace <horizon source> --mods mixins/baritone --out plan.json
horizon-mixins apply --workspace <horizon source> --mods mixins/baritone --out <staged>
```

or `horizon-launcher native-build <folder> --trust-native-code`. Target
revision in `compatibility.json`.
