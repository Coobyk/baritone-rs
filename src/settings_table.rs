// The one settings table, shared by the mod crate (`include!`d into
// lib.rs for the options-page SettingSpecs and the `baritone:setting`
// argument) and the game-side modules (injected as `umbx_baritone_settings`
// — load/to_save/`/b set`/`/b reset`/`/b modified`/completion all derive
// from it so no copy can drift). Keep this file free of `use` lines and
// crate paths so it stays valid in both contexts.

/// One persisted setting.
///
/// `key` is the string `native_setting` stores under; `label` the lang
/// key the options page resolves (`baritone.setting.<key>`, see
/// `assets/baritone/lang`); `display` the Baritone-style name `/b set`
/// and `/b modified` print; `aliases` the other `/b set` names accepted
/// (all name matching is case-insensitive); `default` the value in its
/// persisted string form; `min`/`max` the load/set clamp and `step` the
/// options-page slider step — all three unused for toggles.
pub struct Setting {
    pub key: &'static str,
    pub label: &'static str,
    pub display: &'static str,
    pub aliases: &'static [&'static str],
    pub default: &'static str,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub toggle: bool,
}

pub const SETTINGS: &[Setting] = &[
    Setting { key: "sprint", label: "baritone.setting.sprint", display: "sprint",
        aliases: &["allowSprint"], default: "true",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "allow_break", label: "baritone.setting.allow_break", display: "allowBreak",
        aliases: &["break"], default: "true",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "allow_place", label: "baritone.setting.allow_place", display: "allowPlace",
        aliases: &["place"], default: "true",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "allow_parkour", label: "baritone.setting.allow_parkour", display: "allowParkour",
        aliases: &["parkour"], default: "true",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "max_drop", label: "baritone.setting.max_drop", display: "maxDrop",
        aliases: &["drop", "maxFallHeight", "maxFallHeightNoWater", "fallHeight"],
        default: "4", min: 1.0, max: 20.0, step: 1.0, toggle: false },
    Setting { key: "mine_radius", label: "baritone.setting.mine_radius", display: "mineRadius",
        aliases: &["radius"], default: "32", min: 8.0, max: 96.0, step: 8.0, toggle: false },
    Setting { key: "mine_scan_drops", label: "baritone.setting.mine_scan_drops", display: "mineScanDroppedItems",
        aliases: &["drops"], default: "true",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "exposed_ores", label: "baritone.setting.exposed_ores", display: "onlyExposedOres",
        aliases: &["exposed", "onlyExposed", "exposedOres", "allowOnlyExposedOres"],
        default: "false", min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "follow_radius", label: "baritone.setting.follow_radius", display: "followRadius",
        aliases: &["followR"], default: "3", min: 0.0, max: 32.0, step: 1.0, toggle: false },
    Setting { key: "legit_mine", label: "baritone.setting.legit_mine", display: "legitMine",
        aliases: &["legit"], default: "false",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "assume_water", label: "baritone.setting.assume_water", display: "assumeWalkOnWater",
        aliases: &["assumeWater", "water"], default: "false",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "assume_lava", label: "baritone.setting.assume_lava", display: "assumeWalkOnLava",
        aliases: &["assumeLava", "lava"], default: "false",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "mob_avoid_r", label: "baritone.setting.mob_avoid_r", display: "mobAvoidanceRadius",
        aliases: &["mobAvoidR", "mobR"], default: "0", min: 0.0, max: 32.0, step: 1.0, toggle: false },
    Setting { key: "mob_avoid_c", label: "baritone.setting.mob_avoid_c", display: "mobAvoidanceCoefficient",
        aliases: &["mobAvoidC", "mobC"], default: "1.5", min: 0.0, max: 100.0, step: 0.5, toggle: false },
    Setting { key: "parkour_place", label: "baritone.setting.parkour_place", display: "allowParkourPlace",
        aliases: &["parkourPlace"], default: "false",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "downward", label: "baritone.setting.downward", display: "allowDownward",
        aliases: &["downward", "down"], default: "true",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "vines", label: "baritone.setting.vines", display: "allowVines",
        aliases: &[], default: "true",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "diag_ascend", label: "baritone.setting.diag_ascend", display: "allowDiagonalAscend",
        aliases: &["diagonalAscend", "diagAscend", "dAsc"], default: "false",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "diag_descend", label: "baritone.setting.diag_descend", display: "allowDiagonalDescend",
        aliases: &["diagonalDescend", "diagDescend", "dDesc"], default: "false",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "assume_step", label: "baritone.setting.assume_step", display: "assumeStep",
        aliases: &["step"], default: "false",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "sprint_water", label: "baritone.setting.sprint_water", display: "sprintInWater",
        aliases: &["sprintWater", "sprintW"], default: "true",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "tool_select", label: "baritone.setting.tool_select", display: "toolSelect",
        aliases: &["autoTool", "tool"], default: "true",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
    Setting { key: "bottom_slab", label: "baritone.setting.bottom_slab", display: "allowWalkOnBottomSlab",
        aliases: &["bottomSlab", "slab"], default: "true",
        min: 0.0, max: 0.0, step: 0.0, toggle: true },
];
