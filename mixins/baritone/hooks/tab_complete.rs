fn baritone_tab_complete(
    game: &mut crate::game::Game<'_>,
    player: u128,
    name: &str,
    label: &str,
    arguments: &[String],
    range: [usize; 2],
    suggestions: &mut Vec<crate::mod_api::Suggestion>,
) {
    crate::game::umbx_baritone_glue::tab_complete(
        game,
        player,
        name,
        label,
        arguments,
        range,
        suggestions,
    )
}
