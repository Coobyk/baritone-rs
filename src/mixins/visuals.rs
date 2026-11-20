fn baritone_visuals(
    game: &Game<'_>,
    rims: &mut Vec<mod_api::BlockRim>,
    lights: &mut Vec<mod_api::ColoredLight>,
) {
    crate::game::umbx_baritone_render::visuals(game, rims, lights)
}
