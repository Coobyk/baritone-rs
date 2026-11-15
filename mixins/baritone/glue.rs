// Baritone port: commands, processes and the per-tick driver.
//
// Commands (`/b`, `/baritone`):
//   goto <x> <z> | goto <x> <y> <z> | goto y <y>   walk to a goal
//   mine <block...>                              find + break matching blocks
//   follow <entity>                              stay near a mob kind
//   explore                                      wander to unvisited spots
//   tunnel [n]                                   dig + walk straight ahead
//   thisway [n]                                  walk n blocks facing-ward
//   axis                                         walk to the nearest axis
//   goal  path  pause  resume  stop  set  help   the rest

use crate::game::umbx_baritone as rt;
use crate::game::umbx_pathfind as pf;
use crate::game::umbx_baritone_exec as ex;
use crate::game::Game;

fn feet(p: &crate::motion::PlayerMotion) -> [i32; 3] {
    [
        p.position.x.floor() as i32,
        p.position.y.floor() as i32,
        p.position.z.floor() as i32,
    ]
}

fn on(game: &Game<'_>) -> bool {
    game.native_on(rt::ID)
}

/// The window's own game, not the single player server's.
fn client(game: &Game<'_>) -> bool {
    game.native_window()
}

/// Where the player can stand from where they are: their cell, else the
/// standable cell closest to it.
fn start_cell(game: &Game<'_>) -> Option<[i32; 3]> {
    let w = pf::W { game };
    let player = game.player.as_ref()?;
    let feet = feet(player);
    // Wherever the player's body actually is — physics truth beats the
    // planner's standability model (fences, slabs and fresh placements all
    // collide differently than they read). Picking a "standable" cell the
    // player isn't in made the first move expect them somewhere else.
    if w.body_open(feet) || w.water(feet) {
        if player.ground.on_ground || w.floor(feet) || w.water(feet) {
            return Some(feet);
        }
        // Airborne: feet() floors the mid-jump position a block high, so the
        // plan would start from a cell the player doesn't stand on. Start
        // from where they'll land instead.
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
    // In water there is no standable cell: start where we float and let Swim
    // moves carry the route to shore.
    if w.water(feet) {
        return Some(feet);
    }
    None
}

fn opts(s: &rt::State) -> pf::Opts {
    pf::Opts {
        allow_break: s.set.allow_break,
        allow_place: s.set.allow_place,
        allow_parkour: s.set.allow_parkour,
        max_drop: s.set.max_drop,
    }
}

/// `native_mods`: the mod list entry.
pub(super) fn listed(mods: &mut Vec<crate::game::native_hooks::NativeMod>) {
    mods.push(crate::game::native_hooks::NativeMod {
        id: rt::ID.into(),
        title: format!("Baritone {}", rt::VERSION),
        description: rt::DESCRIPTION.into(),
        keys: Vec::new(),
    });
}

/// `native_commands`: names the chat box completes and colours.
pub(super) fn commands(game: &Game<'_>, commands: &mut Vec<&'static str>) {
    if on(game) && client(game) {
        commands.extend(["b", "baritone"]);
    }
}

/// `native_settings`: buttons mirroring the toggles.
pub(super) fn page(game: &Game<'_>, mod_id: &str, buttons: &mut Vec<String>) {
    if mod_id != rt::ID {
        return;
    }
    let set = rt::with(|s| s.set);
    buttons.extend([
        format!("Sprint: {}", if set.sprint { "on" } else { "off" }),
        format!("Allow break: {}", if set.allow_break { "on" } else { "off" }),
        format!("Allow place: {}", if set.allow_place { "on" } else { "off" }),
        format!(
            "Allow parkour: {}",
            if set.allow_parkour { "on" } else { "off" }
        ),
        format!("Max drop: {}", set.max_drop),
        format!("Mine radius: {}", set.mine_radius),
        format!("Paused: {}", if set.pause { "yes" } else { "no" }),
        "Stop".to_owned(),
        "Commands: /b help in chat".to_owned(),
    ]);
    crate::game::umbx_baritone_sel::settings(game, buttons);
    crate::game::umbx_baritone_build::settings(game, buttons);
    crate::game::umbx_baritone_extras::settings(game, buttons);
    crate::game::umbx_baritone_farm::settings(game, buttons);
    crate::game::umbx_baritone_render::settings(game, buttons);
    crate::game::umbx_baritone_elytra::settings(game, buttons);
}

/// `native_settings_click`: the buttons' actions, in `page` order.
pub(super) fn click(game: &mut Game<'_>, mod_id: &str, button: usize) {
    if mod_id != rt::ID {
        return;
    }
    match button {
        0 => rt::with(|s| s.set.sprint = !s.set.sprint),
        1 => rt::with(|s| s.set.allow_break = !s.set.allow_break),
        2 => rt::with(|s| s.set.allow_place = !s.set.allow_place),
        3 => rt::with(|s| s.set.allow_parkour = !s.set.allow_parkour),
        4 => rt::with(|s| s.set.max_drop = if s.set.max_drop >= 8 { 2 } else { s.set.max_drop + 1 }),
        5 => rt::with(|s| {
            s.set.mine_radius = if s.set.mine_radius >= 64 { 16 } else { s.set.mine_radius + 16 }
        }),
        6 => rt::with(|s| s.set.pause = !s.set.pause),
        7 => rt::with(|s| s.halt()),
        _ if button >= 9 => {
            // Walk the module sections; each module's `settings` fixes its
            // section size.
            let mut at = button - 9;
            for (s_fn, c_fn) in [
                (
                    crate::game::umbx_baritone_sel::settings
                        as fn(&Game<'_>, &mut Vec<String>),
                    crate::game::umbx_baritone_sel::click
                        as fn(&mut Game<'_>, usize) -> bool,
                ),
                (
                    crate::game::umbx_baritone_build::settings,
                    crate::game::umbx_baritone_build::click,
                ),
                (
                    crate::game::umbx_baritone_extras::settings,
                    crate::game::umbx_baritone_extras::click,
                ),
                (
                    crate::game::umbx_baritone_farm::settings,
                    crate::game::umbx_baritone_farm::click,
                ),
                (
                    crate::game::umbx_baritone_render::settings,
                    crate::game::umbx_baritone_render::click,
                ),
                (
                    crate::game::umbx_baritone_elytra::settings,
                    crate::game::umbx_baritone_elytra::click,
                ),
            ] {
                let mut tmp = Vec::new();
                s_fn(game, &mut tmp);
                if at < tmp.len() {
                    c_fn(game, at);
                    break;
                }
                at -= tmp.len();
            }
        }
        _ => {}
    }
    save(game);
}

/// `native_command`: the `b`/`baritone` command tree.
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
        Some("goto" | "g") => goto(game, &args[1..]),
        Some("mine" | "m") => mine(game, &args[1..]),
        Some("follow" | "f") => follow(game, &args[1..]),
        Some("explore" | "e") => explore(game),
        Some("tunnel" | "t") => tunnel(game, &args[1..]),
        Some("thisway") => thisway(game, &args[1..]),
        Some("axis") => axis(game),
        // `goal`/`path`/`set` may be module commands (stored goal, cfg
        // settings); the module gets first refusal, the core falls through.
        Some("goal") => {
            if !crate::game::umbx_baritone_extras::command(game, &args) {
                goal_info(game)
            }
        }
        Some("path" | "repath") => {
            if !crate::game::umbx_baritone_extras::command(game, &args) {
                rt::with(|s| s.path.clear())
            }
        }
        Some("pause") => {
            rt::with(|s| s.set.pause = true);
            game.native_chat("baritone: paused");
        }
        Some("resume" | "unpause") => {
            rt::with(|s| s.set.pause = false);
            game.native_chat("baritone: resumed");
        }
        Some("stop" | "cancel" | "forcecancel" | "force_stop") => {
            rt::with(|s| s.halt());
            game.native_chat("baritone: stopped");
        }
        Some("spawn") => spawn(game),
        Some("click") => click_cmd(game, &args[1..]),
        Some("set" | "s") => {
            if !crate::game::umbx_baritone_extras::command(game, &args) {
                set(game, &args[1..])
            }
        }
        Some("sprint") => set(game, &["sprint", args.get(1).copied().unwrap_or("on")]),
        _ => {
            // Feature modules, in Baritone's command order.
            let handled = crate::game::umbx_baritone_sel::command(game, &args)
                || crate::game::umbx_baritone_build::command(game, &args)
                || crate::game::umbx_baritone_extras::command(game, &args)
                || crate::game::umbx_baritone_farm::command(game, &args)
                || crate::game::umbx_baritone_render::command(game, &args)
                || crate::game::umbx_baritone_elytra::command(game, &args);
            if !handled {
                game.native_chat("baritone: unknown command — /b help");
            }
        }
    }
}

fn help(game: &mut Game<'_>) {
    for line in [
        "/b goto <x> <z> | <x> <y> <z> | y <y>",
        "/b mine <block> — dig out matching blocks",
        "/b follow <entity> — trail a mob kind",
        "/b explore  /b tunnel [n]  /b thisway [n]  /b axis  /b spawn",
        "/b pause  /b resume  /b stop  /b path  /b click [left|right]",
        "/b set <sprint|break|place|parkour|drop|radius> [v]",
    ] {
        game.native_chat(&format!("baritone: {line}"));
    }
    for lines in [
        crate::game::umbx_baritone_sel::help(),
        crate::game::umbx_baritone_build::help(),
        crate::game::umbx_baritone_extras::help(),
        crate::game::umbx_baritone_farm::help(),
        crate::game::umbx_baritone_render::help(),
        crate::game::umbx_baritone_elytra::help(),
    ] {
        for line in lines {
            game.native_chat(&format!("baritone: {line}"));
        }
    }
}

fn say(game: &mut Game<'_>, m: &str) {
    game.native_chat(&format!("baritone: {m}"));
}

/// `~`, `~N`, `~-N`, or an absolute int — Baritone's relative coordinate.
fn coord(a: &str, base: i32) -> Option<i32> {
    if let Some(rel) = a.strip_prefix('~') {
        if rel.is_empty() {
            return Some(base);
        }
        return rel.parse::<i32>().ok().map(|d| base + d);
    }
    a.parse::<i32>().ok()
}

fn goto(game: &mut Game<'_>, args: &[&str]) {
    let Some(me) = game.player.as_ref().map(feet) else {
        return;
    };
    let c = |a: &str, i: usize| coord(a, me[i]);
    let goal = match args {
        [x, y, z] => match (c(x, 0), c(y, 1), c(z, 2)) {
            (Some(x), Some(y), Some(z)) => Some([x, y, z]),
            _ => None,
        },
        ["y", y] => {
            if let Some(y) = c(y, 1) {
                rt::with(|s| s.begin(rt::Proc::YLevel { y }));
                say(game, &format!("going to y={y}"));
                return;
            }
            None
        }
        [x, z] => match (c(x, 0), c(z, 2)) {
            (Some(x), Some(z)) => {
                // GoalXZ: don't resolve the column up-front — it may be far
                // away and unloaded; any standable cell in it counts.
                rt::with(|s| s.begin(rt::Proc::GotoXZ { x, z }));
                say(game, &format!("going to {x} {z}"));
                return;
            }
            _ => None,
        },
        _ => None,
    };
    match goal {
        Some(goal) => {
            rt::with(|s| s.begin(rt::Proc::Goto { goal }));
            say(
                game,
                &format!("going to {} {} {}", goal[0], goal[1], goal[2]),
            );
        }
        None => say(game, "/b goto <x> <z> | <x> <y> <z> | y <y>"),
    }
}

fn mine(game: &mut Game<'_>, args: &[&str]) {
    if args.is_empty() {
        say(game, "/b mine [n] <block> — e.g. /b mine 5 log,diamond_ore");
        return;
    }
    // Upstream `mine <count> <block>`: a leading integer is the quantity.
    let (want, rest) = match args[0].parse::<u32>() {
        Ok(n) => (n, &args[1..]),
        Err(_) => (0, args),
    };
    let names: Vec<String> = rest
        .iter()
        .flat_map(|a| a.split(','))
        .map(|n| n.trim_start_matches("minecraft:").to_ascii_lowercase())
        .filter(|n| !n.is_empty())
        .collect();
    if names.is_empty() {
        say(game, "/b mine [n] <block>");
        return;
    }
    rt::with(|s| {
        s.mine_names = names.clone();
        s.mine_want = want;
        s.mine_done = 0;
        s.begin(rt::Proc::Mine { next: 0 });
    });
    let what = names.join(", ");
    if want > 0 {
        say(game, &format!("mining {want} of {what}"));
    } else {
        say(game, &format!("mining {what}"));
    }
}

fn follow(game: &mut Game<'_>, args: &[&str]) {
    // Upstream forms: `follow player <name>`, `follow entity <type>`,
    // `follow entities`; a bare name keeps the old match-anything form.
    let (mode, name) = match args {
        ["player", name] | ["players", name] => (1u8, (*name).to_string()),
        ["entity", name] | ["entitytype", name] => (2u8, (*name).to_string()),
        ["entities"] => (3u8, String::new()),
        [name, ..] => (0u8, (*name).to_string()),
        [] => {
            say(
                game,
                "/b follow <entity> | player <name> | entity <type> | entities",
            );
            return;
        }
    };
    let name = name.trim_start_matches("minecraft:").to_ascii_lowercase();
    rt::with(|s| {
        s.follow_name = name.clone();
        s.follow_mode = mode;
        s.begin(rt::Proc::Follow);
    });
    match mode {
        1 => say(game, &format!("following player {name}")),
        2 => say(game, &format!("following entity {name}")),
        3 => say(game, "following entities"),
        _ => say(game, &format!("following {name}")),
    }
}

fn explore(game: &mut Game<'_>) {
    rt::with(|s| {
        s.visited.clear();
        s.begin(rt::Proc::Explore);
    });
    say(game, "exploring");
}

fn tunnel(game: &mut Game<'_>, args: &[&str]) {
    // Upstream: `tunnel` or `tunnel <height> <width> <length>` all-or-none.
    // A bare number stays our length shorthand (`tunnel 50` = 1×2 for 50).
    let ints: Vec<i32> = args.iter().filter_map(|a| a.parse::<i32>().ok()).collect();
    let (h, w, len) = match ints.as_slice() {
        [] => (2, 1, 4096),
        [n] => (2, 1, *n),
        [h, w] => (*h, *w, 4096),
        [h, w, l, ..] => (*h, *w, *l),
    };
    let (w, h, len) = (w.clamp(1, 5), h.clamp(1, 5), len.clamp(1, 65536));
    let Some(p) = game.player.as_ref() else { return };
    let yaw = p.yaw;
    // Facing cardinal like Baritone's tunnel.
    let dir = match ((yaw % 360.0 + 360.0) % 360.0 / 45.0).round() as i32 & 7 {
        0 => [0, 1],
        1 | 2 | 3 => [-1, 0],
        4 => [0, -1],
        _ => [1, 0],
    };
    rt::with(|s| {
        s.tunnel_dir = dir;
        s.tunnel_size = [w, h];
        s.begin(rt::Proc::Tunnel { left: len });
    });
    say(game, &format!("tunnelling {w}x{h} for {len} blocks"));
}

/// `spawn`: path to the world spawn (upstream's `spawn` — the level's spawn
/// point, with a bed/respawn point as fallback when world_spawn is unknown).
fn spawn(game: &mut Game<'_>) {
    let goal = game
        .spawn
        .map(|s| {
            [
                s.x.floor() as i32,
                s.y.floor() as i32,
                s.z.floor() as i32,
            ]
        })
        .or(game.level.world_spawn);
    match goal {
        Some(g) => {
            rt::with(|s| s.begin(rt::Proc::GotoXZ { x: g[0], z: g[2] }));
            say(game, &format!("going to spawn {} {}", g[0], g[2]));
        }
        None => say(game, "baritone: no spawn point known"),
    }
}

/// `click [left|right]`: pulse a mouse button for two ticks (upstream
/// `click` presses both at the screen center; an arg picks just one).
fn click_cmd(game: &mut Game<'_>, args: &[&str]) {
    let (l, r) = match args.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("left") => (2, 0),
        Some("right") => (0, 2),
        _ => (2, 2),
    };
    rt::with(|s| {
        s.click_left = l;
        s.click_right = r;
    });
    say(game, "clicked");
}

/// AutoEat: during a process, hold `right` on the best hotbar food once
/// hunger leaves room for it (Baritone eats when `food <= 20 - nutrition`).
/// Overlays `want.right` on free ticks rather than pausing the path — a
/// break or place in flight is never interrupted.
fn auto_eat(game: &mut Game<'_>) {
    let proc_active = rt::with(|s| s.proc != rt::Proc::None && !s.set.pause);
    let Some(food) = game.player.as_ref().map(|p| p.health.food.food) else {
        return;
    };
    let eating = rt::with(|s| s.eat_ticks > 0);
    if !proc_active {
        rt::with(|s| s.eat_ticks = 0);
        return;
    }
    if eating {
        // A bite finishes when hunger rises, the slot stops being food, or
        // a 40-tick fuse blows (eat takes 32 ticks in vanilla).
        let held_food = game
            .held()
            .and_then(|it| game.res.items().get(&it.item))
            .and_then(|i| i.food)
            .is_some();
        let done = rt::with(|s| {
            let up = s.eat_ticks >= 40;
            s.eat_ticks += 1;
            food > s.eat_food || !held_food || up
        });
        if done {
            rt::with(|s| s.eat_ticks = 0);
        } else {
            rt::with(|s| {
                s.want.right = true;
            });
        }
        return;
    }
    // Not eating: only on a free tick (no attack/place in flight), and only
    // when the best food wouldn't overshoot the hunger bar.
    if rt::with(|s| s.want.attack || s.want.right) {
        return;
    }
    let mut best: Option<(u32, usize)> = None;
    for (i, it) in game.inventory.iter().take(9).enumerate() {
        let Some(it) = it else { continue };
        let Some(info) = game.res.items().get(&it.item) else {
            continue;
        };
        let Some((nutrition, _)) = info.food else {
            continue;
        };
        if info.can_always_eat || food + nutrition as i32 <= 20 {
            if best.map_or(true, |(n, _)| nutrition > n) {
                best = Some((nutrition, i));
            }
        }
    }
    if let Some((_, slot)) = best {
        game.selected = slot;
        rt::with(|s| {
            s.want.right = true;
            s.eat_ticks = 1;
            s.eat_food = food;
        });
    }
}

fn thisway(game: &mut Game<'_>, args: &[&str]) {
    let n = args.first().and_then(|a| a.parse::<i32>().ok()).unwrap_or(16).clamp(1, 256);
    let Some(p) = game.player.as_ref() else { return };
    let yaw = p.yaw as f64;
    let (x, z) = (p.position.x, p.position.z);
    let (tx, tz) = (
        (x - yaw.to_radians().sin() * n as f64).floor() as i32,
        (z + yaw.to_radians().cos() * n as f64).floor() as i32,
    );
    let w = pf::W { game };
    let goal = pf::column_top(&w, tx, tz, p.position.y.floor() as i32)
        .unwrap_or([tx, p.position.y.floor() as i32, tz]);
    rt::with(|s| s.begin(rt::Proc::Goto { goal }));
    say(game, &format!("this way to {} {}", tx, tz));
}

fn axis(game: &mut Game<'_>) {
    let Some(f) = game.player.as_ref().map(feet) else { return };
    // Upstream's `axis` is GoalAxis: the nearest point on a diagonal — the
    // x=z or x=-z line (the Nether-highway axes).
    let d1 = (f[0] - f[2]).abs(); // distance-ish from the x=z line
    let d2 = (f[0] + f[2]).abs(); // from the x=-z line
    let (x, z) = if d1 <= d2 {
        let m = ((f[0] as i64 + f[2] as i64) / 2) as i32;
        (m, m)
    } else {
        let m = ((f[0] as i64 - f[2] as i64) / 2) as i32;
        (m, -m)
    };
    rt::with(|s| s.begin(rt::Proc::GotoXZ { x, z }));
    say(game, &format!("to axis {x} {z}"));
}

fn goal_info(game: &mut Game<'_>) {
    let t = rt::with(|s| match s.proc {
        rt::Proc::None => "none".to_owned(),
        rt::Proc::Goto { goal } => format!("goto {} {} {}", goal[0], goal[1], goal[2]),
        rt::Proc::GotoXZ { x, z } => format!("goto {x} {z}"),
        rt::Proc::YLevel { y } => format!("y level {y}"),
        rt::Proc::Mine { .. } => format!("mine {}", s.mine_names.join(",")),
        rt::Proc::Follow => format!("follow {}", s.follow_name),
        rt::Proc::Explore => "explore".to_owned(),
        rt::Proc::Tunnel { left } => format!("tunnel, {left} left"),
    });
    say(game, &format!("current goal: {t}"));
}

fn set(game: &mut Game<'_>, args: &[&str]) {
    let Some(key) = args.first() else {
        say(game, "/b set <sprint|break|place|parkour|drop|radius> [v]");
        return;
    };
    let val = args.get(1).copied();
    let flip = |b: bool, v: Option<&str>| match v {
        Some("off" | "false" | "no" | "0") => false,
        Some("on" | "true" | "yes" | "1") => true,
        Some(_) => b,
        None => !b,
    };
    rt::with(|s| match *key {
        "sprint" => s.set.sprint = flip(s.set.sprint, val),
        "break" | "allowbreak" | "allow_break" => s.set.allow_break = flip(s.set.allow_break, val),
        "place" | "allowplace" | "allow_place" => s.set.allow_place = flip(s.set.allow_place, val),
        "parkour" | "allowparkour" | "allow_parkour" => {
            s.set.allow_parkour = flip(s.set.allow_parkour, val)
        }
        "drop" | "max_drop" | "maxdrop" => {
            if let Some(v) = val.and_then(|v| v.parse::<i32>().ok()) {
                s.set.max_drop = (v as i32).clamp(1, 20);
            }
        }
        "radius" | "mine_radius" | "mineradius" => {
            if let Some(v) = val.and_then(|v| v.parse::<i32>().ok()) {
                s.set.mine_radius = (v as i32).clamp(8, 96);
            }
        }
        "drops" | "mine_scan_drops" | "minescandroppeditems" => {
            s.set.mine_scan_drops = flip(s.set.mine_scan_drops, val);
        }
        _ => say(game, &format!("unknown setting {key}")),
    });
    save(game);
    let text = rt::with(|s| {
        format!(
            "sprint={} break={} place={} parkour={} drop={} radius={} drops={}",
            s.set.sprint, s.set.allow_break, s.set.allow_place, s.set.allow_parkour,
            s.set.max_drop, s.set.mine_radius, s.set.mine_scan_drops
        )
    });
    say(game, &text);
}

fn save(game: &Game<'_>) {
    for (k, v) in rt::to_save() {
        game.native_set_setting(rt::MOD_ID, k, &v);
    }
}

/// `native_tick`: drive the active process.
pub(super) fn tick(game: &mut Game<'_>) {
    if !on(game) || !client(game) {
        return;
    }
    // Note: `rt::load` takes the state lock itself — never nest it inside
    // `rt::with` (std Mutex is not reentrant; that deadlocks the tick).
    for k in ["sprint", "allow_break", "allow_place", "allow_parkour", "max_drop", "mine_radius", "mine_scan_drops"] {
        if let Some(v) = game.native_setting(rt::MOD_ID, k) {
            rt::load(k, &v);
        }
    }
    driver(game);
    auto_eat(game);
    // `click` pulses and the auto-eat hold overlay whatever the driver
    // applied this tick. `game.left`/`game.right` latch in the engine — we
    // release them on the tick after we stop holding (a latched `right`
    // blocks `continue_attack` forever via `using.is_some()`).
    let (cl, cr) = rt::with(|s| {
        let l = s.click_left > 0;
        let r = s.click_right > 0;
        s.click_left = s.click_left.saturating_sub(1);
        s.click_right = s.click_right.saturating_sub(1);
        (l, r)
    });
    let eating = rt::with(|s| s.eat_ticks > 0);
    rt::with(|s| {
        if cl {
            s.held_l = true;
            game.left = true;
            game.swing_arm();
        } else if s.held_l {
            s.held_l = false;
            game.left = false;
        }
        if cr || eating {
            s.held_r = true;
            game.right = true;
        } else if s.held_r {
            s.held_r = false;
            game.right = false;
        }
    });
    // Feature modules drive themselves when active (they set s.busy).
    crate::game::umbx_baritone_sel::tick(game);
    crate::game::umbx_baritone_build::tick(game);
    crate::game::umbx_baritone_extras::tick(game);
    crate::game::umbx_baritone_farm::tick(game);
    crate::game::umbx_baritone_render::tick(game);
    crate::game::umbx_baritone_elytra::tick(game);
}

/// `native_tick`'s real work. Processes pick objectives; the path layer walks
/// them; the executor feeds inputs.
fn driver(game: &mut Game<'_>) {
    let Some(player) = game.player.as_ref() else { return };
    let me = feet(player);
    let grounded = player.ground.on_ground;
    let proc = rt::with(|s| s.proc);
    if proc == rt::Proc::None || rt::with(|s| s.set.pause) {
        rt::with(|s| s.want = rt::Want::default());
        ex::apply(game, &rt::Want::default());
        return;
    }

    // Processes with their own targeting pick an objective cell first.
    match proc {
        rt::Proc::Mine { .. } => mine_step(game),
        rt::Proc::Follow => follow_step(game),
        rt::Proc::Explore => explore_step(game),
        rt::Proc::Tunnel { .. } => tunnel_step(game),
        _ => {}
    }
    if rt::with(|s| s.proc == rt::Proc::None) {
        ex::apply(game, &rt::Want::default());
        return;
    }

    let goal = match proc {
        rt::Proc::Goto { goal } => Some(goal),
        rt::Proc::GotoXZ { x, z } => Some([x, me[1], z]),
        rt::Proc::YLevel { y } => Some([me[0], y, me[2]]),
        rt::Proc::Mine { .. }
        | rt::Proc::Follow
        | rt::Proc::Explore
        | rt::Proc::Tunnel { .. } => rt::with(|s| s.objective),
        rt::Proc::None => None,
    };
    let Some(goal) = goal else { return };

    // Reaching the goal mid-path counts: upstream stops the moment the goal
    // predicate holds, not only when a path exhausts. Mine/Follow/etc. have
    // their own completion semantics and keep running. Grounded only: a jump
    // apex brushing the goal's height isn't arrival.
    let arrived = grounded
        && match proc {
            rt::Proc::Goto { goal } => me == goal,
            rt::Proc::GotoXZ { x, z } => me[0] == x && me[2] == z,
            rt::Proc::YLevel { y } => me[1] == y,
            _ => false,
        };
    if arrived {
        say(game, "arrived");
        rt::with(|s| s.halt());
        ex::apply(game, &rt::Want::default());
        return;
    }

    // Re-plan when the path is exhausted (or was cleared).
    if rt::with(|s| s.at >= s.path.len()) {
        let w = pf::W { game };
        let Some(start) = start_cell(game) else { return };
        let o = rt::with(|s| opts(s));
        let path = match proc {
            rt::Proc::YLevel { y } => {
                pf::find(&w, start, goal, &o, |p, _| w.standable(p) && p[1] == y)
            }
            rt::Proc::Mine { .. } | rt::Proc::Follow => {
                // A dropped-item objective is a point to stand on; a block
                // objective is reached by standing near it.
                if rt::with(|s| s.drop_pickup) && matches!(proc, rt::Proc::Mine { .. }) {
                    pf::find_partial(&w, start, goal, &o, |p, g| p == g, true)
                } else {
                    pf::find_partial(&w, start, goal, &o, |p, _| {
                        w.standable(p)
                            && ((p[0] - goal[0]).pow(2)
                                + (p[1] - goal[1]).pow(2)
                                + (p[2] - goal[2]).pow(2)) as f64
                                <= 18.0
                    }, true)
                }
            }
            // A tunnel excavation cell works exactly like a mine target:
            // stand adjacent, then the appended Break takes it out.
            rt::Proc::Tunnel { .. } if rt::with(|s| s.excavate) => {
                pf::find_partial(&w, start, goal, &o, |p, _| {
                    w.standable(p)
                        && ((p[0] - goal[0]).pow(2)
                            + (p[1] - goal[1]).pow(2)
                            + (p[2] - goal[2]).pow(2)) as f64
                            <= 18.0
                }, true)
            }
            rt::Proc::GotoXZ { .. } => {
                pf::find_partial(&w, start, goal, &o, |p, g| p[0] == g[0] && p[2] == g[2], true)
            }
            rt::Proc::Goto { .. } => {
                pf::find_partial(&w, start, goal, &o, |p, g| p == g, true)
            }
            _ => pf::find(&w, start, goal, &o, |p, g| p == g),
        };
        match path {
            Some(mut path) => {
                // Mine (and tunnel excavation cells): stand near the block,
                // then break the block itself. Dropped-item objectives are
                // walked onto, never broken.
                let break_goal = match proc {
                    rt::Proc::Mine { .. } => !rt::with(|s| s.drop_pickup),
                    rt::Proc::Tunnel { .. } => rt::with(|s| s.excavate),
                    _ => false,
                };
                if break_goal {
                    let stand = path.last().map(|m| m.pos).unwrap_or(start);
                    path.push(pf::Move {
                        pos: stand,
                        kind: pf::Kind::Break { count: 1 },
                        cells: vec![goal],
                    });
                }
                rt::with(|s| {
                    s.path = path;
                    s.at = 0;
                    s.stuck = 0;
                    s.replanned = false;
                    s.stale_cd = 30;
                    s.note.clear();
                });
            }
            None => rt::with(|s| {
                s.note = format!("no path from {:?} to {:?} (me {:?})", start, goal, me);
                s.stuck = 120
            })
        }
    }

    // Revalidate the upcoming path against freshly loaded chunks: moves that
    // need a standable destination were planned optimistically through air,
    // so when reality turns out solid the route is stale and must be re-planned
    // (Baritone re-checks its path every tick the world changes). A fresh
    // plan gets a short grace period first: the client's chunk store can
    // disagree with itself for a tick, and re-checking instantly spins a
    // plan→flag→plan loop that never lets a move run.
    {
        let w = pf::W { game };
        let stale = rt::with(|s| {
            if s.stale_cd > 0 {
                s.stale_cd -= 1;
                return false;
            }
            let bad = s.path[s.at..].iter().take(10).find(|m| {
                match m.kind {
                    pf::Kind::Walk
                    | pf::Kind::Step
                    | pf::Kind::Ladder { .. }
                    | pf::Kind::Parkour { .. } => !w.standable(m.pos),
                    pf::Kind::Drop { .. } => !w.standable(m.pos) && !w.water(m.pos),
                    pf::Kind::Swim => !w.water(m.pos),
                    _ => false,
                }
            });
            if let Some(m) = bad {
                s.note = format!("stale {:?} at {:?}", m.kind, m.pos);
            }
            bad.is_some()
        });
        if stale {
            rt::with(|s| {
                s.path.clear();
                s.stale_cd = 30;
            });
            return;
        }
    }

    // Execute the current move.
    let step = rt::with(|s| s.path.get(s.at).cloned());
    let prev = rt::with(|s| if s.at == 0 { me } else { s.path[s.at - 1].pos });
    let mut vertical_move = false;
    if let Some(m) = step {
        vertical_move = matches!(
            m.kind,
            pf::Kind::Ladder { .. }
                | pf::Kind::Pillar
                | pf::Kind::Drop { .. }
                | pf::Kind::Downward
        );
        match rt::with(|s| ex::drive(game, s, &m, prev)) {
            ex::Out::Done => {
                rt::with(|s| {
                    s.at += 1;
                    s.stuck = 0;
                    s.vert_fails = 0;
                });
                if rt::with(|s| s.at >= s.path.len()) {
                    // A partial path ends short of the goal: re-plan, don't
                    // report arrival. Only the satisfied goal says "arrived".
                    let there = match proc {
                        rt::Proc::Goto { goal } => me == goal,
                        rt::Proc::GotoXZ { x, z } => me[0] == x && me[2] == z,
                        rt::Proc::YLevel { y } => me[1] == y,
                        _ => false,
                    };
                    if there {
                        say(game, "arrived");
                        rt::with(|s| s.halt());
                    } else {
                        rt::with(|s| s.path.clear());
                    }
                }
            }
            ex::Out::Failed => {
                // Baritone treats a failed move as a route defect. A flat
                // node can be skipped; a vertical one can't — every node
                // after it assumes the new elevation — so it forces a
                // re-plan, and identical re-plans get capped.
                if m.pos[1] != prev[1] {
                    let fails = rt::with(|s| {
                        s.path.clear();
                        s.stuck = 0;
                        s.vert_fails += 1;
                        s.vert_fails
                    });
                    if fails >= 5 {
                        say(game, "can't climb that — giving up");
                        rt::with(|s| s.halt());
                    }
                } else {
                    let done = rt::with(|s| {
                        s.at += 1;
                        s.stuck = 0;
                        s.at >= s.path.len()
                    });
                    if done {
                        rt::with(|s| s.path.clear());
                    }
                }
            }
            ex::Out::Working => {}
        }
    }

    // Stuck watchdog: progress is horizontal cell travel, or vertical travel
    // only while a move that climbs/descends is active — hopping in place is
    // not progress. Hop at ~10 still ticks, re-plan once at 40, quit at 120.
    let last = rt::with(|s| s.last_pos);
    let moved = me[0] != last[0]
        || me[2] != last[2]
        || (vertical_move && me[1] != last[1]);
    let stuck = rt::with(|s| {
        s.last_pos = me;
        if moved {
            s.stuck = 0;
        } else {
            s.stuck += 1;
        }
        if s.stuck >= 10 && s.want.forward != 0.0 {
            s.want.jump = true;
        }
        s.stuck
    });
    if stuck == 40 && !rt::with(|s| s.replanned) {
        rt::with(|s| {
            s.replanned = true;
            s.path.clear();
        });
    } else if stuck >= 120 {
        say(game, "stuck — giving up");
        rt::with(|s| s.halt());
    }

    let want = rt::with(|s| s.want);
    ex::apply(game, &want);
}

/// Mine: keep `s.objective` on the nearest matching block in the radius; a
/// finished path means the block is gone, so rescan.
fn mine_step(game: &mut Game<'_>) {
    let (names, radius, cur, was_drop) = rt::with(|s| {
        (
            s.mine_names.clone(),
            s.set.mine_radius,
            s.objective,
            s.drop_pickup,
        )
    });
    let Some(player) = game.player.as_ref() else { return };
    let me = feet(player);
    // If a target is still there and still in scan range, keep it.
    if let Some(t) = cur {
        let in_range = (t[0] - me[0]).abs() <= radius
            && (t[2] - me[2]).abs() <= radius
            && (t[1] - me[1]).abs() <= 16;
        // A drop objective lives or dies by its entity; a block objective
        // by its block. Checking a drop's cell block would always miss.
        let there = if was_drop {
            game.dropped.iter().any(|it| {
                names
                    .iter()
                    .any(|n| it.stack.item.contains(n.as_str()))
                    && (it.position.x.floor() as i32 - t[0]).abs() <= 1
                    && (it.position.y.floor() as i32 - t[1]).abs() <= 1
                    && (it.position.z.floor() as i32 - t[2]).abs() <= 1
            })
        } else {
            let st = game.world.block(t[0], t[1], t[2]);
            names
                .iter()
                .any(|n| game.res.facts().get(st).name.contains(n.as_str()))
        };
        if in_range && there {
            return;
        }
        // Gone but still in range: the block was broken (or the drop was
        // collected). Count it toward `mine <n>`; out-of-range staleness
        // (teleport, unload) just falls through to the rescan below.
        if in_range {
            let done = rt::with(|s| {
                s.mine_done += 1;
                (s.mine_done, s.mine_want)
            });
            if done.1 > 0 && done.0 >= done.1 {
                say(game, &format!("mined {} — done", done.0));
                rt::with(|s| s.halt());
                return;
            }
        }
    }
    if rt::with(|s| s.cooldown > 0) {
        rt::with(|s| s.cooldown -= 1);
        return;
    }
    rt::with(|s| s.cooldown = 5);
    // Scan nearest-first for a matching breakable block.
    let mut best: Option<(i32, [i32; 3], bool)> = None;
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            for dy in -8i32..=8 {
                let p = [me[0] + dx, me[1] + dy, me[2] + dz];
                if p[1] < game.world.shape.min_y || p[1] > game.world.shape.max_y() {
                    continue;
                }
                let st = game.world.block(p[0], p[1], p[2]);
                let f = game.res.facts().get(st);
                if f.hardness < 0.0
                    || crate::game::umbx_baritone_extras::blacklisted(f.name.as_ref())
                {
                    continue;
                }
                if names.iter().any(|n| f.name.contains(n.as_str())) {
                    let d = dx.abs() + dy.abs() + dz.abs();
                    if best.map_or(true, |(b, _, _)| d < b) {
                        best = Some((d, p, false));
                    }
                }
            }
        }
    }
    // `mineScanDroppedItems`: dropped entities matching a target count as
    // objectives too — walking over one collects it.
    if rt::with(|s| s.set.mine_scan_drops) {
        for it in &game.dropped {
            let n = it.stack.item.trim_start_matches("minecraft:");
            if !names.iter().any(|m| n.contains(m.as_str())) {
                continue;
            }
            let c = [
                it.position.x.floor() as i32 - me[0],
                it.position.y.floor() as i32 - me[1],
                it.position.z.floor() as i32 - me[2],
            ];
            if c[0].abs() > radius || c[2].abs() > radius || c[1].abs() > 16 {
                continue;
            }
            let d = c[0].abs() + c[1].abs() + c[2].abs();
            if best.map_or(true, |(b, _, _)| d < b) {
                best = Some((
                    d,
                    [
                        it.position.x.floor() as i32,
                        it.position.y.floor() as i32,
                        it.position.z.floor() as i32,
                    ],
                    true,
                ));
            }
        }
    }
    match best {
        Some((_, t, is_drop)) => {
            let new = rt::with(|s| s.objective != Some(t));
            rt::with(|s| {
                s.objective = Some(t);
                s.drop_pickup = is_drop;
            });
            if new {
                rt::with(|s| s.path.clear());
            }
        }
        None => {
            say(game, "no more matching blocks in range — done mining");
            rt::with(|s| s.halt());
        }
    }
}

/// Follow: keep the objective on the nearest mob whose kind or custom name
/// matches; re-plan when it moves more than a few cells.
fn follow_step(game: &mut Game<'_>) {
    if rt::with(|s| s.cooldown > 0) {
        rt::with(|s| s.cooldown -= 1);
        return;
    }
    rt::with(|s| s.cooldown = 10);
    let (name, mode) = rt::with(|s| (s.follow_name.clone(), s.follow_mode));
    let Some(me_p) = game.player.as_ref().map(|p| p.position) else { return };
    let mut best: Option<(f64, [i32; 3])> = None;
    for m in &game.mobs {
        if m.removed || !m.alive() {
            continue;
        }
        let kind = m
            .kind
            .model
            .name
            .trim_start_matches("minecraft:")
            .to_ascii_lowercase();
        let custom = m
            .custom_name
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let is_player = kind == "player" || !custom.is_empty();
        let hit = match mode {
            // `follow entities`: everything alive.
            3 => true,
            // `follow player <name>`: players and named entities.
            1 => is_player && (name.is_empty() || custom.contains(&name)),
            // `follow entity <type>`: kind only.
            2 => kind.contains(&name),
            // Bare name: kind or custom name contains.
            _ => kind.contains(&name) || custom.contains(&name),
        };
        if !hit {
            continue;
        }
        let d = (m.position.x - me_p.x).powi(2)
            + (m.position.y - me_p.y).powi(2)
            + (m.position.z - me_p.z).powi(2);
        if best.map_or(true, |(b, _)| d < b) {
            best = Some((
                d,
                [
                    m.position.x.floor() as i32,
                    m.position.y.floor() as i32,
                    m.position.z.floor() as i32,
                ],
            ));
        }
    }
    match best {
        Some((_, t)) => {
            let moved = rt::with(|s| {
                s.objective.map_or(true, |o| {
                    (o[0] - t[0]).abs() + (o[1] - t[1]).abs() + (o[2] - t[2]).abs() > 3
                })
            });
            if moved {
                rt::with(|s| {
                    s.objective = Some(t);
                    s.path.clear();
                });
            }
        }
        None => {
            say(game, &format!("no '{name}' in sight — stopped following"));
            rt::with(|s| s.halt());
        }
    }
}

/// Explore: walk to random unvisited columns.
fn explore_step(game: &mut Game<'_>) {
    if rt::with(|s| s.objective.is_some()) {
        return;
    }
    let Some(player) = game.player.as_ref() else { return };
    let me = feet(player);
    let w = pf::W { game };
    for _ in 0..24 {
        let (dx, dz) = rt::with(|s| {
            let a = s.next();
            (((a % 97) as i32) - 48, (((a / 97) % 97) as i32) - 48)
        });
        let (x, z) = (me[0] + dx, me[2] + dz);
        if rt::with(|s| s.visited.contains(&(x, z))) {
            continue;
        }
        if let Some(g) = pf::column_top(&w, x, z, me[1]) {
            rt::with(|s| {
                s.visited.insert((x, z));
                s.objective = Some(g);
            });
            return;
        }
    }
    say(game, "nowhere new to explore");
    rt::with(|s| s.halt());
}

/// The cells of the tunnel cross-section through column `col` (its feet
/// cell), nearest-first to `me`: `w` columns wide across the perpendicular,
/// `h` cells tall.
fn tunnel_cells(
    me: [i32; 3],
    col: [i32; 3],
    dir: [i32; 2],
    size: [i32; 2],
) -> Vec<[i32; 3]> {
    let [w, h] = size;
    // Perpendicular in XZ: dir [1,0] digs a Z-wide cross-section and so on.
    let perp = [dir[1], dir[0]];
    let mut cells = Vec::with_capacity((w * h).max(1) as usize);
    for dy in 0..h {
        for dx in -(w / 2)..=(w - 1 - w / 2) {
            cells.push([
                col[0] + dx * perp[0],
                col[1] + dy,
                col[2] + dx * perp[1],
            ]);
        }
    }
    cells.sort_by_key(|c| {
        (c[0] - me[0]).pow(2) + (c[1] - me[1]).pow(2) + (c[2] - me[2]).pow(2)
    });
    cells
}

/// Tunnel: 1×2 lets the path's own Break moves dig the corridor; a wider
/// cross-section excavates each column's cells first, then steps in —
/// `tunnel_col` tracks the intended column so the line stays straight even
/// if the player is nudged sideways mid-dig.
fn tunnel_step(game: &mut Game<'_>) {
    let Some(player) = game.player.as_ref() else { return };
    let me = feet(player);
    let (dir, left, size) = rt::with(|s| {
        (
            s.tunnel_dir,
            if let rt::Proc::Tunnel { left } = s.proc {
                left
            } else {
                0
            },
            s.tunnel_size,
        )
    });
    if left <= 0 {
        say(game, "tunnel done");
        rt::with(|s| s.halt());
        return;
    }
    if size == [1, 2] {
        rt::with(|s| {
            s.objective = Some([me[0] + dir[0] * 3, me[1], me[2] + dir[1] * 3]);
            if let rt::Proc::Tunnel { left: l } = s.proc {
                s.proc = rt::Proc::Tunnel {
                    left: l.saturating_sub(1),
                };
            }
        });
        return;
    }
    let w = pf::W { game };
    let col = rt::with(|s| s.tunnel_col);
    if col == [i32::MIN; 3] {
        // First step: aim at the column ahead and queue its cross-section.
        let nxt = [me[0] + dir[0], me[1], me[2] + dir[1]];
        let cells = tunnel_cells(me, nxt, dir, size);
        rt::with(|s| {
            s.tunnel_col = nxt;
            s.tunnel_queue = cells.into();
        });
        return;
    }
    // Drop cells already open or undiggable (air, water, lava, bedrock).
    loop {
        let front = rt::with(|s| s.tunnel_queue.front().copied());
        match front {
            Some(c) if w.open_cell(c) || !w.breakable(c) => {
                rt::with(|s| {
                    s.tunnel_queue.pop_front();
                });
            }
            _ => break,
        }
    }
    if rt::with(|s| !s.tunnel_queue.is_empty()) {
        // Excavate the front cell: the driver paths adjacent and appends a
        // Break move on it (the Mine arm).
        rt::with(|s| {
            s.excavate = true;
            s.objective = s.tunnel_queue.front().copied();
        });
        return;
    }
    rt::with(|s| s.excavate = false);
    if me[0] == col[0] && me[2] == col[2] {
        // Standing in the excavated column: advance one step on the line.
        let nxt = [col[0] + dir[0], col[1], col[2] + dir[1]];
        let cells = tunnel_cells(me, nxt, dir, size);
        rt::with(|s| {
            s.tunnel_col = nxt;
            s.tunnel_queue = cells.into();
            if let rt::Proc::Tunnel { left: l } = s.proc {
                s.proc = rt::Proc::Tunnel {
                    left: l.saturating_sub(1),
                };
            }
        });
        return;
    }
    // Column open, not yet inside: walk in.
    rt::with(|s| s.objective = Some(col));
}

/// `native_movement_input`: press the keys the executor asked for.
pub(super) fn movement_input(
    game: &Game<'_>,
    forward: &mut f32,
    strafe: &mut f32,
    jump: &mut bool,
    sneak: &mut bool,
) {
    if !on(game) || !client(game) {
        return;
    }
    rt::with(|s| {
        if (s.proc == rt::Proc::None && !s.busy) || s.set.pause {
            return;
        }
        if s.want.forward != 0.0 {
            *forward = s.want.forward;
            *strafe = 0.0;
        }
        if s.want.jump {
            *jump = true;
        }
        if s.want.sneak {
            *sneak = true;
        }
    });
}
