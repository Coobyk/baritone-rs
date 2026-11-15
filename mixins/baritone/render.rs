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

/// Path: Java Baritone's `colorCurrentPath` — a solid red ribbon through
/// the upcoming nodes (the canvas has no line primitive, so thin quads).
const LINE_SPRITE: &str = "block/white_concrete";
const LINE_RGBA: [u8; 4] = [0xFF, 0x00, 0x00, 0xFF];
/// Ribbon half-width in blocks — a ~4 cm line.
const LINE_HALF: f32 = 0.03;
/// Blocks queued to break / the mine objective: translucent green fill and
/// wireframe rim, like Java's blocks-to-mine highlight.
const TARGET_FILL: [u8; 4] = [0x00, 0xFF, 0x3C, 0x48];
const TARGET_RIM: u32 = 0xFF_00_FF_3C;
const TARGET_W: f32 = 2.0;
/// Objective cell: translucent white fill + rim, like Java's goal box.
const GOAL_FILL: [u8; 4] = [0xFF, 0xFF, 0xFF, 0x40];
const GOAL_ARGB: u32 = 0xFF_22_CC_88;
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
/// Most target blocks filled per frame.
const MAX_TARGETS: usize = 48;
/// Most waypoints stored/drawn.
const MAX_WPS: usize = 64;
/// Ticks between persisted-setting re-reads.
const SYNC_TICKS: u32 = 30;
/// Sprite the translucent fills shade with.
const FILL_SPRITE: &str = "block/white_concrete";

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

/// Solid blocks the process wants broken right now: every queued `Break`
/// cell plus the current objective — Java's blocks-to-mine set, capped.
fn targets(game: &Game<'_>) -> Vec<[i32; 3]> {
    let (cells, objective) = rt::with(|s| {
        (
            s.path
                .iter()
                .skip(s.at)
                .flat_map(|m| m.cells.iter().copied())
                .collect::<Vec<_>>(),
            s.objective,
        )
    });
    let mut out: Vec<[i32; 3]> = objective.into_iter().collect();
    for c in cells {
        if out.len() >= MAX_TARGETS {
            break;
        }
        let solid = game
            .res
            .facts()
            .get(game.world.block(c[0], c[1], c[2]))
            .full_collision;
        if solid && !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// A translucent filled box on `cell` — six quads, each emitted twice
/// (the canvas culls back faces) so the fill reads from every side.
fn fill_box(
    game: &Game<'_>,
    draw: &mut crate::mod_api::NativeWorldDraw,
    cell: [i32; 3],
    color: [u8; 4],
) {
    // Grown slightly past the block faces so the fill doesn't z-fight the
    // world mesh (Java expands the selection box the same way).
    const E: f32 = 0.008;
    const O: f32 = 1.0 + E * 2.0;
    const FACES: [[[f32; 3]; 4]; 6] = [
        [[0., 0., 0.], [O, 0., 0.], [O, 0., O], [0., 0., O]], // down
        [[0., O, 0.], [0., O, O], [O, O, O], [O, O, 0.]], // up
        [[O, 0., 0.], [O, O, 0.], [0., O, 0.], [0., 0., 0.]], // north
        [[0., 0., O], [O, 0., O], [O, O, O], [0., O, O]], // south
        [[0., 0., 0.], [0., O, 0.], [0., O, O], [0., 0., O]], // west
        [[O, 0., O], [O, O, O], [O, O, 0.], [O, 0., 0.]], // east
    ];
    draw.push();
    if draw.translate_world([
        cell[0] as f64 - E as f64,
        cell[1] as f64 - E as f64,
        cell[2] as f64 - E as f64,
    ]) {
        for f in FACES {
            game.native_world_quad(draw, FILL_SPRITE, f, [0., 0., 16., 16.], color);
            let rev = [f[3], f[2], f[1], f[0]];
            game.native_world_quad(draw, FILL_SPRITE, rev, [0., 0., 16., 16.], color);
        }
    }
    draw.pop();
}

/// Java's red path line: a thin quad ribbon between consecutive node
/// centres, flat like a GL line ~4 cm wide (no line primitive exists).
fn line_segment(
    game: &Game<'_>,
    draw: &mut crate::mod_api::NativeWorldDraw,
    a: [i32; 3],
    b: [i32; 3],
) {
    let d = [
        (b[0] - a[0]) as f32,
        (b[1] - a[1]) as f32,
        (b[2] - a[2]) as f32,
    ];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if len < 0.05 {
        return;
    }
    // Ribbon half-width: perpendicular to the segment in the horizontal
    // plane, so flat paths read as a red strip on the ground.
    let mut px = -d[2];
    let mut pz = d[0];
    let pl = (px * px + pz * pz).sqrt();
    if pl < 1e-4 {
        // Vertical segment: any horizontal perpendicular works.
        px = 1.0;
        pz = 0.0;
    } else {
        px /= pl;
        pz /= pl;
    }
    let (px, pz) = (px * LINE_HALF, pz * LINE_HALF);
    let corners: [[f32; 3]; 4] = [
        [px, 0.0, pz],
        [-px, 0.0, -pz],
        [d[0] - px, d[1], d[2] - pz],
        [d[0] + px, d[1], d[2] + pz],
    ];
    draw.push();
    if draw.translate_world([
        a[0] as f64 + 0.5,
        a[1] as f64 + 0.5,
        a[2] as f64 + 0.5,
    ]) {
        game.native_world_quad(draw, LINE_SPRITE, corners, [0., 0., 16., 16.], LINE_RGBA);
        let rev = [corners[3], corners[2], corners[1], corners[0]];
        game.native_world_quad(draw, LINE_SPRITE, rev, [0., 0., 16., 16.], LINE_RGBA);
    }
    draw.pop();
}

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
    // Blocks to break (mine objectives and queued Break cells): the green
    // wireframe part of Java's blocks-to-mine highlight; the fill is drawn
    // on the world canvas in `world`.
    if goal_on {
        for t in targets(game) {
            rims.push(NativeBlockRim {
                pos: t,
                argb: TARGET_RIM,
                width: TARGET_W,
            });
        }
        // Goal: the process's objective cell, else the last path node.
        let goal = rt::with(|s| s.objective.or_else(|| s.path.last().map(|m| m.pos)));
        if let Some(g) = goal {
            rims.push(NativeBlockRim {
                pos: g,
                argb: GOAL_ARGB,
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

/// `native_world_submit`: the red path line, translucent fills on break
/// targets and the goal, and waypoint name labels.
pub fn world(
    game: &crate::game::Game<'_>,
    draw: &mut crate::mod_api::NativeWorldDraw,
) {
    if !on(game) {
        return;
    }
    // Java's `renderPath`: a solid red line through the centres of the
    // upcoming path nodes (a thin cube per segment — no line primitive).
    if with(|s| s.path) {
        let cells = rt::with(|s| {
            s.path
                .iter()
                .skip(s.at)
                .take(MAX_NODES)
                .map(|m| m.pos)
                .collect::<Vec<_>>()
        });
        for w in cells.windows(2) {
            line_segment(game, draw, w[0], w[1]);
        }
    }
    if with(|s| s.goal) {
        // Green fills on blocks queued to break, white fill on the goal.
        for t in targets(game) {
            fill_box(game, draw, t, TARGET_FILL);
        }
        let goal = rt::with(|s| s.objective.or_else(|| s.path.last().map(|m| m.pos)));
        if let Some(g) = goal {
            fill_box(game, draw, g, GOAL_FILL);
        }
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
