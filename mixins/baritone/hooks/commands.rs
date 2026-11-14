fn baritone_commands(game: &crate::game::Game<'_>, commands: &mut Vec<&'static str>) {
    crate::game::umbx_baritone_glue::commands(game, commands)
}
