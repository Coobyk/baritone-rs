fn baritone_command(game: &mut crate::game::Game<'_>, command: &str, ran: &mut bool) {
    crate::game::umbx_baritone_glue::command(game, command, ran)
}
