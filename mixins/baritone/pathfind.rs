// A* over block cells with Baritone-style movement actions. A node is a
// standable cell; an edge is a Movement (walk, step, drop, parkour jump,
// ladder climb, swim, break-through, pillar, bridge, diagonal traverse,
// downward dig). Break/place edges only appear when the matching `allow_*`
// flag is on.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::time::Instant;

use horizon_client::fast_hash::FxHashMap;

pub const MAX_NODES: usize = 120_000;
/// Wall-clock budget for one plan. The search runs on the game thread, so
/// an unlucky long path used to stall frames for seconds; past this the
/// partial path (walk toward the goal, re-plan closer) takes over.
const BUDGET_MS: u64 = 120;

#[derive(Clone, Copy, PartialEq, Debug)]
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
    /// Diagonal Bridge: place the diagonal floor off an orth neighbour's
    /// side face (plus an approach support when its floor is gone too).
    Traverse,
    /// Dig straight down through the floor under our feet and drop in.
    Downward,
    /// Jump a gap where the landing's floor is missing: place it mid-flight
    /// off a support face, then land on it (`cells[0]` = the floor cell).
    ParkourPlace { gap: i8 },
}

/// One path edge: arrive at `pos` by `kind`, breaking `cells` first when
/// `kind` is Break/Downward or placing `cells` when kind is Pillar/Bridge/
/// Traverse.
#[derive(Clone)]
pub struct Move {
    pub pos: [i32; 3],
    pub kind: Kind,
    /// Blocks to break (Break/Downward) or cells to place in order
    /// (Pillar/Bridge/Traverse — Traverse may list an approach support
    /// before the diagonal floor, which is always `cells` last).
    pub cells: Vec<[i32; 3]>,
}

pub struct Opts {
    pub allow_break: bool,
    pub allow_place: bool,
    pub allow_parkour: bool,
    pub max_drop: i32,
    /// `mobAvoidanceRadius`/`mobAvoidanceCoefficient`: cells within this
    /// distance of a live mob cost `c * (r - d)` extra. `r <= 0` disables.
    pub mob_avoid_r: f64,
    pub mob_avoid_c: f64,
    /// `allowParkourPlace`: permit mid-flight floor placement over gaps.
    pub parkour_place: bool,
    /// Upstream's throwaway check: place moves (Pillar, Bridge, Traverse,
    /// ParkourPlace) only exist while the hotbar holds a placeable block,
    /// so the planner can't route over terrain it can't build on.
    pub has_throwaway: bool,
    /// `allowDownward`: Drop edges and digging through the floor. Off pins
    /// routes to one elevation band.
    pub downward: bool,
    /// `allowVines`: ladder/vine/scaffold climbing.
    pub vines: bool,
    /// `allowDiagonalAscend`/`allowDiagonalDescend`: Step/Drop edges on
    /// diagonal directions. Off by default like upstream.
    pub diag_ascend: bool,
    pub diag_descend: bool,
    /// `assumeStep`: Step edges cost like walking.
    pub assume_step: bool,
    /// `sprintInWater`: swimming keeps sprint speed.
    pub sprint_water: bool,
}

/// Read-only world facts the planner needs, borrowing from `Game`. Facts
/// are memoized per cell — neighbouring nodes re-check the same cells
/// constantly, and a bare `world.block` lookup was the search's hot spot.
pub struct W<'a> {
    pub game: &'a crate::game::Game<'a>,
    facts: std::cell::RefCell<FxHashMap<[i32; 3], &'a horizon_items::blocks::StateFacts>>,
    /// Settings snapshots: `assumeWalkOnWater`, `assumeWalkOnLava`,
    /// `allowWalkOnBottomSlab` — read once instead of per-cell state locks.
    aw: bool,
    al: bool,
    slab: bool,
}

impl<'a> W<'a> {
    pub fn new(game: &'a crate::game::Game<'a>) -> Self {
        let (aw, al, slab) = crate::game::umbx_baritone::with(|s| {
            (s.set.assume_water, s.set.assume_lava, s.set.bottom_slab)
        });
        Self {
            game,
            facts: std::cell::RefCell::new(FxHashMap::default()),
            aw,
            al,
            slab,
        }
    }
}

impl W<'_> {
    /// Facts of the block state at `p` (unloaded chunks read as air).
    fn fat(&self, p: [i32; 3]) -> &horizon_items::blocks::StateFacts {
        *self
            .facts
            .borrow_mut()
            .entry(p)
            .or_insert_with(|| {
                self.game
                    .res
                    .facts()
                    .get(self.game.world.block(p[0], p[1], p[2]))
            })
    }

    /// Highest Y worth scanning (world top minus headroom).
    fn top(&self) -> i32 {
        self.game.world.shape.max_y() - 2
    }

    /// The body's two cells are free to stand in (no full collision, no lava)
    /// and the head cell is not underwater — a submerged head means drowning.
    pub fn body_open(&self, p: [i32; 3]) -> bool {
        self.open_cell(p)
            && self.open_cell([p[0], p[1] + 1, p[2]])
            && !self.water([p[0], p[1] + 1, p[2]])
    }

    pub fn open_cell(&self, p: [i32; 3]) -> bool {
        let f = self.fat(p);
        !f.full_collision && f.fluid != horizon_items::blocks::Fluid::Lava
    }

    /// The block at `p` can hold weight: full collision, or a slab/stairs
    /// while `allowWalkOnBottomSlab` is on (all halves accepted — slightly
    /// generous for top slabs).
    fn block_solid(&self, p: [i32; 3]) -> bool {
        let f = self.fat(p);
        if f.full_collision {
            return true;
        }
        if !f.name.contains("slab") && !f.name.contains("stairs") {
            return false;
        }
        self.slab
    }

    /// Something solid to stand on under `p`. `assumeWalkOnWater` /
    /// `assumeWalkOnLava` treat the fluid's surface as floor — planner
    /// cheats for servers where fluids are walkable, off by default.
    pub fn floor(&self, p: [i32; 3]) -> bool {
        let b = [p[0], p[1] - 1, p[2]];
        if self.block_solid(b) {
            return true;
        }
        (self.aw && self.water(b)) || (self.al && self.lava(b))
    }

    /// `p` is standable: two open cells over a floor.
    pub fn standable(&self, p: [i32; 3]) -> bool {
        self.floor(p) && self.body_open(p)
    }

    /// `p` can be broken through by the player.
    pub fn breakable(&self, p: [i32; 3]) -> bool {
        let f = self.fat(p);
        f.hardness >= 0.0
            && f.fluid != horizon_items::blocks::Fluid::Lava
            && !crate::game::umbx_baritone_extras::blacklisted(f.name.as_ref())
    }

    fn break_cost(&self, p: [i32; 3]) -> f64 {
        1.0 + (self.fat(p).hardness.max(0.05) * 2.0).min(30.0) as f64
    }

    /// Ladders, vines, chains, scaffolding: climbable by name.
    fn climbable(&self, p: [i32; 3]) -> bool {
        let n = &self.fat(p).name;
        n.contains("ladder") || n.contains("vine") || n.contains("scaffold")
    }

    pub fn water(&self, p: [i32; 3]) -> bool {
        self.fat(p).fluid == horizon_items::blocks::Fluid::Water
    }

    fn lava(&self, p: [i32; 3]) -> bool {
        self.fat(p).fluid == horizon_items::blocks::Fluid::Lava
    }

    /// Any of `p`'s 4 side neighbours is lava (a cost penalty, not a ban).
    fn lava_near(&self, p: [i32; 3]) -> bool {
        [[1, 0], [-1, 0], [0, 1], [0, -1]]
            .iter()
            .any(|[dx, dz]| self.lava([p[0] + dx, p[1], p[2] + dz]))
    }

    /// `blocksToAvoid`: the body cells at `p` hold a name on the avoid
    /// list — walking through them costs extra (never a hard ban, so a
    /// cornered player still escapes).
    pub fn avoided_through(&self, p: [i32; 3]) -> bool {
        crate::game::umbx_baritone_extras::avoided(self.fat(p).name.as_ref())
            || crate::game::umbx_baritone_extras::avoided(
                self.fat([p[0], p[1] + 1, p[2]]).name.as_ref(),
            )
    }

    /// Distance to the nearest live mob's cell centre, if any.
    pub fn mob_dist(&self, p: [i32; 3]) -> Option<f64> {
        let mut best: Option<f64> = None;
        for m in &self.game.mobs {
            if m.removed || !m.alive() {
                continue;
            }
            let d = ((m.position.x.floor() as i32 - p[0]).pow(2)
                + (m.position.y.floor() as i32 - p[1]).pow(2)
                + (m.position.z.floor() as i32 - p[2]).pow(2)) as f64;
            if best.map_or(true, |b| d < b) {
                best = Some(d);
            }
        }
        best.map(|d2| d2.sqrt())
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
        if o.vines
            && (w.climbable(p) || w.climbable([p[0], p[1] - 1, p[2]]) || w.climbable(up))
        {
            out.push(Move {
                pos: up,
                kind: Kind::Ladder { n: 1 },
                cells: vec![],
            });
        } else if o.allow_place && o.has_throwaway {
            // Pillar: stand on a block placed at our feet cell.
            out.push(Move {
                pos: up,
                kind: Kind::Pillar,
                cells: vec![p],
            });
        }
    }
    if o.vines && w.climbable(p) {
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
    // Downward: dig straight through the floor we stand on and drop in. The
    // cell below must be landable — solid-but-breakable, or an open shaft
    // that bottoms out inside max_drop. Never into lava, never through
    // unbreakable. `allowDownward` pins routes to one elevation band.
    if o.downward && o.allow_break && w.floor(p) {
        let fc = [p[0], p[1] - 1, p[2]];
        let below = [p[0], p[1] - 2, p[2]];
        let below_ok = if w.open_cell(below) {
            // Open shaft: it must reach a solid landing inside max_drop.
            let mut ok = false;
            let mut d = 3;
            while d <= o.max_drop + 1 {
                let c = [p[0], p[1] - d, p[2]];
                if w.lava(c) {
                    break;
                }
                if !w.open_cell(c) {
                    // Solid landing; the open cell above is where we stop.
                    ok = true;
                    break;
                }
                d += 1;
            }
            ok
        } else {
            w.breakable(below)
        };
        if w.breakable(fc) && below_ok {
            out.push(Move {
                pos: fc,
                kind: Kind::Downward,
                cells: vec![fc],
            });
        }
    }
    // Straight drop when the ledge under us is open air. Past max_drop (up
    // to max_drop*3) only when the fall ends in water; dry falls worth
    // >=10hp damage are skipped. Skipped in water: no floor there means
    // open depth, and "dropping" to the lakebed is drowning, not a move.
    if o.downward && !w.floor(p) && !w.water(p) {
        let bound = o.max_drop * 3;
        let mut d = 1;
        while d <= bound {
            let c = [p[0], p[1] - d, p[2]];
            if w.lava(c) {
                break;
            }
            if w.water(c) {
                // Splash down at the surface, not on the lakebed.
                if d >= 2 {
                    out.push(Move {
                        pos: c,
                        kind: Kind::Drop { n: d as i8 },
                        cells: vec![],
                    });
                }
                break;
            }
            if !w.body_open(c) {
                break;
            }
            if w.floor(c) {
                let dmg = d - 3;
                if d >= 2 && d <= o.max_drop && dmg < 10 {
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
        let n = [p[0] + dx, p[1], p[2] + dz];
        // Diagonal corner rule: each orth corner's two body cells must be
        // open now, or breakable so a dig-through can open them. `None`
        // kills the diagonal outright; a non-empty list holds the cells a
        // Break edge must clear before the squeeze-through is legal.
        let corner_cells: Option<Vec<[i32; 3]>> = if !diag {
            Some(Vec::new())
        } else {
            let a = [p[0] + dx, p[1], p[2]];
            let b = [p[0], p[1], p[2] + dz];
            let pair = |c: [i32; 3]| {
                (w.open_cell(c) || w.breakable(c))
                    && (w.open_cell([c[0], c[1] + 1, c[2]])
                        || w.breakable([c[0], c[1] + 1, c[2]]))
            };
            if pair(a) && pair(b) {
                let mut v = Vec::new();
                for c in [a, [a[0], a[1] + 1, a[2]], b, [b[0], b[1] + 1, b[2]]] {
                    if !w.open_cell(c) {
                        v.push(c);
                    }
                }
                Some(v)
            } else {
                None
            }
        };
        let Some(corner_cells) = corner_cells else { continue };
        let corners_open = corner_cells.is_empty();
        if !corners_open && !o.allow_break {
            continue;
        }
        if corners_open {
        if w.standable(n) {
            out.push(Move {
                pos: n,
                kind: Kind::Walk,
                cells: vec![],
            });
            continue;
        }
        // Step up onto a 1-block ledge. Diagonals need allowDiagonalAscend.
        let up1 = [n[0], n[1] + 1, n[2]];
        if w.standable(up1)
            && w.open_cell([p[0], p[1] + 2, p[2]])
            && (!diag || o.diag_ascend)
        {
            out.push(Move {
                pos: up1,
                kind: Kind::Step,
                cells: vec![],
            });
            continue;
        }
        // Drop down the ledge: past max_drop only into water (see above).
        // allowDownward pins routes to one band; diagonals need
        // allowDiagonalDescend.
        if o.downward && (!diag || o.diag_descend) && w.body_open(n) {
            let bound = o.max_drop * 3;
            let mut d = 1;
            while d <= bound {
                let c = [n[0], n[1] - d, n[2]];
                if w.lava(c) {
                    break;
                }
                if w.water(c) {
                    // First water cell in the descent is the surface: splash
                    // down there, never at the lakebed under it.
                    out.push(Move {
                        pos: c,
                        kind: Kind::Drop { n: d as i8 },
                        cells: vec![],
                    });
                    break;
                }
                if !w.body_open(c) {
                    break;
                }
                if w.floor(c) {
                    let dmg = d - 3;
                    if d <= o.max_drop && dmg < 10 {
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
        // Swim: neighbours on the water surface — head cell must be air-ish,
        // else the move would park the player under water until they drown.
        if w.water(n)
            && w.open_cell([n[0], n[1] + 1, n[2]])
            && !w.water([n[0], n[1] + 1, n[2]])
        {
            out.push(Move {
                pos: n,
                kind: Kind::Swim,
                cells: vec![],
            });
        }
        // Swim up: while inside water, rise toward the surface.
        if w.water(p) && w.water([n[0], n[1] + 1, n[2]]) {
            out.push(Move {
                pos: [n[0], n[1] + 1, n[2]],
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
        // ParkourPlace: same jump, but the landing's floor is missing and
        // gets placed mid-flight — needs a solid face next to that cell to
        // click the block onto. Its own setting: risky, off by default.
        if o.parkour_place && o.allow_place && o.has_throwaway && !diag {
            for gap in 1..=2_i32 {
                let land = [p[0] + dx * (gap + 1), p[1], p[2] + dz * (gap + 1)];
                let fl = [land[0], land[1] - 1, land[2]];
                let mut clear = w.body_open(land) && w.open_cell(fl) && !w.lava(fl);
                for g in 1..=gap {
                    clear = clear && w.body_open([p[0] + dx * g, p[1], p[2] + dz * g]);
                }
                let sup = [[1, 0, 0], [-1, 0, 0], [0, 0, 1], [0, 0, -1], [0, -1, 0]]
                    .iter()
                    .any(|o2| w.block_solid([fl[0] + o2[0], fl[1] + o2[1], fl[2] + o2[2]]));
                if clear && sup {
                    out.push(Move {
                        pos: land,
                        kind: Kind::ParkourPlace { gap: gap as i8 },
                        cells: vec![fl],
                    });
                }
            }
        }
        // Bridge: `n`'s floor is missing but the body is open; place it.
        if o.allow_place
            && o.has_throwaway
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
        // Traverse: diagonal Bridge — `n`'s floor hangs on a side face of an
        // orth neighbour's floor. When an orth floor is also missing it is
        // placed first off our own floor's side face (two placements max).
        if diag
            && o.allow_place
            && o.has_throwaway
            && w.body_open(n)
            && !w.floor(n)
            && !w.climbable(p)
        {
            let af = [p[0] + dx, p[1] - 1, p[2]];
            let bf = [p[0], p[1] - 1, p[2] + dz];
            let sa = w.block_solid(af);
            let sb = w.block_solid(bf);
            // `n`'s floor needs at least one existing orth side face.
            if sa || sb {
                let our = [p[0], p[1] - 1, p[2]];
                let mut cells = Vec::new();
                if !sa && w.block_solid(our) {
                    cells.push(af);
                }
                if !sb && w.block_solid(our) {
                    cells.push(bf);
                }
                cells.push([n[0], n[1] - 1, n[2]]);
                out.push(Move {
                    pos: n,
                    kind: Kind::Traverse,
                    cells,
                });
            }
        }
        }
        // Break: the body is walled by breakable blocks, or a diagonal's
        // corners are solid-but-breakable — the blocked cells go on the
        // break list first so the squeeze-through is legal afterwards.
        if o.allow_break {
            let feet_ok = w.open_cell(n) || w.breakable(n);
            let head = [n[0], n[1] + 1, n[2]];
            let head_ok = w.open_cell(head) || w.breakable(head);
            let needs_dig =
                !corner_cells.is_empty() || !w.open_cell(n) || !w.open_cell(head);
            if feet_ok && head_ok && needs_dig {
                let mut cells = corner_cells;
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
    find_partial(w, start, goal, o, done, false)
}

/// `find`, but when the goal is unreachable (unloaded or blocked past
/// `MAX_NODES`), returns the path to the explored cell closest to it —
/// Baritone's walk-toward-the-goal behavior for far `GoalXZ`s.
pub fn find_partial(
    w: &W,
    start: [i32; 3],
    goal: [i32; 3],
    o: &Opts,
    done: impl Fn([i32; 3], [i32; 3]) -> bool,
    partial_ok: bool,
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
    let mut g: FxHashMap<[i32; 3], f64> = FxHashMap::default();
    // Predecessor: cell -> the move that ended there (its `pos` is the cell).
    let mut came: FxHashMap<[i32; 3], ([i32; 3], Move)> = FxHashMap::default();
    g.insert(start, 0.0);
    open.push(Frontier { f: h(start, goal), at: start });
    let t0 = Instant::now();
    let mut pops = 0usize;
    let mut best: [i32; 3] = start;
    let mut best_h = h(start, goal);
    let mut ns: Vec<Move> = Vec::with_capacity(24);

    let path_to = |end: [i32; 3], came: &FxHashMap<[i32; 3], ([i32; 3], Move)>| {
        let mut out = Vec::new();
        let mut cur = end;
        while let Some((prev, m)) = came.get(&cur) {
            out.push(m.clone());
            cur = *prev;
        }
        out.reverse();
        out
    };

    while let Some(Frontier { at: p, .. }) = open.pop() {
        if done(p, goal) {
            return Some(path_to(p, &came));
        }
        let hp = h(p, goal);
        if hp < best_h {
            best_h = hp;
            best = p;
        }
        pops += 1;
        if pops > MAX_NODES
            || (pops & 0x1FF) == 0
                && t0.elapsed() > std::time::Duration::from_millis(BUDGET_MS)
        {
            break;
        }
        let gp = g[&p];
        ns.clear();
        succ(w, p, o, &mut ns);
        for m in ns.drain(..) {
            // Never end a move with lava at feet or head. The floor gets a
            // pass under assumeWalkOnLava — the cheat exists precisely so
            // paths can end standing on a lava surface.
            if w.lava(m.pos)
                || w.lava([m.pos[0], m.pos[1] + 1, m.pos[2]])
                || (!w.al && w.lava([m.pos[0], m.pos[1] - 1, m.pos[2]]))
            {
                continue;
            }
            let mut edge = match m.kind {
                Kind::Walk => if (m.pos[0] - p[0]).abs() + (m.pos[2] - p[2]).abs() == 2 { 1.42 } else { 1.0 },
                // `assumeStep` makes a 1-up step cost like a walk — just
                // above flat cost so a truly flat neighbour still wins.
                Kind::Step => if o.assume_step { 1.05 } else { 2.0 },
                Kind::Drop { n } => {
                    // A fall through water has no damage cost.
                    let wet = (m.pos[1]..p[1])
                        .any(|y| w.water([m.pos[0], y, m.pos[2]]));
                    let mut e = 1.0 + n as f64 * 0.6;
                    if !wet && n > 3 {
                        e += (n as f64 - 3.0) * 0.5;
                    }
                    e
                }
                Kind::Parkour { gap } => 2.0 + gap as f64,
                Kind::ParkourPlace { gap } => 4.5 + gap as f64,
                Kind::Ladder { .. } => 1.8,
                // `sprintInWater` off: no sprint-gliding, water legs cost more.
                // A submerged head at the destination means real drowning
                // time — price it so dry routes win unless the dip is short.
                Kind::Swim => {
                    let base = if o.sprint_water { 2.2 } else { 4.4 };
                    if w.water([m.pos[0], m.pos[1] + 1, m.pos[2]]) {
                        base + 25.0
                    } else {
                        base
                    }
                }
                Kind::Break { .. } => 1.0 + m.cells.iter().map(|c| w.break_cost(*c)).sum::<f64>(),
                Kind::Downward => 2.0 + m.cells.iter().map(|c| w.break_cost(*c)).sum::<f64>(),
                Kind::Pillar => 3.5,
                Kind::Bridge => 3.0,
                Kind::Traverse => 4.0,
            };
            // Standing next to lava is risky: prefer routes that keep away.
            // With assumeWalkOnLava the danger is assumed away too.
            if !w.al && w.lava_near(m.pos) {
                edge += 5.0;
            }
            // `blocksToAvoid`: the body cells at the destination hold an
            // avoid-listed name (webs, bushes) — costly, never banned.
            if w.avoided_through(m.pos) {
                edge += 8.0;
            }
            // `mobAvoidance*`: proximity to a live mob costs extra.
            if o.mob_avoid_r > 0.0
                && let Some(d) = w.mob_dist(m.pos)
                && d < o.mob_avoid_r
            {
                edge += o.mob_avoid_c * (o.mob_avoid_r - d);
            }
            let ng = gp + edge;
            if ng < *g.get(&m.pos).unwrap_or(&f64::INFINITY) {
                let at = m.pos;
                g.insert(at, ng);
                came.insert(at, (p, m));
                open.push(Frontier { f: ng + h(at, goal), at });
            }
        }
    }
    if partial_ok && best != start {
        return Some(path_to(best, &came));
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
