// Baritone port: shared state. One process runs at a time; each tick the glue
// asks the process for its current goal, keeps a path toward it, and hands the
// current move to the executor.

use std::sync::Mutex;

use crate::game::umbx_baritone_settings as st;
use crate::game::umbx_pathfind as pf;

/// `b`/`baritone` mod id, so settings land under `.horizon-mod-data/baritone/`.
pub const MOD_ID: &str = "baritone";

/// `native_on`'s replacement: whether this code mod is enabled in the
/// instance's mod switches (`mod_index_on` tracks Options > Mods).
pub fn enabled() -> bool {
    mod_api::__private::mod_index(MOD_ID).is_some_and(mod_api::__private::mod_index_on)
}

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
    Mine,
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
    /// `allowOnlyExposedOres`: mine targets need at least one face-adjacent
    /// open cell (no x-ray-style mining through walls).
    pub exposed_ores: bool,
    /// `followRadius`: follow's goal re-paths only once the mob is further
    /// than this many cells from the last objective.
    pub follow_radius: i32,
    /// `legitMine`: mine only blocks with a clear line of sight from the
    /// player's eye (or blocks seen before — the seen set persists).
    pub legit_mine: bool,
    /// `assumeWalkOnWater`: treat the water surface as solid floor — a
    /// planner cheat for servers where players can walk on water.
    pub assume_water: bool,
    /// `assumeWalkOnLava`: same cheat for lava surfaces.
    pub assume_lava: bool,
    /// `mobAvoidanceRadius`: distance under which nearby mobs cost extra
    /// path weight (0 disables).
    pub mob_avoid_r: i32,
    /// `mobAvoidanceCoefficient`: cost multiplier on the proximity term.
    pub mob_avoid_c: f64,
    /// `allowParkourPlace`: jump gaps whose landing floor is missing by
    /// placing it mid-flight. Risky; off by default like upstream.
    pub parkour_place: bool,
    /// `allowDownward`: descend more than one block at a time — Drop edges
    /// and digging through the floor. Off keeps routes on one band.
    pub downward: bool,
    /// `allowVines`: climb ladder/vine/scaffold columns.
    pub vines: bool,
    /// `allowDiagonalAscend`: Step edges on diagonal directions.
    pub diag_ascend: bool,
    /// `allowDiagonalDescend`: Drop edges on diagonal directions.
    pub diag_descend: bool,
    /// `assumeStep`: a 1-block ledge costs like walking — the planner
    /// assumes a step-up assist is in play.
    pub assume_step: bool,
    /// `sprintInWater`: keep sprint while swimming (faster crossing).
    pub sprint_water: bool,
    /// `toolSelect`: switch to the best hotbar tool while breaking.
    pub tool_select: bool,
    /// `allowWalkOnBottomSlab`: slabs/stairs count as floors.
    pub bottom_slab: bool,
    /// `/b pause`: session-only by design — not part of `SETTINGS`, so it
    /// is never persisted (a persisted pause would freeze the mod on
    /// every restart). `reset`/halt clear it in memory.
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
    /// Closest xz distance seen to the current move's target — progress is
    /// the number going down, not displacement (bouncing off a ledge and
    /// walking back used to reset the watchdog forever).
    pub last_dist: f64,
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
    /// Retry delay after a failed plan. Without it the driver re-plans every
    /// tick on an unreachable goal, and (previously) a single failure set the
    /// stuck clock to 120 and aborted the whole process in the same tick.
    pub plan_cd: u32,
    /// Consecutive failed plans. A few retries let chunks load / the player
    /// move; a persistent failure eventually gives up with an honest message.
    pub plan_fails: u8,
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
    /// `mine <n> <block>` quantity: 0 = unlimited, else stop after this
    /// many matching items have entered the inventory (Java counts items
    /// gained, not blocks broken — a broken block whose drop was lost
    /// doesn't count).
    pub mine_want: u32,
    /// Matching items gained since `mine` began (inventory delta).
    pub mine_done: u32,
    /// Inventory count of matching items when `mine` started.
    pub mine_have0: i64,
    /// Drop cells banned after a failed plan or a stalled pickup —
    /// `(cell, ticks left)`. An unreachable or phantom drop must not
    /// wedge the rescan on the same target forever.
    pub drop_ban: Vec<([i32; 3], u32)>,
    /// Ticks the current drop objective has persisted while the player
    /// stands on it — a drop that won't despawn or collect gets banned.
    pub drop_stall: u32,
    /// `legitMine`: cells raycast-verified visible this session. Once seen,
    /// an ore stays mineable even after it falls out of sight.
    pub seen_ores: std::collections::HashSet<[i32; 3]>,
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
    /// `/b explore <block...>` (exploreForBlocks): walk toward matching
    /// blocks in columns not yet visited.
    pub explore_names: Vec<String>,
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
    /// Hotbar has a placeable block — refreshed each tick; the planner only
    /// emits place moves while this holds (upstream's throwaway check).
    pub has_throwaway: bool,
}

impl Settings {
    /// Defaults straight from the shared `SETTINGS` table — one source for
    /// `load`, `/b reset`, `/b modified` and the options page.
    pub fn defaults() -> Self {
        let mut set = Self {
            sprint: false,
            allow_break: false,
            allow_place: false,
            allow_parkour: false,
            max_drop: 0,
            mine_radius: 0,
            mine_scan_drops: false,
            exposed_ores: false,
            follow_radius: 0,
            legit_mine: false,
            assume_water: false,
            assume_lava: false,
            mob_avoid_r: 0,
            mob_avoid_c: 0.0,
            parkour_place: false,
            downward: false,
            vines: false,
            diag_ascend: false,
            diag_descend: false,
            assume_step: false,
            sprint_water: false,
            tool_select: false,
            bottom_slab: false,
            pause: false,
        };
        for spec in st::SETTINGS {
            set.apply(spec, spec.default);
        }
        set
    }

    /// Store a persisted string for `spec`: the `native_setting` load path
    /// and `/b reset` share it — unparseable numbers fall back to the
    /// table default and everything clamps to the table range.
    pub fn apply(&mut self, spec: &st::Setting, value: &str) {
        if spec.toggle {
            self.put_bool(spec.key, value == "true");
        } else {
            let v = value
                .parse::<f64>()
                .unwrap_or_else(|_| spec.default.parse().unwrap_or(0.0))
                .clamp(spec.min, spec.max);
            self.put_num(spec.key, v);
        }
    }

    /// The value in its persisted string form (`to_save`, `/b modified`,
    /// `/b set`'s read-back).
    pub fn value(&self, key: &str) -> String {
        match key {
            "sprint" => self.sprint.to_string(),
            "allow_break" => self.allow_break.to_string(),
            "allow_place" => self.allow_place.to_string(),
            "allow_parkour" => self.allow_parkour.to_string(),
            "max_drop" => self.max_drop.to_string(),
            "mine_radius" => self.mine_radius.to_string(),
            "mine_scan_drops" => self.mine_scan_drops.to_string(),
            "exposed_ores" => self.exposed_ores.to_string(),
            "follow_radius" => self.follow_radius.to_string(),
            "legit_mine" => self.legit_mine.to_string(),
            "assume_water" => self.assume_water.to_string(),
            "assume_lava" => self.assume_lava.to_string(),
            "mob_avoid_r" => self.mob_avoid_r.to_string(),
            "mob_avoid_c" => self.mob_avoid_c.to_string(),
            "parkour_place" => self.parkour_place.to_string(),
            "downward" => self.downward.to_string(),
            "vines" => self.vines.to_string(),
            "diag_ascend" => self.diag_ascend.to_string(),
            "diag_descend" => self.diag_descend.to_string(),
            "assume_step" => self.assume_step.to_string(),
            "sprint_water" => self.sprint_water.to_string(),
            "tool_select" => self.tool_select.to_string(),
            "bottom_slab" => self.bottom_slab.to_string(),
            _ => String::new(),
        }
    }

    /// A toggle's current value (`/b set`'s flip needs the old one).
    pub fn flag(&self, key: &str) -> bool {
        match key {
            "sprint" => self.sprint,
            "allow_break" => self.allow_break,
            "allow_place" => self.allow_place,
            "allow_parkour" => self.allow_parkour,
            "mine_scan_drops" => self.mine_scan_drops,
            "exposed_ores" => self.exposed_ores,
            "legit_mine" => self.legit_mine,
            "assume_water" => self.assume_water,
            "assume_lava" => self.assume_lava,
            "parkour_place" => self.parkour_place,
            "downward" => self.downward,
            "vines" => self.vines,
            "diag_ascend" => self.diag_ascend,
            "diag_descend" => self.diag_descend,
            "assume_step" => self.assume_step,
            "sprint_water" => self.sprint_water,
            "tool_select" => self.tool_select,
            "bottom_slab" => self.bottom_slab,
            _ => false,
        }
    }

    pub fn put_bool(&mut self, key: &str, v: bool) {
        match key {
            "sprint" => self.sprint = v,
            "allow_break" => self.allow_break = v,
            "allow_place" => self.allow_place = v,
            "allow_parkour" => self.allow_parkour = v,
            "mine_scan_drops" => self.mine_scan_drops = v,
            "exposed_ores" => self.exposed_ores = v,
            "legit_mine" => self.legit_mine = v,
            "assume_water" => self.assume_water = v,
            "assume_lava" => self.assume_lava = v,
            "parkour_place" => self.parkour_place = v,
            "downward" => self.downward = v,
            "vines" => self.vines = v,
            "diag_ascend" => self.diag_ascend = v,
            "diag_descend" => self.diag_descend = v,
            "assume_step" => self.assume_step = v,
            "sprint_water" => self.sprint_water = v,
            "tool_select" => self.tool_select = v,
            "bottom_slab" => self.bottom_slab = v,
            _ => {}
        }
    }

    /// `v` is already clamped to the table range by the caller.
    pub fn put_num(&mut self, key: &str, v: f64) {
        match key {
            "max_drop" => self.max_drop = v as i32,
            "mine_radius" => self.mine_radius = v as i32,
            "follow_radius" => self.follow_radius = v as i32,
            "mob_avoid_r" => self.mob_avoid_r = v as i32,
            "mob_avoid_c" => self.mob_avoid_c = v,
            _ => {}
        }
    }
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
            last_dist: f64::MAX,
            replanned: false,
            vert_fails: 0,
            stale_cd: 0,
            cooldown: 0,
            plan_cd: 0,
            plan_fails: 0,
            busy: false,
            objective: None,
            tunnel_dir: [0, 0],
            tunnel_size: [1, 2],
            tunnel_queue: std::collections::VecDeque::new(),
            tunnel_col: [i32::MIN; 3],
            excavate: false,
            drop_pickup: false,
            mine_want: 0,
            mine_done: 0,
            mine_have0: 0,
            drop_ban: Vec::new(),
            drop_stall: 0,
            seen_ores: std::collections::HashSet::new(),
            want: Want::default(),
            set: Settings::defaults(),
            visited: std::collections::HashSet::new(),
            explore_names: Vec::new(),
            rng: 0x9e3779b97f4a7c15,
            note: String::new(),
            click_left: 0,
            click_right: 0,
            held_l: false,
            held_r: false,
            eat_ticks: 0,
            eat_food: 20,
            has_throwaway: true,
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
        self.last_dist = f64::MAX;
        self.last_pos = [i32::MIN; 3];
        self.plan_cd = 0;
        self.plan_fails = 0;
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
        self.last_dist = f64::MAX;
        self.last_pos = [i32::MIN; 3];
        self.cooldown = 0;
        self.plan_cd = 0;
        self.plan_fails = 0;
        // A new process must not inherit the previous one's held inputs —
        // several `begin()` callers skip the core driver for a tick.
        self.want = Want::default();
        self.excavate = false;
        self.drop_pickup = false;
        self.drop_stall = 0;
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

/// Run `f` with the global state, creating it on first use. Poison is
/// tolerated (`into_inner`): a panicking command must not wedge the game
/// loop's only state lock. The lock is NOT reentrant — callers that need
/// the world (`pf::W::new`, `load`, `start_cell`) take it themselves, so
/// never call those from inside `with`.
pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(State::new());
    }
    f(g.as_mut().unwrap())
}

/// `native_setting` read. Missing keys leave the defaults.
pub fn load(name: &str, value: &str) {
    let Some(spec) = st::SETTINGS.iter().find(|s| s.key == name) else {
        return;
    };
    with(|s| s.set.apply(spec, value));
}

/// Settings persisted through `native_setting`, in table order.
pub fn to_save() -> Vec<(&'static str, String)> {
    with(|s| {
        st::SETTINGS
            .iter()
            .map(|sp| (sp.key, s.set.value(sp.key)))
            .collect()
    })
}

/// Planner flags every module shares — one site so a new setting can't
/// drift between drivers.
pub fn opts(s: &State) -> crate::game::umbx_pathfind::Opts {
    crate::game::umbx_pathfind::Opts {
        allow_break: s.set.allow_break,
        allow_place: s.set.allow_place,
        allow_parkour: s.set.allow_parkour,
        max_drop: s.set.max_drop,
        mob_avoid_r: s.set.mob_avoid_r as f64,
        mob_avoid_c: s.set.mob_avoid_c,
        parkour_place: s.set.parkour_place,
        has_throwaway: s.has_throwaway,
        downward: s.set.downward,
        vines: s.set.vines,
        diag_ascend: s.set.diag_ascend,
        diag_descend: s.set.diag_descend,
        assume_step: s.set.assume_step,
        sprint_water: s.set.sprint_water,
    }
}

// ---------------------------------------------------------------------------
// Helpers every module used to carry its own copy of.
// ---------------------------------------------------------------------------

/// The player's feet cell.
pub fn feet(p: &crate::motion::PlayerMotion) -> [i32; 3] {
    [
        p.position.x.floor() as i32,
        p.position.y.floor() as i32,
        p.position.z.floor() as i32,
    ]
}

/// Chat line with the `baritone:` prefix.
pub fn say(game: &mut crate::game::Game<'_>, m: &str) {
    game.native_chat(&format!("baritone: {m}"));
}

/// Block-centre point of a cell.
pub fn center(p: [i32; 3]) -> [f64; 3] {
    [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5]
}

/// `minecraft:stone` -> `stone`.
pub fn tail(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// `minecraft:stone`/`Stone` -> `stone` — a lowercase bare tail.
pub fn norm(s: &str) -> String {
    s.trim_start_matches("minecraft:").to_ascii_lowercase()
}

/// Full-collision block at `p`.
pub fn solid(game: &crate::game::Game<'_>, p: [i32; 3]) -> bool {
    game.res
        .facts()
        .get(game.world.block(p[0], p[1], p[2]))
        .full_collision
}

/// The cell is one of the three air states.
pub fn is_air(name: &str) -> bool {
    name == "minecraft:air" || name.ends_with("_air")
}

/// `ends_with` match on the name tail (`stone` also hits `sandstone`;
/// callers that care check exact tails first).
pub fn name_match(f_name: &str, want: &str) -> bool {
    tail(f_name).ends_with(want)
}

/// A `Hit` centred on `face` of `cell`.
pub fn face_hit(cell: [i32; 3], face: [i32; 3]) -> crate::game::Hit {
    crate::game::Hit {
        inside: false,
        pos: cell,
        face,
        point: [
            cell[0] as f64 + 0.5 + face[0] as f64 * 0.5,
            cell[1] as f64 + 0.5 + face[1] as f64 * 0.5,
            cell[2] as f64 + 0.5 + face[2] as f64 * 0.5,
        ],
    }
}

/// Face `at` (a world point) with the camera + body yaw. `pitch` also
/// aims the head up/down at the point — break/place aiming needs it;
/// farm's soil clicks keep a flat yaw.
pub fn aim(game: &mut crate::game::Game<'_>, at: [f64; 3], pitch: bool) {
    let Some(p) = game.player.as_ref() else { return };
    let dx = at[0] - p.position.x;
    let dz = at[2] - p.position.z;
    let yaw = (-dx.atan2(dz).to_degrees()) as f32;
    let dy = at[1] - (p.position.y + 1.62);
    let pv = (-dy.atan2(dx.hypot(dz)).to_degrees()) as f32;
    game.yaw = yaw;
    if pitch {
        game.pitch = pv;
    }
    let p = game.player.as_mut().unwrap();
    p.yaw = yaw;
    p.head_yaw = yaw;
    if pitch {
        p.pitch = pv;
    }
}

/// Where the player can path from. Wherever the player's body actually
/// is — physics truth beats the planner's standability model (fences,
/// slabs and fresh placements all collide differently than they read).
/// Picking a "standable" cell the player isn't in made the first move
/// expect them somewhere else.
///
/// Takes the state lock through `pf::W::new` — never call inside `with`.
pub fn start_cell(game: &crate::game::Game<'_>) -> Option<[i32; 3]> {
    let w = pf::W::new(game);
    let player = game.player.as_ref()?;
    let feet = feet(player);
    if w.body_open(feet) || w.water(feet) {
        if player.ground.on_ground || w.floor(feet) || w.water(feet) {
            return Some(feet);
        }
        // Airborne: feet() floors the mid-jump position a block high, so
        // the plan would start from a cell the player doesn't stand on.
        // Start from where they'll land instead.
        for d in 1..=5 {
            let p = [feet[0], feet[1] - d, feet[2]];
            if w.floor(p) && w.body_open(p) {
                return Some(p);
            }
        }
        return Some(feet);
    }
    // Buried or otherwise unsound: fall back to the nearest standable cell.
    for dy in -2..=2 {
        for dx in -1..=1 {
            for dz in -1..=1 {
                let p = [feet[0] + dx, feet[1] + dy, feet[2] + dz];
                if w.standable(p) {
                    return Some(p);
                }
            }
        }
    }
    // In water there is no standable cell: start where we float and let
    // Swim moves carry the route to shore.
    if w.water(feet) {
        return Some(feet);
    }
    // Buried with nothing standable nearby (suffocating in stone, wedged
    // in gravel): start at the feet anyway — Break successors dig the
    // body out. Returning None here left the process visibly frozen.
    Some(feet)
}

/// Stuck-watchdog thresholds every driver shares: re-plan once at
/// REPLAN_TICKS still ticks, give up at STUCK_TICKS.
pub const REPLAN_TICKS: u32 = 40;
pub const STUCK_TICKS: u32 = 120;

/// Neighbour offsets a placement can hang a block's face on: the four
/// sides and the face below.
pub const SUPPORT_DIRS: &[[i32; 3]] = &[
    [1, 0, 0],
    [-1, 0, 0],
    [0, 0, 1],
    [0, 0, -1],
    [0, -1, 0],
];

/// All six face-neighbour offsets.
pub const SIX_DIRS: &[[i32; 3]] = &[
    [1, 0, 0],
    [-1, 0, 0],
    [0, 0, 1],
    [0, 0, -1],
    [0, 1, 0],
    [0, -1, 0],
];
