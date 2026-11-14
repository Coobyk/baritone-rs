// Baritone port: the build/edit executor (support module
// `umbx_baritone_build`) and the MCEdit `.schematic` loader.
//
// `Edit`s sit in a queue. While it is non-empty the driver sets `rt.busy`
// and `rt.objective` and runs a small goto-style loop: `pf::find` a path to
// a standable cell near the next edit, `ex::drive` each move, and once the
// edit is within reach Break via `forced_hit`+attack or Place via
// `forced_hit` on an adjacent solid block's face + right. `/b stop` (or the
// settings Stop) clears `rt.busy` via `halt()`, which parks the queue —
// `/b build` with no name resumes it.
//
// Ordering note: `game.forced_hit`/`left`/`right` are written every active
// tick through `ex::apply`, so this module's `tick` must run after the core
// `umbx_baritone_glue::tick` (which writes default wants while idle).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;

use crate::game::Game;
use crate::game::umbx_baritone as rt;
use crate::game::umbx_baritone_exec as ex;
use crate::game::umbx_pathfind as pf;

/// Blocks a player can hit (Baritone's 4.5).
const REACH: f64 = 4.5;
/// A standable cell within this squared distance of the edit is "there".
const NEAR: i64 = 12;

/// One queued world edit, pushed by `umbx_baritone_sel` (fill/replace/
/// walls/shell/cut/paste) and by the schematic loader.
#[derive(Clone)]
pub enum Edit {
    /// Mine the cell out (hold attack until it reads air; fluids and
    /// unbreakable blocks count as done — they cannot be mined).
    Break([i32; 3]),
    /// Put the block named `1` at `0` — a tail/`ends_with` match on
    /// `StateFacts::name` (e.g. `"stone"`, `"oak_log"`). A wrong solid block
    /// in the cell is mined out first.
    Place([i32; 3], String),
}

fn edit_pos(e: &Edit) -> [i32; 3] {
    match e {
        Edit::Break(p) | Edit::Place(p, _) => *p,
    }
}

struct Jobs {
    queue: VecDeque<Edit>,
    /// Settings-button pause (persisted as the `build_paused` setting).
    paused: bool,
    /// The edit the current path heads to.
    target: Option<[i32; 3]>,
    path: Vec<pf::Move>,
    at: usize,
    /// Stuck watchdog, mirroring the core driver's.
    stuck: u32,
    last_pos: [i32; 3],
    replanned: bool,
    /// Consecutive `pf::find` failures on `target`.
    misses: u32,
    /// `rt.busy` was set by this module last tick; finding it cleared means
    /// someone called `halt()` (`/b stop`, settings Stop) — park the queue.
    drove: bool,
    /// Edits dropped as unreachable or unsupportable, for the summary.
    skipped: u32,
    /// "approximate material" chat, once per queue refill.
    approx_warned: bool,
}

impl Jobs {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            paused: false,
            target: None,
            path: Vec::new(),
            at: 0,
            stuck: 0,
            last_pos: [i32::MIN; 3],
            replanned: false,
            misses: 0,
            drove: false,
            skipped: 0,
            approx_warned: false,
        }
    }
}

static JOBS: Mutex<Option<Jobs>> = Mutex::new(None);

fn jobs<R>(f: impl FnOnce(&mut Jobs) -> R) -> R {
    let mut g = JOBS.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(Jobs::new());
    }
    f(g.as_mut().unwrap())
}

/// Queue `edits` behind any pending ones and start driving.
pub fn enqueue(game: &Game<'_>, edits: Vec<Edit>) -> usize {
    let n = jobs(|j| {
        if j.queue.is_empty() {
            j.skipped = 0;
            j.approx_warned = false;
        }
        j.queue.extend(edits);
        j.paused = false;
        j.queue.len()
    });
    game.native_set_setting(rt::MOD_ID, "build_paused", "false");
    n
}

/// Edits still queued, for status lines.
pub fn pending() -> usize {
    jobs(|j| j.queue.len())
}

fn say(game: &mut Game<'_>, m: &str) {
    game.native_chat(&format!("baritone: {m}"));
}

fn feet(p: &crate::motion::PlayerMotion) -> [i32; 3] {
    [
        p.position.x.floor() as i32,
        p.position.y.floor() as i32,
        p.position.z.floor() as i32,
    ]
}

fn center(p: [i32; 3]) -> [f64; 3] {
    [p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5]
}

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
}

/// Squared cell distance in i64 (world-edge coordinates overflow i32²).
fn cell_dist2(a: [i32; 3], b: [i32; 3]) -> i64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    d[0] as i64 * d[0] as i64 + d[1] as i64 * d[1] as i64 + d[2] as i64 * d[2] as i64
}

/// Face `at` (a world point) with the camera yaw (same as the executor's).
fn aim(game: &mut Game<'_>, at: [f64; 3]) {
    let Some(p) = game.player.as_ref() else { return };
    let dx = at[0] - p.position.x;
    let dz = at[2] - p.position.z;
    let yaw = (-dx.atan2(dz).to_degrees()) as f32;
    let dy = at[1] - (p.position.y + 1.62);
    let pitch = (-dy.atan2(dx.hypot(dz)).to_degrees()) as f32;
    game.yaw = yaw;
    let p = game.player.as_mut().unwrap();
    p.yaw = yaw;
    p.head_yaw = yaw;
    let _ = pitch;
}

fn face_hit(cell: [i32; 3], face: [i32; 3]) -> crate::game::Hit {
    crate::game::Hit {
        pos: cell,
        face,
        point: [
            cell[0] as f64 + 0.5 + face[0] as f64 * 0.5,
            cell[1] as f64 + 0.5 + face[1] as f64 * 0.5,
            cell[2] as f64 + 0.5 + face[2] as f64 * 0.5,
        ],
    }
}

fn solid(game: &Game<'_>, p: [i32; 3]) -> bool {
    game.res
        .facts()
        .get(game.world.block(p[0], p[1], p[2]))
        .full_collision
}

fn is_air(name: &str) -> bool {
    name == "minecraft:air" || name.ends_with("_air")
}

fn tail(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// `ends_with` match on the name tail (`stone` also hits `sandstone`).
fn name_match(f_name: &str, want: &str) -> bool {
    tail(f_name).ends_with(want)
}

/// Where the player can stand from where they are (same as the core's).
fn start_cell(game: &Game<'_>) -> Option<[i32; 3]> {
    let w = pf::W { game };
    let feet = feet(game.player.as_ref()?);
    if w.standable(feet) {
        return Some(feet);
    }
    for dy in -2..=2 {
        for dx in -1..=1 {
            for dz in -1..=1 {
                let p = [feet[0] + dx, feet[1] + dy, feet[2] + dz];
                if w.standable(p) {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn opts(s: &rt::State) -> pf::Opts {
    pf::Opts {
        allow_break: s.set.allow_break,
        allow_place: s.set.allow_place,
        allow_parkour: s.set.allow_parkour,
        max_drop: s.set.max_drop,
    }
}

// ---------------------------------------------------------------------------
// Module interface
// ---------------------------------------------------------------------------

/// `command`: `build <name>` loads a schematic, `build` alone resumes the
/// queue, `schematica` prints the info line.
pub fn command(game: &mut Game<'_>, args: &[&str]) -> bool {
    let Some(head) = args.first() else { return false };
    if !matches!(head.to_ascii_lowercase().as_str(), "build" | "schematica" | "schem") {
        return false;
    }
    if !game.native_on(rt::ID) || !game.native_window() {
        return true;
    }
    match head.to_ascii_lowercase().as_str() {
        "build" => {
            build_cmd(game, &args[1..]);
            true
        }
        "schematica" | "schem" => {
            say(
                game,
                "schematica: /b build <name> loads schematics/<name>.schematic under the game dir or ~/.local/share/horizon/",
            );
            true
        }
        _ => false,
    }
}

/// Every client tick: reload the persisted pause flag, then drive the queue.
pub fn tick(game: &mut Game<'_>) {
    if !game.native_on(rt::ID) || !game.native_window() {
        return;
    }
    if let Some(v) = game.native_setting(rt::MOD_ID, "build_paused") {
        jobs(|j| j.paused = v == "true");
    }
    driver(game);
}

/// Settings-page button: `Build: <status>` toggles pause/resume.
pub fn settings(game: &Game<'_>, out: &mut Vec<String>) {
    let _ = game;
    let (n, paused) = jobs(|j| (j.queue.len(), j.paused));
    let status = if n == 0 {
        "idle".to_owned()
    } else if paused {
        format!("paused ({n} edits)")
    } else {
        format!("{n} edits left")
    };
    out.push(format!("Build: {status}"));
}

pub fn click(game: &mut Game<'_>, button: usize) -> bool {
    if button != 0 {
        return false;
    }
    let paused = jobs(|j| {
        j.paused = !j.paused;
        j.paused
    });
    game.native_set_setting(rt::MOD_ID, "build_paused", &paused.to_string());
    true
}

pub fn help() -> &'static [&'static str] {
    &[
        "/b build <name> — build a schematic file",
        "/b build — resume the queue  /b schematica — info",
    ]
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

fn driver(game: &mut Game<'_>) {
    let (proc, busy, pause) = rt::with(|s| (s.proc, s.busy, s.set.pause));
    // `/b stop`/settings Stop ran `halt()`: `busy` cleared under a running
    // queue. Park instead of re-driving (resume with `/b build`).
    if jobs(|j| j.drove) && !busy {
        jobs(|j| {
            j.drove = false;
            j.paused = true;
        });
        game.native_set_setting(rt::MOD_ID, "build_paused", "true");
    }
    if pause || jobs(|j| j.paused) {
        // Hold still but keep the queue (it resumes on unpause).
        jobs(|j| j.drove = false);
        rt::with(|s| {
            s.busy = false;
            s.objective = None;
            s.want = rt::Want::default();
        });
        ex::apply(game, &rt::Want::default());
        return;
    }
    let Some(player) = game.player.as_ref() else { return };
    let me = feet(player);
    let eye = [player.position.x, player.position.y + 1.62, player.position.z];

    // Drop satisfied edits and pick the nearest pending one.
    let pick = jobs(|j| {
        let mut i = 0;
        while i < j.queue.len() {
            if satisfied(game, &j.queue[i]) {
                j.queue.remove(i);
            } else {
                i += 1;
            }
        }
        let mut best: Option<(f64, usize)> = None;
        for (i, e) in j.queue.iter().enumerate() {
            let d = dist2(eye, center(edit_pos(e)));
            if best.map_or(true, |(b, _)| d < b) {
                best = Some((d, i));
            }
        }
        best.map(|(_, i)| i)
    });
    let Some(i) = pick else {
        // Queue drained.
        let finished = jobs(|j| {
            let was = j.drove;
            j.drove = false;
            j.target = None;
            was
        });
        // Only release the shared input slot when the queue was actually
        // driving — clearing it unconditionally would zero `s.want` every
        // tick and starve whatever process owns it (e.g. goto's Walk).
        if finished {
            rt::with(|s| {
                s.busy = false;
                s.objective = None;
                s.want = rt::Want::default();
            });
            ex::apply(game, &rt::Want::default());
            let skipped = jobs(|j| j.skipped);
            say(game, &format!("build done ({skipped} edits skipped)"));
        }
        return;
    };
    if proc != rt::Proc::None {
        // A core process (goto/mine/...) owns the inputs this tick; the
        // queue resumes when it finishes.
        return;
    }

    let pos = jobs(|j| edit_pos(&j.queue[i]));
    rt::with(|s| {
        s.busy = true;
        s.objective = Some(pos);
    });
    jobs(|j| j.drove = true);

    if dist2(eye, center(pos)) <= REACH * REACH {
        execute(game, i, pos, eye);
        return;
    }
    drive_to(game, i, pos, me);
}

/// True when the edit needs nothing more (Break: air/fluid/unbreakable;
/// Place: a matching block already sits there).
fn satisfied(game: &Game<'_>, e: &Edit) -> bool {
    let p = edit_pos(e);
    let f = game
        .res
        .facts()
        .get(game.world.block(p[0], p[1], p[2]));
    match e {
        Edit::Break(_) => {
            is_air(f.name.as_ref())
                || f.fluid != horizon_items::blocks::Fluid::None
                || f.hardness < 0.0
        }
        Edit::Place(_, name) => name_match(f.name.as_ref(), name),
    }
}

/// Remove a stuck/impossible edit and reset the path toward it.
fn drop_edit(i: usize) {
    jobs(|j| {
        if i < j.queue.len() {
            j.queue.remove(i);
            j.skipped += 1;
        }
        j.target = None;
        j.path.clear();
        j.at = 0;
        j.stuck = 0;
        j.misses = 0;
    });
}

/// The edit is in reach: attack it, or right-click a neighbour's face to
/// place the requested block.
fn execute(game: &mut Game<'_>, i: usize, pos: [i32; 3], eye: [f64; 3]) {
    let place = jobs(|j| match &j.queue[i] {
        Edit::Place(_, n) => Some(n.clone()),
        Edit::Break(_) => None,
    });
    let Some(name) = place else {
        attack(game, pos);
        return;
    };
    let (occupied, matching) = {
        let f = game
            .res
            .facts()
            .get(game.world.block(pos[0], pos[1], pos[2]));
        (
            f.full_collision && !f.replaceable,
            name_match(f.name.as_ref(), &name),
        )
    };
    if occupied && !matching {
        // Wrong block in the cell: mine it out, place next tick.
        attack(game, pos);
        return;
    }
    let Some(sup) = support(game, eye, pos) else {
        drop_edit(i);
        say(game, "no support block to place on — edit dropped");
        return;
    };
    if !select_block(game, &name) && !jobs(|j| j.approx_warned) {
        jobs(|j| j.approx_warned = true);
        say(
            game,
            &format!("no '{name}' in the hotbar — placing with an approximate block"),
        );
    }
    let face = [pos[0] - sup[0], pos[1] - sup[1], pos[2] - sup[2]];
    aim(game, center(pos));
    let w = rt::Want {
        hit: Some(face_hit(sup, face)),
        right: true,
        ..rt::Want::default()
    };
    rt::with(|s| s.want = w);
    ex::apply(game, &w);
}

/// Hold attack on `pos`'s top face until it clears.
fn attack(game: &mut Game<'_>, pos: [i32; 3]) {
    aim(game, center(pos));
    let w = rt::Want {
        hit: Some(face_hit(pos, [0, 1, 0])),
        attack: true,
        ..rt::Want::default()
    };
    rt::with(|s| s.want = w);
    ex::apply(game, &w);
}

/// A solid neighbour of `pos` to right-click, preferring the face turned
/// toward the player's eye.
fn support(game: &Game<'_>, eye: [f64; 3], pos: [i32; 3]) -> Option<[i32; 3]> {
    let mut dirs = [
        [0, -1, 0],
        [1, 0, 0],
        [-1, 0, 0],
        [0, 0, 1],
        [0, 0, -1],
        [0, 1, 0],
    ];
    let c = center(pos);
    // A support on the far side of the player shows the player the face
    // `pos - support`; rank by d dot (cell - eye).
    dirs.sort_by(|a, b| {
        let score = |d: [i32; 3]| {
            d[0] as f64 * (c[0] - eye[0])
                + d[1] as f64 * (c[1] - eye[1])
                + d[2] as f64 * (c[2] - eye[2])
        };
        score(*b).partial_cmp(&score(*a)).unwrap_or(Ordering::Equal)
    });
    dirs.iter()
        .map(|d| [pos[0] + d[0], pos[1] + d[1], pos[2] + d[2]])
        .find(|&n| solid(game, n))
}

/// Select the hotbar stack whose block's `StateFacts::item` maps to a state
/// named `name` (exact tail first, then `ends_with`). Falls back to any
/// placeable block; returns false in that case (approximate placement).
fn select_block(game: &mut Game<'_>, name: &str) -> bool {
    let slots = game.inventory.len().min(9);
    for slot in 0..slots {
        let Some(stack) = game.inventory[slot].as_ref() else {
            continue;
        };
        if stack.count == 0 {
            continue;
        }
        let exact = game
            .res
            .facts()
            .states
            .iter()
            .any(|f| f.item.as_ref() == stack.item.as_ref() && tail(f.name.as_ref()) == name);
        if exact {
            game.selected = slot;
            return true;
        }
    }
    for slot in 0..slots {
        let Some(stack) = game.inventory[slot].as_ref() else {
            continue;
        };
        if stack.count == 0 {
            continue;
        }
        let matched = game
            .res
            .facts()
            .states
            .iter()
            .any(|f| f.item.as_ref() == stack.item.as_ref() && name_match(f.name.as_ref(), name));
        if matched {
            game.selected = slot;
            return true;
        }
    }
    for slot in 0..slots {
        let Some(stack) = game.inventory[slot].as_ref() else {
            continue;
        };
        if stack.count == 0 {
            continue;
        }
        let placeable = game
            .res
            .facts()
            .states
            .iter()
            .any(|f| f.item.as_ref() == stack.item.as_ref() && f.full_collision);
        if placeable {
            game.selected = slot;
            return false;
        }
    }
    false
}

/// Path toward a standable cell within `NEAR` (squared) of `goal`, then run
/// the moves through `ex::drive` — the same loop the core driver runs.
fn drive_to(game: &mut Game<'_>, i: usize, goal: [i32; 3], me: [i32; 3]) {
    rt::with(|s| s.want = rt::Want::default());
    if jobs(|j| j.target != Some(goal)) {
        jobs(|j| {
            j.target = Some(goal);
            j.path.clear();
            j.at = 0;
            j.stuck = 0;
            j.replanned = false;
            j.misses = 0;
        });
    }
    if jobs(|j| j.at >= j.path.len()) {
        let o = rt::with(|s| opts(s));
        let found = {
            let w = pf::W { game };
            start_cell(game).and_then(|start| {
                pf::find(&w, start, goal, &o, |p, _| {
                    w.standable(p) && cell_dist2(p, goal) <= NEAR
                })
            })
        };
        match found {
            Some(path) => {
                let empty = path.is_empty();
                jobs(|j| {
                    j.path = path;
                    j.at = 0;
                    j.misses = 0;
                });
                if empty {
                    // Path-goal reached but just outside arm's reach — try
                    // anyway (`forced_hit` bypasses the crosshair check).
                    if let Some(eye) = game.player.as_ref().map(|p| {
                        [p.position.x, p.position.y + 1.62, p.position.z]
                    }) {
                        execute(game, i, goal, eye);
                        return;
                    }
                }
            }
            None => {
                let drop_it = jobs(|j| {
                    j.misses += 1;
                    j.misses >= 3
                });
                if drop_it {
                    drop_edit(i);
                    say(game, "can't reach an edit — dropped it");
                }
                let want = rt::with(|s| s.want);
                ex::apply(game, &want);
                return;
            }
        }
    }

    let step = jobs(|j| j.path.get(j.at).cloned());
    let prev = jobs(|j| if j.at == 0 { me } else { j.path[j.at - 1].pos });
    if let Some(m) = step {
        match rt::with(|s| ex::drive(game, s, &m, prev)) {
            ex::Out::Done => {
                jobs(|j| {
                    j.at += 1;
                    j.stuck = 0;
                });
            }
            ex::Out::Failed => {
                drop_edit(i);
                say(game, "can't do that move — edit dropped");
            }
            ex::Out::Working => {}
        }
    }

    // Stuck watchdog, like the core driver's (re-plan once, then drop).
    let moved = me != jobs(|j| j.last_pos);
    let stuck = jobs(|j| {
        j.last_pos = me;
        if moved {
            j.stuck = 0;
        } else {
            j.stuck += 1;
        }
        j.stuck
    });
    if stuck == 40 && !jobs(|j| j.replanned) {
        jobs(|j| {
            j.replanned = true;
            j.path.clear();
        });
    } else if stuck >= 120 {
        drop_edit(i);
        say(game, "stuck — edit dropped");
    }

    let want = rt::with(|s| s.want);
    ex::apply(game, &want);
}

// ---------------------------------------------------------------------------
// /b build and the .schematic loader
// ---------------------------------------------------------------------------

fn build_cmd(game: &mut Game<'_>, args: &[&str]) {
    let Some(name) = args.first() else {
        let n = pending();
        if n == 0 {
            say(game, "/b build <name> — files in schematics/ next to the game or ~/.local/share/horizon/schematics/");
            return;
        }
        jobs(|j| j.paused = false);
        game.native_set_setting(rt::MOD_ID, "build_paused", "false");
        say(game, &format!("build resumed ({n} edits)"));
        return;
    };
    match load_schematic(game, name) {
        Ok(msg) => say(game, &msg),
        Err(e) => say(game, &e),
    }
}

/// `<name>` as-is, then `schematics/<name>[.schematic]` under the game dir
/// (the working directory) and `$HORIZON_HOME`/ `~/.local/share/horizon`.
fn schematic_paths(name: &str) -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from(name)];
    let roots = [
        std::env::current_dir().ok(),
        std::env::var_os("HORIZON_HOME").map(PathBuf::from),
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".local/share/horizon")),
    ];
    for root in roots.into_iter().flatten() {
        out.push(root.join("schematics").join(name));
        out.push(
            root.join("schematics")
                .join(format!("{name}.schematic")),
        );
    }
    out
}

fn tag_int(t: Option<&horizon_world::nbt::Tag>) -> Option<i32> {
    match t {
        Some(horizon_world::nbt::Tag::Byte(v)) => Some(i32::from(*v)),
        Some(horizon_world::nbt::Tag::Short(v)) => Some(i32::from(*v)),
        Some(horizon_world::nbt::Tag::Int(v)) => Some(*v),
        _ => None,
    }
}

fn tag_bytes(t: Option<&horizon_world::nbt::Tag>) -> Option<&[u8]> {
    match t {
        Some(horizon_world::nbt::Tag::ByteArray(v)) => Some(v.as_slice()),
        _ => None,
    }
}

/// Read an MCEdit `.schematic` (gzip-or-plain NBT through
/// `horizon_world::datafix::files`, `Tag::Compound` fields Width/Height/
/// Length/Blocks/Data[/AddBlocks]) and enqueue it, bottom layer first,
/// nearest-within-layer. Schematic `air` cells clear whatever is loaded.
///
/// Legacy (id, data) pairs map through the built-in `legacy_name` table —
/// the datafixer's own `BLOCK_IDS` table is private — and any id with no
/// modern name is skipped and counted in the summary.
fn load_schematic(game: &mut Game<'_>, name: &str) -> Result<String, String> {
    let Some(path) = schematic_paths(name).into_iter().find(|p| p.is_file()) else {
        return Err(format!(
            "schematic '{name}' not found (tried {name} and schematics/{name}.schematic)"
        ));
    };
    let root = horizon_world::datafix::files::read_saved_dat(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .ok_or_else(|| format!("{}: empty", path.display()))?;
    let get = |k: &str| root.get(k);
    let (w, h, l) = match (
        tag_int(get("Width")),
        tag_int(get("Height")),
        tag_int(get("Length")),
    ) {
        (Some(w), Some(h), Some(l)) => (w, h, l),
        _ => {
            return Err(format!(
                "{}: not an MCEdit schematic (missing Width/Height/Length)",
                path.display()
            ));
        }
    };
    if w <= 0 || h <= 0 || l <= 0 || w as i64 * h as i64 * l as i64 > 4_000_000 {
        return Err("schematic is empty or too large (4M cells max)".to_owned());
    }
    let Some(blocks) = tag_bytes(get("Blocks")) else {
        return Err(format!("{}: no Blocks array", path.display()));
    };
    if blocks.len() < (w * h * l) as usize {
        return Err(format!("{}: Blocks shorter than W*H*L", path.display()));
    }
    let data = tag_bytes(get("Data")).unwrap_or(&[]);
    let add = tag_bytes(get("AddBlocks")).or_else(|| tag_bytes(get("Add")));
    let Some(me) = game.player.as_ref().map(feet) else {
        return Err("no player".to_owned());
    };

    // Modern block names that exist in this build, for skipping unknown ids.
    let known: HashSet<&str> = game
        .res
        .facts()
        .states
        .iter()
        .map(|f| tail(f.name.as_ref()))
        .collect();
    let mut cache: HashMap<(i32, u8), Option<&'static str>> = HashMap::new();

    let mut edits = Vec::new();
    let mut skipped = 0u32;
    let mut cleared = 0u32;
    for y in 0..h {
        // MCEdit order: index = (y * Length + z) * Width + x.
        let mut layer = Vec::new();
        for z in 0..l {
            for x in 0..w {
                let i = ((y * l + z) * w + x) as usize;
                let hi = add.map_or(0, |a| {
                    (i32::from(a.get(i >> 1).copied().unwrap_or(0)) >> ((i & 1) as i32 * 4))
                        & 0xF
                });
                let id = (hi << 8) | i32::from(blocks[i]);
                let d = data.get(i).copied().unwrap_or(0) & 0xF;
                layer.push(([x, y, z], id, d));
            }
        }
        // Nearest to the paste origin first within the layer.
        layer.sort_by_key(|(o, _, _)| {
            o[0] as i64 * o[0] as i64 + o[2] as i64 * o[2] as i64
        });
        for (o, id, d) in layer {
            let at = [me[0] + o[0], me[1] + o[1], me[2] + o[2]];
            if id == 0 {
                // Schematic air clears what is already there.
                let f = game
                    .res
                    .facts()
                    .get(game.world.block(at[0], at[1], at[2]));
                if !is_air(f.name.as_ref())
                    && f.hardness >= 0.0
                    && f.fluid == horizon_items::blocks::Fluid::None
                {
                    edits.push(Edit::Break(at));
                    cleared += 1;
                }
                continue;
            }
            let resolved = cache
                .entry((id, d))
                .or_insert_with(|| legacy_name(id, d).filter(|n| known.contains(*n)));
            match *resolved {
                Some(n) => edits.push(Edit::Place(at, n.to_string())),
                None => skipped += 1,
            }
        }
    }
    let n = enqueue(game, edits);
    Ok(format!(
        "{}: queued {n} edits ({skipped} unknown block ids skipped, {cleared} cells to clear)",
        path.display()
    ))
}

// ---------------------------------------------------------------------------
// Legacy (id, data) -> modern block name. Common blocks only; the vanilla
// datafixer table (`datafix::flat_tables::BLOCK_IDS`) is not `pub`, so
// unmapped ids are skipped and reported.
// ---------------------------------------------------------------------------

const WOOL: [&str; 16] = [
    "white_wool", "orange_wool", "magenta_wool", "light_blue_wool", "yellow_wool",
    "lime_wool", "pink_wool", "gray_wool", "light_gray_wool", "cyan_wool",
    "purple_wool", "blue_wool", "brown_wool", "green_wool", "red_wool", "black_wool",
];
const STAINED_GLASS: [&str; 16] = [
    "white_stained_glass", "orange_stained_glass", "magenta_stained_glass",
    "light_blue_stained_glass", "yellow_stained_glass", "lime_stained_glass",
    "pink_stained_glass", "gray_stained_glass", "light_gray_stained_glass",
    "cyan_stained_glass", "purple_stained_glass", "blue_stained_glass",
    "brown_stained_glass", "green_stained_glass", "red_stained_glass",
    "black_stained_glass",
];
const STAINED_GLASS_PANE: [&str; 16] = [
    "white_stained_glass_pane", "orange_stained_glass_pane",
    "magenta_stained_glass_pane", "light_blue_stained_glass_pane",
    "yellow_stained_glass_pane", "lime_stained_glass_pane", "pink_stained_glass_pane",
    "gray_stained_glass_pane", "light_gray_stained_glass_pane",
    "cyan_stained_glass_pane", "purple_stained_glass_pane", "blue_stained_glass_pane",
    "brown_stained_glass_pane", "green_stained_glass_pane", "red_stained_glass_pane",
    "black_stained_glass_pane",
];
const CARPET: [&str; 16] = [
    "white_carpet", "orange_carpet", "magenta_carpet", "light_blue_carpet",
    "yellow_carpet", "lime_carpet", "pink_carpet", "gray_carpet",
    "light_gray_carpet", "cyan_carpet", "purple_carpet", "blue_carpet",
    "brown_carpet", "green_carpet", "red_carpet", "black_carpet",
];
const TERRACOTTA_C: [&str; 16] = [
    "white_terracotta", "orange_terracotta", "magenta_terracotta",
    "light_blue_terracotta", "yellow_terracotta", "lime_terracotta",
    "pink_terracotta", "gray_terracotta", "light_gray_terracotta",
    "cyan_terracotta", "purple_terracotta", "blue_terracotta", "brown_terracotta",
    "green_terracotta", "red_terracotta", "black_terracotta",
];
const CONCRETE: [&str; 16] = [
    "white_concrete", "orange_concrete", "magenta_concrete", "light_blue_concrete",
    "yellow_concrete", "lime_concrete", "pink_concrete", "gray_concrete",
    "light_gray_concrete", "cyan_concrete", "purple_concrete", "blue_concrete",
    "brown_concrete", "green_concrete", "red_concrete", "black_concrete",
];
const CONCRETE_POWDER: [&str; 16] = [
    "white_concrete_powder", "orange_concrete_powder", "magenta_concrete_powder",
    "light_blue_concrete_powder", "yellow_concrete_powder", "lime_concrete_powder",
    "pink_concrete_powder", "gray_concrete_powder", "light_gray_concrete_powder",
    "cyan_concrete_powder", "purple_concrete_powder", "blue_concrete_powder",
    "brown_concrete_powder", "green_concrete_powder", "red_concrete_powder",
    "black_concrete_powder",
];
const SHULKER: [&str; 16] = [
    "white_shulker_box", "orange_shulker_box", "magenta_shulker_box",
    "light_blue_shulker_box", "yellow_shulker_box", "lime_shulker_box",
    "pink_shulker_box", "gray_shulker_box", "light_gray_shulker_box",
    "cyan_shulker_box", "purple_shulker_box", "blue_shulker_box",
    "brown_shulker_box", "green_shulker_box", "red_shulker_box",
    "black_shulker_box",
];
const GLAZED: [&str; 16] = [
    "white_glazed_terracotta", "orange_glazed_terracotta",
    "magenta_glazed_terracotta", "light_blue_glazed_terracotta",
    "yellow_glazed_terracotta", "lime_glazed_terracotta", "pink_glazed_terracotta",
    "gray_glazed_terracotta", "light_gray_glazed_terracotta",
    "cyan_glazed_terracotta", "purple_glazed_terracotta", "blue_glazed_terracotta",
    "brown_glazed_terracotta", "green_glazed_terracotta", "red_glazed_terracotta",
    "black_glazed_terracotta",
];
const STONE_SLAB: [&str; 8] = [
    "stone_slab", "sandstone_slab", "oak_slab", "cobblestone_slab", "brick_slab",
    "stone_brick_slab", "nether_brick_slab", "quartz_slab",
];
const WOOD_SLAB: [&str; 6] = [
    "oak_slab", "spruce_slab", "birch_slab", "jungle_slab", "acacia_slab",
    "dark_oak_slab",
];
const LOGS: [&str; 4] = ["oak_log", "spruce_log", "birch_log", "jungle_log"];
const LEAVES: [&str; 4] = [
    "oak_leaves", "spruce_leaves", "birch_leaves", "jungle_leaves",
];
const FLOWERS: [&str; 9] = [
    "poppy", "blue_orchid", "allium", "azure_bluet", "red_tulip", "orange_tulip",
    "white_tulip", "pink_tulip", "oxeye_daisy",
];
const INFESTED: [&str; 6] = [
    "infested_stone", "infested_cobblestone", "infested_stone_bricks",
    "infested_mossy_stone_bricks", "infested_cracked_stone_bricks",
    "infested_chiseled_stone_bricks",
];
const STONE_BRICKS: [&str; 4] = [
    "stone_bricks", "mossy_stone_bricks", "cracked_stone_bricks",
    "chiseled_stone_bricks",
];
const DOUBLE_PLANT: [&str; 6] = [
    "sunflower", "lilac", "tall_grass", "large_fern", "rose_bush", "peony",
];

fn legacy_name(id: i32, data: u8) -> Option<&'static str> {
    let d = (data & 0xF) as usize;
    Some(match id {
        1 => "stone",
        2 => "grass_block",
        3 => "dirt",
        4 => "cobblestone",
        5 => WOOD_SLAB_PLANKS[d.min(5)],
        6 => SAPLINGS[d.min(5)],
        7 => "bedrock",
        8 | 9 => "water",
        10 | 11 => "lava",
        12 => "sand",
        13 => "gravel",
        14 => "gold_ore",
        15 => "iron_ore",
        16 => "coal_ore",
        17 => LOGS[d & 3],
        18 => LEAVES[d & 3],
        19 => "sponge",
        20 => "glass",
        21 => "lapis_ore",
        22 => "lapis_block",
        23 => "dispenser",
        24 => match d {
            1 => "chiseled_sandstone",
            2 => "smooth_sandstone",
            _ => "sandstone",
        },
        25 => "note_block",
        27 => "powered_rail",
        28 => "detector_rail",
        29 => "sticky_piston",
        30 => "cobweb",
        31 => if d == 2 { "fern" } else { "short_grass" },
        32 => "dead_bush",
        33 => "piston",
        35 => WOOL[d.min(15)],
        37 => "dandelion",
        38 => FLOWERS[d.min(8)],
        39 => "brown_mushroom",
        40 => "red_mushroom",
        41 => "gold_block",
        42 => "iron_block",
        43 | 44 => STONE_SLAB[(d & 7).min(7)],
        45 => "bricks",
        46 => "tnt",
        47 => "bookshelf",
        48 => "mossy_cobblestone",
        49 => "obsidian",
        50 => "torch",
        52 => "spawner",
        53 => "oak_stairs",
        54 => "chest",
        55 => "redstone_wire",
        56 => "diamond_ore",
        57 => "diamond_block",
        58 => "crafting_table",
        59 => "wheat",
        60 => "farmland",
        61 | 62 => "furnace",
        63 | 68 => "oak_sign",
        64 => "oak_door",
        65 => "ladder",
        66 => "rail",
        67 => "cobblestone_stairs",
        69 => "lever",
        70 => "stone_pressure_plate",
        71 => "iron_door",
        72 => "oak_pressure_plate",
        73 | 74 => "redstone_ore",
        75 | 76 => "redstone_torch",
        77 => "stone_button",
        78 => "snow",
        79 => "ice",
        80 => "snow_block",
        81 => "cactus",
        82 => "clay",
        83 => "sugar_cane",
        84 => "jukebox",
        85 => "oak_fence",
        86 => "pumpkin",
        87 => "netherrack",
        88 => "soul_sand",
        89 => "glowstone",
        91 => "jack_o_lantern",
        92 => "cake",
        93 | 94 => "repeater",
        95 => STAINED_GLASS[d.min(15)],
        96 => "oak_trapdoor",
        97 => INFESTED[d.min(5)],
        98 => STONE_BRICKS[d.min(3)],
        99 => "brown_mushroom_block",
        100 => "red_mushroom_block",
        101 => "iron_bars",
        102 => "glass_pane",
        103 => "melon",
        104 => "pumpkin_stem",
        105 => "melon_stem",
        106 => "vine",
        107 => "oak_fence_gate",
        108 => "brick_stairs",
        109 => "stone_brick_stairs",
        110 => "mycelium",
        111 => "lily_pad",
        112 => "nether_bricks",
        113 => "nether_brick_fence",
        114 => "nether_brick_stairs",
        115 => "nether_wart",
        116 => "enchanting_table",
        117 => "brewing_stand",
        118 => "cauldron",
        120 => "end_portal_frame",
        121 => "end_stone",
        122 => "dragon_egg",
        123 | 124 => "redstone_lamp",
        125 | 126 => WOOD_SLAB[d.min(5)],
        127 => "cocoa",
        128 => "sandstone_stairs",
        129 => "emerald_ore",
        130 => "ender_chest",
        131 => "tripwire_hook",
        133 => "emerald_block",
        134 => "spruce_stairs",
        135 => "birch_stairs",
        136 => "jungle_stairs",
        137 => "command_block",
        138 => "beacon",
        139 => if d & 1 == 1 { "mossy_cobblestone_wall" } else { "cobblestone_wall" },
        140 => "flower_pot",
        141 => "carrots",
        142 => "potatoes",
        143 => "oak_button",
        144 => "skeleton_skull",
        145 => match d & 3 {
            1 => "chipped_anvil",
            2 => "damaged_anvil",
            _ => "anvil",
        },
        146 => "trapped_chest",
        147 => "light_weighted_pressure_plate",
        148 => "heavy_weighted_pressure_plate",
        149 | 150 => "comparator",
        151 | 178 => "daylight_detector",
        152 => "redstone_block",
        153 => "nether_quartz_ore",
        154 => "hopper",
        155 => match d {
            1 => "chiseled_quartz_block",
            2 => "quartz_pillar",
            _ => "quartz_block",
        },
        156 => "quartz_stairs",
        157 => "activator_rail",
        158 => "dropper",
        159 => TERRACOTTA_C[d.min(15)],
        160 => STAINED_GLASS_PANE[d.min(15)],
        161 => if d & 1 == 1 { "dark_oak_leaves" } else { "acacia_leaves" },
        162 => if d & 1 == 1 { "dark_oak_log" } else { "acacia_log" },
        163 => "acacia_stairs",
        164 => "dark_oak_stairs",
        165 => "slime_block",
        166 => "barrier",
        167 => "iron_trapdoor",
        168 => match d {
            1 => "prismarine_bricks",
            2 => "dark_prismarine",
            _ => "prismarine",
        },
        169 => "sea_lantern",
        170 => "hay_block",
        171 => CARPET[d.min(15)],
        172 => "terracotta",
        173 => "coal_block",
        174 => "packed_ice",
        175 => DOUBLE_PLANT[d.min(5)],
        176 | 177 => "white_banner",
        179 => if d == 1 { "chiseled_red_sandstone" } else { "red_sandstone" },
        180 => "red_sandstone_stairs",
        181 | 182 => "red_sandstone_slab",
        183 => "spruce_fence_gate",
        184 => "birch_fence_gate",
        185 => "jungle_fence_gate",
        186 => "dark_oak_fence_gate",
        187 => "acacia_fence_gate",
        188 => "spruce_fence",
        189 => "birch_fence",
        190 => "jungle_fence",
        191 => "dark_oak_fence",
        192 => "acacia_fence",
        193 => "spruce_door",
        194 => "birch_door",
        195 => "jungle_door",
        196 => "acacia_door",
        197 => "dark_oak_door",
        198 => "end_rod",
        199 => "chorus_plant",
        200 => "chorus_flower",
        201 => "purpur_block",
        202 => "purpur_pillar",
        203 => "purpur_stairs",
        204 | 205 => "purpur_slab",
        206 => "end_stone_bricks",
        207 => "beetroots",
        208 => "dirt_path",
        210 => "repeating_command_block",
        211 => "chain_command_block",
        212 => "ice",
        213 => "magma_block",
        214 => "nether_wart_block",
        215 => "red_nether_bricks",
        216 => "bone_block",
        217 => "structure_void",
        218 => "observer",
        219..=234 => SHULKER[(id - 219) as usize],
        235..=250 => GLAZED[(id - 235) as usize],
        251 => CONCRETE[d.min(15)],
        252 => CONCRETE_POWDER[d.min(15)],
        255 => "structure_block",
        _ => return None,
    })
}

const WOOD_SLAB_PLANKS: [&str; 6] = [
    "oak_planks", "spruce_planks", "birch_planks", "jungle_planks",
    "acacia_planks", "dark_oak_planks",
];
const SAPLINGS: [&str; 6] = [
    "oak_sapling", "spruce_sapling", "birch_sapling", "jungle_sapling",
    "acacia_sapling", "dark_oak_sapling",
];
