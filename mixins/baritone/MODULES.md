# Baritone module contract

Every feature module lives in `mixins/baritone/<name>.rs` and is injected as a
support module named `umbx_baritone_<name>` into `crates/horizon-client/src/game.rs`
(declare it in `horizon-mixin.toml` — the integrator does this, not you).

Modules are plain `pub` items inside the `horizon_client` crate. They MUST NOT
edit `runtime.rs`, `glue.rs`, `pathfind.rs`, `exec.rs`, `horizon-mixin.toml`,
`compatibility.json`, or anything under `hooks/` — those are shared/owned.

## Required interface (all `pub`)

```rust
/// Return true if the module handled `args` (a `/b` subcommand, already
/// lower-cased head + args slice). Print replies with `game.native_chat`.
pub fn command(game: &mut crate::game::Game<'_>, args: &[&str]) -> bool
/// Every client tick; early-out cheap when the module's process is idle.
pub fn tick(game: &mut crate::game::Game<'_>)
/// Append labels to the mod's settings page (mirror of the toggles).
pub fn settings(game: &crate::game::Game<'_>, out: &mut Vec<String>)
/// Handle the click on your own button index (the integrator passes your
/// section's local index). Return true when consumed.
pub fn click(game: &mut crate::game::Game<'_>, button: usize) -> bool
/// `/b help` lines this module contributes.
pub fn help() -> &'static [&'static str]
```

## State

Keep module state in your own `static ...: Mutex<...>` (pattern:
`static S: Mutex<Option<S>> = Mutex::new(None)` + a `with()` helper —
see `runtime.rs`). Persist settings via `game.native_set_setting("baritone",
key, value)` / read with `game.native_setting("baritone", key) -> Option<String>`.

## Driving movement

- `crate::game::umbx_baritone as rt` — `rt::with(|s| s.want)` is the per-tick
  input request (`rt::Want { forward, jump, sneak, sprint, attack, right, hit }`).
  Write it, then call `crate::game::umbx_baritone_exec::apply(game, &want)`.
- The core driver only runs while `s.proc != Proc::None`; while a module's own
  process is active it MUST `rt::with(|s| s.want = rt::Want::default())` first
  so stale inputs don't linger. The input hook applies `want` whenever
  `!s.set.pause`.
- `crate::game::umbx_pathfind::{W, find, Move, Kind, Opts, column_top}` —
  `W { game }` (immutable borrow; `standable`, `body_open`, `floor`,
  `breakable` are `pub`), `find(&w, start, goal, &opts, done_pred) ->
  Option<Vec<Move>>`.
- `crate::game::umbx_baritone_exec::{drive, apply, Out}` —
  `drive(game, &mut rt_state, &move_, prev_pos)` executes one move.

## Game API cheat sheet (all verified in this checkout)

- `game.player: Option<PlayerMotion>` — `position` (Vec3 `.x/.y/.z`),
  `yaw`, `head_yaw`, `ground.on_ground`, `sprinting`.
- `game.world.block(x, y, z)` -> state id (unloaded = air);
  `game.world.shape.min_y` / `.max_y()`; `game.world.light(x,y,z)` packed.
- `game.res.facts().get(state)` -> `StateFacts` — `name` (`minecraft:stone`),
  `hardness` (<0 unbreakable), `full_collision`, `fluid`
  (`horizon_items::blocks::Fluid::{None,Water,Lava}`), `fluid_level`,
  `item` (item id for the block), `replaceable`, `classes` (class chain).
  `game.res.facts().states` is `Vec<StateFacts>` (item -> block lookup).
- `game.mobs: Vec<Mob>` — `position`, `kind` (`m.kind.model.name`),
  `custom_name: Option<String>`, `removed`, `alive()`, `species()`.
- `game.inventory: Vec<Option<ItemStack>>`, `game.selected: usize` (hotbar),
  `game.held() -> Option<&ItemStack>`; `ItemStack { item: Arc<str>, count: u32, .. }`.
- `game.forced_hit = Some(crate::game::Hit { pos, face, point })` overrides the
  crosshair target; `game.left`/`game.right` are the attack/use held flags;
  `game.swing_arm()`.
- `game.native_chat(&str)` — client chat line.
- `game.native_on("baritone")` — mod enabled; `game.native_window()` — the
  window's own game (always check both before acting).
- `game.yaw` — camera yaw; set it + `player.yaw`/`head_yaw` to steer.
- Vec3: `horizon_math::Vec3` — has `.x/.y/.z` f64 fields.
