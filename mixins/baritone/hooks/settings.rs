fn baritone_page(game: &crate::game::Game<'_>, mod_id: &str, buttons: &mut Vec<String>) {
    crate::game::umbx_baritone_glue::page(game, mod_id, buttons)
}
