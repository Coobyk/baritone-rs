// Baritone port: path/goal/waypoint rendering. `visuals` feeds the native
// rim/light lists (path crumbs on each floor cell, a rim on the objective,
// a rim per waypoint); `world` draws waypoint name labels on the world
// canvas, flat on the marked cell like gamma's light-level numbers (the
// canvas culls back faces, so labels are invisible from below).
//
// Waypoints: the extras module owns `/b wp` and keeps its own statics — this
// module keeps its OWN copy for drawing. `umbx_baritone_extras` SHOULD call
// `umbx_baritone_render::note_waypoint(name, pos)` on every add/save and
// `umbx_baritone_render::clear_waypoints()` on delete/clear; a snapshot is
// available through `waypoints(&mut vec)`. If extras never calls these,
// nothing breaks — markers just don't appear. As a fallback channel this
// module also re-reads the `wp_list` setting on a slow cadence: when present
// it is authoritative, format `name=x,y,z;name=x,y,z`.
//
// `/b render <path|goal|waypoints> [on|off]` toggles each layer; the flags
// persist as `render_path` / `render_goal` / `render_waypoints`.

use std::sync::Mutex;

use crate::game::native_hooks::NativeBlockRim;
use crate::game::umbx_baritone as rt;
use crate::game::Game;
use horizon_client::text::Text;

/// Path crumbs: translucent amber rim on the floor cell below each node.
const PATH_ARGB: u32 = 0x66_FF_CC_00;
const PATH_W: f32 = 1.0;
/// Objective: green; red when the cell is solid (a Break target).
const GOAL_ARGB: u32 = 0xFF_22_CC_88;
const BREAK_ARGB: u32 = 0xFF_EE_33_33;
const GOAL_W: f32 = 2.0;
/// Waypoints: blue rim + light name text.
const WP_ARGB: u32 = 0xFF_55_AA_FF;
const WP_W: f32 = 1.5;
const WP_TEXT: u32 = 0xFF_DD_EE_FF;
const OUTLINE: u32 = 0xB0_00_00_00;
/// World-text scale: ~9px glyphs at this size sit on a block top.
const SCALE: f32 = 0.045;
/// Most path nodes drawn per frame.
const MAX_NODES: usize = 256;
/// Most waypoints stored/drawn.
const MAX_WPS: usize = 64;
/// Ticks between persisted-setting re-reads.
const SYNC_TICKS: u32 = 30;

struct S {
    path: bool,
    goal: bool,
    wps_on: bool,
    /// Persisted keys were read at least once.
    loaded: bool,
    syncs: u32,
    /// `(name, cell)` waypoint markers, fed by extras via `note_waypoint`.
    wps: Vec<(String, [i32; 3])>,
}

static STATE: Mutex<Option<S>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut S) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(S {
            path: true,
            goal: true,
            wps_on: true,
            loaded: false,
            syncs: 0,
            wps: Vec::new(),
        });
    }
    f(g.as_mut().unwrap())
}

fn on(game: &Game<'_>) -> bool {
    game.native_on(rt::MOD_ID) && game.native_window()
}

fn onoff(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

fn key_slot<'a>(s: &'a mut S, key: &str) -> Option<&'a mut bool> {
    match key {
        "render_path" => Some(&mut s.path),
        "render_goal" => Some(&mut s.goal),
        "render_waypoints" => Some(&mut s.wps_on),
        _ => None,
    }
}

fn parse_wp_list(v: &str) -> Option<Vec<(String, [i32; 3])>> {
    let mut out = Vec::new();
    for e in v.split(';') {
        let e = e.trim();
        if e.is_empty() {
            continue;
        }
        let (name, coords) = e.split_once('=')?;
        let mut it = coords.split(',');
        let x = it.next()?.trim().parse().ok()?;
        let y = it.next()?.trim().parse().ok()?;
        let z = it.next()?.trim().parse().ok()?;
        out.push((name.trim().to_owned(), [x, y, z]));
        if out.len() >= MAX_WPS {
            break;
        }
    }
    Some(out)
}

/// Re-read the persisted toggles (and `wp_list`) on a slow cadence; the
/// first call after enable always loads so saved flags apply.
fn sync(game: &Game<'_>) {
    let due = with(|s| {
        s.syncs = s.syncs.wrapping_add(1);
        !s.loaded || s.syncs % SYNC_TICKS == 0
    });
    if !due {
        return;
    }
    let p = game.native_setting(rt::MOD_ID, "render_path");
    let g = game.native_setting(rt::MOD_ID, "render_goal");
    let w = game.native_setting(rt::MOD_ID, "render_waypoints");
    let list = game.native_setting(rt::MOD_ID, "wp_list");
    with(|s| {
        s.loaded = true;
        if let Some(v) = p {
            s.path = v == "true";
        }
        if let Some(v) = g {
            s.goal = v == "true";
        }
        if let Some(v) = w {
            s.wps_on = v == "true";
        }
        // A present, parseable `wp_list` is authoritative — extras may
        // publish through settings instead of `note_waypoint`.
        if let Some(v) = list {
            if let Some(wps) = parse_wp_list(&v) {
                s.wps = wps;
            }
        }
    });
}

// ---- extras-facing waypoint store ------------------------------------

/// Record/replace a waypoint marker. `umbx_baritone_extras` should call
/// this from `/b wp save ...` (same name overwrites).
pub fn note_waypoint(name: &str, pos: [i32; 3]) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    with(|s| {
        if let Some(e) = s.wps.iter_mut().find(|(n, _)| n.as_str() == name) {
            e.1 = pos;
        } else if s.wps.len() < MAX_WPS {
            s.wps.push((name.to_owned(), pos));
        }
    });
}

/// Drop every stored marker. `umbx_baritone_extras` should call this from
/// `/b wp clear` (and `note_waypoint` re-adds what's kept).
pub fn clear_waypoints() {
    with(|s| s.wps.clear());
}

/// Snapshot the stored markers: `let mut v = Vec::new(); waypoints(&mut v);`
pub fn waypoints(out: &mut Vec<(String, [i32; 3])>) {
    with(|s| out.extend(s.wps.iter().cloned()));
}

// ---- module contract --------------------------------------------------

/// `native_visuals`: path crumbs, goal rim, waypoint rims.
pub fn visuals(
    game: &crate::game::Game<'_>,
    rims: &mut Vec<crate::game::native_hooks::NativeBlockRim>,
    lights: &mut Vec<crate::game::native_hooks::NativeColoredLight>,
) {
    if !on(game) {
        return;
    }
    let (path_on, goal_on, wp_on) = with(|s| (s.path, s.goal, s.wps_on));
    if !path_on && !goal_on && !wp_on {
        return;
    }
    // Path: one translucent rim on the floor cell below each upcoming node
    // (the canvas has no verified quad helper, so crumbs ride the rim list).
    if path_on {
        let cells = rt::with(|s| {
            s.path
                .iter()
                .skip(s.at)
                .take(MAX_NODES)
                .map(|m| m.pos)
                .collect::<Vec<_>>()
        });
        for p in cells {
            rims.push(NativeBlockRim {
                pos: [p[0], p[1] - 1, p[2]],
                argb: PATH_ARGB,
                width: PATH_W,
            });
        }
    }
    // Goal: the process's objective cell, else the last path node. Solid
    // cells (mine targets) get the red "break me" rim.
    if goal_on {
        let goal = rt::with(|s| s.objective.or_else(|| s.path.last().map(|m| m.pos)));
        if let Some(g) = goal {
            let solid = game
                .res
                .facts()
                .get(game.world.block(g[0], g[1], g[2]))
                .full_collision;
            rims.push(NativeBlockRim {
                pos: g,
                argb: if solid { BREAK_ARGB } else { GOAL_ARGB },
                width: GOAL_W,
            });
        }
    }
    if wp_on {
        with(|s| {
            for (_, pos) in s.wps.iter().take(MAX_WPS) {
                rims.push(NativeBlockRim {
                    pos: *pos,
                    argb: WP_ARGB,
                    width: WP_W,
                });
            }
        });
    }
    // No dynamic lights yet; the goal could get one later.
    let _ = lights;
}

/// `native_world_submit`: waypoint name labels, flat on the marked cell.
pub fn world(
    game: &crate::game::Game<'_>,
    draw: &mut crate::mod_api::NativeWorldDraw,
) {
    if !on(game) {
        return;
    }
    let wps = with(|s| {
        if s.wps_on {
            s.wps.iter().take(MAX_WPS).cloned().collect::<Vec<_>>()
        } else {
            Vec::new()
        }
    });
    for (name, pos) in wps {
        draw.push();
        // Flat on the bottom face level of the marked cell — the floor top
        // when the marker is a feet cell — glyph top facing north.
        if draw.translate_world([
            f64::from(pos[0]) + 0.5,
            f64::from(pos[1]) + 0.01,
            f64::from(pos[2]) + 0.5,
        ]) {
            draw.rotate_about([1.0, 0.0, 0.0], 90.0, [0.0, 0.0, 0.0]);
            draw.scale([SCALE, SCALE, SCALE]);
            draw.translate([-2.5 * name.len() as f32, -4.0, 0.0]);
            game.native_world_text(
                draw,
                &Text::literal(name),
                WP_TEXT,
                Some(OUTLINE),
            );
        }
        draw.pop();
    }
}

/// `/b render <path|goal|waypoints> [on|off]`; args arrive lower-cased.
pub fn command(game: &mut crate::game::Game<'_>, args: &[&str]) -> bool {
    if !on(game) {
        return false;
    }
    match args.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("render" | "draw") => {
            render_cmd(game, &args[1..]);
            true
        }
        _ => false,
    }
}

fn render_cmd(game: &mut Game<'_>, args: &[&str]) {
    let Some(which) = args.first() else {
        let (p, g, w) = with(|s| (s.path, s.goal, s.wps_on));
        game.native_chat(&format!(
            "baritone: path={} goal={} waypoints={} — /b render <path|goal|waypoints> [on|off]",
            onoff(p),
            onoff(g),
            onoff(w)
        ));
        return;
    };
    let (key, label) = match which.to_ascii_lowercase().as_str() {
        "path" => ("render_path", "path overlay"),
        "goal" => ("render_goal", "goal marker"),
        "waypoints" | "waypoint" | "wp" | "wps" => ("render_waypoints", "waypoint markers"),
        _ => {
            game.native_chat("baritone: /b render <path|goal|waypoints> [on|off]");
            return;
        }
    };
    let new = with(|s| {
        let cur = *key_slot(s, key).unwrap();
        let new = match args.get(1).copied() {
            Some("off" | "false" | "no" | "0") => false,
            Some("on" | "true" | "yes" | "1") => true,
            _ => !cur,
        };
        *key_slot(s, key).unwrap() = new;
        new
    });
    game.native_set_setting(rt::MOD_ID, key, if new { "true" } else { "false" });
    game.native_chat(&format!("baritone: {label} {}", onoff(new)));
}

/// `native_tick`: settings sync only — drawing happens in the two hooks.
/// Cheap early-out once every layer is off.
pub fn tick(game: &mut crate::game::Game<'_>) {
    if !on(game) {
        return;
    }
    sync(game);
    if with(|s| !(s.path || s.goal || s.wps_on)) {
        return;
    }
}

/// Settings page labels, mirrors of the toggles (local indices 0..=2).
pub fn settings(game: &crate::game::Game<'_>, out: &mut Vec<String>) {
    if !on(game) {
        return;
    }
    sync(game);
    let (p, g, w) = with(|s| (s.path, s.goal, s.wps_on));
    out.extend([
        format!("Path overlay: {}", onoff(p)),
        format!("Goal marker: {}", onoff(g)),
        format!("Waypoint markers: {}", onoff(w)),
    ]);
}

/// Consume clicks on our three buttons (local indices 0..=2).
pub fn click(game: &mut crate::game::Game<'_>, button: usize) -> bool {
    if !on(game) {
        return false;
    }
    let key = match button {
        0 => "render_path",
        1 => "render_goal",
        2 => "render_waypoints",
        _ => return false,
    };
    let new = with(|s| {
        let slot = key_slot(s, key).unwrap();
        *slot = !*slot;
        *slot
    });
    game.native_set_setting(rt::MOD_ID, key, if new { "true" } else { "false" });
    true
}

/// `/b help` contribution.
pub fn help() -> &'static [&'static str] {
    &["/b render <path|goal|waypoints> [on|off] — drawing toggles"]
}
