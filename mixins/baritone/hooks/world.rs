fn baritone_world(
    game: &crate::game::Game<'_>,
    draw: &mut crate::mod_api::NativeWorldDraw,
) {
    crate::game::umbx_baritone_render::world(game, draw)
}
