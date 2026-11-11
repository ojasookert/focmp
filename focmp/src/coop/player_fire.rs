//! Remote player flames use native OnFire presentation, never its damage tick.
use crate::engine::{G, V, is_struct, real};
use crate::net::{MAX_PLAYERS, PeerId};

const ROOTS: &str = "focmp_player_fires";

pub fn update(g: G, peer: PeerId, anchor: V, burning: bool) {
    if !burning || g.first("objParticles").is_none() {
        clear(g, peer);
        return;
    }
    // Cosmetic particles must not advance the local gameplay RNG.
    let _rng = super::rng::Scope::preserve();
    let mut roots = g.global(ROOTS);
    if roots.kind() != crate::gm::KIND_ARRAY || g.arr_len(roots) != MAX_PLAYERS {
        roots = g.call("array_create", &[real(MAX_PLAYERS as f64), V::undefined()]);
        g.call("variable_global_set", &[g.s(ROOTS), roots]);
    }
    let index = real(peer as f64);
    let mut fire = g.arr_get(roots, peer as usize);
    if !is_struct(&fire) {
        fire = g.call("@@NewGMLObject@@", &[g.asset("OnFire"), g.get(anchor, "x"), g.get(anchor, "y")]);
        if !is_struct(&fire) { return; }
        // Keep a native GC root until cleanup, even after a room change has
        // destroyed the anchor. Rust values alone do not keep structs alive.
        g.call("array_set", &[roots, index, fire]);
        let update = g.call("method", &[fire, g.get(fire, "update")]);
        g.set(fire, "focmp_update", update);
        g.set(fire, "focmp_args", g.call("array_create", &[real(3.0)]));
    }
    let args = g.get(fire, "focmp_args");
    for (i, field) in ["x", "y", "image_angle"].into_iter().enumerate() {
        g.call("array_set", &[args, real(i as f64), g.get(anchor, field)]);
    }
    // update emits the native flame trail. do_damage and Player.Step remain
    // exclusively owned by the originating player.
    g.call("method_call", &[g.get(fire, "focmp_update"), args]);
}

pub fn clear(g: G, peer: PeerId) {
    let roots = g.global(ROOTS);
    if roots.kind() != crate::gm::KIND_ARRAY || g.arr_len(roots) != MAX_PLAYERS { return; }
    let fire = g.arr_get(roots, peer as usize);
    if !is_struct(&fire) { return; }
    // Native destroy releases the loop sound; emitted particles expire normally.
    let destroy = g.call("method", &[fire, g.get(fire, "destroy")]);
    g.call("method_call", &[destroy, g.call("array_create", &[real(0.0)])]);
    g.call("array_set", &[roots, real(peer as f64), V::undefined()]);
}
