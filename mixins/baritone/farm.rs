// Baritone port: FarmProcess — scan the columns around the player for
// farmland, harvest whatever crop is on it, replant from the hotbar and till
// bare dirt/grass with a hoe.
//
// `/b farm [radius]` (default 48, clamped 8..96), `/b farm off` to stop.
// `/b stop` is handled by the core: `rt::State::halt` clears `busy`, and the
// next farm tick notices and resets this module's statics.
//
// Deviations from desktop Baritone, by necessity:
//  - Block properties (crop age) aren't readable here, so HARVEST breaks any
//    matching crop block whether it is mature or not.
//  - "Matching" seeds can't be resolved crop->item, so any whitelisted seed
//    in the hotbar counts as matching.
//  - Ticks must run after the glue driver: when `proc` is `None` the core
//    applies a default `want` (wiping `forced_hit`), so this module writes
//    its `want` afterwards every tick.

use std::collections::HashSet;
use std::sync::Mutex;

use crate::game::umbx_baritone as rt;
use crate::game::umbx_baritone_exec as ex;
use crate::game::umbx_pathfind as pf;

/// `/b farm` default scan radius.
const DEFAULT_RADIUS: i32 = 48;
/// Vanilla-ish block reach; the path goal accepts any standable cell whose
/// centre is within this of the job cell.
const REACH: f64 = 4.2;
const REACH2: f64 = REACH * REACH;
/// Vertical band scanned above/below the player's feet.
const Y_BAND: i32 = 8;
/// Ticks spent acting on one job before it is given up on.
const ACT_TIMEOUT: u32 = 60;
/// Ticks without movement before re-planning / dropping a job.
const REPLAN_TICKS: u32 = 40;
const STUCK_TICKS: u32 = 120;
/// A radius-96 scan walks ~160k cells — hold off at least this long after
/// one so a drained queue in leaf-litter terrain doesn't rescan per tick.
const SCAN_CD: u32 = 30;
/// Keep only this many nearest jobs: the far tail of a big scan is what
/// takes all the time, and the near half is what actually gets worked.
const JOBS_CAP: usize = 256;

/// Crop block names we harvest (substring-matched on `StateFacts::name`).
const CROPS: [&str; 8] = [
    "wheat",
    "carrot",
    "potato",
    "beetroot",
    "nether_wart",
    "cocoa",
    "pumpkin",
    "melon",
];
/// Seed item names accepted for replanting (hotbar substring match).
const SEEDS: [&str; 7] = [
    "wheat_seeds",
    "carrot",
    "potato",
    "beetroot_seeds",
    "nether_wart",
    "pumpkin_seeds",
    "melon_seeds",
];

/// What one job does at its cell.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    /// Break the crop at `cell` (the block above farmland).
    Harvest,
    /// Right-click `cell`'s top face (farmland) with seeds selected.
    Replant,
    /// Right-click `cell`'s top face (dirt/grass) with a hoe selected.
    Till,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Job {
    kind: Kind,
    /// Crop cell for Harvest; the soil cell itself for Replant/Till.
    cell: [i32; 3],
}

struct Farm {
    running: bool,
    radius: i32,
    /// Remaining jobs, farthest first — `pop` yields the nearest.
    jobs: Vec<Job>,
    /// Jobs that proved unreachable/failing this run; rescans skip them.
    skip: HashSet<Job>,
    job: Option<Job>,
    path: Vec<pf::Move>,
    at: usize,
    /// Ticks spent acting on the current job.
    act: u32,
    stuck: u32,
    last_pos: [i32; 3],
    /// Ticks until the next rescan is allowed (`scan` is expensive).
    scan_cd: u32,
}

impl Farm {
    fn new(radius: i32) -> Self {
        Self {
            running: true,
            radius,
            jobs: Vec::new(),
            skip: HashSet::new(),
            job: None,
            path: Vec::new(),
            at: 0,
            act: 0,
            stuck: 0,
            last_pos: [i32::MIN; 3],
            scan_cd: 0,
        }
    }

    fn idle() -> Self {
        Self {
            running: false,
            ..Self::new(0)
        }
    }
}

static S: Mutex<Option<Farm>> = Mutex::new(None);

/// Run `f` with this module's state, creating it on first use.
fn with<R>(f: impl FnOnce(&mut Farm) -> R) -> R {
    let mut g = S.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(Farm::idle());
    }
    f(g.as_mut().unwrap())
}

fn feet(p: &crate::motion::PlayerMotion) -> [i32; 3] {
    [
        p.position.x.floor() as i32,
        p.position.y.floor() as i32,
        p.position.z.floor() as i32,
    ]
}

fn on(game: &crate::game::Game<'_>) -> bool {
    game.native_on(rt::ID)
}

/// The window's own game, not the single player server's.
fn client(game: &crate::game::Game<'_>) -> bool {
    game.native_window()
}

fn say(game: &mut crate::game::Game<'_>, m: &str) {
    game.native_chat(&format!("baritone: {m}"));
}

fn opts(s: &rt::State) -> pf::Opts {
    rt::opts(s)
}

/// Where the player can path from: their cell, else the nearest standable.
fn start_cell(game: &crate::game::Game<'_>) -> Option<[i32; 3]> {
    let w = pf::W::new(game);
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

/// Squared distance between two cell centres.
fn cell_dist2(a: [i32; 3], b: [i32; 3]) -> f64 {
    let dx = (a[0] - b[0]) as f64;
    let dy = (a[1] - b[1]) as f64;
    let dz = (a[2] - b[2]) as f64;
    dx * dx + dy * dy + dz * dz
}

/// Right-click/attack hit on a cell's top face, centred on that face.
fn hit_top(cell: [i32; 3]) -> crate::game::Hit {
    crate::game::Hit {
        pos: cell,
        face: [0, 1, 0],
        point: [
            cell[0] as f64 + 0.5,
            cell[1] as f64 + 1.0,
            cell[2] as f64 + 0.5,
        ],
    }
}

/// Face `at` (a world point) with the camera yaw.
fn aim(game: &mut crate::game::Game<'_>, at: [f64; 3]) {
    let Some(p) = game.player.as_ref() else { return };
    let dx = at[0] - p.position.x;
    let dz = at[2] - p.position.z;
    let yaw = (-dx.atan2(dz).to_degrees()) as f32;
    game.yaw = yaw;
    let p = game.player.as_mut().unwrap();
    p.yaw = yaw;
    p.head_yaw = yaw;
}

fn block_name<'a>(game: &'a crate::game::Game<'_>, p: [i32; 3]) -> &'a str {
    &game.res.facts().get(game.world.block(p[0], p[1], p[2])).name
}

fn crop(name: &str) -> bool {
    CROPS.iter().any(|n| name.contains(n))
}

fn tillable(name: &str) -> bool {
    name.contains("grass_block") || name.contains("dirt")
}

/// First hotbar slot (inventory[0..9]) holding a whitelisted seed item.
fn seed_slot(game: &crate::game::Game<'_>) -> Option<usize> {
    game.inventory.iter().take(9).position(|it| {
        it.as_ref().map_or(false, |it| {
            let n = it.item.as_ref();
            SEEDS.iter().any(|p| n.contains(p))
        })
    })
}

/// First hotbar slot holding a hoe.
fn hoe_slot(game: &crate::game::Game<'_>) -> Option<usize> {
    game.inventory
        .iter()
        .take(9)
        .position(|it| it.as_ref().map_or(false, |it| it.item.contains("_hoe")))
}

/// Can the job be attempted with the current hotbar?
fn equipped(game: &crate::game::Game<'_>, job: Job) -> bool {
    match job.kind {
        Kind::Harvest => true,
        Kind::Replant => seed_slot(game).is_some(),
        Kind::Till => hoe_slot(game).is_some(),
    }
}

/// The cell above `cell` is strictly air. Till/Replant need this rather
/// than `open_cell`: vanilla's use-action hits the plant on top (tall
/// grass, leaf litter), never the soil face — counting those cells as
/// jobs buried forests in unreachable till work.
fn air_above(game: &crate::game::Game<'_>, cell: [i32; 3]) -> bool {
    block_name(game, [cell[0], cell[1] + 1, cell[2]]).contains("air")
}

/// Does the job still need doing? Also the completion check: a Harvest job
/// stops being valid the tick the crop is gone, a Replant job the tick a
/// crop appears on the farmland, a Till job the tick the dirt is farmland.
fn valid(game: &crate::game::Game<'_>, job: Job) -> bool {
    match job.kind {
        Kind::Harvest => crop(block_name(game, job.cell)),
        Kind::Replant => {
            // Done once a crop sits on the farmland: crops have no full
            // collision, so `open_cell` alone can't see the planting.
            let above = [job.cell[0], job.cell[1] + 1, job.cell[2]];
            block_name(game, job.cell).contains("farmland")
                && air_above(game, job.cell)
                && !crop(block_name(game, above))
        }
        Kind::Till => {
            tillable(block_name(game, job.cell)) && air_above(game, job.cell)
        }
    }
}

/// Close enough to act: the player's feet cell is within reach of the job
/// cell, matching the path's done predicate.
fn near(game: &crate::game::Game<'_>, job: Job) -> bool {
    let Some(me) = game.player.as_ref().map(feet) else {
        return false;
    };
    cell_dist2(me, job.cell) <= REACH2
}

/// Rebuild the work list, nearest-first. A farmable column is a `farmland`
/// block with a crop or open cell above; `grass_block`/`dirt`/`dirt_path`
/// with an open cell above is tillable when a hoe is on the hotbar. Returns
/// the number of jobs found.
fn scan(game: &crate::game::Game<'_>) -> usize {
    let Some(player) = game.player.as_ref() else { return 0 };
    let me = feet(player);
    let (radius, skip) = with(|s| (s.radius, s.skip.clone()));
    let seeds = seed_slot(game).is_some();
    let hoe = hoe_slot(game).is_some();
    let (min_y, max_y) = (game.world.shape.min_y, game.world.shape.max_y());
    let mut jobs: Vec<(f64, Job)> = Vec::new();
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            for dy in -Y_BAND..=Y_BAND {
                let p = [me[0] + dx, me[1] + dy, me[2] + dz];
                if p[1] < min_y || p[1] > max_y {
                    continue;
                }
                let above = [p[0], p[1] + 1, p[2]];
                let name = block_name(game, p);
                let d2 = cell_dist2(me, p);
                if name.contains("farmland") {
                    if crop(block_name(game, above)) {
                        let j = Job { kind: Kind::Harvest, cell: above };
                        if !skip.contains(&j) {
                            jobs.push((d2, j));
                        }
                    } else if seeds && block_name(game, above).contains("air") {
                        let j = Job { kind: Kind::Replant, cell: p };
                        if !skip.contains(&j) {
                            jobs.push((d2, j));
                        }
                    }
                } else if hoe && tillable(name) && block_name(game, above).contains("air") {
                    let j = Job { kind: Kind::Till, cell: p };
                    if !skip.contains(&j) {
                        jobs.push((d2, j));
                    }
                }
            }
        }
    }
    // Nearest-first: sort ascending by distance, keep the closest
    // JOBS_CAP, store reversed so `pop` takes the closest job.
    jobs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    jobs.truncate(JOBS_CAP);
    let n = jobs.len();
    with(|s| {
        s.jobs = jobs.into_iter().map(|(_, j)| j).rev().collect();
    });
    n
}

/// Pop the next doable job, rescanning when the list runs dry. Chats
/// "farm done" and stops the process once a scan finds nothing.
fn advance(game: &mut crate::game::Game<'_>) {
    // At most one rescan per call; any leftovers wait for the next tick.
    for _ in 0..2 {
        match with(|s| s.jobs.pop()) {
            Some(job) => {
                if !valid(game, job) || !equipped(game, job) {
                    continue;
                }
                with(|s| {
                    s.job = Some(job);
                    s.path.clear();
                    s.at = 0;
                    s.act = 0;
                    s.stuck = 0;
                });
                return;
            }
            None => {
                // Rescans are the expensive part of farming; on cooldown
                // the queue just stays empty for a few ticks.
                if with(|s| {
                    if s.scan_cd > 0 {
                        s.scan_cd -= 1;
                        true
                    } else {
                        false
                    }
                }) {
                    return;
                }
                with(|s| s.scan_cd = SCAN_CD);
                if scan(game) == 0 {
                    say(game, "farm done");
                    stop(game);
                    return;
                }
            }
        }
    }
}

/// Act on the current job: hold attack on the crop, or select the right
/// hotbar slot and hold use on the soil's top face.
fn work(game: &mut crate::game::Game<'_>, job: Job) {
    let timed_out = with(|s| {
        s.act += 1;
        s.act > ACT_TIMEOUT
    });
    if timed_out {
        with(|s| {
            s.skip.insert(job);
            s.job = None;
            s.act = 0;
        });
        return;
    }
    let mut want = rt::Want::default();
    match job.kind {
        Kind::Harvest => {
            want.hit = Some(hit_top(job.cell));
            want.attack = true;
        }
        Kind::Replant => {
            let Some(slot) = seed_slot(game) else {
                // Out of seeds: drop the job, the rescan won't re-add it.
                with(|s| {
                    s.job = None;
                    s.act = 0;
                });
                return;
            };
            game.selected = slot;
            want.hit = Some(hit_top(job.cell));
            want.right = true;
        }
        Kind::Till => {
            let Some(slot) = hoe_slot(game) else {
                with(|s| {
                    s.job = None;
                    s.act = 0;
                });
                return;
            };
            game.selected = slot;
            want.hit = Some(hit_top(job.cell));
            want.right = true;
        }
    }
    if let Some(h) = want.hit {
        aim(game, h.point);
    }
    rt::with(|s| s.want = want);
}

/// One farming tick: keep a current job, path within reach of it, act.
fn step(game: &mut crate::game::Game<'_>) {
    let Some(player) = game.player.as_ref() else { return };
    let me = feet(player);

    if with(|s| s.job.is_none()) {
        advance(game);
        if !with(|s| s.running) {
            return;
        }
    }
    let Some(job) = with(|s| s.job) else { return };

    // Finished or invalidated jobs drop out; the next tick picks another.
    if !valid(game, job) {
        with(|s| {
            s.job = None;
            s.path.clear();
            s.at = 0;
            s.act = 0;
        });
        return;
    }

    if near(game, job) {
        work(game, job);
        return;
    }

    // Path to any standable cell whose centre is within reach of the job.
    if with(|s| s.at >= s.path.len()) {
        let w = pf::W::new(game);
        let Some(start) = start_cell(game) else { return };
        let o = rt::with(|s| opts(s));
        let target = job.cell;
        let path = pf::find(&w, start, target, &o, |p, _| {
            w.standable(p) && cell_dist2(p, target) <= REACH2
        });
        match path {
            Some(path) => with(|s| {
                s.path = path;
                s.at = 0;
                s.stuck = 0;
            }),
            None => {
                // Unreachable column: don't retry it this run.
                with(|s| {
                    s.skip.insert(job);
                    s.job = None;
                });
                return;
            }
        }
    }

    let mv = with(|s| s.path.get(s.at).cloned());
    let prev = with(|s| {
        if s.at == 0 {
            me
        } else {
            s.path.get(s.at - 1).map(|m| m.pos).unwrap_or(me)
        }
    });
    if let Some(m) = mv {
        match rt::with(|s| ex::drive(game, s, &m, prev)) {
            ex::Out::Done => {
                with(|s| {
                    s.at += 1;
                    s.stuck = 0;
                });
            }
            ex::Out::Failed => {
                with(|s| {
                    s.skip.insert(job);
                    s.job = None;
                    s.path.clear();
                    s.at = 0;
                });
                return;
            }
            ex::Out::Working => {}
        }
    }

    // Stuck watchdog, same thresholds as the core driver: re-plan at 40
    // still ticks, abandon the job at 120.
    let moved = me != with(|s| s.last_pos);
    let stuck = with(|s| {
        s.last_pos = me;
        if moved {
            s.stuck = 0;
        } else {
            s.stuck += 1;
        }
        s.stuck
    });
    if stuck == REPLAN_TICKS {
        with(|s| {
            s.path.clear();
            s.at = 0;
        });
    } else if stuck >= STUCK_TICKS {
        with(|s| {
            s.skip.insert(job);
            s.job = None;
            s.path.clear();
            s.at = 0;
            s.stuck = 0;
        });
    }
}

/// Start (or restart) the farm process.
fn start(game: &mut crate::game::Game<'_>, radius: i32) {
    let radius = radius.clamp(8, 96);
    // Fresh statics every start; take over the shared process slot.
    with(|s| *s = Farm::new(radius));
    rt::with(|s| {
        s.halt();
        s.busy = true;
    });
    let n = scan(game);
    if n == 0 {
        with(|s| s.running = false);
        rt::with(|s| s.busy = false);
        say(game, "nothing to farm in range — check seeds/hoe");
        return;
    }
    say(game, &format!("farming {n} jobs within {radius} blocks"));
}

/// Stop the process and release the shared input slot.
fn stop(game: &mut crate::game::Game<'_>) {
    with(|s| *s = Farm::idle());
    rt::with(|s| {
        s.busy = false;
        s.want = rt::Want::default();
    });
    ex::apply(game, &rt::Want::default());
}

/// `/b farm [radius]` / `/b farm off`.
pub fn command(game: &mut crate::game::Game<'_>, args: &[&str]) -> bool {
    if !on(game) || !client(game) {
        return false;
    }
    if args.first().copied() != Some("farm") {
        return false;
    }
    match args.get(1).copied() {
        None => start(game, DEFAULT_RADIUS),
        Some("off" | "stop") => {
            stop(game);
            say(game, "farm stopped");
        }
        Some(v) => match v.parse::<i32>() {
            Ok(r) => start(game, r),
            Err(_) => say(game, "/b farm [radius] — radius in blocks, 8..96"),
        },
    }
    true
}

/// Drive the process; cheap early-out while idle.
pub fn tick(game: &mut crate::game::Game<'_>) {
    if !with(|s| s.running) {
        return;
    }
    if !on(game) || !client(game) {
        // Mod off or another window: drop inputs so nothing lingers.
        rt::with(|s| s.want = rt::Want::default());
        ex::apply(game, &rt::Want::default());
        return;
    }
    // `/b stop`/halt cleared `busy`, or a core process took over.
    if rt::with(|s| !s.busy || s.proc != rt::Proc::None) {
        with(|s| *s = Farm::idle());
        rt::with(|s| s.want = rt::Want::default());
        ex::apply(game, &rt::Want::default());
        return;
    }
    // Honour `/b pause` like the core driver does.
    if rt::with(|s| s.set.pause) {
        rt::with(|s| s.want = rt::Want::default());
        ex::apply(game, &rt::Want::default());
        return;
    }
    // A module process must clear stale inputs before writing its own.
    rt::with(|s| s.want = rt::Want::default());
    step(game);
    let want = rt::with(|s| s.want);
    ex::apply(game, &want);
}

/// Settings page label: a single Farm toggle.
pub fn settings(game: &crate::game::Game<'_>, out: &mut Vec<String>) {
    let running = with(|s| s.running);
    out.push(format!("Farm: {}", if running { "on" } else { "off" }));
    let _ = game;
}

/// Our section's one button toggles the process.
pub fn click(game: &mut crate::game::Game<'_>, button: usize) -> bool {
    if button != 0 {
        return false;
    }
    if with(|s| s.running) {
        stop(game);
    } else {
        start(game, DEFAULT_RADIUS);
    }
    true
}

/// `/b help` contribution.
pub fn help() -> &'static [&'static str] {
    &[
        "/b farm [radius] — harvest, replant and till near you (default 48)",
        "/b farm off — stop farming",
    ]
}
