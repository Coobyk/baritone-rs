// Elytra glide process — a best-effort port of Baritone's `/b elytra`. The
// real solver simulates the whole glide; this one jumps, deploys, steers
// yaw at the goal column and pitches to manage descent. It can overshoot,
// undershoot and pancake into hills: honest best-effort.
//
// HOW THE GLIDE STARTS (verified in this checkout,
// crates/horizon-client/src/motion.rs ~L1088-1105, `Player.tryToStartFallFlying`
// inside `LocalPlayer.aiStep`): a fresh jump press — `jumping && !was_jumping`,
// i.e. an edge — while `canGlide` holds (`player.glider` = a usable elytra is
// worn, airborne, not creative-flying, no levitation, not in water/lava, not
// on a climbable) sets `player.fall_flying = true`. So this module never
// writes `fall_flying` first: it pulses `want.jump` one tick mid-fall, like a
// player double-tapping space. The field IS pub and directly writable
// (`PlayerMotion.fall_flying`); we use that only as a single-player fallback
// when pulses time out — on a LAN/remote session `player.server_owns_glide`
// is set and the server would just overwrite it, so only the vanilla
// jump-edge path is honest there.
//
// PITCH SIGN: `PlayerMotion.pitch` is vanilla `xRot` — positive looks DOWN
// (`view_vector` gives `-sin(pitch)` for y, and `fall_flying_movement` at
// motion.rs:2932 treats `pitch < 0` as the nose-up lift case). The task's
// angles were written flight-sim style (positive = nose up); the mapping used
// here is cruise = +12 (nose slightly down, keeps speed) and flare = -35/-45
// (nose up, bleeds speed for landing).
//
// Movement rides the shared `rt::Want`/`s.busy` channel: while active we mark
// `busy` so `native_movement_input` applies our want even though `proc` is
// None, we rewrite `s.want` every tick (the core driver clears it when
// `proc == None`), and `ex::apply` pushes the non-movement parts.
// `/b stop` clears `busy` in the core state; the next tick sees
// `active && !busy` and winds this process down itself.

use std::sync::Mutex;

use crate::game::umbx_baritone as rt;
use crate::game::umbx_baritone_exec as ex;
use crate::game::Game;

/// Vanilla `xRot`, degrees: positive looks down.
const CRUISE_PITCH: f32 = 12.0;
const FLARE_PITCH: f32 = -35.0;
/// Nose right up for the last metres over the goal column.
const LAND_PITCH: f32 = -45.0;
/// Flare when this close to the goal column horizontally, ...
const FLARE_DIST: f64 = 8.0;
/// ... or when the first solid block below is nearer than this.
const LOW_GROUND: i32 = 6;
/// Landed-close-enough radius.
const ARRIVE_DIST: f64 = 3.0;
/// Firework re-use period.
const ROCKET_TICKS: u32 = 40;
/// Air-ticks before the first deploy pulse: ride the jump's arc to the top.
const DEPLOY_AT: u32 = 4;
/// Deploy attempts this long before the direct-flag fallback / abort.
const DEPLOY_TIMEOUT: u32 = 80;
/// Longer cap on a server, where only the jump-edge can start the glide.
const DEPLOY_TIMEOUT_SERVER: u32 = 160;
/// Launch phase cap: still grounded after this means there was no room.
const LAUNCH_TIMEOUT: u32 = 60;
/// Airborne ticks with no horizontal progress before giving up.
const STALL_TICKS: u32 = 240;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// On the ground: sprint-jump to get height for the deploy.
    Launch,
    /// Airborne, not gliding yet: pulse jump on the vanilla edge gate.
    Deploy,
    /// Gliding toward the goal.
    Glide,
}

struct S {
    active: bool,
    /// Goal column (block coords; we steer at its centre).
    goal: [i32; 2],
    phase: Phase,
    /// Ticks in the current phase.
    ticks: u32,
    /// Consecutive airborne ticks (deploy timing).
    air: u32,
    /// Whether our last tick's `want.jump` was down, so pulses stay edges.
    pressed: bool,
    /// Ground->launch attempts used.
    tries: u32,
    /// Ticks until the next firework may fire.
    rocket: u32,
    /// Last horizontal position + still-tick count (stall watchdog).
    last: [f64; 2],
    still: u32,
}

impl S {
    fn idle() -> Self {
        Self {
            active: false,
            goal: [0; 2],
            phase: Phase::Launch,
            ticks: 0,
            air: 0,
            pressed: false,
            tries: 0,
            rocket: 0,
            last: [f64::NAN; 2],
            still: 0,
        }
    }

    fn start(goal: [i32; 2], phase: Phase) -> Self {
        Self {
            active: true,
            goal,
            phase,
            ..Self::idle()
        }
    }
}

static S: Mutex<Option<S>> = Mutex::new(None);

/// Run `f` with the process state, creating the idle one on first use.
fn with<R>(f: impl FnOnce(&mut S) -> R) -> R {
    let mut g = S.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(S::idle());
    }
    f(g.as_mut().unwrap())
}

/// Blocks of air between the feet cell and the first solid block below,
/// capped at 16 (unloaded columns read as air, i.e. "deep").
fn clearance(game: &Game<'_>) -> i32 {
    let Some(p) = game.player.as_ref() else { return 16 };
    let f = rt::feet(p);
    let mut d = 0;
    while d < 16 && f[1] - 1 - d >= game.world.shape.min_y {
        let st = game.world.block(f[0], f[1] - 1 - d, f[2]);
        if game.res.facts().get(st).full_collision {
            return d;
        }
        d += 1;
    }
    d
}

/// Camera + body yaw straight at the goal column's centre; pitch too when the
/// caller is managing it (the glide reads `player.pitch` for its lift).
fn aim_goal(game: &mut Game<'_>, goal: [i32; 2], pitch: Option<f32>) {
    let Some(p) = game.player.as_ref() else { return };
    let dx = goal[0] as f64 + 0.5 - p.position.x;
    let dz = goal[1] as f64 + 0.5 - p.position.z;
    let yaw = (-dx.atan2(dz).to_degrees()) as f32;
    game.yaw = yaw;
    if let Some(pitch) = pitch {
        game.pitch = pitch;
    }
    let p = game.player.as_mut().unwrap();
    p.yaw = yaw;
    p.head_yaw = yaw;
    if let Some(pitch) = pitch {
        p.pitch = pitch;
    }
}

fn horizontal(game: &Game<'_>, goal: [i32; 2]) -> f64 {
    let Some(p) = game.player.as_ref() else { return f64::MAX };
    let dx = goal[0] as f64 + 0.5 - p.position.x;
    let dz = goal[1] as f64 + 0.5 - p.position.z;
    dx.hypot(dz)
}

/// Held stack looks like a firework rocket (`minecraft:firework_rocket` —
/// "firework" is the substring the spec asks for; a star can't hurt anyway).
fn firework_held(game: &Game<'_>) -> bool {
    game.held()
        .is_some_and(|it| it.item.contains("firework"))
}

/// Mark the process over: drop `busy`, clear stale input, tell chat.
fn finish(game: &mut Game<'_>, msg: &str) {
    with(|s| s.active = false);
    rt::with(|s| {
        s.busy = false;
        s.want = rt::Want::default();
    });
    ex::apply(game, &rt::Want::default());
    rt::say(game, msg);
}

/// `/b elytra [<x> <z>]` — the head word must be ours; anything else
/// returns false so other modules and the core can claim their args.
pub fn command(game: &mut Game<'_>, args: &[&str]) -> bool {
    let args = match args.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("elytra" | "ely") => &args[1..],
        _ => return false,
    };
    match args {
        [] => {
            let t = with(|s| {
                s.active.then(|| {
                    format!(
                        "elytra: {:?} to {} {} ({} blocks out)",
                        s.phase,
                        s.goal[0],
                        s.goal[1],
                        horizontal(game, s.goal) as i32
                    )
                })
            });
            rt::say(game, &t.unwrap_or_else(|| "elytra: idle".to_owned()));
            true
        }
        [x, z] => {
            let (Ok(x), Ok(z)) = (x.parse::<i32>(), z.parse::<i32>()) else {
                return false;
            };
            let Some(p) = game.player.as_ref() else { return true };
            let (flying, glider, gliding, grounded) =
                (p.flying, p.glider, p.fall_flying, p.ground.on_ground);
            if flying {
                rt::say(game, "elytra: not while flying — land first");
                return true;
            }
            if !glider && !gliding {
                rt::say(game, "elytra: warning — no glider equipped, trying anyway");
            }
            let phase = if gliding {
                Phase::Glide
            } else if !grounded {
                Phase::Deploy
            } else {
                Phase::Launch
            };
            with(|s| *s = S::start([x, z], phase));
            // Take the shared input slot from every other driver: a core
            // `proc` would fold this run away on the next tick, and a farm/
            // build process would keep writing `want` alongside ours.
            crate::game::umbx_baritone_farm::park();
            crate::game::umbx_baritone_build::park(game);
            rt::with(|s| {
                s.halt();
                s.busy = true;
            });
            rt::say(game, &format!("elytra: gliding toward {x} {z}"));
            true
        }
        _ => false,
    }
}

pub fn tick(game: &mut Game<'_>) {
    // Cheap out: no run, no work.
    if !with(|s| s.active) {
        return;
    }
    if !rt::enabled() || !game.native_window() {
        return;
    }
    // `/b stop` clears `busy`, and another `/b` process claims `proc`: either
    // way one process runs at a time, so fold ours away quietly.
    let kicked = rt::with(|s| !s.busy || s.proc != rt::Proc::None);
    if kicked {
        finish(game, "elytra: stopped");
        return;
    }
    if rt::with(|s| s.set.pause) {
        rt::with(|s| s.want = rt::Want::default());
        return;
    }
    let Some(player) = game.player.as_ref() else { return };
    let on_ground = player.ground.on_ground;
    let gliding = player.fall_flying;
    let falling = player.velocity.y < 0.0;
    let pos = player.position;
    let phase = with(|s| s.phase);

    let mut w = rt::Want::default();
    match phase {
        Phase::Launch => {
            // Drive forward and hop; once airborne, hand over to Deploy.
            if on_ground {
                let goal = with(|s| s.goal);
                aim_goal(game, goal, None);
                w.forward = 1.0;
                w.sprint = true;
                w.jump = true;
                let over = with(|s| {
                    s.pressed = true;
                    s.ticks += 1;
                    s.ticks > LAUNCH_TIMEOUT
                });
                if over {
                    finish(game, "elytra: no room to launch — giving up");
                    return;
                }
            } else {
                with(|s| {
                    s.phase = Phase::Deploy;
                    s.ticks = 0;
                    s.air = 1;
                    // We were still holding jump: release before the pulse.
                    s.pressed = true;
                });
            }
        }
        Phase::Deploy => {
            let goal = with(|s| s.goal);
            aim_goal(game, goal, None);
            w.forward = 1.0;
            w.sprint = true;
            if gliding {
                with(|s| {
                    s.phase = Phase::Glide;
                    s.ticks = 0;
                });
            } else if on_ground {
                // Came down before the deploy: jump again, limited tries.
                let tries = with(|s| {
                    s.tries += 1;
                    s.tries
                });
                if tries >= 3 {
                    finish(game, "elytra: couldn't start the glide — giving up");
                    return;
                }
                with(|s| {
                    s.phase = Phase::Launch;
                    s.ticks = 0;
                    s.air = 0;
                });
            } else {
                let (air, ticks) = with(|s| {
                    s.air += 1;
                    s.ticks += 1;
                    (s.air, s.ticks)
                });
                // Ride to the top of the arc, then pulse jump on a fresh edge:
                // release a tick, press a tick, repeat until the flag lands.
                if falling || air >= DEPLOY_AT {
                    let press = !with(|s| s.pressed);
                    w.jump = press;
                    with(|s| s.pressed = press);
                } else {
                    w.jump = false;
                    with(|s| s.pressed = false);
                }
                if ticks > DEPLOY_TIMEOUT {
                    let (glider, server) = game
                        .player
                        .as_ref()
                        .map_or((false, false), |p| (p.glider, p.server_owns_glide));
                    if glider && !server {
                        // Honest single-player nudge: the flag is pub and the
                        // jump-edge has had its chances.
                        if let Some(p) = game.player.as_mut() {
                            p.fall_flying = true;
                        }
                        with(|s| {
                            s.phase = Phase::Glide;
                            s.ticks = 0;
                        });
                        rt::say(game, "elytra: glide flag set directly (jump press missed)");
                    } else if !glider {
                        finish(game, "elytra: no glider equipped — aborted");
                        return;
                    } else if ticks > DEPLOY_TIMEOUT_SERVER {
                        finish(game, "elytra: server never started the glide — aborted");
                        return;
                    }
                }
            }
        }
        Phase::Glide => {
            let goal = with(|s| s.goal);
            if on_ground {
                let hd = horizontal(game, goal);
                let msg = if hd <= ARRIVE_DIST + 2.0 {
                    "elytra: landed".to_owned()
                } else {
                    format!("elytra: down {hd:.0} blocks short")
                };
                finish(game, &msg);
                return;
            }
            if !gliding {
                // Glide ended early (climbable, server sync, water): redeploy.
                rt::with(|s| s.want = rt::Want::default());
                with(|s| {
                    s.phase = Phase::Deploy;
                    s.ticks = 0;
                    s.pressed = false;
                });
                return;
            }
            let hd = horizontal(game, goal);
            let clear = clearance(game);
            let pitch = if hd < ARRIVE_DIST {
                LAND_PITCH
            } else if hd < FLARE_DIST || clear < LOW_GROUND {
                FLARE_PITCH
            } else {
                CRUISE_PITCH
            };
            aim_goal(game, goal, Some(pitch));
            // Elytra moves by facing: keep forward held, never jump mid-glide.
            w.forward = 1.0;
            w.jump = false;
            // Best-effort rockets: one use every ~40 ticks while one is held.
            let rocket = with(|s| {
                s.rocket = s.rocket.saturating_sub(1);
                s.rocket
            });
            if rocket == 0 && firework_held(game) {
                w.right = true; // one tick: forced_hit stays None -> use on air
                with(|s| s.rocket = ROCKET_TICKS);
            }
            // Stall watchdog: no horizontal progress for a while -> give up.
            let stuck = with(|s| {
                if s.last[0].is_nan() {
                    s.last = [pos.x, pos.z];
                }
                let moved = (pos.x - s.last[0]).hypot(pos.z - s.last[1]) > 0.05;
                s.last = [pos.x, pos.z];
                if moved {
                    s.still = 0;
                } else {
                    s.still += 1;
                }
                s.still
            });
            if stuck >= STALL_TICKS {
                finish(game, "elytra: not making progress — giving up");
                return;
            }
        }
    }

    rt::with(|s| s.want = w);
    ex::apply(game, &w);
}

pub fn help() -> &'static [&'static str] {
    &["/b elytra <x> <z> — glide there (needs an elytra and some height)"]
}
