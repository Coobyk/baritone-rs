// Baritone port: path/goal/waypoint rendering. `visuals` feeds the native
// rim/light lists (path crumbs on each floor cell, a rim on the objective,
// a rim per waypoint); `world` draws waypoint name labels on the world
// canvas, flat on the marked cell like gamma's light-level numbers (the
// canvas culls back faces, so labels are invisible from below).
//
// Waypoints: the extras module owns `/b wp` and publishes the markers as
// the `wp_list` setting (`name=x,y,z;name=x,y,z`); this module re-reads it
// on a slow cadence and draws a rim + name label per entry.
//
// `/b render <path|goal|waypoints> [on|off]` toggles each layer; the flags
// persist as `render_path` / `render_goal` / `render_waypoints`.

use std::sync::Mutex;

use crate::game::umbx_baritone as rt;
use crate::game::umbx_pathfind as pf;
use crate::game::Game;


/// Path: Java Baritone's `colorCurrentPath` — a solid red line through
/// the upcoming nodes.
const LINE_ARGB: u32 = 0xFF_FF_00_00;
/// Line width in screen pixels.
const LINE_W: f32 = 2.0;
/// Blocks queued to break / the mine objective: translucent green fill and
/// wireframe rim, like Java's blocksToBreak highlight.
const TARGET_FILL_ARGB: u32 = 0x48_00_FF_3C;
const TARGET_RIM: u32 = 0xFF_00_FF_3C;
const TARGET_W: f32 = 2.0;
/// Cells queued to place on (Pillar/Bridge/Traverse/ParkourPlace floors):
/// Java's blocksToPlace highlight in orange, distinct from break's green.
const PLACE_FILL_ARGB: u32 = 0x48_FF_88_00;
const PLACE_RIM: u32 = 0xFF_FF_88_00;
/// Objective cell: translucent white fill + rim, like Java's goal box.
const GOAL_FILL_ARGB: u32 = 0x40_FF_FF_FF;
const GOAL_ARGB: u32 = 0xFF_22_CC_88;
const GOAL_W: f32 = 2.0;
/// Waypoints: blue rim + light name text.
const WP_ARGB: u32 = 0xFF_55_AA_FF;
const WP_W: f32 = 1.5;
const WP_TEXT: u32 = 0xFF_DD_EE_FF;
/// Most path nodes drawn per frame.
const MAX_NODES: usize = 256;
/// Most target blocks filled per frame.
const MAX_TARGETS: usize = 48;
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
    /// `(name, cell)` waypoint markers, fed by extras' `wp_list` setting.
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
    rt::enabled() && game.native_window()
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
    let p = game.setting(rt::MOD_ID, "render_path");
    let g = game.setting(rt::MOD_ID, "render_goal");
    let w = game.setting(rt::MOD_ID, "render_waypoints");
    let list = game.setting(rt::MOD_ID, "wp_list");
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

// ---- module contract --------------------------------------------------

/// Cells the remaining path moves will break vs place, split by `kind` —
/// Java's blocksToBreak/blocksToPlace sets. Kind, not a world-solid read,
/// so soft breakables (flowers, grass) draw and freshly-placed floors
/// don't vanish the tick the block lands. The process's objective cell
/// counts as a break target.
fn targets() -> (Vec<[i32; 3]>, Vec<[i32; 3]>) {
    let (breaks, mut places, objective) = rt::with(|s| {
        let mut b: Vec<[i32; 3]> = Vec::new();
        let mut p: Vec<[i32; 3]> = Vec::new();
        for m in s.path.iter().skip(s.at) {
            let dst = match m.kind {
                pf::Kind::Break | pf::Kind::Downward => &mut b,
                pf::Kind::Pillar
                | pf::Kind::Bridge
                | pf::Kind::Traverse
                | pf::Kind::ParkourPlace { .. } => &mut p,
                _ => continue,
            };
            for c in m.cells.iter().copied() {
                if !dst.contains(&c) {
                    dst.push(c);
                }
            }
        }
        (b, p, s.objective)
    });
    let mut breaks = breaks;
    if let Some(o) = objective {
        if !breaks.contains(&o) {
            breaks.push(o);
        }
    }
    breaks.truncate(MAX_TARGETS);
    places.truncate(MAX_TARGETS);
    (breaks, places)
}

/// A translucent filled box on `cell` — six two-sided quads, grown
/// slightly past the block faces so the fill doesn't z-fight the world
/// mesh (Java expands the selection box the same way).
fn fill_box(quads: &mut Vec<mod_api::WorldQuad>, cell: [i32; 3], color: u32) {
    const E: f64 = 0.008;
    const O: f64 = 1.0 + E * 2.0;
    const FACES: [[[f64; 3]; 4]; 6] = [
        [[0., 0., 0.], [O, 0., 0.], [O, 0., O], [0., 0., O]], // down
        [[0., O, 0.], [0., O, O], [O, O, O], [O, O, 0.]], // up
        [[O, 0., 0.], [O, O, 0.], [0., O, 0.], [0., 0., 0.]], // north
        [[0., 0., O], [O, 0., O], [O, O, O], [0., O, O]], // south
        [[0., 0., 0.], [0., O, 0.], [0., O, O], [0., 0., O]], // west
        [[O, 0., O], [O, O, O], [O, O, 0.], [O, 0., 0.]], // east
    ];
    let (x, y, z) = (
        cell[0] as f64 - E,
        cell[1] as f64 - E,
        cell[2] as f64 - E,
    );
    for f in FACES {
        quads.push(mod_api::WorldQuad::new(
            [
                [x + f[0][0], y + f[0][1], z + f[0][2]],
                [x + f[1][0], y + f[1][1], z + f[1][2]],
                [x + f[2][0], y + f[2][1], z + f[2][2]],
                [x + f[3][0], y + f[3][1], z + f[3][2]],
            ],
            color,
            mod_api::WorldDepth::Tested,
        ));
    }
}

/// Java's red path line: a `WorldLine` through the centres of the
/// upcoming path nodes — the geometry API has a real line primitive now.
fn line_segment(lines: &mut Vec<mod_api::WorldLine>, a: [i32; 3], b: [i32; 3]) {
    lines.push(mod_api::WorldLine::new(
        [a[0] as f64 + 0.5, a[1] as f64 + 0.5, a[2] as f64 + 0.5],
        [b[0] as f64 + 0.5, b[1] as f64 + 0.5, b[2] as f64 + 0.5],
        LINE_ARGB,
        LINE_W,
        mod_api::WorldDepth::Tested,
    ));
}

/// `native_visuals`: path crumbs, goal rim, waypoint rims.
pub fn visuals(
    game: &crate::game::Game<'_>,
    rims: &mut Vec<mod_api::BlockRim>,
    lights: &mut Vec<mod_api::ColoredLight>,
) {
    if !on(game) {
        return;
    }
    let (path_on, goal_on, wp_on) = with(|s| (s.path, s.goal, s.wps_on));
    if !path_on && !goal_on && !wp_on {
        return;
    }
    // Break/place queues (mine objectives and queued cells): the wireframe
    // part of Java's blocksToBreak/blocksToPlace highlights; the fills are
    // drawn on the world canvas in `world`.
    if goal_on {
        let (breaks, places) = targets();
        for t in breaks {
            rims.push(mod_api::BlockRim::new(t, TARGET_RIM, TARGET_W));
        }
        for t in places {
            rims.push(mod_api::BlockRim::new(t, PLACE_RIM, TARGET_W));
        }
        // Goal: the process's objective cell, else the last path node.
        let goal = rt::with(|s| s.objective.or_else(|| s.path.last().map(|m| m.pos)));
        if let Some(g) = goal {
            rims.push(mod_api::BlockRim::new(g, GOAL_ARGB, GOAL_W));
        }
    }
    if wp_on {
        with(|s| {
            for (_, pos) in s.wps.iter().take(MAX_WPS) {
                rims.push(mod_api::BlockRim::new(*pos, WP_ARGB, WP_W));
            }
        });
    }
    // No dynamic lights yet; the goal could get one later.
    let _ = lights;
}

/// `native_submit_custom_geometry`: the red path line, translucent fills
/// on break targets and the goal, and waypoint name labels.
pub fn world(
    game: &crate::game::Game<'_>,
    _view: &mod_api::WorldRenderView,
    draw: &mut horizon_client::world_geometry::WorldGeometry,
) {
    if !on(game) {
        return;
    }
    let mut lines: Vec<mod_api::WorldLine> = Vec::new();
    let mut quads: Vec<mod_api::WorldQuad> = Vec::new();
    // Java's `renderPath`: a solid red line through the centres of the
    // upcoming path nodes.
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
            line_segment(&mut lines, w[0], w[1]);
        }
    }
    if with(|s| s.goal) {
        // Green fills on blocks queued to break, orange on cells queued to
        // place, white fill on the goal.
        let (breaks, places) = targets();
        for t in breaks {
            fill_box(&mut quads, t, TARGET_FILL_ARGB);
        }
        for t in places {
            fill_box(&mut quads, t, PLACE_FILL_ARGB);
        }
        let goal = rt::with(|s| s.objective.or_else(|| s.path.last().map(|m| m.pos)));
        if let Some(g) = goal {
            fill_box(&mut quads, g, GOAL_FILL_ARGB);
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
        label(&mut quads, &name, pos);
    }
    let _ = draw.lines(&lines);
    let _ = draw.quads(&quads);
}

/// A waypoint's name drawn flat on the marked cell, readable from above —
/// `native_world_text` is gone, so each glyph is tiny quads of a 5x7
/// bitmap font, like the light-level overlay's digits.
fn label(quads: &mut Vec<mod_api::WorldQuad>, name: &str, pos: [i32; 3]) {
    // Glyph cell size in blocks; the text sits centred on the cell top.
    const PX: f64 = 0.09;
    let width = name.len() as f64 * 6.0 * PX - PX;
    let ox = pos[0] as f64 + 0.5 - width / 2.0;
    let oy = pos[1] as f64 + 0.02;
    let oz = pos[2] as f64 + 0.5 + 3.5 * PX;
    for (i, c) in name.chars().enumerate() {
        let Some(glyph) = glyph(c) else { continue };
        for (col, bits) in glyph.iter().enumerate() {
            for row in 0..7 {
                if bits & (1 << row) == 0 {
                    continue;
                }
                let x = ox + (i * 6 + col) as f64 * PX;
                let z = oz - row as f64 * PX;
                quads.push(mod_api::WorldQuad::new(
                    [
                        [x, oy, z - PX],
                        [x + PX, oy, z - PX],
                        [x + PX, oy, z],
                        [x, oy, z],
                    ],
                    WP_TEXT,
                    mod_api::WorldDepth::Tested,
                ));
            }
        }
    }
}

/// 5x7 bitmap glyphs (columns, bit 0 = top row) for waypoint names.
fn glyph(c: char) -> Option<[u8; 5]> {
    Some(match c.to_ascii_uppercase() {
        'A' => [0x7E, 0x11, 0x11, 0x11, 0x7E],
        'B' => [0x7F, 0x49, 0x49, 0x49, 0x36],
        'C' => [0x3E, 0x41, 0x41, 0x41, 0x22],
        'D' => [0x7F, 0x41, 0x41, 0x22, 0x1C],
        'E' => [0x7F, 0x49, 0x49, 0x49, 0x41],
        'F' => [0x7F, 0x09, 0x09, 0x09, 0x01],
        'G' => [0x3E, 0x41, 0x49, 0x49, 0x7A],
        'H' => [0x7F, 0x08, 0x08, 0x08, 0x7F],
        'I' => [0x00, 0x41, 0x7F, 0x41, 0x00],
        'J' => [0x20, 0x40, 0x41, 0x3F, 0x01],
        'K' => [0x7F, 0x08, 0x14, 0x22, 0x41],
        'L' => [0x7F, 0x40, 0x40, 0x40, 0x40],
        'M' => [0x7F, 0x02, 0x0C, 0x02, 0x7F],
        'N' => [0x7F, 0x04, 0x08, 0x10, 0x7F],
        'O' => [0x3E, 0x41, 0x41, 0x41, 0x3E],
        'P' => [0x7F, 0x09, 0x09, 0x09, 0x06],
        'Q' => [0x3E, 0x41, 0x51, 0x21, 0x5E],
        'R' => [0x7F, 0x09, 0x19, 0x29, 0x46],
        'S' => [0x46, 0x49, 0x49, 0x49, 0x31],
        'T' => [0x01, 0x01, 0x7F, 0x01, 0x01],
        'U' => [0x3F, 0x40, 0x40, 0x40, 0x3F],
        'V' => [0x1F, 0x20, 0x40, 0x20, 0x1F],
        'W' => [0x3F, 0x40, 0x38, 0x40, 0x3F],
        'X' => [0x63, 0x14, 0x08, 0x14, 0x63],
        'Y' => [0x07, 0x08, 0x70, 0x08, 0x07],
        'Z' => [0x61, 0x51, 0x49, 0x45, 0x43],
        '0' => [0x3E, 0x51, 0x49, 0x45, 0x3E],
        '1' => [0x00, 0x42, 0x7F, 0x40, 0x00],
        '2' => [0x42, 0x61, 0x51, 0x49, 0x46],
        '3' => [0x21, 0x41, 0x45, 0x4B, 0x31],
        '4' => [0x18, 0x14, 0x12, 0x7F, 0x10],
        '5' => [0x27, 0x45, 0x45, 0x45, 0x39],
        '6' => [0x3C, 0x4A, 0x49, 0x49, 0x30],
        '7' => [0x01, 0x71, 0x09, 0x05, 0x03],
        '8' => [0x36, 0x49, 0x49, 0x49, 0x36],
        '9' => [0x06, 0x49, 0x49, 0x29, 0x1E],
        '-' => [0x08, 0x08, 0x08, 0x08, 0x08],
        '_' => [0x40, 0x40, 0x40, 0x40, 0x40],
        '.' => [0x00, 0x60, 0x60, 0x00, 0x00],
        ':' => [0x00, 0x36, 0x36, 0x00, 0x00],
        ' ' => return None,
        _ => [0x7F, 0x41, 0x41, 0x41, 0x7F], // unknown: a box
    })
}

/// `/b render <path|goal|waypoints> [on|off]` (also `/b draw`).
pub fn command(game: &mut crate::game::Game<'_>, args: &[&str]) -> bool {
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
        let new = match args.get(1).map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("off" | "false" | "no" | "0") => false,
            Some("on" | "true" | "yes" | "1") => true,
            _ => !cur,
        };
        *key_slot(s, key).unwrap() = new;
        new
    });
    game.set_setting(rt::MOD_ID, key, if new { "true" } else { "false" });
    game.native_chat(&format!("baritone: {label} {}", onoff(new)));
}

/// `native_tick`: settings sync only — drawing happens in the two hooks.
pub fn tick(game: &mut crate::game::Game<'_>) {
    if !on(game) {
        return;
    }
    sync(game);
}

/// `/b help` contribution.
pub fn help() -> &'static [&'static str] {
    &["/b render <path|goal|waypoints> [on|off] — drawing toggles"]
}
