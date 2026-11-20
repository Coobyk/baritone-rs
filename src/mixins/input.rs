fn baritone_input(game: &Game<'_>, call: mod_api::__private::MovementInputCall<'_>) {
    let mut call = call;
    crate::game::umbx_baritone_glue::movement_input(game, &mut call);
    game.fire_movement_input(call);
}
