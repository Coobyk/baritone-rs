# baritone-rs

A [Baritone](https://github.com/cabaletta/baritone)-style pathing bot for
[Horizon](https://github.com/horizon-rust/horizon), as a native mixin mod
(`baritone.hnmod`).

## Commands — `/b` or `/baritone`

| Command | What it does |
| --- | --- |
| `/b goto <x> <z>` | Walk to the standable surface at that column |
| `/b goto <x> <y> <z>` | Walk to an exact cell |
| `/b goto y <y>` | Reach a Y level |
| `/b mine <block>` | Find and dig out matching blocks (e.g. `diamond_ore`, `log`, comma-separated) |
| `/b follow <entity>` | Stay near the nearest mob of that kind (or custom name) |
| `/b explore` | Wander to unvisited spots |
| `/b tunnel [n]` | Dig and walk straight ahead (default 64) |
| `/b thisway [n]` | Walk n blocks in the facing direction |
| `/b axis` | Walk to the nearest coordinate axis |
| `/b pause` `/b resume` `/b stop` `/b path` | Control the active task |
| `/b set <key> [v]` | `sprint`, `break`, `place`, `parkour`, `drop`, `radius` |
| `/b goal` `/b help` | Status and usage |

## Movements

Cost-weighted A* over block cells: walk, step-up, drops (up to
`max_drop`), parkour gaps (1–2 cells), ladder/vine/scaffold climbs,
water, **break-through** (digs walls when `allow_break`), **pillar**
(jump + place underfoot) and **bridge** (sneak + place ahead) when
`allow_place` and a placeable block is held.

Breaking and placing run through the game's own input: the mod points
`forced_hit` at the target face and holds `left`/`right`, so the normal
dig timer and use rules apply.

## Not ported

Schematic building (`#build`), waypoints, selection API, elytra,
lava/liquid pathing beyond water swims, fall-damage-aware drops, and
Baritone's chunk cache.

## Layout

`mixins/baritone/` — `pathfind.rs` (A* + movement actions), `exec.rs`
(move executor), `runtime.rs` (state/processes/settings), `glue.rs`
(commands, driver, settings page), `hooks/` (injection helpers).
