// Baritone extras: the parity commands that sit beside the core process
// model — waypoints, a stored `/b goal` (+ invert), find, surface/top,
// proc/eta, a break blacklist, and Baritone's `/b set` name list stored as
// no-op values so real setting names never error.
//
// Persistence (native_setting under "baritone"):
//   wp_names        comma-joined waypoint names (index for enumeration)
//   wp_<name>       "x,y,z" waypoint cell
//   blacklist       comma-joined block-name substrings
//   cfg_<Name>      stored value of a Baritone-named /b set entry
//
// Module contract (MODULES.md): command/tick/settings/click/help. Movement
// is driven only by starting `rt::Proc::Goto`, so `s.busy` stays untouched.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::game::umbx_baritone as rt;
use crate::game::umbx_pathfind as pf;
use crate::game::Game;

/// Baritone setting names accepted as stored no-ops (`cfg_<name>`). The
/// string is the default reported by `/b set`, `/b modified` and `/b reset`.
/// Names that map onto a real setting (allowBreak, allowSprint, allowPlace,
/// allowParkour, allowDownward, allowVines, the diagonals, assumeStep,
/// sprintInWater, toolSelect, allowWalkOnBottomSlab) are handled by
/// `set_real` before this table is consulted. `buildIgnoreBlocks`,
/// `buildSubstitutes` and `exploreForBlocks` are stored here but read by
/// build/explore logic — real, just cfg-backed.
const CFG_DEFAULTS: &[(&str, &str)] = &[
    ("allowInventory", "false"),
    ("buildIgnoreBlocks", ""),
    ("buildSubstitutes", ""),
    ("breakFromAbove", "false"),
    ("chatControl", "true"),
    ("colorCurrentPath", "255,0,0"),
    ("colorGoalBox", "255,255,255"),
    ("exploreForBlocks", "true"),
    ("freeLook", "true"),
    ("renderCachedChunks", "false"),
    ("strictDirectionCheck", "false"),
    ("yLevelBoxSize", "4"),
];

#[derive(Default)]
struct S {
    /// name -> feet cell; loaded from `wp_names` + `wp_<name>` once.
    wps: HashMap<String, [i32; 3]>,
    /// Block-name substrings Break moves must never touch.
    blacklist: HashSet<String>,
    /// `blocksToAvoid`: name substrings paths pay extra to walk through
    /// (webs, sweet-berry bushes) but may still traverse.
    avoid: HashSet<String>,
    /// Baritone setting name -> stored value (defaults filled on load).
    cfg: HashMap<String, String>,
    /// `/b goal` target, waiting for `/b path`.
    goal: Option<[i32; 3]>,
    /// `/b invert`: the next `/b path` runs away from the goal instead.
    invert: bool,
    /// Persisted state has been loaded this session.
    loaded: bool,
    /// `visited` count at last save — autosave skips when unchanged.
    visited_saved: usize,
}

static S: Mutex<Option<S>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut S) -> R) -> R {
    let mut g = S.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(S::default());
    }
    f(g.as_mut().unwrap())
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

fn parse_pos(v: &str) -> Option<[i32; 3]> {
    let mut it = v.split(',');
    let x = it.next()?.parse().ok()?;
    let y = it.next()?.parse().ok()?;
    let z = it.next()?.parse().ok()?;
    Some([x, y, z])
}

fn flip(b: bool, v: Option<&str>) -> bool {
    match v {
        Some("off" | "false" | "no" | "0") => false,
        Some("on" | "true" | "yes" | "1") => true,
        Some(_) => b,
        None => !b,
    }
}

fn cfg_lookup(name: &str) -> Option<(&'static str, &'static str)> {
    CFG_DEFAULTS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .copied()
}

/// The defaults `rt::State::new` uses (its `Settings` differs from the
/// derived `Default`), for `/b modified` and `/b reset`.
fn defaults() -> rt::Settings {
    rt::Settings {
        sprint: true,
        allow_break: true,
        allow_place: true,
        allow_parkour: true,
        max_drop: 4,
        mine_radius: 32,
        mine_scan_drops: true,
        exposed_ores: false,
        follow_radius: 3,
        legit_mine: false,
        assume_water: false,
        assume_lava: false,
        mob_avoid_r: 0,
        mob_avoid_c: 1.5,
        parkour_place: false,
        downward: true,
        vines: true,
        diag_ascend: false,
        diag_descend: false,
        assume_step: false,
        sprint_water: true,
        tool_select: true,
        bottom_slab: true,
        pause: false,
    }
}

/// Persist the six real settings (`rt::to_save` keys).
fn save_settings(game: &Game<'_>) {
    for (k, v) in rt::to_save() {
        game.native_set_setting(rt::MOD_ID, k, &v);
    }
}

/// Write `wp_names` and every `wp_<name>`. Deleted waypoints leave a stale
/// `wp_<name>` key that is never read — the index is authoritative.
fn save_wps(game: &Game<'_>) {
    let mut items: Vec<(String, [i32; 3])> =
        with(|s| s.wps.iter().map(|(n, p)| (n.clone(), *p)).collect());
    items.sort();
    let names = items
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(",");
    game.native_set_setting(rt::MOD_ID, "wp_names", &names);
    // `wp_list`: the render module's marker source (`name=x,y,z;...`).
    let list = items
        .iter()
        .map(|(n, p)| format!("{n}={},{},{}", p[0], p[1], p[2]))
        .collect::<Vec<_>>()
        .join(";");
    game.native_set_setting(rt::MOD_ID, "wp_list", &list);
    for (n, p) in items {
        game.native_set_setting(
            rt::MOD_ID,
            &format!("wp_{n}"),
            &format!("{},{},{}", p[0], p[1], p[2]),
        );
    }
}

fn save_blacklist(game: &Game<'_>) {
    let mut v: Vec<String> = with(|s| s.blacklist.iter().cloned().collect());
    v.sort();
    game.native_set_setting(rt::MOD_ID, "blacklist", &v.join(","));
}

fn save_avoid(game: &Game<'_>) {
    let mut v: Vec<String> = with(|s| s.avoid.iter().cloned().collect());
    v.sort();
    game.native_set_setting(rt::MOD_ID, "avoid", &v.join(","));
}

/// `explorefilter`'s backing store: every visited chunk as "x,z;x,z;…".
fn save_visited(game: &Game<'_>) {
    let mut v: Vec<(i32, i32)> = rt::with(|s| s.visited.iter().copied().collect());
    v.sort();
    let text = v
        .iter()
        .map(|(x, z)| format!("{x},{z}"))
        .collect::<Vec<_>>()
        .join(";");
    game.native_set_setting(rt::MOD_ID, "visited", &text);
    let n = v.len();
    with(|s| s.visited_saved = n);
}

/// First-tick load of everything `native_setting` holds for this module.
fn load_once(game: &Game<'_>) {
    if with(|s| s.loaded) {
        return;
    }
    let mut wps = HashMap::new();
    if let Some(names) = game.native_setting(rt::MOD_ID, "wp_names") {
        for n in names.split(',') {
            if n.is_empty() {
                continue;
            }
            if let Some(v) = game.native_setting(rt::MOD_ID, &format!("wp_{n}")) {
                if let Some(p) = parse_pos(&v) {
                    wps.insert(n.to_owned(), p);
                }
            }
        }
    }
    let blacklist: HashSet<String> = game
        .native_setting(rt::MOD_ID, "blacklist")
        .unwrap_or_default()
        .split(',')
        .filter(|n| !n.is_empty())
        .map(|n| n.to_ascii_lowercase())
        .collect();
    let avoid: HashSet<String> = game
        .native_setting(rt::MOD_ID, "avoid")
        .unwrap_or_default()
        .split(',')
        .filter(|n| !n.is_empty())
        .map(|n| n.to_ascii_lowercase())
        .collect();
    // `explorefilter`'s chunk list: "x,z;x,z;…" like upstream's file.
    let visited: HashSet<(i32, i32)> = game
        .native_setting(rt::MOD_ID, "visited")
        .unwrap_or_default()
        .split(';')
        .filter_map(|pair| {
            let (x, z) = pair.split_once(',')?;
            Some((x.parse().ok()?, z.parse().ok()?))
        })
        .collect();
    let mut cfg = HashMap::new();
    for &(name, def) in CFG_DEFAULTS {
        let v = game
            .native_setting(rt::MOD_ID, &format!("cfg_{name}"))
            .unwrap_or_else(|| def.to_owned());
        cfg.insert(name.to_owned(), v);
    }
    let n_visited = visited.len();
    rt::with(|s| s.visited = visited);
    with(|s| {
        s.wps = wps;
        s.blacklist = blacklist;
        s.avoid = avoid;
        s.cfg = cfg;
        s.loaded = true;
        s.visited_saved = n_visited;
    });
}

/// Other modules ask before breaking: is `name` (a `StateFacts::name` or a
/// bare block id) off-limits? Entries substring-match like `mine` names do.
pub fn blacklisted(name: &str) -> bool {
    let n = name.trim_start_matches("minecraft:").to_ascii_lowercase();
    with(|s| s.blacklist.iter().any(|b| n.contains(b.as_str())))
}

/// `blocksToAvoid`: true when `name` matches an avoid entry — traversal
/// stays legal, the planner just prices the cells higher.
pub fn avoided(name: &str) -> bool {
    let n = name.trim_start_matches("minecraft:").to_ascii_lowercase();
    !n.is_empty() && with(|s| s.avoid.iter().any(|b| n.contains(b.as_str())))
}

/// Waypoint names for chat completion (`glue::tab_complete`).
pub fn wp_names(game: &Game<'_>) -> Vec<String> {
    load_once(game);
    with(|s| s.wps.keys().cloned().collect())
}

/// Every `/b set` key — real settings, their aliases and the cfg-backed
/// ones — for chat completion and `/b reset`/`/b modified`.
pub fn setting_names() -> Vec<String> {
    let mut out: Vec<String> = REAL_SETTING_NAMES
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    out.extend(CFG_DEFAULTS.iter().map(|(n, _)| (*n).to_owned()));
    out
}

/// Canonical (first the names `/b set` prints, then accepted aliases)
/// real-setting names for completion.
const REAL_SETTING_NAMES: &[&str] = &[
    "allowSprint",
    "allowBreak",
    "allowPlace",
    "allowParkour",
    "allowParkourPlace",
    "maxFallHeight",
    "maxFallHeightNoWater",
    "mineRadius",
    "mineScanDroppedItems",
    "onlyExposedOres",
    "followRadius",
    "legitMine",
    "assumeWalkOnWater",
    "assumeWalkOnLava",
    "mobAvoidanceRadius",
    "mobAvoidanceCoefficient",
    "allowDownward",
    "allowVines",
    "allowDiagonalAscend",
    "allowDiagonalDescend",
    "assumeStep",
    "sprintInWater",
    "toolSelect",
    "allowWalkOnBottomSlab",
    "sprint",
    "break",
    "place",
    "parkour",
    "parkourPlace",
    "drop",
    "radius",
    "drops",
    "exposed",
    "followR",
    "legit",
    "water",
    "lava",
    "mobR",
    "mobC",
    "down",
    "vines",
    "dAsc",
    "dDesc",
    "step",
    "sprintW",
    "tool",
    "slab",
];

/// A stored setting's value by Baritone name (`buildIgnoreBlocks` etc.).
pub fn cfg_get(name: &str) -> Option<String> {
    with(|s| s.cfg.get(name).cloned())
}

// ---- commands -------------------------------------------------------------

fn wp(game: &mut Game<'_>, args: &[&str]) {
    match args.first().map(|a| a.to_ascii_lowercase()).as_deref() {
        Some("save") => {
            let Some(&name) = args.get(1) else {
                say(game, "/b wp save <name>");
                return;
            };
            let name = name.to_ascii_lowercase();
            if name.contains(',') || name == "names" {
                say(game, "bad waypoint name");
                return;
            }
            let Some(f) = game.player.as_ref().map(feet) else {
                return;
            };
            with(|s| s.wps.insert(name.clone(), f));
            save_wps(game);
            say(game, &format!("saved '{name}' at {} {} {}", f[0], f[1], f[2]));
        }
        Some("goto") => {
            let Some(&name) = args.get(1) else {
                say(game, "/b wp goto <name>");
                return;
            };
            let name = name.to_ascii_lowercase();
            match with(|s| s.wps.get(&name).copied()) {
                Some(goal) => {
                    rt::with(|s| s.begin(rt::Proc::Goto { goal }));
                    say(
                        game,
                        &format!(
                            "going to '{name}' at {} {} {}",
                            goal[0], goal[1], goal[2]
                        ),
                    );
                }
                None => say(game, &format!("no waypoint '{name}'")),
            }
        }
        Some("delete") | Some("del") | Some("remove") => {
            let Some(&name) = args.get(1) else {
                say(game, "/b wp delete <name>");
                return;
            };
            let name = name.to_ascii_lowercase();
            let had = with(|s| s.wps.remove(&name).is_some());
            save_wps(game);
            if had {
                say(game, &format!("deleted '{name}'"));
            } else {
                say(game, &format!("no waypoint '{name}'"));
            }
        }
        Some("clear") => {
            with(|s| s.wps.clear());
            save_wps(game);
            say(game, "waypoints cleared");
        }
        Some("list") | None => {
            let mut items: Vec<(String, [i32; 3])> =
                with(|s| s.wps.iter().map(|(n, p)| (n.clone(), *p)).collect());
            items.sort();
            if items.is_empty() {
                say(game, "no waypoints — /b wp save <name>");
                return;
            }
            say(game, &format!("{} waypoint(s):", items.len()));
            for (n, p) in items {
                say(game, &format!("  {n}: {} {} {}", p[0], p[1], p[2]));
            }
        }
        Some(other) => say(
            game,
            &format!("unknown wp subcommand '{other}' — save|list|goto|delete|clear"),
        ),
    }
}

fn goal(game: &mut Game<'_>, args: &[&str]) {
    if args.is_empty() {
        let (g, inv) = with(|s| (s.goal, s.invert));
        match g {
            Some(g) => say(
                game,
                &format!(
                    "goal: {} {} {}{}",
                    g[0],
                    g[1],
                    g[2],
                    if inv { " (inverted)" } else { "" }
                ),
            ),
            None => say(game, "no goal set — /b goal <x> <z> | <x> <y> <z>"),
        }
        return;
    }
    if args.first() == Some(&"clear") {
        with(|s| s.goal = None);
        say(game, "goal cleared");
        return;
    }
    let ints: Vec<Option<i32>> = args.iter().map(|a| a.parse::<i32>().ok()).collect();
    let got = match ints.as_slice() {
        [Some(x), Some(y), Some(z)] => Some([*x, *y, *z]),
        [Some(x), Some(z)] => {
            let ny = game
                .player
                .as_ref()
                .map(feet)
                .map(|f| f[1])
                .unwrap_or(64);
            let w = pf::W { game };
            pf::column_top(&w, *x, *z, ny)
        }
        _ => None,
    };
    match got {
        Some(g) => {
            with(|s| s.goal = Some(g));
            say(
                game,
                &format!("goal set to {} {} {} — /b path to walk it", g[0], g[1], g[2]),
            );
        }
        None => say(game, "/b goal <x> <z> | <x> <y> <z>"),
    }
}

/// `/b path`: goto the stored goal, or away from it when inverted. Returns
/// false with no stored goal so the core's repath still works.
fn path(game: &mut Game<'_>) -> bool {
    let (stored, inv) = with(|s| (s.goal, s.invert));
    let Some(stored) = stored else {
        return false;
    };
    let goal = if inv {
        let Some(p) = game.player.as_ref() else {
            return true;
        };
        // 64 blocks away from the goal, through the player.
        let dx = p.position.x - stored[0] as f64 - 0.5;
        let dz = p.position.z - stored[2] as f64 - 0.5;
        let len = dx.hypot(dz);
        let (tx, tz) = if len > 1e-6 {
            (
                (p.position.x + dx / len * 64.0).floor() as i32,
                (p.position.z + dz / len * 64.0).floor() as i32,
            )
        } else {
            (
                p.position.x.floor() as i32 + 64,
                p.position.z.floor() as i32,
            )
        };
        let w = pf::W { game };
        pf::column_top(&w, tx, tz, p.position.y.floor() as i32)
            .unwrap_or([tx, p.position.y.floor() as i32, tz])
    } else {
        stored
    };
    rt::with(|s| s.begin(rt::Proc::Goto { goal }));
    say(
        game,
        &format!("pathing to {} {} {}", goal[0], goal[1], goal[2]),
    );
    true
}

/// `/b find <block>`: same nearest-first scan as `mine` (mine_radius, name
/// contains) minus the breakable filter, then goto a cell next to it.
fn find_cmd(game: &mut Game<'_>, args: &[&str]) {
    let Some(&q) = args.first() else {
        say(game, "/b find <block>");
        return;
    };
    let q = q.trim_start_matches("minecraft:").to_ascii_lowercase();
    if q.is_empty() {
        say(game, "/b find <block>");
        return;
    }
    let Some(me) = game.player.as_ref().map(feet) else {
        return;
    };
    let radius = rt::with(|s| s.set.mine_radius);
    let mut best: Option<(i32, [i32; 3])> = None;
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            for dy in -8i32..=8 {
                let p = [me[0] + dx, me[1] + dy, me[2] + dz];
                if p[1] < game.world.shape.min_y || p[1] > game.world.shape.max_y() {
                    continue;
                }
                let st = game.world.block(p[0], p[1], p[2]);
                if game.res.facts().get(st).name.contains(q.as_str()) {
                    let d = dx.abs() + dy.abs() + dz.abs();
                    if best.map_or(true, |(b, _)| d < b) {
                        best = Some((d, p));
                    }
                }
            }
        }
    }
    let Some((_, b)) = best else {
        say(game, &format!("no '{q}' within {radius} blocks"));
        return;
    };
    // Stand next to the block: a standable face-adjacent cell, else whatever
    // the column offers above it.
    let goal = {
        let w = pf::W { game };
        let mut g = None;
        for off in [
            [1, 0, 0],
            [-1, 0, 0],
            [0, 0, 1],
            [0, 0, -1],
            [0, 1, 0],
            [0, -1, 0],
        ] {
            let c = [b[0] + off[0], b[1] + off[1], b[2] + off[2]];
            if w.standable(c) {
                g = Some(c);
                break;
            }
        }
        g.or_else(|| pf::column_top(&w, b[0], b[2], b[1]))
    };
    match goal {
        Some(g) => {
            rt::with(|s| s.begin(rt::Proc::Goto { goal: g }));
            say(
                game,
                &format!("going to '{q}' near {} {} {}", g[0], g[1], g[2]),
            );
        }
        None => say(
            game,
            &format!(
                "found '{q}' at {} {} {} but can't stand near it",
                b[0], b[1], b[2]
            ),
        ),
    }
}

/// `/b surface`/`/b top`: stand on top of the player's column.
fn surface(game: &mut Game<'_>) {
    let Some(f) = game.player.as_ref().map(feet) else {
        return;
    };
    let near = game.world.shape.max_y();
    let w = pf::W { game };
    match pf::column_top(&w, f[0], f[2], near) {
        Some(g) => {
            rt::with(|s| s.begin(rt::Proc::Goto { goal: g }));
            say(game, &format!("going to the surface at y={}", g[1]));
        }
        None => say(game, "no standable spot in this column"),
    }
}

fn proc(game: &mut Game<'_>) {
    let (name, at, len, obj, dbg) = rt::with(|s| {
        (
            match s.proc {
                rt::Proc::None => "none",
                rt::Proc::Goto { .. } | rt::Proc::GotoXZ { .. } => "goto",
                rt::Proc::YLevel { .. } => "ylevel",
                rt::Proc::Mine { .. } => "mine",
                rt::Proc::Follow => "follow",
                rt::Proc::Explore => "explore",
                rt::Proc::Tunnel { .. } => "tunnel",
            },
            s.at,
            s.path.len(),
            s.objective,
            // Live diagnostics: current move kind, wanted inputs, stuck ticks.
            format!(
                "move={:?} to={:?} want.f={} j={} sn={} atk={} use={} stuck={} busy={} {} {} g.l={} g.r={} hit={} mining={:?}",
                s.path.get(s.at).map(|m| &m.kind),
                s.path.get(s.at).map(|m| m.pos),
                s.want.forward,
                s.want.jump as u8,
                s.want.sneak as u8,
                s.want.attack as u8,
                s.want.right as u8,
                s.stuck,
                s.busy as u8,
                if s.last_pos[0] == i32::MIN {
                    String::new()
                } else {
                    format!("last={:?}", s.last_pos)
                },
                s.note,
                game.left as u8,
                game.right as u8,
                game.forced_hit.is_some(),
                game.mining.as_ref().map(|m| m.ticks),
            ),
        )
    });
    let obj = obj
        .map(|o| format!("{} {} {}", o[0], o[1], o[2]))
        .unwrap_or_else(|| "-".to_owned());
    let rest = rt::with(|s| {
        s.path
            .iter()
            .enumerate()
            .skip(s.at)
            .map(|(i, m)| format!("{}{:?}→{:?}", if i == s.at { ">" } else { "" }, m.kind, m.pos))
            .collect::<Vec<_>>()
            .join(" ")
    });
    say(game, &format!("proc: {name} — path {at}/{len}, objective {obj}\nbaritone: {dbg}\nbaritone: {rest}"));
}

fn eta(game: &mut Game<'_>) {
    let (active, left, sprint) = rt::with(|s| {
        (
            s.proc != rt::Proc::None,
            s.path.len().saturating_sub(s.at),
            s.set.sprint,
        )
    });
    if !active || left == 0 {
        say(game, "eta: idle");
        return;
    }
    let secs = left as f64 / if sprint { 5.6 } else { 4.3 };
    if secs >= 60.0 {
        say(
            game,
            &format!(
                "eta: ~{}m{:02.0}s ({left} nodes left)",
                (secs / 60.0).floor() as i32,
                secs % 60.0
            ),
        );
    } else {
        say(game, &format!("eta: ~{secs:.0}s ({left} nodes left)"));
    }
}

/// `/b avoid` — `blocksToAvoid`: cells holding these names cost extra to
/// path through (webs, berry bushes) without being outright impassable.
fn avoid_cmd(game: &mut Game<'_>, args: &[&str]) {
    match args.first().map(|a| a.to_ascii_lowercase()) {
        Some(a) if a == "clear" => {
            with(|s| s.avoid.clear());
            save_avoid(game);
            say(game, "avoid list cleared");
        }
        Some(_) => {
            let n = args[0].to_ascii_lowercase();
            if n.is_empty() {
                say(game, "/b avoid <block|clear>");
                return;
            }
            let added = with(|s| s.avoid.insert(n.clone()));
            save_avoid(game);
            if added {
                say(game, &format!("avoiding {n}"));
            } else {
                say(game, &format!("{n} already avoided"));
            }
        }
        None => {
            let mut v: Vec<String> = with(|s| s.avoid.iter().cloned().collect());
            v.sort();
            if v.is_empty() {
                say(game, "avoid list is empty");
            } else {
                say(game, &format!("avoiding: {}", v.join(", ")));
            }
        }
    }
}

fn blacklist_cmd(game: &mut Game<'_>, args: &[&str]) {
    match args.first().map(|a| a.to_ascii_lowercase()).as_deref() {
        Some("clear") => {
            with(|s| s.blacklist.clear());
            save_blacklist(game);
            say(game, "blacklist cleared");
        }
        Some(name) => {
            let n = name.trim_start_matches("minecraft:").to_ascii_lowercase();
            if n.is_empty() {
                say(game, "/b blacklist <block|clear>");
                return;
            }
            let added = with(|s| s.blacklist.insert(n.clone()));
            save_blacklist(game);
            if added {
                say(game, &format!("blacklisted {n}"));
            } else {
                say(game, &format!("{n} is already blacklisted"));
            }
        }
        None => {
            let mut v: Vec<String> = with(|s| s.blacklist.iter().cloned().collect());
            v.sort();
            if v.is_empty() {
                say(game, "blacklist is empty");
            } else {
                say(game, &format!("blacklist: {}", v.join(", ")));
            }
        }
    }
}

fn modified(game: &mut Game<'_>) {
    let cur = rt::with(|s| s.set);
    let d = defaults();
    let mut lines: Vec<String> = Vec::new();
    if cur.sprint != d.sprint {
        lines.push(format!("sprint = {}", cur.sprint));
    }
    if cur.allow_break != d.allow_break {
        lines.push(format!("allowBreak = {}", cur.allow_break));
    }
    if cur.allow_place != d.allow_place {
        lines.push(format!("allowPlace = {}", cur.allow_place));
    }
    if cur.allow_parkour != d.allow_parkour {
        lines.push(format!("allowParkour = {}", cur.allow_parkour));
    }
    if cur.max_drop != d.max_drop {
        lines.push(format!("maxDrop = {}", cur.max_drop));
    }
    if cur.mine_radius != d.mine_radius {
        lines.push(format!("mineRadius = {}", cur.mine_radius));
    }
    if cur.mine_scan_drops != d.mine_scan_drops {
        lines.push(format!("mineScanDroppedItems = {}", cur.mine_scan_drops));
    }
    if cur.exposed_ores != d.exposed_ores {
        lines.push(format!("onlyExposedOres = {}", cur.exposed_ores));
    }
    if cur.follow_radius != d.follow_radius {
        lines.push(format!("followRadius = {}", cur.follow_radius));
    }
    if cur.legit_mine != d.legit_mine {
        lines.push(format!("legitMine = {}", cur.legit_mine));
    }
    if cur.assume_water != d.assume_water {
        lines.push(format!("assumeWalkOnWater = {}", cur.assume_water));
    }
    if cur.assume_lava != d.assume_lava {
        lines.push(format!("assumeWalkOnLava = {}", cur.assume_lava));
    }
    if cur.parkour_place != d.parkour_place {
        lines.push(format!("allowParkourPlace = {}", cur.parkour_place));
    }
    if cur.mob_avoid_r != d.mob_avoid_r {
        lines.push(format!("mobAvoidanceRadius = {}", cur.mob_avoid_r));
    }
    if cur.mob_avoid_c != d.mob_avoid_c {
        lines.push(format!("mobAvoidanceCoefficient = {}", cur.mob_avoid_c));
    }
    if cur.downward != d.downward {
        lines.push(format!("allowDownward = {}", cur.downward));
    }
    if cur.vines != d.vines {
        lines.push(format!("allowVines = {}", cur.vines));
    }
    if cur.diag_ascend != d.diag_ascend {
        lines.push(format!("allowDiagonalAscend = {}", cur.diag_ascend));
    }
    if cur.diag_descend != d.diag_descend {
        lines.push(format!("allowDiagonalDescend = {}", cur.diag_descend));
    }
    if cur.assume_step != d.assume_step {
        lines.push(format!("assumeStep = {}", cur.assume_step));
    }
    if cur.sprint_water != d.sprint_water {
        lines.push(format!("sprintInWater = {}", cur.sprint_water));
    }
    if cur.tool_select != d.tool_select {
        lines.push(format!("toolSelect = {}", cur.tool_select));
    }
    if cur.bottom_slab != d.bottom_slab {
        lines.push(format!("allowWalkOnBottomSlab = {}", cur.bottom_slab));
    }
    if cur.pause != d.pause {
        lines.push(format!("pause = {}", cur.pause));
    }
    for &(name, def) in CFG_DEFAULTS {
        let v = with(|s| s.cfg.get(name).cloned()).unwrap_or_else(|| def.to_owned());
        if v != def {
            lines.push(format!("{name} = {v}"));
        }
    }
    if lines.is_empty() {
        say(game, "no settings differ from defaults");
    } else {
        say(game, "modified settings:");
        for l in lines {
            say(game, &format!("  {l}"));
        }
    }
}

/// Reset a real setting; returns its display label when handled.
fn reset_real(game: &Game<'_>, key: &str) -> Option<&'static str> {
    let d = defaults();
    let (k, v, label): (&'static str, String, &'static str) =
        match key.to_ascii_lowercase().as_str() {
            "sprint" | "allowsprint" => ("sprint", d.sprint.to_string(), "sprint"),
            "break" | "allowbreak" | "allow_break" => {
                ("allow_break", d.allow_break.to_string(), "allowBreak")
            }
            "place" | "allowplace" | "allow_place" => {
                ("allow_place", d.allow_place.to_string(), "allowPlace")
            }
            "parkour" | "allowparkour" | "allow_parkour" => {
                ("allow_parkour", d.allow_parkour.to_string(), "allowParkour")
            }
            "drop" | "maxdrop" | "max_drop" => {
                ("max_drop", d.max_drop.to_string(), "maxDrop")
            }
            "radius" | "mineradius" | "mine_radius" => {
                ("mine_radius", d.mine_radius.to_string(), "mineRadius")
            }
            "drops" | "mine_scan_drops" | "minescandroppeditems" => (
                "mine_scan_drops",
                d.mine_scan_drops.to_string(),
                "mineScanDroppedItems",
            ),
            "exposed" | "onlyexposed" | "exposedores" | "allowonlyexposedores" => {
                ("exposed_ores", d.exposed_ores.to_string(), "onlyExposedOres")
            }
            "follow_radius" | "followradius" => {
                ("follow_radius", d.follow_radius.to_string(), "followRadius")
            }
            "legit" | "legitmine" | "legit_mine" => {
                ("legit_mine", d.legit_mine.to_string(), "legitMine")
            }
            "assumewater" | "assumewalkonwater" | "assume_water" => {
                ("assume_water", d.assume_water.to_string(), "assumeWalkOnWater")
            }
            "assumelava" | "assumewalkonlava" | "assume_lava" => {
                ("assume_lava", d.assume_lava.to_string(), "assumeWalkOnLava")
            }
            "parkourplace" | "allowparkourplace" | "parkour_place" => (
                "parkour_place",
                d.parkour_place.to_string(),
                "allowParkourPlace",
            ),
            "mobavoidanceradius" | "mob_avoid_r" | "mobavoidr" => (
                "mob_avoid_r",
                d.mob_avoid_r.to_string(),
                "mobAvoidanceRadius",
            ),
            "mobavoidancecoefficient" | "mob_avoid_c" | "mobavoidc" => (
                "mob_avoid_c",
                d.mob_avoid_c.to_string(),
                "mobAvoidanceCoefficient",
            ),
            "downward" | "allowdownward" | "allow_downward" => {
                ("downward", d.downward.to_string(), "allowDownward")
            }
            "vines" | "allowvines" => ("vines", d.vines.to_string(), "allowVines"),
            "diagascend" | "allowdiagonalascend" | "diagonalascend" => (
                "diag_ascend",
                d.diag_ascend.to_string(),
                "allowDiagonalAscend",
            ),
            "diagdescend" | "allowdiagonaldescend" | "diagonaldescend" => (
                "diag_descend",
                d.diag_descend.to_string(),
                "allowDiagonalDescend",
            ),
            "assumestep" | "step" => {
                ("assume_step", d.assume_step.to_string(), "assumeStep")
            }
            "sprintinwater" | "sprintwater" => {
                ("sprint_water", d.sprint_water.to_string(), "sprintInWater")
            }
            "toolselect" | "autotool" => {
                ("tool_select", d.tool_select.to_string(), "toolSelect")
            }
            "allowwalkonbottomslab" | "bottomslab" => (
                "bottom_slab",
                d.bottom_slab.to_string(),
                "allowWalkOnBottomSlab",
            ),
            "maxfallheight" | "maxfallheightnowater" | "fallheight" => (
                "max_drop",
                d.max_drop.to_string(),
                "maxFallHeightNoWater",
            ),
            "pause" => {
                rt::with(|s| s.set.pause = false);
                return Some("pause");
            }
            _ => return None,
        };
    match k {
        "sprint" => rt::with(|s| s.set.sprint = d.sprint),
        "allow_break" => rt::with(|s| s.set.allow_break = d.allow_break),
        "allow_place" => rt::with(|s| s.set.allow_place = d.allow_place),
        "allow_parkour" => rt::with(|s| s.set.allow_parkour = d.allow_parkour),
        "max_drop" => rt::with(|s| s.set.max_drop = d.max_drop),
        "mine_scan_drops" => rt::with(|s| s.set.mine_scan_drops = d.mine_scan_drops),
        "exposed_ores" => rt::with(|s| s.set.exposed_ores = d.exposed_ores),
        "follow_radius" => rt::with(|s| s.set.follow_radius = d.follow_radius),
        "legit_mine" => rt::with(|s| s.set.legit_mine = d.legit_mine),
        "assume_water" => rt::with(|s| s.set.assume_water = d.assume_water),
        "assume_lava" => rt::with(|s| s.set.assume_lava = d.assume_lava),
        "parkour_place" => rt::with(|s| s.set.parkour_place = d.parkour_place),
        "mob_avoid_r" => rt::with(|s| s.set.mob_avoid_r = d.mob_avoid_r),
        "mob_avoid_c" => rt::with(|s| s.set.mob_avoid_c = d.mob_avoid_c),
        "downward" => rt::with(|s| s.set.downward = d.downward),
        "vines" => rt::with(|s| s.set.vines = d.vines),
        "diag_ascend" => rt::with(|s| s.set.diag_ascend = d.diag_ascend),
        "diag_descend" => rt::with(|s| s.set.diag_descend = d.diag_descend),
        "assume_step" => rt::with(|s| s.set.assume_step = d.assume_step),
        "sprint_water" => rt::with(|s| s.set.sprint_water = d.sprint_water),
        "tool_select" => rt::with(|s| s.set.tool_select = d.tool_select),
        "bottom_slab" => rt::with(|s| s.set.bottom_slab = d.bottom_slab),
        _ => rt::with(|s| s.set.mine_radius = d.mine_radius),
    }
    game.native_set_setting(rt::MOD_ID, k, &v);
    Some(label)
}

fn reset(game: &mut Game<'_>, args: &[&str]) {
    let Some(&key) = args.first() else {
        say(game, "/b reset <all|setting>");
        return;
    };
    if key.eq_ignore_ascii_case("all") {
        rt::with(|s| s.set = defaults());
        save_settings(game);
        for &(name, def) in CFG_DEFAULTS {
            with(|s| {
                s.cfg.insert(name.to_owned(), def.to_owned());
            });
            game.native_set_setting(rt::MOD_ID, &format!("cfg_{name}"), def);
        }
        say(game, "all settings reset to defaults");
        return;
    }
    if let Some(label) = reset_real(game, key) {
        say(game, &format!("reset {label}"));
        return;
    }
    if let Some((canon, def)) = cfg_lookup(key) {
        with(|s| {
            s.cfg.insert(canon.to_owned(), def.to_owned());
        });
        game.native_set_setting(rt::MOD_ID, &format!("cfg_{canon}"), def);
        say(game, &format!("reset {canon} = {def}"));
        return;
    }
    say(game, &format!("unknown setting {key}"));
}

/// Real `/b set` keys: glue's aliases plus the Baritone names that map onto
/// them (allowBreak, allowSprint, allowPlace, allowParkour, maxDrop,
/// mineRadius). Returns true when handled.
fn set_real(game: &mut Game<'_>, key: &str, val: Option<&str>) -> bool {
    match key.to_ascii_lowercase().as_str() {
        "sprint" | "allowsprint" => rt::with(|s| s.set.sprint = flip(s.set.sprint, val)),
        "break" | "allowbreak" | "allow_break" => {
            rt::with(|s| s.set.allow_break = flip(s.set.allow_break, val))
        }
        "place" | "allowplace" | "allow_place" => {
            rt::with(|s| s.set.allow_place = flip(s.set.allow_place, val))
        }
        "parkour" | "allowparkour" | "allow_parkour" => {
            rt::with(|s| s.set.allow_parkour = flip(s.set.allow_parkour, val))
        }
        "pause" => rt::with(|s| s.set.pause = flip(s.set.pause, val)),
        "drop" | "maxdrop" | "max_drop" => {
            let Some(v) = val.and_then(|v| v.parse::<i32>().ok()) else {
                let cur = rt::with(|s| s.set.max_drop);
                say(game, &format!("maxDrop = {cur}"));
                return true;
            };
            rt::with(|s| s.set.max_drop = v.clamp(1, 20));
        }
        "radius" | "mineradius" | "mine_radius" => {
            let Some(v) = val.and_then(|v| v.parse::<i32>().ok()) else {
                let cur = rt::with(|s| s.set.mine_radius);
                say(game, &format!("mineRadius = {cur}"));
                return true;
            };
            rt::with(|s| s.set.mine_radius = v.clamp(8, 96));
        }
        "drops" | "mine_scan_drops" | "minescandroppeditems" => {
            rt::with(|s| s.set.mine_scan_drops = flip(s.set.mine_scan_drops, val))
        }
        "exposed" | "onlyexposed" | "exposedores" | "allowonlyexposedores" => {
            rt::with(|s| s.set.exposed_ores = flip(s.set.exposed_ores, val))
        }
        "follow_radius" | "followradius" => {
            let Some(v) = val.and_then(|v| v.parse::<i32>().ok()) else {
                let cur = rt::with(|s| s.set.follow_radius);
                say(game, &format!("followRadius = {cur}"));
                return true;
            };
            rt::with(|s| s.set.follow_radius = v.clamp(0, 32));
        }
        "legit" | "legitmine" | "legit_mine" => {
            rt::with(|s| s.set.legit_mine = flip(s.set.legit_mine, val))
        }
        "assumewater" | "assumewalkonwater" | "assume_water" => {
            rt::with(|s| s.set.assume_water = flip(s.set.assume_water, val))
        }
        "assumelava" | "assumewalkonlava" | "assume_lava" => {
            rt::with(|s| s.set.assume_lava = flip(s.set.assume_lava, val))
        }
        "parkourplace" | "allowparkourplace" | "parkour_place" => {
            rt::with(|s| s.set.parkour_place = flip(s.set.parkour_place, val))
        }
        "mobavoidanceradius" | "mob_avoid_r" | "mobavoidr" => {
            let Some(v) = val.and_then(|v| v.parse::<i32>().ok()) else {
                let cur = rt::with(|s| s.set.mob_avoid_r);
                say(game, &format!("mobAvoidanceRadius = {cur}"));
                return true;
            };
            rt::with(|s| s.set.mob_avoid_r = v.clamp(0, 32));
        }
        "mobavoidancecoefficient" | "mob_avoid_c" | "mobavoidc" => {
            let Some(v) = val.and_then(|v| v.parse::<f64>().ok()) else {
                let cur = rt::with(|s| s.set.mob_avoid_c);
                say(game, &format!("mobAvoidanceCoefficient = {cur}"));
                return true;
            };
            rt::with(|s| s.set.mob_avoid_c = v.clamp(0.0, 100.0));
        }
        "downward" | "allowdownward" | "allow_downward" => {
            rt::with(|s| s.set.downward = flip(s.set.downward, val))
        }
        "vines" | "allowvines" => {
            rt::with(|s| s.set.vines = flip(s.set.vines, val))
        }
        "diagascend" | "allowdiagonalascend" | "diagonalascend" => {
            rt::with(|s| s.set.diag_ascend = flip(s.set.diag_ascend, val))
        }
        "diagdescend" | "allowdiagonaldescend" | "diagonaldescend" => {
            rt::with(|s| s.set.diag_descend = flip(s.set.diag_descend, val))
        }
        "assumestep" | "step" => {
            rt::with(|s| s.set.assume_step = flip(s.set.assume_step, val))
        }
        "sprintinwater" | "sprintwater" => {
            rt::with(|s| s.set.sprint_water = flip(s.set.sprint_water, val))
        }
        "toolselect" | "autotool" => {
            rt::with(|s| s.set.tool_select = flip(s.set.tool_select, val))
        }
        "allowwalkonbottomslab" | "bottomslab" => {
            rt::with(|s| s.set.bottom_slab = flip(s.set.bottom_slab, val))
        }
        // Upstream's fall-height names share our maxDrop.
        "maxfallheight" | "maxfallheightnowater" | "fallheight" => {
            let Some(v) = val.and_then(|v| v.parse::<i32>().ok()) else {
                let cur = rt::with(|s| s.set.max_drop);
                say(game, &format!("maxFallHeightNoWater = {cur}"));
                return true;
            };
            rt::with(|s| s.set.max_drop = v.clamp(1, 20));
        }
        // `blocksToAvoid` as a comma list is the same data as `/b avoid`.
        "blockstoavoid" | "avoid" => {
            let Some(v) = val else {
                let cur: Vec<String> = with(|s| s.avoid.iter().cloned().collect());
                say(game, &format!("blocksToAvoid = {}", cur.join(",")));
                return true;
            };
            let list: std::collections::HashSet<String> = v
                .split(',')
                .filter(|n| !n.is_empty())
                .map(|n| n.to_ascii_lowercase())
                .collect();
            with(|s| s.avoid = list);
            save_avoid(game);
            say(game, &format!("blocksToAvoid = {v}"));
            return true;
        }
        // `blocksToAvoidBreaking` as a comma list is the blacklist.
        "blockstoavoidbreaking" => {
            let Some(v) = val else {
                let cur: Vec<String> = with(|s| s.blacklist.iter().cloned().collect());
                say(game, &format!("blocksToAvoidBreaking = {}", cur.join(",")));
                return true;
            };
            let list: std::collections::HashSet<String> = v
                .split(',')
                .filter(|n| !n.is_empty())
                .map(|n| n.to_ascii_lowercase())
                .collect();
            with(|s| s.blacklist = list);
            save_blacklist(game);
            say(game, &format!("blocksToAvoidBreaking = {v}"));
            return true;
        }
        _ => return false,
    }
    save_settings(game);
    let text = rt::with(|s| {
        format!(
            "sprint={} break={} place={} parkour={} parkourPlace={} drop={} radius={} drops={} exposed={} followR={} legit={} water={} lava={} mobR={} mobC={} down={} vines={} dAsc={} dDesc={} step={} sprintW={} tool={} slab={}",
            s.set.sprint,
            s.set.allow_break,
            s.set.allow_place,
            s.set.allow_parkour,
            s.set.parkour_place,
            s.set.max_drop,
            s.set.mine_radius,
            s.set.mine_scan_drops,
            s.set.exposed_ores,
            s.set.follow_radius,
            s.set.legit_mine,
            s.set.assume_water,
            s.set.assume_lava,
            s.set.mob_avoid_r,
            s.set.mob_avoid_c,
            s.set.downward,
            s.set.vines,
            s.set.diag_ascend,
            s.set.diag_descend,
            s.set.assume_step,
            s.set.sprint_water,
            s.set.tool_select,
            s.set.bottom_slab,
        )
    });
    say(game, &text);
    true
}

fn set_cmd(game: &mut Game<'_>, args: &[&str]) {
    let Some(&key) = args.first() else {
        // No args: list real settings then every stored Baritone name.
        let cur = rt::with(|s| s.set);
        say(
            game,
            &format!(
                "sprint={} allowBreak={} allowPlace={} allowParkour={} allowParkourPlace={} maxDrop={} mineRadius={}",
                cur.sprint, cur.allow_break, cur.allow_place, cur.allow_parkour,
                cur.parkour_place, cur.max_drop, cur.mine_radius
            ),
        );
        say(
            game,
            &format!(
                "mineScanDroppedItems={} onlyExposedOres={} followRadius={} legitMine={}",
                cur.mine_scan_drops, cur.exposed_ores, cur.follow_radius, cur.legit_mine
            ),
        );
        say(
            game,
            &format!(
                "assumeWalkOnWater={} assumeWalkOnLava={} mobAvoidanceRadius={} mobAvoidanceCoefficient={}",
                cur.assume_water, cur.assume_lava, cur.mob_avoid_r, cur.mob_avoid_c
            ),
        );
        say(
            game,
            &format!(
                "allowDownward={} allowVines={} allowDiagonalAscend={} allowDiagonalDescend={} assumeStep={} sprintInWater={} toolSelect={} allowWalkOnBottomSlab={}",
                cur.downward, cur.vines, cur.diag_ascend, cur.diag_descend,
                cur.assume_step, cur.sprint_water, cur.tool_select, cur.bottom_slab
            ),
        );
        let mut cfgs: Vec<(String, String)> =
            with(|s| s.cfg.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        cfgs.sort();
        for chunk in cfgs.chunks(5) {
            say(
                game,
                &chunk
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        return;
    };
    let val = args.get(1).copied();
    if set_real(game, key, val) {
        return;
    }
    if let Some((canon, _)) = cfg_lookup(key) {
        match val {
            Some(v) => {
                with(|s| {
                    s.cfg.insert(canon.to_owned(), v.to_owned());
                });
                game.native_set_setting(rt::MOD_ID, &format!("cfg_{canon}"), v);
                say(game, &format!("{canon} = {v} (stored; no effect here)"));
            }
            None => {
                let v = with(|s| s.cfg.get(canon).cloned())
                    .unwrap_or_else(|| "?".to_owned());
                say(game, &format!("{canon} = {v}"));
            }
        }
        return;
    }
    say(game, &format!("unknown setting {key}"));
}

// ---- module contract ------------------------------------------------------

/// `/b` subcommands this module owns. `args` is the lower-cased head + args;
/// returns false for everything the core (or another module) handles.
pub fn command(game: &mut Game<'_>, args: &[&str]) -> bool {
    if !game.native_on(rt::ID) || !game.native_window() {
        return false;
    }
    let Some(&head) = args.first() else {
        return false;
    };
    load_once(game);
    match head {
        "wp" | "waypoint" | "waypoints" => wp(game, &args[1..]),
        "goal" => goal(game, &args[1..]),
        "invert" => {
            let inv = with(|s| {
                s.invert = !s.invert;
                s.invert
            });
            if inv {
                say(game, "inverted — next /b path runs away from the goal");
            } else {
                say(game, "inversion off");
            }
        }
        // No stored goal? Let the core's `path` (repath) take it.
        "path" => return path(game),
        "find" => find_cmd(game, &args[1..]),
        "surface" | "top" => surface(game),
        "come" => {
            let Some(f) = game.player.as_ref().map(feet) else {
                return true;
            };
            rt::with(|s| s.begin(rt::Proc::Goto { goal: f }));
            say(game, "coming to you");
        }
        "proc" | "process" => proc(game),
        "eta" => eta(game),
        // env!("CARGO_PKG_VERSION") would resolve to the *host* crate's
        // version once injected, so the port's own version is hardcoded.
        "version" | "ver" => say(game, "baritone-rs 0.4.0 for Horizon 9f42064"),
        "gc" => say(game, "gc: nothing to collect (no-op)"),
        "reloadall" => say(game, "reloadall: nothing to reload (no-op)"),
        "saveall" => {
            save_wps(game);
            save_blacklist(game);
            save_avoid(game);
            save_visited(game);
            say(game, "saveall: waypoints + blacklist + avoid + visited saved");
        }
        "blacklist" => blacklist_cmd(game, &args[1..]),
        "avoid" | "blockstoavoid" => avoid_cmd(game, &args[1..]),
        "modified" => modified(game),
        "reset" => reset(game, &args[1..]),
        // Upstream repack resaves the CachedWorld; this port keeps no
        // world cache, so it's a truthful no-op.
        "repack" => say(game, "repack: no world cache — nothing to resave"),
        "explorefilter" => {
            // Visited chunks persist under the `visited` setting; the set
            // autosaves in tick and `clear` drops it outright.
            match args.get(1).copied() {
                Some("clear") => {
                    rt::with(|s| s.visited.clear());
                    save_visited(game);
                    say(game, "explore filter cleared and saved");
                }
                Some("save") => {
                    save_visited(game);
                    let n = rt::with(|s| s.visited.len());
                    say(game, &format!("explorefilter: {n} chunks saved"));
                }
                Some("count") | None => {
                    let n = rt::with(|s| s.visited.len());
                    say(game, &format!("explorefilter: {n} chunks tracked"));
                }
                _ => say(game, "/b explorefilter <save|clear|count>"),
            }
        }
        "set" | "s" => set_cmd(game, &args[1..]),
        // pause/resume/stop/goto/mine/follow/explore/tunnel/thisway/axis/help
        // belong to the core glue — return false.
        _ => return false,
    }
    true
}

/// First tick: pull persisted waypoints, blacklist and cfg values. After
/// that, autosave `explorefilter`'s visited chunks every ~30s of ticks
/// when the set has grown.
pub fn tick(game: &mut Game<'_>) {
    if !game.native_on(rt::ID) || !game.native_window() {
        return;
    }
    load_once(game);
    if game.ticks % 600 == 0 {
        let (cur, saved) = {
            let n = rt::with(|s| s.visited.len());
            (n, with(|s| s.visited_saved))
        };
        if cur != saved {
            save_visited(game);
        }
    }
}

/// Settings-page buttons; `click` receives the same local index.
pub fn settings(game: &Game<'_>, out: &mut Vec<String>) {
    let (wps, bl) = with(|s| (s.wps.len(), s.blacklist.len()));
    out.push(format!("Waypoints: {wps}"));
    out.push(format!("Blacklist: {bl}"));
    out.push("Surface".to_owned());
    let _ = game;
}

pub fn click(game: &mut Game<'_>, button: usize) -> bool {
    match button {
        0 => command(game, &["wp", "list"]),
        1 => command(game, &["blacklist"]),
        2 => command(game, &["surface"]),
        _ => return false,
    }
}

pub fn help() -> &'static [&'static str] {
    &[
        "/b wp save|goto|delete <name>  /b wp list  /b wp clear",
        "/b goal [<x> <z> | <x> <y> <z> | clear]  /b path  /b invert",
        "/b find <block>  /b surface|top  /b come",
        "/b proc  /b eta  /b version",
        "/b blacklist [<block> | clear]  /b modified  /b reset <all|name>",
        "/b set [<name> [value]] — Baritone names stored as no-ops",
        "/b gc  /b reloadall  /b saveall — parity no-ops",
    ]
}
