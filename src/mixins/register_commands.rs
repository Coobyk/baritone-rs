fn baritone_register_commands(
    game: &mut Game<'_>,
    commands: &mut Vec<mod_api::Command>,
) {
    crate::game::umbx_baritone_glue::register_commands(game, commands)
}
