fn baritone_clicked(game: &mut crate::game::Game<'_>, mod_id: &str, button: usize) {
    crate::game::umbx_baritone_glue::click(game, mod_id, button)
}
