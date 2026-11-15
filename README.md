# baritone-rs

A [Baritone](https://github.com/cabaletta/baritone)-style pathing bot for
[Horizon](https://github.com/horizon-rust/horizon), as a native mixin mod
(`baritone.hnmod`).

## Commands — `/b` or `/baritone`

Both aliases register a real Brigadier grammar with the game, so chat shows
subcommand usage and inline tab-completion instead of a raw `[<args>]` hint.
Completions are context-aware: setting names and `on`/`off` values after
`/b set`, waypoint names after `/b wp goto`, block names after `/b mine`
(comma-separated lists complete the part after the last comma), schematic
files after `/b build`, directions after `/b sel expand`, entity types after
`/b follow entity`, and more.

| Command | What it does |
| --- | --- |
| `/b goto <x> <z>` | Walk to the standable surface at that column |
| `/b goto <x> <y> <z>` | Walk to an exact cell |
| `/b goto y <y>` | Reach a Y level; `~`/`~-N`/`~N` coords work in all forms |
| `/b mine [n] <block>` | Find and dig out matching blocks — optional count, comma-separated kinds (`5 log,diamond_ore`); also picks up matching drops |
| `/b follow <entity>` | Trail a mob: `player <name>`, `entity <type>`, `entities`, or a bare name |
| `/b explore [block]` | Wander to unvisited spots, or toward the nearest matching block (`exploreForBlocks`) |
| `/b tunnel <h> <w> [len]` | Excavate a corridor cross-section ahead (default 2×1) |
| `/b thisway [n]` | Walk n blocks in the facing direction |
| `/b axis` | Walk to the nearest coordinate axis |
| `/b spawn` `/b click [left\|right]` | Path to world spawn / pulse a click |
| `/b pause` `/b resume` `/b stop` `/b path` | Control the active task |
| `/b set <key> [v]` | `sprint`, `break`, `place`, `parkour`, `parkourPlace`, `drop` (aliases `maxFallHeight`/`maxFallHeightNoWater`), `radius`, `drops`, `exposed`, `followRadius`, `legit`, `water`, `lava`, `mobR`, `mobC`, `allowDownward`, `allowVines`, `allowDiagonalAscend`/`allowDiagonalDescend`, `assumeStep`, `sprintInWater`, `toolSelect`, `allowWalkOnBottomSlab` |
| `/b goal <x> <z>` `/b invert` | Stored goal; `invert` + `path` runs away from it |
| `/b blacklist <block\|clear>` | Blocks the pathfinder won't break and mine skips |
| `/b avoid <block\|clear>` | Blocks the pathfinder still uses but charges +8 (upstream `blocksToAvoid`) |
| `/b sel ...` | Selections: pos1/pos2, fill, replace, walls, shell, expand, shift, copy/cut/paste |
| `/b build <name>` | Build a `.schematic` file; `/b build` resumes the queue |
| `/b farm [r]` | Harvest, replant and till farmland (nearest-first, capped job list, throttled rescans) |
| `/b wp save\|list\|goto\|delete\|clear` | Waypoints |
| `/b find <block>` `/b surface` `/b come` `/b schematica` | More goals / info |
| `/b render <path\|goal\|waypoints>` | World overlays: path crumbs, goal rim, waypoint labels |
| `/b elytra <x> <z>` | Best-effort elytra glide (see below) |
| `/b proc` `/b eta` `/b version` `/b modified` `/b reset` | Status/settings |
| `/b goal` `/b help` | Status and usage |

## Movements

Cost-weighted A* over block cells: walk, step-up, drops (up to
`max_drop`), parkour gaps (1–2 cells), ladder/vine/scaffold climbs,
water, **break-through** (digs walls — including diagonal corner cells —
when `allow_break`), **pillar** (jump + place underfoot), **bridge**
(sneak + place ahead) and **ParkourPlace** (mid-air floor placement over
a gap, `allowParkourPlace`) when `allow_place` and a placeable block is
held. `legit` mine mode raycasts targets first (`seen_ores` keeps ones
once visible); `mobR`/`mobC` charge path cost near mobs; `water`/`lava`
assume fluids walkable for planning (upstream's `assumeWalkOnWater` /
`assumeWalkOnLava` cheats); `allowDownward`/`allowVines`/`allowDiagonal*`
gate those move kinds; `assumeStep`/`sprintInWater`/`toolSelect`/
`allowWalkOnBottomSlab` adjust costs and the executor; explored cells
are remembered across restarts (`explorefilter`). All settings persist
through Horizon's per-mod store.

Breaking and placing run through the game's own input: the mod points
`forced_hit` at the target face and holds `left`/`right`, so the normal
dig timer and use rules apply. Mod-owned inputs are tracked and released
the tick after they stop being requested — `/b stop` never leaves a
button held.

AutoTool picks the best hotbar tool per break using the vanilla
`mining_speed`/`correct_tool_for_drops` rules; AutoEat selects the best
food on the hotbar and eats when hunger drops.

## Elytra

`/b elytra` is the honest simplified port: launch → deploy on the
jump-edge like vanilla `tryToStartFallFlying` → steer yaw at the goal,
cruise/flare pitch, firework boost if one is held. It is best-effort,
not Baritone's full elytra solver.

## Not ported

Baritone's long-range chunk cache/waypoint world data, the full Builder
rule set, and ~60 individual settings beyond the ones here (`/b set`
still accepts the cfg-backed ones like `allowInventory` — there is no
native inventory-swap API — and stores them harmlessly).
`assumeWalkOnLava` plans over lava
floors but vanilla physics still sinks the player — the cheat only
changes the planner, same as upstream's.

## Layout

`mixins/baritone/` — `pathfind.rs` (A* + movement actions), `exec.rs`
(move executor), `runtime.rs` (state/processes/settings), `glue.rs`
(commands, driver, settings page), `hooks/` (injection helpers).
