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
| `/b sel ...` | Selections: pos1/pos2, fill, replace, walls, shell, expand, shift, copy/cut/paste |
| `/b build <name>` | Build a `.schematic` file; `/b build` resumes the queue |
| `/b farm [r]` | Harvest, replant and till farmland |
| `/b wp save\|list\|goto\|delete\|clear` | Waypoints |
| `/b find <block>` `/b surface` `/b come` `/b invert` `/b blacklist` | More goals |
| `/b render <path\|goal\|waypoints>` | World overlays: path crumbs, goal rim, waypoint labels |
| `/b elytra <x> <z>` | Best-effort elytra glide (see below) |
| `/b proc` `/b eta` `/b version` `/b modified` `/b reset` | Status/settings |
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

## Elytra

`/b elytra` is the honest simplified port: launch → deploy on the
jump-edge like vanilla `tryToStartFallFlying` → steer yaw at the goal,
cruise/flare pitch, firework boost if one is held. It is best-effort,
not Baritone's full elytra solver.

## Not ported

Baritone's long-range chunk cache/waypoint world data, `assume*`
walk-on-lava cheats, mob-avoidance path costs, and its ~80 individual
settings beyond the ones here (`/b set` still accepts their names and
stores them harmlessly).

## Layout

`mixins/baritone/` — `pathfind.rs` (A* + movement actions), `exec.rs`
(move executor), `runtime.rs` (state/processes/settings), `glue.rs`
(commands, driver, settings page), `hooks/` (injection helpers).
