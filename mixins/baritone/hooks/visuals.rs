fn baritone_visuals(
    game: &crate::game::Game<'_>,
    rims: &mut Vec<crate::game::native_hooks::NativeBlockRim>,
    lights: &mut Vec<crate::game::native_hooks::NativeColoredLight>,
) {
    crate::game::umbx_baritone_render::visuals(game, rims, lights)
}
