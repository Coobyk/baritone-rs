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
    game.pitch = pitch;
    let p = game.player.as_mut().unwrap();
    p.yaw = yaw;
    p.head_yaw = yaw;
    p.pitch = pitch;
}

fn center(p: [i32; 3]) -> [f64; 3] {
    [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5]
}

/// The face of `b` most visible from the player's eye — dominant axis from
/// the block centre toward the eye.
fn face_toward(game: &crate::game::Game<'_>, b: [i32; 3]) -> [i32; 3] {
    let Some(p) = game.player.as_ref() else { return [0, 1, 0] };
    let eye = [p.position.x, p.position.y + 1.62, p.position.z];
    let d = [
        eye[0] - (b[0] as f64 + 0.5),
        eye[1] - (b[1] as f64 + 0.5),
        eye[2] - (b[2] as f64 + 0.5),
    ];
    if d[0].abs() >= d[1].abs() && d[0].abs() >= d[2].abs() {
        [d[0].signum() as i32, 0, 0]
    } else if d[1].abs() >= d[2].abs() {
        [0, d[1].signum() as i32, 0]
    } else {
        [0, 0, d[2].signum() as i32]
    }
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
    let facts = game.res.facts().get(st);
    let name = facts.name.to_string();
    let requires = facts.requires_tool;
    if let Some(slot) = best_tool(game, &name, requires) {
        game.selected = slot;
    }
}

/// Best hotbar slot for `block` by vanilla's own mining rules
/// (`Items::mining_speed` + `has_correct_tool_for_drops`), when it beats the
/// held stack. Upstream AutoTool: a tool that can't harvest is nearly
/// worthless on `requires_tool` blocks, so it scores a hundredth.
fn best_tool(game: &crate::game::Game<'_>, block: &str, requires: bool) -> Option<usize> {
    let items = game.res.items();
    let score = |held: Option<&horizon_items::items::ItemStack>| -> f32 {
        let speed = items.mining_speed(held, block);
        if items.has_correct_tool_for_drops(held, requires, block) {
            speed
        } else {
            speed / 100.0
        }
    };
    let held = score(game.held());
    let mut best: Option<(f32, usize)> = None;
    for (i, it) in game.inventory.iter().take(9).enumerate() {
        let s = score(it.as_ref());
        if best.map_or(true, |(b, _)| s > b) {
            best = Some((s, i));
        }
    }
    match best {
        Some((s, i)) if s > held => Some(i),
        _ => None,
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

/// Hotbar slot holding a placeable block, preferring upstream's throwaway
/// kinds (dirt, cobble, netherrack…) over anything valuable.
fn place_slot(game: &crate::game::Game<'_>) -> Option<usize> {
    const THROWAWAY: &[&str] = &[
        "dirt",
        "cobblestone",
        "cobbled_deepslate",
        "netherrack",
        "grass_block",
        "stone",
        "deepslate",
        "sand",
        "gravel",
        "andesite",
        "diorite",
        "granite",
        "tuff",
        "planks",
    ];
    let mut any: Option<usize> = None;
    for (i, it) in game.inventory.iter().take(9).enumerate() {
        let Some(it) = it else { continue };
        let name = it.item.as_ref();
        if !game
            .res
            .facts()
            .states
            .iter()
            .any(|s| s.item.as_ref() == name && s.full_collision)
        {
            continue;
        }
        if THROWAWAY.iter().any(|t| name.contains(t)) {
            return Some(i);
        }
        if any.is_none() {
            any = Some(i);
        }
    }
    any
}

/// Upstream's throwaway check for the planner: any placeable hotbar block.
pub fn has_placeable(game: &crate::game::Game<'_>) -> bool {
    place_slot(game).is_some()
}

/// Pillar/Bridge/Traverse all need a block in hand first: switch to one and
/// spend a tick letting the selection land before the caller re-runs.
fn hold_block(game: &mut crate::game::Game<'_>) -> Out {
    match place_slot(game) {
        Some(i) => {
            game.selected = i;
            Out::Working
        }
        None => Out::Failed,
    }
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
    let on_ground = player.ground.on_ground;
    // Swimming: the body never reports on_ground while floating, so moves
    // that demand it would never arrive — water counts as its own footing.
    let in_water = player.fluids.was_touching_water;
    let eye_in_water = player.fluids.was_eye_in_water;
    let footing = on_ground || in_water;
    let me = feet(player);
    let c = center(m.pos);
    // A move ends once we're firmly on its cell — integer feet can hover on
    // the cell edge (aim at the centre flips 180° each tick), so also accept
    // standing within ~0.4 blocks of the centre at the right height.
    let mut arrived = me == m.pos;
    if !arrived
        && !matches!(m.kind, pf::Kind::Break { .. })
        && (pos.x - c[0]).powi(2) + (pos.z - c[2]).powi(2) < 0.16
        && pos.y > c[1] - 0.6
        && pos.y < c[1] + 1.4
    {
        arrived = true;
    }
    // Landing moves arrive when the player STANDS at the target — a hop apex
    // sweeping through the cell mid-air doesn't count (they bounce off and
    // come back down short of it). Pillar additionally needs its placed
    // block to actually exist.
    if arrived
        && matches!(
            m.kind,
            pf::Kind::Walk
                | pf::Kind::Step
                | pf::Kind::Pillar
                | pf::Kind::Bridge
                | pf::Kind::Traverse
                | pf::Kind::Parkour { .. }
                | pf::Kind::ParkourPlace { .. }
        )
        && !footing
    {
        arrived = false;
    }
    if arrived && matches!(m.kind, pf::Kind::Pillar) && !solid(game, m.cells[0]) {
        arrived = false;
    }
    if arrived && !matches!(m.kind, pf::Kind::Break { .. }) {
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
            // A 1-cell swim that can't land in ~5s is pressed against a
            // wall or fighting a current — failing forces a re-plan, and
            // submerged cells now cost enough to route around the water.
            if m.kind == pf::Kind::Swim && ticks_on(m, 3) > 100 {
                return Out::Failed;
            }
            aim(game, c);
            w.forward = 1.0;
            // Stay up while crossing water; hop when the route climbs even
            // within what the planner labelled a flat walk (slabs, snow,
            // slopes whose midpoint cell read as the same y).
            w.jump = m.kind == pf::Kind::Swim || m.pos[1] > me[1];
            // `sprintInWater` off: no sprint-gliding through water.
            if m.kind == pf::Kind::Swim && !st.set.sprint_water {
                w.sprint = false;
            }
        }
        pf::Kind::Step => {
            aim(game, c);
            w.forward = 1.0;
            // No sprint: a sprint-jump overshoots the ledge and hits the
            // face low. A controlled hop is what mounts a 1-block step.
            w.sprint = false;
            w.jump = true;
        }
        pf::Kind::Parkour { .. } => {
            aim(game, c);
            w.forward = 1.0;
            w.sprint = true;
            w.jump = true;
        }
        pf::Kind::ParkourPlace { .. } => {
            let fl = m.cells[0];
            if solid(game, fl) {
                // Floor's down (placed it last tick, or it already was):
                // ordinary parkour drive to the landing.
                aim(game, c);
                w.forward = 1.0;
                w.sprint = true;
                w.jump = true;
            } else {
                if ticks_on(m, 0) > 200 {
                    return Out::Failed;
                }
                if !placeable_held(game) {
                    return hold_block(game);
                }
                let Some((s, face)) = support_face(game, fl) else {
                    return Out::Failed;
                };
                let hit = face_hit(s, face);
                // Steer toward the landing; the forced hit clicks the
                // support face regardless of look direction, so the block
                // lands under us as we cross the gap.
                aim(game, c);
                w.forward = 1.0;
                w.sprint = true;
                w.jump = true;
                w.hit = Some(hit);
                w.right = true;
            }
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
                    // `toolSelect`: keep the held item when off.
                    if st.set.tool_select {
                        tool_for(game, b);
                    }
                    aim(game, center(b));
                    // Face the side the player can actually see.
                    w.hit = Some(face_hit(b, face_toward(game, b)));
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
                if st.set.tool_select {
                    tool_for(game, f);
                }
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
                return hold_block(game);
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
                return hold_block(game);
            }
            // Jump and place at our feet cell (the move's `cells[0]`). The
            // use rate-limit (use_delay=4) means a placement only lands when
            // a use tick falls inside the few airborne frames where the body
            // has vacated the cell — ~1 in 4 jumps — so the bail fuse has to
            // allow many hops, not two seconds.
            let place = m.cells[0];
            if !solid(game, place) && ticks_on(m, 1) > 200 {
                return Out::Failed;
            }
            w.sneak = false;
            w.jump = true;
            if !solid(game, place) {
                // Still placing: hop in place over the cell and put the block
                // on the support face under it.
                w.forward = 0.0;
                w.hit = Some(face_hit(
                    [place[0], place[1] - 1, place[2]],
                    [0, 1, 0],
                ));
                w.right = true;
                aim(game, [c[0], pos.y - 1.0, c[2]]);
            } else {
                // Block's down: steer onto it — jumping in place beside the
                // new floor never completes the move.
                aim(game, c);
                w.forward = 1.0;
            }
        }
        pf::Kind::Bridge => {
            if !placeable_held(game) {
                return hold_block(game);
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
    // Drowning beats route fidelity: a submerged head or a floating body
    // with no footing keeps paddling upward whatever the move wants.
    if eye_in_water || (in_water && !on_ground) {
        w.jump = true;
    }
    st.want = w;
    Out::Working
}

/// Write `st.want` onto the game: movement inputs are delivered through the
/// `native_movement_input` hook, everything else here.
pub fn apply(game: &mut crate::game::Game<'_>, w: &rt::Want) {
    let w = *w;
    game.forced_hit = w.hit;
    // `left`/`right` latch: the engine only rewrites them on input events, so
    // a flag we set and never clear keeps clicking forever after the process
    // stops. Release only the ones we ourselves set.
    static HELD: Mutex<(bool, bool)> = Mutex::new((false, false));
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    if w.attack {
        game.left = true;
        held.0 = true;
    } else if held.0 {
        game.left = false;
        held.0 = false;
    }
    if w.right {
        game.right = true;
        held.1 = true;
        game.swing_arm();
    } else if held.1 {
        game.right = false;
        held.1 = false;
    }
    drop(held);
    if let Some(p) = game.player.as_mut() {
        p.sprinting = w.sprint && w.forward > 0.0;
    }
}
