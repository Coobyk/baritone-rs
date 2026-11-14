// The executor: one `pathfind::Move` becomes per-tick input. Walking moves
// steer yaw + forward; Break aims `forced_hit` and holds `left`; Pillar and
// Bridge aim `forced_hit` at the support face and hold `right` while jumping
// or sneaking.

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
            // Break each listed cell until it is open, then walk in.
            let next = m
                .cells
                .iter()
                .copied()
                .find(|&b| solid(game, b));
            match next {
                Some(b) => {
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
        pf::Kind::Pillar => {
            if !placeable_held(game) {
                return Out::Failed;
            }
            // Jump and place at our feet cell (the move's `cells[0]`).
            let place = m.cells[0];
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
