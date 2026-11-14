// A* over the block grid, on foot only: it steps up one block, drops up to
// MAX_DROP and cuts diagonals between open corners. It does not break or
// place blocks, bridge, climb or swim — paths it cannot walk, it reports as
// not found.
use super::Game;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

type Node = [i32; 3];

/// Expanded-node budget: a path that does not settle inside it fails.
const MAX_NODES: usize = 80_000;
/// The furthest drop taken, blocks of fall (three is no damage).
const MAX_DROP: i32 = 4;
const DIAG: f32 = std::f32::consts::SQRT_2;
/// Extra cost for the climb and for each fallen block.
const UP: f32 = 1.6;
const PER_FALL: f32 = 0.8;

fn solid(game: &Game<'_>, x: i32, y: i32, z: i32) -> bool {
    game.res.facts().get(game.world.block(x, y, z)).full_collision
}

/// Two open cells, for the body's feet and head.
fn open(game: &Game<'_>, x: i32, y: i32, z: i32) -> bool {
    !solid(game, x, y, z) && !solid(game, x, y + 1, z)
}

/// Open feet and head on a solid floor: somewhere to stand.
fn standable(game: &Game<'_>, x: i32, y: i32, z: i32) -> bool {
    open(game, x, y, z) && solid(game, x, y - 1, z)
}

fn step(game: &Game<'_>, n: Node, dx: i32, dz: i32, out: &mut Vec<(Node, f32)>) {
    let (x, y, z) = (n[0], n[1], n[2]);
    let (tx, tz) = (x + dx, z + dz);
    let dist = if dx != 0 && dz != 0 { DIAG } else { 1.0 };
    // No corner cutting: a diagonal needs both orthogonal cells open.
    if dx != 0 && dz != 0 && (!open(game, tx, y, z) || !open(game, x, y, tz)) {
        return;
    }
    // A wall at feet level: stand on it, with room above it and above us.
    if solid(game, tx, y, tz) {
        if !solid(game, x, y + 2, z)
            && !solid(game, tx, y + 1, tz)
            && !solid(game, tx, y + 2, tz)
        {
            out.push(([tx, y + 1, tz], dist * UP));
        }
        return;
    }
    if !open(game, tx, y, tz) {
        return;
    }
    if solid(game, tx, y - 1, tz) {
        out.push(([tx, y, tz], dist));
        return;
    }
    // A drop: land on the first floor, at most MAX_DROP blocks down.
    for k in 2..=MAX_DROP + 1 {
        if solid(game, tx, y - k, tz) {
            if open(game, tx, y - k + 1, tz) {
                out.push(([tx, y - k + 1, tz], dist + PER_FALL * (k - 1) as f32));
            }
            break;
        }
    }
}

fn heuristic(n: Node, to: Node) -> f32 {
    let dx = (n[0] - to[0]).abs() as f32;
    let dz = (n[2] - to[2]).abs() as f32;
    let (mn, mx) = (dx.min(dz), dx.max(dz));
    mn * (DIAG - 1.0) + mx + (n[1] - to[1]).abs() as f32 * 1.5
}

struct Open {
    f: f32,
    g: f32,
    n: Node,
}
impl PartialEq for Open {
    fn eq(&self, o: &Self) -> bool {
        self.n == o.n
    }
}
impl Eq for Open {}
impl PartialOrd for Open {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Open {
    fn cmp(&self, o: &Self) -> Ordering {
        // A min-heap: smaller f first, then the deeper node (bigger g), then
        // the position for determinism.
        o.f.partial_cmp(&self.f)
            .unwrap_or(Ordering::Equal)
            .then_with(|| self.g.partial_cmp(&o.g).unwrap_or(Ordering::Equal))
            .then_with(|| self.n.cmp(&o.n))
    }
}

/// A walkable path's standing nodes from just after `from` to `to`, or None.
pub(super) fn find(game: &Game<'_>, from: Node, to: Node) -> Option<Vec<Node>> {
    if from == to {
        return Some(Vec::new());
    }
    let mut came: HashMap<Node, Node> = HashMap::new();
    let mut cost: HashMap<Node, f32> = HashMap::new();
    let mut heap = BinaryHeap::new();
    cost.insert(from, 0.0);
    heap.push(Open {
        f: heuristic(from, to),
        g: 0.0,
        n: from,
    });
    let mut expanded = 0usize;
    while let Some(Open { n, g, .. }) = heap.pop() {
        if n == to {
            let mut path = vec![to];
            let mut cur = to;
            while let Some(&p) = came.get(&cur) {
                path.push(p);
                cur = p;
            }
            path.reverse();
            path.remove(0);
            return Some(path);
        }
        if expanded >= MAX_NODES {
            return None;
        }
        expanded += 1;
        if g > cost.get(&n).copied().unwrap_or(f32::INFINITY) + 0.001 {
            continue;
        }
        let mut succ = Vec::with_capacity(8);
        for dz in -1..=1 {
            for dx in -1..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                step(game, n, dx, dz, &mut succ);
            }
        }
        for (m, c) in succ {
            let ng = g + c;
            if ng < cost.get(&m).copied().unwrap_or(f32::INFINITY) {
                cost.insert(m, ng);
                came.insert(m, n);
                heap.push(Open {
                    f: ng + heuristic(m, to),
                    g: ng,
                    n: m,
                });
            }
        }
    }
    None
}

/// The standable node at or nearest `p` vertically (the feet may be mid-air).
pub(super) fn nearest_standable(game: &Game<'_>, p: Node) -> Option<Node> {
    for dy in [0, -1, -2, 1, 2] {
        let y = p[1] + dy;
        if standable(game, p[0], y, p[2]) {
            return Some([p[0], y, p[2]]);
        }
    }
    None
}

/// The standable node in column `(x, z)` nearest `near_y`: `goto x z`'s goal.
pub(super) fn column_top(game: &Game<'_>, x: i32, z: i32, near_y: i32) -> Option<Node> {
    let mut best: Option<(i32, i32)> = None;
    for y in game.world.shape.min_y + 1..game.world.shape.max_y() - 1 {
        if standable(game, x, y, z) {
            let d = (y - near_y).abs();
            if best.is_none_or(|(b, _)| d < b) {
                best = Some((d, y));
            }
            if y > near_y + 64 && best.is_some() {
                break;
            }
        }
    }
    best.map(|(_, y)| [x, y, z])
}
