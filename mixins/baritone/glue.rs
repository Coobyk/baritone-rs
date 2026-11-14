use super::native_hooks::NativeMod;
use super::umbx_baritone as rt;
use super::umbx_pathfind as pf;
use super::Game;

fn on(game: &Game<'_>) -> bool {
    game.native_on(rt::ID)
}
/// The window's own game, not the single player server's.
fn client(game: &Game<'_>) -> bool {
    game.native_window()
}

pub(super) fn listed(mods: &mut Vec<NativeMod>) {
    mods.push(NativeMod {
        id: rt::ID.into(),
        title: format!("Baritone {}", rt::VERSION),
        description: rt::DESCRIPTION.into(),
        keys: Vec::new(),
    });
}

/// `native_commands`: name the chat box completes and colours.
pub(super) fn commands(game: &Game<'_>, commands: &mut Vec<&'static str>) {
    if on(game) && client(game) {
        commands.extend(["b", "baritone"]);
    }
}

fn feet_of(player: &crate::motion::PlayerMotion) -> [i32; 3] {
    [
        player.position.x.floor() as i32,
        player.position.y.floor() as i32,
        player.position.z.floor() as i32,
    ]
}

/// `native_command`: `b`/`baritone` subcommands.
pub(super) fn command(game: &mut Game<'_>, command: &str, ran: &mut bool) {
    if !on(game) || !client(game) {
        return;
    }
    let mut words = command.split_whitespace();
    let head = words.next().unwrap_or_default();
    if !head.eq_ignore_ascii_case("b") && !head.eq_ignore_ascii_case("baritone") {
        return;
    }
    *ran = true;
    let args: Vec<&str> = words.collect();
    match args.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        None | Some("help" | "h") => help(game),
        Some("goto" | "g" | "goal") => goto(game, &args[1..]),
        Some("stop" | "cancel" | "pause") => {
            rt::halt();
            game.native_chat("baritone: stopped");
        }
        Some("path" | "repath") => repath(game),
        Some("sprint") => match args.get(1).map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("off" | "false" | "no" | "0") => {
                rt::set_sprint(game, false);
                game.native_chat("baritone: sprint off");
            }
            Some("on" | "true" | "yes" | "1") | None => {
                rt::set_sprint(game, true);
                game.native_chat("baritone: sprint on");
            }
            _ => game.native_chat("baritone: /b sprint [on|off]"),
        },
        _ => game.native_chat("baritone: unknown — /b help"),
    }
}

fn help(game: &mut Game<'_>) {
    game.native_chat("baritone: /b goto <x> <z> or /b goto <x> <y> <z>");
    game.native_chat("baritone: /b stop  /b path  /b sprint [on|off]");
}

fn goto(game: &mut Game<'_>, args: &[&str]) {
    let Some(player) = game.player.as_ref() else {
        return;
    };
    let feet = feet_of(player);
    let ints: Vec<Option<i32>> = args.iter().map(|a| a.parse::<i32>().ok()).collect();
    let goal = match ints.as_slice() {
        [Some(x), Some(y), Some(z)] => Some([*x, *y, *z]),
        [Some(x), Some(z)] => pf::column_top(game, *x, *z, feet[1]),
        _ => {
            game.native_chat("baritone: /b goto <x> <z> or /b goto <x> <y> <z>");
            return;
        }
    };
    let Some(goal) = goal.and_then(|g| pf::nearest_standable(game, g).or(Some(g))) else {
        game.native_chat("baritone: nowhere to stand at the goal");
        return;
    };
    let Some(start) = pf::nearest_standable(game, feet) else {
        game.native_chat("baritone: no footing here");
        return;
    };
    walk(game, start, goal);
}

fn walk(game: &mut Game<'_>, start: [i32; 3], goal: [i32; 3]) {
    match pf::find(game, start, goal) {
        Some(path) => {
            let n = path.len();
            rt::start(path, goal);
            game.native_chat(&format!(
                "baritone: walking to {} {} {} ({n} nodes)",
                goal[0], goal[1], goal[2]
            ));
        }
        None => game.native_chat("baritone: no path found"),
    }
}

fn repath(game: &mut Game<'_>) {
    let (active, goal) = rt::with(|s| (s.active, s.goal));
    if !active {
        game.native_chat("baritone: no goal set");
        return;
    }
    let Some(player) = game.player.as_ref() else {
        return;
    };
    let Some(start) = pf::nearest_standable(game, feet_of(player)) else {
        game.native_chat("baritone: no footing here");
        return;
    };
    walk(game, start, goal);
}

/// `native_tick`: steer the body along the path, a node at a time.
pub(super) fn tick(game: &mut Game<'_>) {
    if !on(game) || !client(game) {
        return;
    }
    // Read the sprint setting's file outside the state lock.
    rt::sprint(game);
    rt::with(|s| driver(game, s));
}

fn driver(game: &mut Game<'_>, s: &mut rt::State) {
    if !s.active || !game.native_playing() {
        s.movement = rt::Move::default();
        return;
    }
    let Some(player) = game.player.as_ref() else {
        s.movement = rt::Move::default();
        return;
    };
    let pos = player.position;
    let on_ground = player.ground.on_ground;
    let feet = feet_of(player);
    // Advance past nodes reached or passed; a node counts when the body is
    // near its centre horizontally and at (or below, having fallen) its y.
    while s.at < s.path.len() {
        let n = s.path[s.at];
        let dx = pos.x - (f64::from(n[0]) + 0.5);
        let dz = pos.z - (f64::from(n[2]) + 0.5);
        let close = dx * dx + dz * dz < 0.35 * 0.35;
        if feet == n || (close && n[1] <= feet[1] + 1 && feet[1] - n[1] <= 4) {
            s.at += 1;
        } else {
            break;
        }
    }
    if s.at >= s.path.len() {
        s.active = false;
        s.movement = rt::Move::default();
        game.native_chat("baritone: arrived");
        return;
    }
    let n = s.path[s.at];
    // Face the node's centre.
    let yaw = (-(f64::from(n[0]) + 0.5 - pos.x)
        .atan2(f64::from(n[2]) + 0.5 - pos.z))
    .to_degrees() as f32;
    game.yaw = yaw;
    let sprint = s.sprint;
    if let Some(p) = game.player.as_mut() {
        p.yaw = yaw;
        p.head_yaw = yaw;
        if sprint {
            p.sprinting = true;
        }
    }
    s.movement = rt::Move {
        forward: 1.0,
        strafe: 0.0,
        jump: n[1] > feet[1] && on_ground,
        sneak: false,
    };
    // Stuck: no XZ progress while trying to walk — hop, then re-plan once.
    if on_ground && (pos.x - s.last[0]).abs() + (pos.z - s.last[1]).abs() < 0.05 {
        s.still += 1;
    } else {
        s.still = 0;
        s.last = [pos.x, pos.z];
    }
    if s.still > 40 {
        s.still = 0;
        if s.repathed {
            s.reset();
            game.native_chat("baritone: stuck, giving up");
            return;
        }
        s.repathed = true;
        s.active = false;
        if let Some(start) = pf::nearest_standable(game, feet) {
            let goal = s.goal;
            match pf::find(game, start, goal) {
                Some(path) => s.begin(path, goal),
                None => {
                    s.reset();
                    game.native_chat("baritone: stuck, and no way around");
                }
            }
        } else {
            s.reset();
        }
    } else if s.still > 12 {
        s.movement.jump = true;
    }
}

/// `native_movement_input`: while a path runs, the mod's inputs replace the
/// keys'.
pub(super) fn movement_input(
    game: &Game<'_>,
    forward: &mut f32,
    strafe: &mut f32,
    jump: &mut bool,
    sneak: &mut bool,
) {
    if !on(game) {
        return;
    }
    rt::with(|s| {
        if s.active {
            *forward = s.movement.forward;
            *strafe = s.movement.strafe;
            *jump = s.movement.jump;
            *sneak = s.movement.sneak;
        }
    });
}

fn labels(game: &Game<'_>) -> Vec<String> {
    vec![
        "Commands: /b goto <x> <z>, /b goto <x> <y> <z>".to_owned(),
        "/b stop · /b path · /b sprint".to_owned(),
        format!("Sprint while walking: {}", if rt::sprint(game) { "on" } else { "off" }),
        "Stop walking".to_owned(),
    ]
}

pub(super) fn page(game: &Game<'_>, mod_id: &str, buttons: &mut Vec<String>) {
    if mod_id == rt::ID {
        buttons.extend(labels(game));
    }
}

pub(super) fn click(game: &mut Game<'_>, mod_id: &str, button: usize) {
    if mod_id != rt::ID {
        return;
    }
    match button {
        2 => rt::set_sprint(game, !rt::sprint(game)),
        3 => {
            rt::halt();
            game.native_chat("baritone: stopped");
        }
        _ => {}
    }
}
