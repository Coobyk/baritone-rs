// Completion-name tables shared by the game-side modules (injected as the
// `umbx_baritone_names` module) and the mod crate (`include!`d into lib.rs
// for the `baritone:*` command argument types). Keep this file free of
// `use` lines and crate paths so it stays valid in both contexts.

/// Block names `/b mine`, `find`, `explore`, `blacklist`, `avoid` and
/// `sel fill` complete. The block registry isn't enumerable through the
/// mod surface, so this is the common set — every name the commands accept
/// still parses whether it's listed or not.
pub const BLOCK_NAMES: &[&str] = &[
    "air", "stone", "granite", "diorite", "andesite", "deepslate", "tuff", "dirt",
    "grass_block", "dirt_path", "farmland", "sand", "red_sand", "gravel", "clay", "mud",
    "cobblestone", "mossy_cobblestone", "stone_bricks", "bricks", "bedrock", "obsidian",
    "coal_ore", "iron_ore", "copper_ore", "gold_ore", "lapis_ore", "redstone_ore",
    "diamond_ore", "emerald_ore",
    "deepslate_coal_ore", "deepslate_iron_ore", "deepslate_copper_ore",
    "deepslate_gold_ore", "deepslate_lapis_ore", "deepslate_redstone_ore",
    "deepslate_diamond_ore", "deepslate_emerald_ore",
    "nether_gold_ore", "nether_quartz_ore", "ancient_debris",
    "oak_log", "spruce_log", "birch_log", "jungle_log", "acacia_log",
    "dark_oak_log", "mangrove_log", "cherry_log", "pale_oak_log",
    "crimson_stem", "warped_stem",
    "oak_leaves", "spruce_leaves", "birch_leaves", "jungle_leaves",
    "acacia_leaves", "dark_oak_leaves",
    "water", "lava", "ice", "snow", "snow_block", "powder_snow",
    "glass", "cobweb", "torch", "ladder", "vine", "scaffolding",
    "wheat", "carrots", "potatoes", "beetroots", "sugar_cane", "cactus",
    "pumpkin", "melon", "sweet_berry_bush",
    "rail", "powered_rail", "detector_rail", "activator_rail",
    "netherrack", "soul_sand", "soul_soil", "basalt", "blackstone",
    "nether_bricks", "end_stone",
    "short_grass", "tall_grass", "fern", "dead_bush", "leaf_litter",
    "chest", "crafting_table", "furnace", "barrel", "spawner",
    "nether_portal", "end_portal_frame",
];

/// Entity names `/b follow entity` completes.
pub const ENTITY_NAMES: &[&str] = &[
    "player", "zombie", "skeleton", "creeper", "spider", "cave_spider", "enderman",
    "witch", "slime", "cow", "pig", "sheep", "chicken", "horse", "villager",
    "iron_golem", "wolf", "axolotl", "item",
];

/// `sel` directions (`expand`/`shift` take these).
pub const SEL_DIRS: &[&str] = &[
    "up", "down", "north", "south", "east", "west", "u", "d", "n", "s", "e", "w",
];
