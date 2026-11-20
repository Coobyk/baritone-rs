fn baritone_client_commands(
    game: &mut Game<'_>,
    commands: &mut Vec<mod_api::Command>,
) {
    crate::game::umbx_baritone_glue::client_commands(game, commands)
}
