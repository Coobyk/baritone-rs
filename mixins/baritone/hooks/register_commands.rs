fn baritone_register_commands(
    game: &mut crate::game::Game<'_>,
    commands: &mut Vec<crate::mod_api::NativeCommand>,
) {
    crate::game::umbx_baritone_glue::register_commands(game, commands)
}
