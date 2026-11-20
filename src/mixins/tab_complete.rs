fn baritone_tab_complete(
    game: &mut Game<'_>,
    player: u128,
    name: &str,
    command: &str,
    range: [usize; 2],
    suggestions: &mut Vec<Suggestion>,
) {
    crate::game::umbx_baritone_glue::tab_complete(game, player, name, command, range, suggestions)
}
