//! Baritone's own crate: settings page (`Options > Mods > Baritone`) and
//! the `baritone:*` command argument types the `/b` usages name. The game
//! itself runs the injected modules under `src/` (`[[native.module]]`), so
//! the heavy logic lives there; this crate registers what the host asks.

use std::sync::Mutex;

use mod_api::{
    ChoiceArgument, ClientHost, ClientTickPreEvent, CommandArgumentKind, CommandArgumentType, CommandSender,
    CommandSuggestion, Hooks, Registrar, RegistrationContext, SettingSpec,
};

// The completion-name tables and the settings table are also injected
// into the game as `umbx_baritone_names`/`umbx_baritone_settings`
// (single source for both sides).
include!("names.rs");
include!("settings_table.rs");

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

/// `baritone:setting`: every name `/b set` accepts — keys, display names
/// and aliases from the shared `SETTINGS` table, so completion can never
/// drift from the game-side parser.
struct SettingArg;
impl CommandArgumentType for SettingArg {
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
        let mut out: Vec<&'static str> = Vec::new();
        for s in SETTINGS {
            for c in [s.key, s.display]
                .into_iter()
                .chain(s.aliases.iter().copied())
            {
                if c.to_ascii_lowercase().starts_with(&p) && !out.contains(&c) {
                    out.push(c);
                }
            }
        }
        out.into_iter().map(CommandSuggestion::new).collect()
    }
}

/// `baritone:schematic`: `.schematic` files under `schematics/` in the
/// dirs the game-side loader reads (cwd, `$HORIZON_HOME`,
/// `~/.local/share/horizon`).
struct Schematic;
impl CommandArgumentType for Schematic {
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
        let roots = [
            std::env::current_dir().ok(),
            std::env::var_os("HORIZON_HOME").map(std::path::PathBuf::from),
            std::env::var_os("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".local/share/horizon")),
        ];
        let mut out: Vec<String> = Vec::new();
        for root in roots.into_iter().flatten() {
            let Ok(dir) = std::fs::read_dir(root.join("schematics")) else {
                continue;
            };
            for e in dir.flatten() {
                let Some(name) = e.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if let Some(base) = name.strip_suffix(".schematic") {
                    if base.to_ascii_lowercase().starts_with(&p) && !out.iter().any(|o| o == base) {
                        out.push(base.to_owned());
                    }
                }
            }
        }
        out.sort();
        out.into_iter().map(CommandSuggestion::new).collect()
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
        reg.register_command_argument("setting", SettingArg);
        reg.register_command_argument("schematic", Schematic);
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
        reg.register_command_argument("dir", word_arg(SEL_DIRS));
        reg.register_command_argument(
            "render_target",
            ChoiceArgument::new(["path", "goal", "waypoints"]),
        );

        // One spec per row of the shared table — the runtime re-reads
        // the persisted values every tick, so toggling applies live.
        // The render_* toggles live in the render module, not rt::Settings.
        for s in SETTINGS {
            let spec = if s.toggle {
                SettingSpec::toggle(s.key, s.label, s.default == "true")
            } else {
                SettingSpec::range(
                    s.key,
                    s.label,
                    s.min,
                    s.max,
                    s.step,
                    s.default.parse().unwrap_or(0.0),
                )
            };
            let _ = reg.register_setting(spec);
        }
        for spec in [
            SettingSpec::toggle("render_path", "baritone.setting.render_path", true),
            SettingSpec::toggle("render_goal", "baritone.setting.render_goal", true),
            SettingSpec::toggle("render_waypoints", "baritone.setting.render_waypoints", true),
        ] {
            let _ = reg.register_setting(spec);
        }
    }
}
