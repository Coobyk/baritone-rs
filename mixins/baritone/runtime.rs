// Baritone port: shared state. One process runs at a time; each tick the glue
// asks the process for its current goal, keeps a path toward it, and hands the
// current move to the executor.

use std::sync::Mutex;

/// `b`/`baritone` mod id, so settings land under `.horizon-mod-data/baritone/`.
pub const MOD_ID: &str = "baritone";
pub const ID: &str = MOD_ID;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DESCRIPTION: &str =
    "Baritone-style pathing: goto, mine, follow, explore, tunnel";

#[derive(Clone, Copy, PartialEq)]
pub enum Proc {
    None,
    /// GoalBlock-style: stand on/near this cell.
    Goto { goal: [i32; 3] },
    /// GoalXZ-style: any standable cell in this column (y resolved en route).
    GotoXZ { x: i32, z: i32 },
    /// GoalYLevel: any standable cell at this height.
    YLevel { y: i32 },
    /// Find and break matching blocks until none remain nearby.
    Mine { next: usize },
    /// Stay near a mob/guest.
    Follow,
    /// Walk to random reachable points.
    Explore,
    /// Dig straight ahead for `left` more columns.
    Tunnel { left: i32 },
}

/// Per-tick outputs the executor asks `native_movement_input`/`native_tick`
/// to apply. `forced_hit` is written straight onto `game.forced_hit`.
#[derive(Clone, Copy, Default)]
pub struct Want {
    pub forward: f32,
    pub jump: bool,
    pub sneak: bool,
    pub sprint: bool,
    /// Hold attack (game.left) at `hit`.
    pub attack: bool,
    /// Hold use (game.right) at `hit`.
    pub right: bool,
    pub hit: Option<crate::game::Hit>,
}

#[derive(Clone, Copy, Default)]
pub struct Settings {
    pub sprint: bool,
    pub allow_break: bool,
    pub allow_place: bool,
    pub allow_parkour: bool,
    /// Deepest drop the planner may take (0 damage at <=3).
    pub max_drop: i32,
    /// Scan radius for `mine`.
    pub mine_radius: i32,
    /// `mineScanDroppedItems`: walk to dropped items matching a mine target.
    pub mine_scan_drops: bool,
    pub pause: bool,
}

pub struct State {
    pub proc: Proc,
    /// Mine target names (substring match against `StateFacts::name`).
    pub mine_names: Vec<String>,
    /// Follow target: mob kind name or guest name.
    pub follow_name: String,
    /// `follow` match mode: 0 = kind or name contains, 1 = player names
    /// only (`follow player`), 2 = kind only (`follow entity`),
    /// 3 = every living mob (`follow entities`).
    pub follow_mode: u8,
    /// Current path and progress.
    pub path: Vec<crate::game::umbx_pathfind::Move>,
    pub at: usize,
    /// Ticks without progress, for the stuck watchdog.
    pub stuck: u32,
    pub last_pos: [i32; 3],
    /// Re-path once after an unstick jump.
    pub replanned: bool,
    /// Consecutive failed vertical moves: skipping a climb/descent desyncs
    /// the path, so they force re-plans — and identical re-plans need a cap.
    pub vert_fails: u8,
    /// Ticks before the fresh plan is re-validated against the world — the
    /// planner's own reads can't be instantly stale, and re-checking every
    /// tick makes the client chunk store's flapping thrash the path.
    pub stale_cd: u8,
    /// Ticks until the process's next decision (follow repath, mine rescan).
    pub cooldown: u32,
    /// Tunnel's step direction.
    pub tunnel_dir: [i32; 2],
    /// `tunnel <h> <w>` cross-section size (width × height; 1×2 is the
    /// plain corridor that lets the path's own Break moves dig).
    pub tunnel_size: [i32; 2],
    /// Wide-tunnel excavation: cells of the current column still to dig.
    pub tunnel_queue: std::collections::VecDeque<[i32; 3]>,
    /// The column being excavated / walked into (i32::MIN = unset).
    pub tunnel_col: [i32; 3],
    /// Objective is an excavation cell: the driver paths adjacent and
    /// appends a Break move on it, exactly like Mine does.
    pub excavate: bool,
    /// Mine objective is a dropped item entity, not a block: walk onto it
    /// (no appended Break move).
    pub drop_pickup: bool,
    /// The process's current target cell (mine block, follow spot, ...).
    pub objective: Option<[i32; 3]>,
    /// A feature module's process is active: the input hook should apply
    /// `want` even though `proc` is None (module drives its own moves).
    pub busy: bool,
    /// Latest executor request for the input hook.
    pub want: Want,
    pub set: Settings,
    /// Explored-points memory so `explore` doesn't repeat itself.
    pub visited: std::collections::HashSet<(i32, i32)>,
    /// Xorshift rng for explore.
    pub rng: u64,
    /// Diagnostic: why the last plan/stale-check did what it did.
    pub note: String,
    /// Ticks to keep holding attack / use for a `click` pulse.
    pub click_left: u8,
    pub click_right: u8,
    /// Direct `game.left`/`game.right` writes we own (click pulses, eating):
    /// the engine only rewrites them on input events, so we must release
    /// them ourselves when done.
    pub held_l: bool,
    pub held_r: bool,
    /// Eating lockout: while >0, auto_eat keeps holding use on the food slot.
    pub eat_ticks: u8,
    /// Hunger level when the current bite started (completion = it rose).
    pub eat_food: i32,
}

impl State {
    fn new() -> Self {
        Self {
            proc: Proc::None,
            mine_names: Vec::new(),
            follow_name: String::new(),
            follow_mode: 0,
            path: Vec::new(),
            at: 0,
            stuck: 0,
            last_pos: [i32::MIN; 3],
            replanned: false,
            vert_fails: 0,
            stale_cd: 0,
            cooldown: 0,
            busy: false,
            objective: None,
            tunnel_dir: [0, 0],
            tunnel_size: [1, 2],
            tunnel_queue: std::collections::VecDeque::new(),
            tunnel_col: [i32::MIN; 3],
            excavate: false,
            drop_pickup: false,
            want: Want::default(),
            set: Settings {
                sprint: true,
                allow_break: true,
                allow_place: true,
                allow_parkour: true,
                max_drop: 4,
                mine_radius: 32,
                mine_scan_drops: true,
                pause: false,
            },
            visited: std::collections::HashSet::new(),
            rng: 0x9e3779b97f4a7c15,
            note: String::new(),
            click_left: 0,
            click_right: 0,
            held_l: false,
            held_r: false,
            eat_ticks: 0,
            eat_food: 20,
        }
    }

    /// Stop whatever is running and clear motion state.
    pub fn halt(&mut self) {
        self.proc = Proc::None;
        self.busy = false;
        self.objective = None;
        self.path.clear();
        self.at = 0;
        self.stuck = 0;
        self.replanned = false;
        self.vert_fails = 0;
        self.stale_cd = 0;
        self.want = Want::default();
        self.set.pause = false;
        self.eat_ticks = 0;
    }

    /// Begin `proc`, dropping any in-flight path so the next tick re-plans.
    pub fn begin(&mut self, proc: Proc) {
        self.proc = proc;
        self.objective = None;
        self.path.clear();
        self.at = 0;
        self.stuck = 0;
        self.replanned = false;
        self.vert_fails = 0;
        self.stale_cd = 0;
        self.cooldown = 0;
        self.excavate = false;
        self.drop_pickup = false;
        self.tunnel_queue.clear();
        self.tunnel_col = [i32::MIN; 3];
    }

    pub fn next(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Run `f` with the global state, creating it on first use.
pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(State::new());
    }
    f(g.as_mut().unwrap())
}

/// `native_setting` read. Missing keys leave the defaults.
pub fn load(name: &str, value: &str) {
    with(|s| {
        let set = &mut s.set;
        match name {
            "sprint" => set.sprint = value == "true",
            "allow_break" => set.allow_break = value == "true",
            "allow_place" => set.allow_place = value == "true",
            "allow_parkour" => set.allow_parkour = value == "true",
            "max_drop" => set.max_drop = value.parse().unwrap_or(4).clamp(1, 20),
            "mine_radius" => set.mine_radius = value.parse().unwrap_or(32).clamp(8, 96),
            "mine_scan_drops" => set.mine_scan_drops = value == "true",
            _ => {}
        }
    });
}

/// Settings persisted through `native_setting`.
pub fn to_save() -> Vec<(&'static str, String)> {
    with(|s| {
        vec![
            ("sprint", s.set.sprint.to_string()),
            ("allow_break", s.set.allow_break.to_string()),
            ("allow_place", s.set.allow_place.to_string()),
            ("allow_parkour", s.set.allow_parkour.to_string()),
            ("max_drop", s.set.max_drop.to_string()),
            ("mine_radius", s.set.mine_radius.to_string()),
            ("mine_scan_drops", s.set.mine_scan_drops.to_string()),
        ]
    })
}
