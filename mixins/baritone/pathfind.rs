// A* over block cells with Baritone-style movement actions. A node is a
// standable cell; an edge is a Movement (walk, step, drop, parkour jump,
// ladder climb, swim, break-through, pillar, bridge). Break/place edges only
// appear when the matching `allow_*` flag is on.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::collections::HashMap;

pub const MAX_NODES: usize = 120_000;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Walk,
    Step,
    /// Fall `n` cells (2..=max_drop).
    Drop { n: i8 },
    /// Jump a horizontal `gap` (1 or 2 empty cells).
    Parkour { gap: i8 },
    /// Ride a ladder/vine column `n` cells (signed).
    Ladder { n: i8 },
    Swim,
    /// Break `count` cells listed in `cells` before walking in.
    Break { count: u8 },
    /// Rise straight up one cell, placing a block under feet.
    Pillar,
    /// Cross to a floor-less cell, placing the floor first.
    Bridge,
}

/// One path edge: arrive at `pos` by `kind`, breaking `cells` first when
/// `kind == Break` or placing `cells` when `kind` is Pillar/Bridge.
#[derive(Clone)]
pub struct Move {
    pub pos: [i32; 3],
    pub kind: Kind,
    /// Blocks to break (Break) or the cell to place (Pillar/Bridge) plus the
    /// support hit as `cells[1]` (`cells[0]` is always the placed cell).
    pub cells: Vec<[i32; 3]>,
}

pub struct Opts {
    pub allow_break: bool,
    pub allow_place: bool,
    pub allow_parkour: bool,
    pub max_drop: i32,
}

/// Read-only world facts the planner needs, borrowing from `Game`.
pub struct W<'a> {
    pub game: &'a crate::game::Game<'a>,
}

impl W<'_> {
    /// Facts of the block state at `p` (unloaded chunks read as air).
    fn fat(&self, p: [i32; 3]) -> &horizon_items::blocks::StateFacts {
        self.game
            .res
            .facts()
            .get(self.game.world.block(p[0], p[1], p[2]))
    }

    /// Highest Y worth scanning (world top minus headroom).
    fn top(&self) -> i32 {
        self.game.world.shape.max_y() - 2
    }

    /// The body's two cells are free to stand in (no full collision, no lava).
    pub fn body_open(&self, p: [i32; 3]) -> bool {
        self.open_cell(p) && self.open_cell([p[0], p[1] + 1, p[2]])
    }

    pub fn open_cell(&self, p: [i32; 3]) -> bool {
        let f = self.fat(p);
        !f.full_collision && f.fluid != horizon_items::blocks::Fluid::Lava
    }

    /// Something solid to stand on under `p`.
    pub fn floor(&self, p: [i32; 3]) -> bool {
        self.fat([p[0], p[1] - 1, p[2]]).full_collision
    }

    /// `p` is standable: two open cells over a floor.
    pub fn standable(&self, p: [i32; 3]) -> bool {
        self.floor(p) && self.body_open(p)
    }

    /// `p` can be broken through by the player.
    pub fn breakable(&self, p: [i32; 3]) -> bool {
        let f = self.fat(p);
        f.hardness >= 0.0 && f.fluid != horizon_items::blocks::Fluid::Lava
    }

    fn break_cost(&self, p: [i32; 3]) -> f64 {
        4.0 + (self.fat(p).hardness.max(0.05) * 5.0).min(40.0) as f64
    }

    /// Ladders, vines, chains, scaffolding: climbable by name.
    fn climbable(&self, p: [i32; 3]) -> bool {
        let n = &self.fat(p).name;
        n.contains("ladder") || n.contains("vine") || n.contains("scaffold")
    }

    fn water(&self, p: [i32; 3]) -> bool {
        self.fat(p).fluid == horizon_items::blocks::Fluid::Water
    }
}

fn h(p: [i32; 3], g: [i32; 3]) -> f64 {
    let dx = (p[0] - g[0]).abs() as f64;
    let dy = (p[1] - g[1]).abs() as f64;
    let dz = (p[2] - g[2]).abs() as f64;
    dx + dy * 1.5 + dz
}

const DIRS: [[i32; 2]; 8] = [
    [1, 0],
    [-1, 0],
    [0, 1],
    [0, -1],
    [1, 1],
    [1, -1],
    [-1, 1],
    [-1, -1],
];

/// Generate every successor edge from standable cell `p`.
fn succ(w: &W, p: [i32; 3], o: &Opts, out: &mut Vec<Move>) {
    // Vertical moves first: ladder, pillar, drop of any depth.
    let up = [p[0], p[1] + 1, p[2]];
    if w.body_open(up) {
        if w.climbable(p) || w.climbable([p[0], p[1] - 1, p[2]]) || w.climbable(up) {
            out.push(Move {
                pos: up,
                kind: Kind::Ladder { n: 1 },
                cells: vec![],
            });
        } else if o.allow_place {
            // Pillar: stand on a block placed at our feet cell.
            out.push(Move {
                pos: up,
                kind: Kind::Pillar,
                cells: vec![p],
            });
        }
    }
    if w.climbable(p) {
        // Climb back down the ladder.
        let down = [p[0], p[1] - 1, p[2]];
        if w.climbable(down) || w.open_cell(down) {
            out.push(Move {
                pos: down,
                kind: Kind::Ladder { n: -1 },
                cells: vec![],
            });
        }
    }
    // Straight drop of 2..=max_drop when the ledge under us is open air.
    if !w.floor(p) {
        let mut d = 1;
        while d <= o.max_drop {
            let c = [p[0], p[1] - d, p[2]];
            if !w.body_open(c) {
                break;
            }
            if w.floor(c) {
                if d >= 2 {
                    out.push(Move {
                        pos: c,
                        kind: Kind::Drop { n: d as i8 },
                        cells: vec![],
                    });
                }
                break;
            }
            d += 1;
        }
    }

    for [dx, dz] in DIRS {
        let diag = dx != 0 && dz != 0;
        // Vanilla can't cut a diagonal through two solid corners.
        if diag
            && (!w.open_cell([p[0] + dx, p[1], p[2]]) || !w.open_cell([p[0], p[1], p[2] + dz]))
        {
            continue;
        }
        let n = [p[0] + dx, p[1], p[2] + dz];
        if w.standable(n) {
            out.push(Move {
                pos: n,
                kind: Kind::Walk,
                cells: vec![],
            });
            continue;
        }
        // Step up onto a 1-block ledge.
        let up1 = [n[0], n[1] + 1, n[2]];
        if w.standable(up1) && w.open_cell([p[0], p[1] + 2, p[2]]) {
            out.push(Move {
                pos: up1,
                kind: Kind::Step,
                cells: vec![],
            });
            continue;
        }
        // Drop down the ledge (2..=max_drop).
        if w.body_open(n) {
            let mut d = 1;
            while d <= o.max_drop {
                let c = [n[0], n[1] - d, n[2]];
                if !w.body_open(c) {
                    break;
                }
                if w.floor(c) {
                    out.push(Move {
                        pos: c,
                        kind: Kind::Drop { n: d as i8 },
                        cells: vec![],
                    });
                    break;
                }
                d += 1;
            }
        }
        // Swim: neighbours inside water.
        if w.water(n) && w.open_cell([n[0], n[1] + 1, n[2]]) {
            out.push(Move {
                pos: n,
                kind: Kind::Swim,
                cells: vec![],
            });
        }
        // Parkour: jump over 1..2 open gap cells to standable ground.
        if o.allow_parkour && !diag {
            for gap in 1..=2_i32 {
                let land = [p[0] + dx * (gap + 1), p[1], p[2] + dz * (gap + 1)];
                let mut clear = w.standable(land);
                for g in 1..=gap {
                    clear = clear && w.body_open([p[0] + dx * g, p[1], p[2] + dz * g]);
                }
                if clear {
                    out.push(Move {
                        pos: land,
                        kind: Kind::Parkour { gap: gap as i8 },
                        cells: vec![],
                    });
                }
            }
        }
        // Bridge: `n`'s floor is missing but the body is open; place it.
        if o.allow_place
            && w.body_open(n)
            && !w.floor(n)
            && !w.climbable(p)
            && self_has_bridge_support(w, p, n)
        {
            out.push(Move {
                pos: n,
                kind: Kind::Bridge,
                cells: vec![[n[0], n[1] - 1, n[2]]],
            });
        }
        // Break: the body is walled by breakable blocks.
        if o.allow_break && !diag {
            let feet_ok = w.open_cell(n) || w.breakable(n);
            let head = [n[0], n[1] + 1, n[2]];
            let head_ok = w.open_cell(head) || w.breakable(head);
            if feet_ok && head_ok && (!w.open_cell(n) || !w.open_cell(head)) {
                let mut cells = Vec::new();
                if !w.open_cell(n) {
                    cells.push(n);
                }
                if !w.open_cell(head) {
                    cells.push(head);
                }
                // Don't tunnel through the floor: the move ends standable only
                // if there is (still) a floor, or the drop below is passable.
                if w.floor(n) || w.breakable([n[0], n[1] - 1, n[2]]) {
                    out.push(Move {
                        pos: n,
                        kind: Kind::Break {
                            count: cells.len() as u8,
                        },
                        cells,
                    });
                }
            }
        }
    }
}

/// Can the floor under `n` be placed from `p`? A side face of `p`'s floor
/// must be sturdy (the block we stand on), facing `n`.
fn self_has_bridge_support(w: &W, p: [i32; 3], _n: [i32; 3]) -> bool {
    w.floor(p)
}

/// A* from `start` to `goal`. `done` lets goals accept more than one cell
/// (GoalXZ, GoalYLevel, GoalNear). Returns the move list, or None on failure.
pub fn find(
    w: &W,
    start: [i32; 3],
    goal: [i32; 3],
    o: &Opts,
    done: impl Fn([i32; 3], [i32; 3]) -> bool,
) -> Option<Vec<Move>> {
    if done(start, goal) {
        return Some(Vec::new());
    }
    // Heap of lowest-f frontier nodes; `at` identifies the cell.
    struct Frontier {
        f: f64,
        at: [i32; 3],
    }
    impl PartialEq for Frontier {
        fn eq(&self, o: &Self) -> bool {
            self.f == o.f
        }
    }
    impl Eq for Frontier {}
    impl PartialOrd for Frontier {
        fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
            Some(self.cmp(o))
        }
    }
    impl Ord for Frontier {
        fn cmp(&self, o: &Self) -> Ordering {
            o.f.partial_cmp(&self.f).unwrap_or(Ordering::Equal)
        }
    }

    let mut open = BinaryHeap::new();
    let mut g: HashMap<[i32; 3], f64> = HashMap::new();
    // Predecessor: cell -> the move that ended there (its `pos` is the cell).
    let mut came: HashMap<[i32; 3], ([i32; 3], Move)> = HashMap::new();
    g.insert(start, 0.0);
    open.push(Frontier { f: h(start, goal), at: start });
    let mut pops = 0usize;

    while let Some(Frontier { at: p, .. }) = open.pop() {
        if done(p, goal) {
            let mut out = Vec::new();
            let mut cur = p;
            while let Some((prev, m)) = came.get(&cur) {
                out.push(m.clone());
                cur = *prev;
            }
            out.reverse();
            return Some(out);
        }
        pops += 1;
        if pops > MAX_NODES {
            return None;
        }
        let gp = g[&p];
        let mut ns = Vec::new();
        succ(w, p, o, &mut ns);
        for m in ns {
            let edge = match m.kind {
                Kind::Walk => if (m.pos[0] - p[0]).abs() + (m.pos[2] - p[2]).abs() == 2 { 1.42 } else { 1.0 },
                Kind::Step => 2.0,
                Kind::Drop { n } => 1.0 + n as f64 * 0.6,
                Kind::Parkour { gap } => 2.0 + gap as f64,
                Kind::Ladder { .. } => 1.8,
                Kind::Swim => 2.2,
                Kind::Break { .. } => 1.0 + m.cells.iter().map(|c| w.break_cost(*c)).sum::<f64>(),
                Kind::Pillar => 3.5,
                Kind::Bridge => 3.0,
            };
            let ng = gp + edge;
            if ng < *g.get(&m.pos).unwrap_or(&f64::INFINITY) {
                g.insert(m.pos, ng);
                came.insert(m.pos, (p, m.clone()));
                open.push(Frontier { f: ng + h(m.pos, goal), at: m.pos });
            }
        }
    }
    None
}

/// Nearest standable cell in column `(x,z)` to `near_y` (goto x z's goal).
pub fn column_top(w: &W, x: i32, z: i32, near_y: i32) -> Option<[i32; 3]> {
    let mut best: Option<(i32, [i32; 3])> = None;
    for y in -63..=w.top() {
        let p = [x, y, z];
        if w.standable(p) {
            let d = (y - near_y).abs();
            if best.as_ref().map_or(true, |(b, _)| d < *b) {
                best = Some((d, p));
            }
            if y > near_y + 64 && best.is_some() {
                break;
            }
        }
    }
    best.map(|(_, p)| p)
}
