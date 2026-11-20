fn baritone_world(
    game: &Game<'_>,
    view: &mod_api::WorldRenderView,
    draw: &mut horizon_client::world_geometry::WorldGeometry,
) {
    crate::game::umbx_baritone_render::world(game, view, draw)
}
