use super::Game;
use std::sync::Mutex;

pub(super) const ID: &str = "baritone";
pub(super) const VERSION: &str = "0.1.0";
pub(super) const DESCRIPTION: &str =
    "Walks where you send it: /b goto <x> <z>, /b goto <x> <y> <z>, /b stop.";

/// The movement `native_movement_input` forces while a path runs.
#[derive(Clone, Copy, Default)]
pub(super) struct Move {
    pub forward: f32,
    pub strafe: f32,
    pub jump: bool,
    pub sneak: bool,
}

/// Pathing state. `native_tick` drives it on the client window's game.
pub(super) struct State {
    /// The path's standing nodes; `at` is the next one to reach.
    pub path: Vec<[i32; 3]>,
    pub at: usize,
    pub goal: [i32; 3],
    pub active: bool,
    /// XZ progress bookkeeping for stuck detection.
    pub last: [f64; 2],
    pub still: u32,
    /// Already re-planned once for this leg.
    pub repathed: bool,
    /// The inputs to force while `active`.
    pub movement: Move,
    /// The `sprint` setting and whether it was read from disk.
    pub sprint: bool,
    sprint_loaded: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    path: Vec::new(),
    at: 0,
    goal: [0; 3],
    active: false,
    last: [0.0; 2],
    still: 0,
    repathed: false,
    movement: Move {
        forward: 0.0,
        strafe: 0.0,
        jump: false,
        sneak: false,
    },
    sprint: true,
    sprint_loaded: false,
});

/// Everything happens under the one lock.
pub(super) fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    f(&mut STATE.lock().unwrap_or_else(|e| e.into_inner()))
}

impl State {
    /// Start a walk: `path`'s nodes from the next one to the goal.
    pub(super) fn begin(&mut self, path: Vec<[i32; 3]>, goal: [i32; 3]) {
        self.path = path;
        self.at = 0;
        self.goal = goal;
        self.active = true;
        self.still = 0;
        self.repathed = false;
        self.movement = Move::default();
    }

    pub(super) fn reset(&mut self) {
        self.active = false;
        self.path.clear();
        self.movement = Move::default();
    }
}

/// The sprint setting, read from the files once.
pub(super) fn sprint(game: &Game<'_>) -> bool {
    with(|s| {
        if !s.sprint_loaded {
            s.sprint_loaded = true;
            s.sprint = game.native_setting(ID, "sprint").as_deref() != Some("0");
        }
        s.sprint
    })
}

pub(super) fn set_sprint(game: &Game<'_>, on: bool) {
    with(|s| {
        s.sprint = on;
        s.sprint_loaded = true;
    });
    game.native_set_setting(ID, "sprint", if on { "1" } else { "0" });
}

/// Start a walk: `path`'s nodes from the next one to the goal.
pub(super) fn start(path: Vec<[i32; 3]>, goal: [i32; 3]) {
    with(|s| s.begin(path, goal));
}

pub(super) fn halt() {
    with(|s| s.reset());
}
