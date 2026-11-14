// The executor: one `pathfind::Move` becomes per-tick input. Walking moves
// steer yaw + forward; Break aims `forced_hit` and holds `left`; Pillar and
// Bridge aim `forced_hit` at the support face and hold `right` while jumping
// or sneaking.

use std::sync::Mutex;

use crate::game::umbx_baritone as rt;
use crate::game::umbx_pathfind as pf;

pub enum Out {
    Working,
    Done,
    Failed,
}

fn feet(p: &crate::motion::PlayerMotion) -> [i32; 3] {
    [
        p.position.x.floor() as i32,
        p.position.y.floor() as i32,
        p.position.z.floor() as i32,
    ]
}

/// Face `at` (a world point) with the camera yaw.
fn aim(game: &mut crate::game::Game<'_>, at: [f64; 3]) {
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

fn center(p: [i32; 3]) -> [f64; 3] {
    [p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5]
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

fn solid(game: &crate::game::Game<'_>, p: [i32; 3]) -> bool {
    game.res
        .facts()
        .get(game.world.block(p[0], p[1], p[2]))
        .full_collision
}

/// Can take a placed block's face — same rule as pathfind's floors (full
/// collision or a slab/stairs).
fn sturdy(game: &crate::game::Game<'_>, p: [i32; 3]) -> bool {
    let f = game
        .res
        .facts()
        .get(game.world.block(p[0], p[1], p[2]));
    f.full_collision || f.name.contains("slab") || f.name.contains("stairs")
}

/// A sturdy neighbour of `f` and the face that would place `f` on it.
fn support_face(game: &crate::game::Game<'_>, f: [i32; 3]) -> Option<([i32; 3], [i32; 3])> {
    for d in [[1, 0, 0], [-1, 0, 0], [0, 0, 1], [0, 0, -1], [0, -1, 0]] {
        let s = [f[0] + d[0], f[1] + d[1], f[2] + d[2]];
        if sturdy(game, s) {
            return Some((s, [-d[0], -d[1], -d[2]]));
        }
    }
    None
}

/// Ticks spent on the current move; resets when a different move is driven.
/// `tag` keeps kinds of the same pos/cells shape apart (0 break, 1 place).
fn ticks_on(m: &pf::Move, tag: u8) -> u32 {
    static T: Mutex<Option<(u8, [i32; 3], usize, u32)>> = Mutex::new(None);
    let mut g = T.lock().unwrap_or_else(|e| e.into_inner());
    match *g {
        Some((t, p, n, ticks)) if t == tag && p == m.pos && n == m.cells.len() => {
            let ticks = ticks + 1;
            *g = Some((t, p, n, ticks));
            ticks
        }
        _ => {
            *g = Some((tag, m.pos, m.cells.len(), 1));
            1
        }
    }
}

/// Rescan the hotbar once per target block for a better tool.
fn tool_for(game: &mut crate::game::Game<'_>, b: [i32; 3]) {
    static SEEN: Mutex<Option<[i32; 3]>> = Mutex::new(None);
    {
        let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if *g == Some(b) {
            return;
        }
        *g = Some(b);
    }
    let st = game.world.block(b[0], b[1], b[2]);
    let name = game.res.facts().get(st).name.to_string();
    if let Some(slot) = best_tool(game, &name) {
        game.selected = slot;
    }
}

/// Best hotbar slot for `block`, when it scores above the held stack.
fn best_tool(game: &crate::game::Game<'_>, block: &str) -> Option<usize> {
    let mut best: Option<(i32, usize)> = None;
    for (i, it) in game.inventory.iter().take(9).enumerate() {
        let Some(it) = it else { continue };
        let s = tool_score(&it.item, block);
        if best.map_or(true, |(b, _)| s > b) {
            best = Some((s, i));
        }
    }
    let held = game.held().map_or(0, |it| tool_score(&it.item, block));
    match best {
        Some((s, i)) if s > held && s > 0 => Some(i),
        _ => None,
    }
}

/// Tool quality vs a block name: 10+tier when the tool kind fits the
/// material, bare tier for an off-kind tool, 0 for non-tools.
fn tool_score(item: &str, block: &str) -> i32 {
    let pick = item.contains("pickaxe");
    let axe = !pick && item.contains("axe");
    let shovel = item.contains("shovel");
    let shears = item.contains("shears");
    let sword = item.contains("sword");
    if !(pick || axe || shovel || shears || sword) {
        return 0;
    }
    let bh = |keys: &[&str]| keys.iter().any(|k| block.contains(k));
    let kind = (pick
        && bh(&[
            "ore", "stone", "deepslate", "sandstone", "netherrack", "iron", "gold",
            "diamond", "emerald", "lapis", "redstone", "copper", "coal", "obsidian",
            "anvil", "furnace", "brick",
        ]))
        || (axe && bh(&["log", "wood", "stem", "planks"]))
        || (shovel && bh(&["dirt", "sand", "gravel", "clay", "snow", "mud", "soul"]))
        || (shears && bh(&["leaves", "wool", "vine"]))
        || (sword && bh(&["web", "bamboo"]));
    let tier = ["netherite", "diamond", "iron", "stone", "golden", "wooden"]
        .iter()
        .position(|t| item.contains(t))
        .map_or(0, |i| (6 - i) as i32);
    if kind {
        10 + tier
    } else {
        tier
    }
}

/// Is the held hotbar stack a placeable block?
fn placeable_held(game: &crate::game::Game<'_>) -> bool {
    let Some(it) = game.held() else { return false };
    game.res
        .facts()
        .states
        .iter()
        .any(|s| s.item.as_ref() == it.item.as_ref() && s.full_collision)
}

/// Advance the current move; writes `st.want` for the input hook and touches
/// `game` directly for yaw, hit, left/right and sprinting.
pub fn drive(
    game: &mut crate::game::Game<'_>,
    st: &mut rt::State,
    m: &pf::Move,
    prev: [i32; 3],
) -> Out {
    let Some(player) = game.player.as_ref() else {
        return Out::Working;
    };
    let pos = player.position;
    let _on_ground = player.ground.on_ground;
    let me = feet(player);
    // Break moves finish by clearing their cells even when we already stand
    // on the destination (the mine's last step breaks the block itself).
    if me == m.pos && !matches!(m.kind, pf::Kind::Break { .. }) {
        st.want = rt::Want::default();
        return Out::Done;
    }
    let mut w = rt::Want {
        sprint: st.set.sprint,
        ..rt::Want::default()
    };
    let c = center(m.pos);
    match m.kind {
        pf::Kind::Walk | pf::Kind::Drop { .. } | pf::Kind::Swim => {
            aim(game, c);
            w.forward = 1.0;
            if m.kind == pf::Kind::Swim {
                // Stay up: hold jump while crossing water.
                w.jump = true;
            }
        }
        pf::Kind::Step => {
            aim(game, c);
            w.forward = 1.0;
            w.jump = true;
        }
        pf::Kind::Parkour { .. } => {
            aim(game, c);
            w.forward = 1.0;
            w.sprint = true;
            w.jump = true;
        }
        pf::Kind::Ladder { n } => {
            aim(game, [c[0], pos.y, c[2]]);
            w.forward = 1.0;
            if n < 0 {
                w.sneak = true;
            }
        }
        pf::Kind::Break { .. } => {
            // 200-tick fuse on the cells list before we give up.
            if ticks_on(m, 0) > 200 {
                return Out::Failed;
            }
            // Break each listed cell until it is open, then walk in.
            let next = m
                .cells
                .iter()
                .copied()
                .find(|&b| solid(game, b));
            match next {
                Some(b) => {
                    tool_for(game, b);
                    aim(game, center(b));
                    // Face the block's top face and hold attack.
                    w.hit = Some(face_hit(b, [0, 1, 0]));
                    w.attack = true;
                    w.forward = 0.0;
                    st.want = w;
                    return Out::Working;
                }
                None if me == m.pos => {
                    st.want = rt::Want::default();
                    return Out::Done;
                }
                None => {
                    aim(game, c);
                    w.forward = 1.0;
                }
            }
        }
        pf::Kind::Downward => {
            // Break the floor cell under our feet, then drop in.
            let f = m.cells[0];
            if solid(game, f) {
                if ticks_on(m, 0) > 200 {
                    return Out::Failed;
                }
                tool_for(game, f);
                aim(game, center(f));
                w.hit = Some(face_hit(f, [0, 1, 0]));
                w.attack = true;
                w.forward = 0.0;
                st.want = w;
                return Out::Working;
            }
            if me[1] <= m.pos[1] {
                st.want = rt::Want::default();
                return Out::Done;
            }
            // Floor's gone — gravity does the rest.
            w.forward = 0.0;
        }
        pf::Kind::Traverse => {
            if !placeable_held(game) {
                return Out::Failed;
            }
            // Place the listed cells in order (approach support first, then
            // the diagonal floor), hanging each off a neighbour's face.
            let todo = m.cells.iter().copied().find(|&f| !solid(game, f));
            match todo {
                Some(f) => {
                    let Some((s, face)) = support_face(game, f) else {
                        return Out::Failed;
                    };
                    let hit = face_hit(s, face);
                    aim(game, hit.point);
                    w.sneak = true;
                    w.sprint = false;
                    w.forward = 1.0;
                    w.hit = Some(hit);
                    w.right = true;
                }
                None => {
                    aim(game, c);
                    w.forward = 1.0;
                }
            }
        }
        pf::Kind::Pillar => {
            if !placeable_held(game) {
                return Out::Failed;
            }
            // Jump and place at our feet cell (the move's `cells[0]`); bail
            // if the placement hasn't materialised in ~2s.
            let place = m.cells[0];
            if !solid(game, place) && ticks_on(m, 1) > 40 {
                return Out::Failed;
            }
            w.sneak = false;
            w.jump = true;
            w.forward = 0.0;
            if !solid(game, place) {
                w.hit = Some(face_hit(
                    [place[0], place[1] - 1, place[2]],
                    [0, 1, 0],
                ));
                w.right = true;
            }
            aim(game, [c[0], pos.y - 1.0, c[2]]);
        }
        pf::Kind::Bridge => {
            if !placeable_held(game) {
                return Out::Failed;
            }
            let place = m.cells[0];
            if solid(game, place) {
                // Floor placed: walk the last step normally.
                aim(game, c);
                w.forward = 1.0;
            } else {
                // Sneak to the edge and place on the support's side face.
                w.sneak = true;
                w.sprint = false;
                let floor = [prev[0], prev[1] - 1, prev[2]];
                let face = [
                    (m.pos[0] - prev[0]).signum(),
                    0,
                    (m.pos[2] - prev[2]).signum(),
                ];
                w.hit = Some(face_hit(floor, face));
                w.right = true;
                aim(game, center(place));
                w.forward = 1.0;
            }
        }
    }
    st.want = w;
    Out::Working
}

/// Write `st.want` onto the game: movement inputs are delivered through the
/// `native_movement_input` hook, everything else here.
pub fn apply(game: &mut crate::game::Game<'_>, w: &rt::Want) {
    let w = *w;
    game.forced_hit = w.hit;
    if w.attack || w.right {
        if w.attack {
            game.left = true;
        }
        if w.right {
            game.right = true;
            game.swing_arm();
        }
    }
    if let Some(p) = game.player.as_mut() {
        p.sprinting = w.sprint && w.forward > 0.0;
    }
}
