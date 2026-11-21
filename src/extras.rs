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
// Module contract: command/tick/help/park. Movement
// is driven only by starting `rt::Proc::Goto`, so `s.busy` stays untouched.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::game::umbx_baritone as rt;
use crate::game::umbx_baritone_settings as st;
use crate::game::umbx_pathfind as pf;
use crate::game::Game;

/// `/b find`'s scan radius — its own constant, not `mineRadius`.
const FIND_RADIUS: i32 = 64;

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

fn parse_pos(v: &str) -> Option<[i32; 3]> {
    let mut it = v.split(',');
    let x = it.next()?.parse().ok()?;
    let y = it.next()?.parse().ok()?;
    let z = it.next()?.parse().ok()?;
    Some([x, y, z])
}

fn flip(b: bool, v: Option<&str>) -> bool {
    // Command arguments keep their original case, so compare case-insensitive.
    match v.map(|s| s.to_ascii_lowercase()).as_deref() {
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

/// Persist the real settings (`rt::to_save` iterates the shared table).
fn save_settings(game: &Game<'_>) {
    for (k, v) in rt::to_save() {
        game.set_setting(rt::MOD_ID, k, &v);
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
    game.set_setting(rt::MOD_ID, "wp_names", &names);
    // `wp_list`: the render module's marker source (`name=x,y,z;...`).
    let list = items
        .iter()
        .map(|(n, p)| format!("{n}={},{},{}", p[0], p[1], p[2]))
        .collect::<Vec<_>>()
        .join(";");
    game.set_setting(rt::MOD_ID, "wp_list", &list);
    for (n, p) in items {
        game.set_setting(
            rt::MOD_ID,
            &format!("wp_{n}"),
            &format!("{},{},{}", p[0], p[1], p[2]),
        );
    }
}

fn save_blacklist(game: &Game<'_>) {
    let mut v: Vec<String> = with(|s| s.blacklist.iter().cloned().collect());
    v.sort();
    game.set_setting(rt::MOD_ID, "blacklist", &v.join(","));
}

fn save_avoid(game: &Game<'_>) {
    let mut v: Vec<String> = with(|s| s.avoid.iter().cloned().collect());
    v.sort();
    game.set_setting(rt::MOD_ID, "avoid", &v.join(","));
}

/// `explorefilter`'s backing store: every visited column as "x,z;x,z;…".
fn save_visited(game: &Game<'_>) {
    let mut v: Vec<(i32, i32)> = rt::with(|s| s.visited.iter().copied().collect());
    v.sort();
    let text = v
        .iter()
        .map(|(x, z)| format!("{x},{z}"))
        .collect::<Vec<_>>()
        .join(";");
    game.set_setting(rt::MOD_ID, "visited", &text);
    let n = v.len();
    with(|s| s.visited_saved = n);
}

/// First-tick load of everything `native_setting` holds for this module.
fn load_once(game: &Game<'_>) {
    if with(|s| s.loaded) {
        return;
    }
    let mut wps = HashMap::new();
    if let Some(names) = game.setting(rt::MOD_ID, "wp_names") {
        for n in names.split(',') {
            if n.is_empty() {
                continue;
            }
            if let Some(v) = game.setting(rt::MOD_ID, &format!("wp_{n}")) {
                if let Some(p) = parse_pos(&v) {
                    wps.insert(n.to_owned(), p);
                }
            }
        }
    }
    let blacklist: HashSet<String> = game
        .setting(rt::MOD_ID, "blacklist")
        .unwrap_or_default()
        .split(',')
        .filter(|n| !n.is_empty())
        .map(|n| n.to_ascii_lowercase())
        .collect();
    let avoid: HashSet<String> = game
        .setting(rt::MOD_ID, "avoid")
        .unwrap_or_default()
        .split(',')
        .filter(|n| !n.is_empty())
        .map(|n| n.to_ascii_lowercase())
        .collect();
    // `explorefilter`'s column list: "x,z;x,z;…" like upstream's file.
    let visited: HashSet<(i32, i32)> = game
        .setting(rt::MOD_ID, "visited")
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
            .setting(rt::MOD_ID, &format!("cfg_{name}"))
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
    let n = rt::norm(name);
    with(|s| s.blacklist.iter().any(|b| n.contains(b.as_str())))
}

/// `blocksToAvoid`: true when `name` matches an avoid entry — traversal
/// stays legal, the planner just prices the cells higher.
pub fn avoided(name: &str) -> bool {
    let n = rt::norm(name);
    !n.is_empty() && with(|s| s.avoid.iter().any(|b| n.contains(b.as_str())))
}

/// Waypoint names for chat completion (`glue::tab_complete`).
pub fn wp_names(game: &Game<'_>) -> Vec<String> {
    load_once(game);
    with(|s| s.wps.keys().cloned().collect())
}

/// Every `/b set` key — real settings (key, display name and aliases,
/// all from the shared table) plus the cfg-backed ones — for chat
/// completion and `/b reset`/`/b modified`.
pub fn setting_names() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |n: &str| {
        if !out.iter().any(|o| o == n) {
            out.push(n.to_owned());
        }
    };
    for s in st::SETTINGS {
        add(s.display);
        add(s.key);
        for a in s.aliases {
            add(a);
        }
    }
    for (n, _) in CFG_DEFAULTS {
        add(n);
    }
    out
}

/// The `Setting` row a `/b set` name resolves to — key, display name or
/// alias, all case-insensitive (upstream prints camelCase but parses any).
fn find_setting(key: &str) -> Option<&'static st::Setting> {
    st::SETTINGS.iter().find(|s| {
        s.key.eq_ignore_ascii_case(key)
            || s.display.eq_ignore_ascii_case(key)
            || s.aliases.iter().any(|a| a.eq_ignore_ascii_case(key))
    })
}

/// A stored setting's value by Baritone name (`buildIgnoreBlocks` etc.).
pub fn cfg_get(name: &str) -> Option<String> {
    with(|s| s.cfg.get(name).cloned())
}

// ---- commands -------------------------------------------------------------

fn wp(game: &mut Game<'_>, args: &[&str]) {
    match args.first().map(|a| a.to_ascii_lowercase()).as_deref() {
        Some("save") => {
            let Some(&name) = args.get(1) else {
                rt::say(game, "/b wp save <name>");
                return;
            };
            let name = name.to_ascii_lowercase();
            // The persisted form is `name=x,y,z;name=x,y,z`, so these
            // separators would corrupt every marker on the next reload;
            // `names`/`list` collide with the index key and the subcommand.
            if name.contains([',', '=', ';', ' ']) || matches!(name.as_str(), "names" | "list") {
                rt::say(game, "bad waypoint name");
                return;
            }
            let Some(f) = game.player.as_ref().map(rt::feet) else {
                return;
            };
            with(|s| s.wps.insert(name.clone(), f));
            save_wps(game);
            rt::say(game, &format!("saved '{name}' at {} {} {}", f[0], f[1], f[2]));
        }
        Some("goto") => {
            let Some(&name) = args.get(1) else {
                rt::say(game, "/b wp goto <name>");
                return;
            };
            let name = name.to_ascii_lowercase();
            match with(|s| s.wps.get(&name).copied()) {
                Some(goal) => {
                    rt::with(|s| s.begin(rt::Proc::Goto { goal }));
                    rt::say(
                        game,
                        &format!(
                            "going to '{name}' at {} {} {}",
                            goal[0], goal[1], goal[2]
                        ),
                    );
                }
                None => rt::say(game, &format!("no waypoint '{name}'")),
            }
        }
        Some("delete") | Some("del") | Some("remove") => {
            let Some(&name) = args.get(1) else {
                rt::say(game, "/b wp delete <name>");
                return;
            };
            let name = name.to_ascii_lowercase();
            let had = with(|s| s.wps.remove(&name).is_some());
            save_wps(game);
            if had {
                rt::say(game, &format!("deleted '{name}'"));
            } else {
                rt::say(game, &format!("no waypoint '{name}'"));
            }
        }
        Some("clear") => {
            with(|s| s.wps.clear());
            save_wps(game);
            rt::say(game, "waypoints cleared");
        }
        Some("list") | None => {
            let mut items: Vec<(String, [i32; 3])> =
                with(|s| s.wps.iter().map(|(n, p)| (n.clone(), *p)).collect());
            items.sort();
            if items.is_empty() {
                rt::say(game, "no waypoints — /b wp save <name>");
                return;
            }
            rt::say(game, &format!("{} waypoint(s):", items.len()));
            for (n, p) in items {
                rt::say(game, &format!("  {n}: {} {} {}", p[0], p[1], p[2]));
            }
        }
        Some(other) => rt::say(
            game,
            &format!("unknown wp subcommand '{other}' — save|list|goto|delete|clear"),
        ),
    }
}

fn goal(game: &mut Game<'_>, args: &[&str]) {
    if args.is_empty() {
        let (g, inv) = with(|s| (s.goal, s.invert));
        match g {
            Some(g) => rt::say(
                game,
                &format!(
                    "goal: {} {} {}{}",
                    g[0],
                    g[1],
                    g[2],
                    if inv { " (inverted)" } else { "" }
                ),
            ),
            None => rt::say(game, "no goal set — /b goal <x> <z> | <x> <y> <z>"),
        }
        return;
    }
    if args.first() == Some(&"clear") {
        with(|s| s.goal = None);
        rt::say(game, "goal cleared");
        return;
    }
    let ints: Vec<Option<i32>> = args.iter().map(|a| a.parse::<i32>().ok()).collect();
    let got = match ints.as_slice() {
        [Some(x), Some(y), Some(z)] => Some([*x, *y, *z]),
        [Some(x), Some(z)] => {
            let ny = game
                .player
                .as_ref()
                .map(rt::feet)
                .map(|f| f[1])
                .unwrap_or(64);
            let w = pf::W::new(game);
            pf::column_top(&w, *x, *z, ny)
        }
        _ => None,
    };
    match got {
        Some(g) => {
            with(|s| s.goal = Some(g));
            rt::say(
                game,
                &format!("goal set to {} {} {} — /b path to walk it", g[0], g[1], g[2]),
            );
        }
        None => rt::say(game, "/b goal <x> <z> | <x> <y> <z>"),
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
        let w = pf::W::new(game);
        pf::column_top(&w, tx, tz, p.position.y.floor() as i32)
            .unwrap_or([tx, p.position.y.floor() as i32, tz])
    } else {
        stored
    };
    rt::with(|s| s.begin(rt::Proc::Goto { goal }));
    rt::say(
        game,
        &format!("pathing to {} {} {}", goal[0], goal[1], goal[2]),
    );
    true
}

/// `/b find <block>`: same nearest-first scan as `mine` (FIND_RADIUS, name
/// contains) minus the breakable filter, then goto a cell next to it.
fn find_cmd(game: &mut Game<'_>, args: &[&str]) {
    let Some(&q) = args.first() else {
        rt::say(game, "/b find <block>");
        return;
    };
    let q = rt::norm(q);
    if q.is_empty() {
        rt::say(game, "/b find <block>");
        return;
    }
    let Some(me) = game.player.as_ref().map(rt::feet) else {
        return;
    };
    let radius = FIND_RADIUS;
    let mut best: Option<(i32, [i32; 3])> = None;
    let mut bd = i32::MAX;
    for dx in -radius..=radius {
        if dx.abs() > bd {
            continue;
        }
        for dz in -radius..=radius {
            if dx.abs() + dz.abs() > bd {
                continue;
            }
            for dy in -8i32..=8 {
                if dx.abs() + dz.abs() + dy.abs() > bd {
                    continue;
                }
                let p = [me[0] + dx, me[1] + dy, me[2] + dz];
                if p[1] < game.world.shape.min_y || p[1] > game.world.shape.max_y() {
                    continue;
                }
                let st = game.world.block(p[0], p[1], p[2]);
                if game.res.facts().get(st).name.contains(q.as_str()) {
                    let d = dx.abs() + dy.abs() + dz.abs();
                    if d < bd {
                        bd = d;
                        best = Some((d, p));
                    }
                }
            }
        }
    }
    let Some((_, b)) = best else {
        rt::say(game, &format!("no '{q}' within {radius} blocks"));
        return;
    };
    // Stand next to the block: a standable face-adjacent cell, else whatever
    // the column offers above it.
    let goal = {
        let w = pf::W::new(game);
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
            rt::say(
                game,
                &format!("going to '{q}' near {} {} {}", g[0], g[1], g[2]),
            );
        }
        None => rt::say(
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
    let Some(f) = game.player.as_ref().map(rt::feet) else {
        return;
    };
    let near = game.world.shape.max_y();
    let w = pf::W::new(game);
    match pf::column_top(&w, f[0], f[2], near) {
        Some(g) => {
            rt::with(|s| s.begin(rt::Proc::Goto { goal: g }));
            rt::say(game, &format!("going to the surface at y={}", g[1]));
        }
        None => rt::say(game, "no standable spot in this column"),
    }
}

fn proc(game: &mut Game<'_>) {
    let (name, at, len, obj, dbg) = rt::with(|s| {
        (
            match s.proc {
                rt::Proc::None => "none",
                rt::Proc::Goto { .. } | rt::Proc::GotoXZ { .. } => "goto",
                rt::Proc::YLevel { .. } => "ylevel",
                rt::Proc::Mine => "mine",
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
    rt::say(game, &format!("proc: {name} — path {at}/{len}, objective {obj}\nbaritone: {dbg}\nbaritone: {rest}"));
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
        rt::say(game, "eta: idle");
        return;
    }
    let secs = left as f64 / if sprint { 5.6 } else { 4.3 };
    if secs >= 60.0 {
        rt::say(
            game,
            &format!(
                "eta: ~{}m{:02.0}s ({left} nodes left)",
                (secs / 60.0).floor() as i32,
                secs % 60.0
            ),
        );
    } else {
        rt::say(game, &format!("eta: ~{secs:.0}s ({left} nodes left)"));
    }
}

/// `/b avoid` — `blocksToAvoid`: cells holding these names cost extra to
/// path through (webs, berry bushes) without being outright impassable.
fn avoid_cmd(game: &mut Game<'_>, args: &[&str]) {
    match args.first().map(|a| a.to_ascii_lowercase()) {
        Some(a) if a == "clear" => {
            with(|s| s.avoid.clear());
            save_avoid(game);
            rt::say(game, "avoid list cleared");
        }
        Some(_) => {
            let n = args[0].to_ascii_lowercase();
            if n.is_empty() {
                rt::say(game, "/b avoid <block|clear>");
                return;
            }
            let added = with(|s| s.avoid.insert(n.clone()));
            save_avoid(game);
            if added {
                rt::say(game, &format!("avoiding {n}"));
            } else {
                rt::say(game, &format!("{n} already avoided"));
            }
        }
        None => {
            let mut v: Vec<String> = with(|s| s.avoid.iter().cloned().collect());
            v.sort();
            if v.is_empty() {
                rt::say(game, "avoid list is empty");
            } else {
                rt::say(game, &format!("avoiding: {}", v.join(", ")));
            }
        }
    }
}

fn blacklist_cmd(game: &mut Game<'_>, args: &[&str]) {
    match args.first().map(|a| a.to_ascii_lowercase()).as_deref() {
        Some("clear") => {
            with(|s| s.blacklist.clear());
            save_blacklist(game);
            rt::say(game, "blacklist cleared");
        }
        Some(name) => {
            let n = rt::norm(name);
            if n.is_empty() {
                rt::say(game, "/b blacklist <block|clear>");
                return;
            }
            let added = with(|s| s.blacklist.insert(n.clone()));
            save_blacklist(game);
            if added {
                rt::say(game, &format!("blacklisted {n}"));
            } else {
                rt::say(game, &format!("{n} is already blacklisted"));
            }
        }
        None => {
            let mut v: Vec<String> = with(|s| s.blacklist.iter().cloned().collect());
            v.sort();
            if v.is_empty() {
                rt::say(game, "blacklist is empty");
            } else {
                rt::say(game, &format!("blacklist: {}", v.join(", ")));
            }
        }
    }
}

fn modified(game: &mut Game<'_>) {
    let cur = rt::with(|s| s.set);
    let mut lines: Vec<String> = Vec::new();
    for spec in st::SETTINGS {
        let v = cur.value(spec.key);
        if v != spec.default {
            lines.push(format!("{} = {v}", spec.display));
        }
    }
    // `pause` isn't persisted — it isn't in SETTINGS at all.
    if cur.pause {
        lines.push(format!("pause = {}", cur.pause));
    }
    for &(name, def) in CFG_DEFAULTS {
        let v = with(|s| s.cfg.get(name).cloned()).unwrap_or_else(|| def.to_owned());
        if v != def {
            lines.push(format!("{name} = {v}"));
        }
    }
    if lines.is_empty() {
        rt::say(game, "no settings differ from defaults");
    } else {
        rt::say(game, "modified settings:");
        for l in lines {
            rt::say(game, &format!("  {l}"));
        }
    }
}

/// Reset a real setting to the table default; returns its display label
/// when handled.
fn reset_real(game: &Game<'_>, key: &str) -> Option<&'static str> {
    if key.eq_ignore_ascii_case("pause") {
        rt::with(|s| s.set.pause = false);
        return Some("pause");
    }
    let spec = find_setting(key)?;
    rt::with(|s| s.set.apply(spec, spec.default));
    game.set_setting(rt::MOD_ID, spec.key, spec.default);
    Some(spec.display)
}

fn reset(game: &mut Game<'_>, args: &[&str]) {
    let Some(&key) = args.first() else {
        rt::say(game, "/b reset <all|setting>");
        return;
    };
    if key.eq_ignore_ascii_case("all") {
        rt::with(|s| s.set = rt::Settings::defaults());
        save_settings(game);
        for &(name, def) in CFG_DEFAULTS {
            with(|s| {
                s.cfg.insert(name.to_owned(), def.to_owned());
            });
            game.set_setting(rt::MOD_ID, &format!("cfg_{name}"), def);
        }
        rt::say(game, "all settings reset to defaults");
        return;
    }
    if let Some(label) = reset_real(game, key) {
        rt::say(game, &format!("reset {label}"));
        return;
    }
    if let Some((canon, def)) = cfg_lookup(key) {
        with(|s| {
            s.cfg.insert(canon.to_owned(), def.to_owned());
        });
        game.set_setting(rt::MOD_ID, &format!("cfg_{canon}"), def);
        rt::say(game, &format!("reset {canon} = {def}"));
        return;
    }
    rt::say(game, &format!("unknown setting {key}"));
}

/// `/b set` on a real setting — names, toggles, ranges and persistence
/// all come from the shared `SETTINGS` table. Returns true when handled.
fn set_real(game: &mut Game<'_>, key: &str, val: Option<&str>) -> bool {
    // `pause` lives on rt::Settings but isn't persisted — session-only.
    if key.eq_ignore_ascii_case("pause") {
        rt::with(|s| s.set.pause = flip(s.set.pause, val));
        rt::say(game, &format!("pause = {}", rt::with(|s| s.set.pause)));
        return true;
    }
    if let Some(spec) = find_setting(key) {
        if spec.toggle {
            let cur = rt::with(|s| s.set.flag(spec.key));
            rt::with(|s| s.set.put_bool(spec.key, flip(cur, val)));
        } else {
            let Some(v) = val.and_then(|v| v.parse::<f64>().ok()) else {
                let cur = rt::with(|s| s.set.value(spec.key));
                rt::say(game, &format!("{} = {cur}", spec.display));
                return true;
            };
            rt::with(|s| s.set.put_num(spec.key, v.clamp(spec.min, spec.max)));
        }
        save_settings(game);
        let cur = rt::with(|s| s.set.value(spec.key));
        rt::say(game, &format!("{} = {cur}", spec.display));
        return true;
    }
    match key.to_ascii_lowercase().as_str() {
        // `blocksToAvoid` as a comma list is the same data as `/b avoid`.
        "blockstoavoid" | "avoid" => {
            let Some(v) = val else {
                let cur: Vec<String> = with(|s| s.avoid.iter().cloned().collect());
                rt::say(game, &format!("blocksToAvoid = {}", cur.join(",")));
                return true;
            };
            let list: std::collections::HashSet<String> = v
                .split(',')
                .filter(|n| !n.is_empty())
                .map(|n| n.to_ascii_lowercase())
                .collect();
            with(|s| s.avoid = list);
            save_avoid(game);
            rt::say(game, &format!("blocksToAvoid = {v}"));
            true
        }
        // `blocksToAvoidBreaking` as a comma list is the blacklist.
        "blockstoavoidbreaking" => {
            let Some(v) = val else {
                let cur: Vec<String> = with(|s| s.blacklist.iter().cloned().collect());
                rt::say(game, &format!("blocksToAvoidBreaking = {}", cur.join(",")));
                return true;
            };
            let list: std::collections::HashSet<String> = v
                .split(',')
                .filter(|n| !n.is_empty())
                .map(|n| n.to_ascii_lowercase())
                .collect();
            with(|s| s.blacklist = list);
            save_blacklist(game);
            rt::say(game, &format!("blocksToAvoidBreaking = {v}"));
            true
        }
        _ => false,
    }
}

fn set_cmd(game: &mut Game<'_>, args: &[&str]) {
    let Some(&key) = args.first() else {
        // No args: list real settings then every stored Baritone name.
        let cur = rt::with(|s| s.set);
        let mut items: Vec<String> = st::SETTINGS
            .iter()
            .map(|sp| format!("{}={}", sp.display, cur.value(sp.key)))
            .collect();
        let mut cfgs: Vec<(String, String)> =
            with(|s| s.cfg.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        cfgs.sort();
        items.extend(cfgs.iter().map(|(k, v)| format!("{k}={v}")));
        for chunk in items.chunks(5) {
            rt::say(game, &chunk.join(" "));
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
                game.set_setting(rt::MOD_ID, &format!("cfg_{canon}"), v);
                rt::say(game, &format!("{canon} = {v} (stored; no effect here)"));
            }
            None => {
                let v = with(|s| s.cfg.get(canon).cloned())
                    .unwrap_or_else(|| "?".to_owned());
                rt::say(game, &format!("{canon} = {v}"));
            }
        }
        return;
    }
    rt::say(game, &format!("unknown setting {key}"));
}

// ---- module contract ------------------------------------------------------

/// `/b` subcommands this module owns. `args` is the lower-cased head + args;
/// returns false for everything the core (or another module) handles.
pub fn command(game: &mut Game<'_>, args: &[&str]) -> bool {
    let Some(head) = args.first().map(|s| s.to_ascii_lowercase()) else {
        return false;
    };
    load_once(game);
    match head.as_str() {
        "wp" | "waypoint" | "waypoints" => wp(game, &args[1..]),
        "goal" => goal(game, &args[1..]),
        "invert" => {
            let inv = with(|s| {
                s.invert = !s.invert;
                s.invert
            });
            if inv {
                rt::say(game, "inverted — next /b path runs away from the goal");
            } else {
                rt::say(game, "inversion off");
            }
        }
        // No stored goal? Let the core's `path` (repath) take it.
        "path" => return path(game),
        "find" => find_cmd(game, &args[1..]),
        "surface" | "top" => surface(game),
        "come" => {
            let Some(f) = game.player.as_ref().map(rt::feet) else {
                return true;
            };
            rt::with(|s| s.begin(rt::Proc::Goto { goal: f }));
            rt::say(game, "coming to you");
        }
        "proc" | "process" => proc(game),
        "eta" => eta(game),
        // env!("CARGO_PKG_VERSION") would resolve to the *host* crate's
        // version once injected, so the port's own version is hardcoded.
        "version" | "ver" => rt::say(game, "baritone-rs 0.5.0 for Horizon f00125f"),
        "gc" => rt::say(game, "gc: nothing to collect (no-op)"),
        "reloadall" => rt::say(game, "reloadall: nothing to reload (no-op)"),
        "saveall" => {
            save_wps(game);
            save_blacklist(game);
            save_avoid(game);
            save_visited(game);
            rt::say(game, "saveall: waypoints + blacklist + avoid + visited saved");
        }
        "blacklist" => blacklist_cmd(game, &args[1..]),
        "avoid" | "blockstoavoid" => avoid_cmd(game, &args[1..]),
        "modified" => modified(game),
        "reset" => reset(game, &args[1..]),
        // Upstream repack resaves the CachedWorld; this port keeps no
        // world cache, so it's a truthful no-op.
        "repack" => rt::say(game, "repack: no world cache — nothing to resave"),
        "explorefilter" => {
            // Visited columns persist under the `visited` setting; the set
            // autosaves in tick and `clear` drops it outright.
            match args.get(1).copied() {
                Some("clear") => {
                    rt::with(|s| s.visited.clear());
                    save_visited(game);
                    rt::say(game, "explore filter cleared and saved");
                }
                Some("save") => {
                    save_visited(game);
                    let n = rt::with(|s| s.visited.len());
                    rt::say(game, &format!("explorefilter: {n} columns saved"));
                }
                Some("count") | None => {
                    let n = rt::with(|s| s.visited.len());
                    rt::say(game, &format!("explorefilter: {n} columns tracked"));
                }
                _ => rt::say(game, "/b explorefilter <save|clear|count>"),
            }
        }
        "set" | "s" => set_cmd(game, &args[1..]),
        // `/b sprint [state]`: upstream's convenience alias for
        // `set sprint` — no state toggles, like any `/b set` toggle.
        "sprint" => {
            set_real(game, "sprint", args.get(1).copied());
        }
        // pause/resume/stop/goto/mine/follow/explore/tunnel/thisway/axis/help
        // belong to the core glue — return false.
        _ => return false,
    }
    true
}

/// First tick: pull persisted waypoints, blacklist and cfg values. After
/// that, autosave `explorefilter`'s visited columns every ~30s of ticks
/// when the set has grown.
pub fn tick(game: &mut Game<'_>) {
    if !rt::enabled() || !game.native_window() {
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
