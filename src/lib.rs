//! Baritone's own crate: settings page (`Options > Mods > Baritone`) and
//! the `baritone:*` command argument types the `/b` usages name. The game
//! itself runs the injected modules under `src/` (`[[native.module]]`), so
//! the heavy logic lives there; this crate registers what the host asks.

use std::sync::Mutex;

use mod_api::{
    ChoiceArgument, ClientHost, ClientTickPreEvent, CommandArgumentKind, CommandArgumentType, CommandSender,
    CommandSuggestion, Hooks, Registrar, RegistrationContext, SettingSpec,
};

// The completion-name tables also injected into the game as
// `umbx_baritone_names` (single source for both sides).
include!("names.rs");

/// Setting keys the game-side runtime persists (`rt::KEYS` mirrors them).
const SETTING_NAMES: &[&str] = &[
    "sprint",
    "allow_break",
    "allow_place",
    "allow_parkour",
    "max_drop",
    "mine_radius",
    "mine_scan_drops",
    "exposed_ores",
    "follow_radius",
    "legit_mine",
    "assume_water",
    "assume_lava",
    "mob_avoid_r",
    "mob_avoid_c",
    "parkour_place",
    "downward",
    "vines",
    "diag_ascend",
    "diag_descend",
    "assume_step",
    "sprint_water",
    "tool_select",
    "bottom_slab",
];

/// Waypoint names for `wp goto`/`wp delete` completion, refreshed from the
/// persisted `wp_names` setting on the client tick.
static WAYPOINTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn word_arg(list: &'static [&'static str]) -> impl CommandArgumentType {
    struct Arg(&'static [&'static str]);
    impl CommandArgumentType for Arg {
        fn kind(&self) -> CommandArgumentKind {
            CommandArgumentKind::Word
        }
        fn parse(&self, input: &str) -> Result<String, mod_api::CommandArgumentError> {
            Ok(input.to_ascii_lowercase())
        }
        fn suggest(
            &self,
            _sender: Option<&CommandSender>,
            prefix: &str,
        ) -> Vec<CommandSuggestion> {
            let p = prefix.to_ascii_lowercase();
            self.0
                .iter()
                .filter(|c| c.starts_with(p.as_str()))
                .map(|c| CommandSuggestion::new(*c))
                .collect()
        }
    }
    Arg(list)
}

/// `baritone:blocks`: a greedy block list (`mine stone iron_ore`,
/// `mine stone,iron_ore`) completing the token after the last space or
/// comma.
struct Blocks;
impl CommandArgumentType for Blocks {
    fn kind(&self) -> CommandArgumentKind {
        CommandArgumentKind::Greedy
    }
    fn parse(&self, input: &str) -> Result<String, mod_api::CommandArgumentError> {
        Ok(input.to_ascii_lowercase())
    }
    fn suggest(
        &self,
        _sender: Option<&CommandSender>,
        prefix: &str,
    ) -> Vec<CommandSuggestion> {
        let start = prefix.rfind([' ', ',']).map(|i| i + 1).unwrap_or(0);
        let typed = prefix[start..].to_ascii_lowercase();
        BLOCK_NAMES
            .iter()
            .filter(|c| c.starts_with(typed.as_str()))
            .map(|c| CommandSuggestion::new(*c).replacing_from(start))
            .collect()
    }
}

/// `baritone:waypoint`: a waypoint saved with `/b wp save`, read from the
/// persisted `wp_names` setting (the game-side module writes it).
struct Waypoint;
impl CommandArgumentType for Waypoint {
    fn kind(&self) -> CommandArgumentKind {
        CommandArgumentKind::Word
    }
    fn parse(&self, input: &str) -> Result<String, mod_api::CommandArgumentError> {
        Ok(input.to_owned())
    }
    fn suggest(
        &self,
        _sender: Option<&CommandSender>,
        prefix: &str,
    ) -> Vec<CommandSuggestion> {
        let p = prefix.to_ascii_lowercase();
        WAYPOINTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|c| c.to_ascii_lowercase().starts_with(&p))
            .map(|c| CommandSuggestion::new(c.clone()))
            .collect()
    }
}

#[derive(Default)]
struct Mod;
#[mod_api::horizon_mod]
impl Hooks for Mod {
    fn client_tick_pre(&self, host: &mut dyn ClientHost, _: &mut ClientTickPreEvent) {
        if let Some(names) = host.setting("baritone", "wp_names") {
            *WAYPOINTS.lock().unwrap_or_else(|e| e.into_inner()) = names
                .split(',')
                .filter(|n| !n.is_empty())
                .map(str::to_owned)
                .collect();
        }
    }

    fn register(&self, reg: &mut dyn Registrar, _: &RegistrationContext) {
        reg.register_command_argument("blocks", Blocks);
        reg.register_command_argument("waypoint", Waypoint);
        reg.register_command_argument("entity", word_arg(ENTITY_NAMES));
        reg.register_command_argument("setting", word_arg(SETTING_NAMES));
        reg.register_command_argument(
            "click_button",
            ChoiceArgument::new(["left", "right"]),
        );
        reg.register_command_argument(
            "bool_value",
            ChoiceArgument::new(["on", "off", "true", "false"]),
        );
        reg.register_command_argument(
            "explore_action",
            ChoiceArgument::new(["save", "clear", "count"]),
        );
        reg.register_command_argument(
            "dir",
            ChoiceArgument::new(["n", "s", "e", "w", "u", "d", "north", "south", "east", "west", "up", "down"]),
        );
        reg.register_command_argument(
            "render_target",
            ChoiceArgument::new(["path", "goal", "waypoints"]),
        );

        // The buttons of the old settings page, as specs — the runtime
        // re-reads the files every tick, so toggling applies live.
        for spec in [
            SettingSpec::toggle("sprint", "baritone.setting.sprint", true),
            SettingSpec::toggle("allow_break", "baritone.setting.allow_break", true),
            SettingSpec::toggle("allow_place", "baritone.setting.allow_place", true),
            SettingSpec::toggle("allow_parkour", "baritone.setting.allow_parkour", true),
            SettingSpec::range("max_drop", "baritone.setting.max_drop", 2.0, 8.0, 1.0, 4.0),
            SettingSpec::range("mine_radius", "baritone.setting.mine_radius", 8.0, 96.0, 8.0, 32.0),
            SettingSpec::toggle("mine_scan_drops", "baritone.setting.mine_scan_drops", true),
            SettingSpec::toggle("exposed_ores", "baritone.setting.exposed_ores", false),
            SettingSpec::range("follow_radius", "baritone.setting.follow_radius", 0.0, 32.0, 1.0, 3.0),
            SettingSpec::toggle("legit_mine", "baritone.setting.legit_mine", false),
            SettingSpec::toggle("assume_water", "baritone.setting.assume_water", false),
            SettingSpec::toggle("assume_lava", "baritone.setting.assume_lava", false),
            SettingSpec::range("mob_avoid_r", "baritone.setting.mob_avoid_r", 0.0, 32.0, 1.0, 0.0),
            SettingSpec::range("mob_avoid_c", "baritone.setting.mob_avoid_c", 0.0, 100.0, 0.5, 1.5),
            SettingSpec::toggle("parkour_place", "baritone.setting.parkour_place", false),
            SettingSpec::toggle("downward", "baritone.setting.downward", true),
            SettingSpec::toggle("vines", "baritone.setting.vines", true),
            SettingSpec::toggle("diag_ascend", "baritone.setting.diag_ascend", false),
            SettingSpec::toggle("diag_descend", "baritone.setting.diag_descend", false),
            SettingSpec::toggle("assume_step", "baritone.setting.assume_step", false),
            SettingSpec::toggle("sprint_water", "baritone.setting.sprint_water", true),
            SettingSpec::toggle("tool_select", "baritone.setting.tool_select", true),
            SettingSpec::toggle("bottom_slab", "baritone.setting.bottom_slab", true),
            SettingSpec::toggle("render_path", "baritone.setting.render_path", true),
            SettingSpec::toggle("render_goal", "baritone.setting.render_goal", true),
            SettingSpec::toggle("render_waypoints", "baritone.setting.render_waypoints", true),
        ] {
            let _ = reg.register_setting(spec);
        }
    }
}
