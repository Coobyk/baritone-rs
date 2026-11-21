// Baritone port: WorldEdit-style selections (support module
// `umbx_baritone_sel`). Keeps the pos1/pos2 corners and the clipboard, and
// turns region edits (`fill`, `walls`, `shell`, `replace`, `cut`, `paste`)
// into Break/Place jobs on `umbx_baritone_build`'s queue.
//
// `pos1`/`pos2` target: `game.forced_hit` (a LAN guest's or another mod's
// aim) when set, else the block under the player's feet. There is no public
// crosshair raycast, so "the block being looked at" is approximated that
// way — the same trade-off Baritone makes for its own forced hits.

use std::sync::Mutex;

use crate::game::Game;
use crate::game::umbx_baritone as rt;
use crate::game::umbx_baritone_build as bd;

/// Biggest region a single command will touch (safety cap, not Baritone's).
const MAX_CELLS: i64 = 200_000;

struct Sel {
    p1: Option<[i32; 3]>,
    p2: Option<[i32; 3]>,
    /// Copied cells: (offset from the region's min corner, block name).
    clip: Vec<([i32; 3], String)>,
    /// Clipboard bounds, for chat feedback.
    clip_size: [i32; 3],
    /// Min corner offset from the player's feet at `copy` time — `paste`
    /// keeps the same relative offset (WorldEdit-style).
    clip_anchor: [i32; 3],
}

impl Sel {
    /// The (min, max) corners when both are set.
    fn region(&self) -> Option<([i32; 3], [i32; 3])> {
        let (a, b) = (self.p1?, self.p2?);
        Some((
            [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
            [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
        ))
    }
}

static S: Mutex<Option<Sel>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Sel) -> R) -> R {
    let mut g = S.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(Sel {
            p1: None,
            p2: None,
            clip: Vec::new(),
            clip_size: [0; 3],
            clip_anchor: [0; 3],
        });
    }
    f(g.as_mut().unwrap())
}

/// n/e/s/w/u/d (plus the long names and axis spellings) as an offset.
fn dir(s: &str) -> Option<[i32; 3]> {
    Some(match s.to_ascii_lowercase().as_str() {
        "n" | "north" | "-z" => [0, 0, -1],
        "s" | "south" | "+z" => [0, 0, 1],
        "w" | "west" | "-x" => [-1, 0, 0],
        "e" | "east" | "+x" => [1, 0, 0],
        "u" | "up" | "+y" => [0, 1, 0],
        "d" | "down" | "-y" => [0, -1, 0],
        _ => return None,
    })
}

/// Every cell of the box, bottom layer first, nearest-to-`near` within a
/// layer (the order the build queue works best in).
fn cells_sorted(lo: [i32; 3], hi: [i32; 3], near: [i32; 3]) -> Vec<[i32; 3]> {
    let mut out = Vec::new();
    for y in lo[1]..=hi[1] {
        for z in lo[2]..=hi[2] {
            for x in lo[0]..=hi[0] {
                out.push([x, y, z]);
            }
        }
    }
    out.sort_by_key(|p| {
        (
            p[1],
            (p[0] - near[0]) as i64 * (p[0] - near[0]) as i64
                + (p[2] - near[2]) as i64 * (p[2] - near[2]) as i64,
        )
    });
    out
}

/// The block `pos1`/`pos2` aim at: `forced_hit` when set, else the block
/// under the player's feet (documented approximation of "looked at").
fn target_block(game: &Game<'_>) -> Option<[i32; 3]> {
    if let Some(h) = &game.forced_hit {
        return Some(h.pos);
    }
    let p = game.player.as_ref()?;
    let f = rt::feet(p);
    Some([f[0], f[1] - 1, f[2]])
}

/// `command` interface: the `sel`/`selection` subcommand.
pub fn command(game: &mut Game<'_>, args: &[&str]) -> bool {
    let Some(head) = args.first() else { return false };
    if !matches!(head.to_ascii_lowercase().as_str(), "sel" | "selection") {
        return false;
    }
    sel(game, &args[1..]);
    true
}

/// Selections are passive; all per-tick work lives in the build queue.
pub fn tick(_game: &mut Game<'_>) {}

pub fn help() -> &'static [&'static str] {
    &[
        "/b sel pos1|pos2 — corner at the aimed block (or under you)",
        "/b sel clear|area — reset / print size",
        "/b sel expand|shift <dir> <n> — grow/move (n e s w u d)",
        "/b sel copy|cut|paste — clipboard",
        "/b sel fill <block>  /b sel replace <from> <to>",
        "/b sel walls <block>  /b sel shell <block>",
    ]
}

fn sel(game: &mut Game<'_>, args: &[&str]) {
    match args.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("pos1" | "p1" | "1") => set_pos(game, true),
        Some("pos2" | "p2" | "2") => set_pos(game, false),
        Some("clear" | "c") => {
            with(|s| {
                s.p1 = None;
                s.p2 = None;
            });
            rt::say(game, "selection cleared");
        }
        Some("area" | "a" | "size") => area(game),
        Some("expand" | "e" | "ex") => grow(game, &args[1..], false),
        Some("shift" | "s" | "sh") => grow(game, &args[1..], true),
        Some("copy" | "cp") => copy(game, false),
        Some("cut") => copy(game, true),
        Some("paste" | "p") => paste(game),
        Some("fill" | "f" | "set") => fill(game, &args[1..]),
        Some("replace" | "r") => replace(game, &args[1..]),
        Some("walls" | "w") => shell_like(game, &args[1..], false),
        Some("shell") => shell_like(game, &args[1..], true),
        _ => rt::say(
            game,
            "/b sel <pos1|pos2|clear|area|expand|shift|copy|cut|paste|fill|replace|walls|shell>",
        ),
    }
}

fn set_pos(game: &mut Game<'_>, first: bool) {
    let Some(t) = target_block(game) else {
        rt::say(game, "no target block");
        return;
    };
    with(|s| if first { s.p1 = Some(t) } else { s.p2 = Some(t) });
    rt::say(
        game,
        &format!("{} = {} {} {}", if first { "pos1" } else { "pos2" }, t[0], t[1], t[2]),
    );
}

fn area(game: &mut Game<'_>) {
    let (p1, p2) = with(|s| (s.p1, s.p2));
    match (p1, p2) {
        (Some(a), Some(b)) => {
            let (lo, hi) = (
                [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
                [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
            );
            let d = [hi[0] - lo[0] + 1, hi[1] - lo[1] + 1, hi[2] - lo[2] + 1];
            rt::say(
                game,
                &format!(
                    "selection {}x{}x{} = {} blocks ({} {} {} .. {} {} {})",
                    d[0],
                    d[1],
                    d[2],
                    d[0] as i64 * d[1] as i64 * d[2] as i64,
                    lo[0], lo[1], lo[2], hi[0], hi[1], hi[2],
                ),
            );
        }
        _ => {
            let show = |p: Option<[i32; 3]>| {
                p.map_or("unset".to_owned(), |p| format!("{} {} {}", p[0], p[1], p[2]))
            };
            rt::say(game, &format!("pos1: {}  pos2: {}", show(p1), show(p2)));
        }
    }
}

/// `expand` grows the boundary the direction points at; `shift` translates
/// the whole box. Both take n/e/s/w/u/d (or the long names).
fn grow(game: &mut Game<'_>, args: &[&str], shift: bool) {
    let Some(d) = args.first().and_then(|d| dir(d)) else {
        rt::say(game, "/b sel expand|shift <n|e|s|w|u|d> <blocks>");
        return;
    };
    let n = args
        .get(1)
        .and_then(|a| a.parse::<i32>().ok())
        .unwrap_or(1)
        .clamp(-100_000, 100_000);
    let Some((mut lo, mut hi)) = with(|s| s.region()) else {
        rt::say(game, "set pos1 and pos2 first");
        return;
    };
    for a in 0..3 {
        if shift {
            lo[a] += d[a] * n;
            hi[a] += d[a] * n;
        } else if d[a] > 0 {
            hi[a] += n;
        } else if d[a] < 0 {
            lo[a] -= n;
        }
        if lo[a] > hi[a] {
            lo[a] = hi[a];
        }
    }
    with(|s| {
        s.p1 = Some(lo);
        s.p2 = Some(hi);
    });
    let verb = if shift { "shifted" } else { "expanded" };
    rt::say(
        game,
        &format!(
            "{verb} to {} {} {} .. {} {} {}",
            lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]
        ),
    );
}

/// `copy` stores non-air cells as name strings; `cut` also queues the
/// removal of every breakable non-fluid cell.
fn copy(game: &mut Game<'_>, cut: bool) {
    let Some((lo, hi)) = with(|s| s.region()) else {
        rt::say(game, "set pos1 and pos2 first");
        return;
    };
    if !region_ok(game, lo, hi) {
        return;
    }
    let Some(me) = game.player.as_ref().map(rt::feet) else { return };
    let mut clip = Vec::new();
    let mut edits = Vec::new();
    for p in cells_sorted(lo, hi, me) {
        let st = game.world.block(p[0], p[1], p[2]);
        let f = game.res.facts().get(st);
        if rt::is_air(f.name.as_ref()) {
            continue;
        }
        // Bare tail, not the namespaced name: the build queue's Place /
        // hotbar select match on `rt::tail`, so storing `minecraft:stone`
        // would never match and re-break the pasted cell forever.
        clip.push(
            (
                [p[0] - lo[0], p[1] - lo[1], p[2] - lo[2]],
                rt::tail(f.name.as_ref()).to_string(),
            ),
        );
        if cut && f.hardness >= 0.0 && f.fluid == horizon_items::blocks::Fluid::None {
            edits.push(bd::Edit::Break(p));
        }
    }
    let n = clip.len();
    with(|s| {
        s.clip = clip;
        s.clip_size = [hi[0] - lo[0] + 1, hi[1] - lo[1] + 1, hi[2] - lo[2] + 1];
        s.clip_anchor = [lo[0] - me[0], lo[1] - me[1], lo[2] - me[2]];
    });
    if cut {
        let q = bd::enqueue(game, edits);
        rt::say(game, &format!("cut {n} blocks ({q} edits queued)"));
    } else {
        rt::say(game, &format!("copied {n} blocks"));
    }
}

/// `paste` places the clipboard at the same offset from the player's feet
/// it was copied at (WorldEdit anchors the copy at the player).
fn paste(game: &mut Game<'_>) {
    let Some(me) = game.player.as_ref().map(rt::feet) else { return };
    let Some((clip, anchor, size)) = with(|s| {
        if s.clip.is_empty() {
            None
        } else {
            Some((s.clip.clone(), s.clip_anchor, s.clip_size))
        }
    }) else {
        rt::say(game, "clipboard empty — /b sel copy first");
        return;
    };
    let base = [me[0] + anchor[0], me[1] + anchor[1], me[2] + anchor[2]];
    let mut edits: Vec<bd::Edit> = clip
        .into_iter()
        .map(|(o, n)| {
            bd::Edit::Place(
                [base[0] + o[0], base[1] + o[1], base[2] + o[2]],
                n,
            )
        })
        .collect();
    edits.sort_by_key(|e| match e {
        bd::Edit::Place(p, _) => (p[1], p[0].abs() + p[2].abs()),
        bd::Edit::Break(p) => (p[1], p[0].abs() + p[2].abs()),
    });
    let n = edits.len();
    let q = bd::enqueue(game, edits);
    rt::say(
        game,
        &format!(
            "pasting {}x{}x{} ({n} cells, {q} edits queued)",
            size[0], size[1], size[2]
        ),
    );
}

/// `fill <block>`: a Place edit per cell (`fill air` queues Break edits —
/// no held item selects "air"). The queue breaks non-matching occupants
/// itself, so no extra Break edits are needed here.
fn fill(game: &mut Game<'_>, args: &[&str]) {
    let Some(name) = args.first().map(|a| rt::norm(a)) else {
        rt::say(game, "/b sel fill <block>");
        return;
    };
    let Some((lo, hi)) = with(|s| s.region()) else {
        rt::say(game, "set pos1 and pos2 first");
        return;
    };
    if !region_ok(game, lo, hi) {
        return;
    }
    let Some(me) = game.player.as_ref().map(rt::feet) else { return };
    let air = name == "air";
    let edits: Vec<bd::Edit> = cells_sorted(lo, hi, me)
        .into_iter()
        .map(|p| {
            if air {
                bd::Edit::Break(p)
            } else {
                bd::Edit::Place(p, name.clone())
            }
        })
        .collect();
    let n = bd::enqueue(game, edits);
    rt::say(game, &format!("fill {name}: {n} edits queued"));
}

/// `replace <from> <to>`: cells whose current name tail ends with `from`
/// become `to` (`to air` queues Break edits).
fn replace(game: &mut Game<'_>, args: &[&str]) {
    let (Some(from), Some(to)) = (args.first(), args.get(1)) else {
        rt::say(game, "/b sel replace <from> <to>");
        return;
    };
    let (from, to) = (rt::norm(from), rt::norm(to));
    let Some((lo, hi)) = with(|s| s.region()) else {
        rt::say(game, "set pos1 and pos2 first");
        return;
    };
    if !region_ok(game, lo, hi) {
        return;
    }
    let Some(me) = game.player.as_ref().map(rt::feet) else { return };
    let mut edits = Vec::new();
    for p in cells_sorted(lo, hi, me) {
        let st = game.world.block(p[0], p[1], p[2]);
        if !rt::name_match(game.res.facts().get(st).name.as_ref(), &from) {
            continue;
        }
        if to == "air" {
            edits.push(bd::Edit::Break(p));
        } else {
            edits.push(bd::Edit::Place(p, to.clone()));
        }
    }
    let found = edits.len();
    let q = bd::enqueue(game, edits);
    rt::say(game, &format!("replace {from} -> {to}: {found} cells ({q} edits queued)"));
}

/// `walls <block>`: the four vertical sides. `shell <block>`: the whole
/// outer surface (floor and ceiling too).
fn shell_like(game: &mut Game<'_>, args: &[&str], shell: bool) {
    let Some(name) = args.first().map(|a| rt::norm(a)) else {
        rt::say(game, "/b sel walls|shell <block>");
        return;
    };
    let Some((lo, hi)) = with(|s| s.region()) else {
        rt::say(game, "set pos1 and pos2 first");
        return;
    };
    if !region_ok(game, lo, hi) {
        return;
    }
    let Some(me) = game.player.as_ref().map(rt::feet) else { return };
    let on_shell = |p: [i32; 3]| {
        p[0] == lo[0]
            || p[0] == hi[0]
            || p[2] == lo[2]
            || p[2] == hi[2]
            || (shell && (p[1] == lo[1] || p[1] == hi[1]))
    };
    let edits: Vec<bd::Edit> = cells_sorted(lo, hi, me)
        .into_iter()
        .filter(|p| on_shell(*p))
        .map(|p| bd::Edit::Place(p, name.clone()))
        .collect();
    let n = edits.len();
    let q = bd::enqueue(game, edits);
    let kind = if shell { "shell" } else { "walls" };
    rt::say(game, &format!("{kind} {name}: {n} cells ({q} edits queued)"));
}

fn region_ok(game: &mut Game<'_>, lo: [i32; 3], hi: [i32; 3]) -> bool {
    let cells = (hi[0] - lo[0] + 1) as i64
        * (hi[1] - lo[1] + 1) as i64
        * (hi[2] - lo[2] + 1) as i64;
    if cells > MAX_CELLS {
        rt::say(game, &format!("selection too big ({cells} cells, max {MAX_CELLS})"));
        return false;
    }
    true
}
