//! Mirror native light objects, retaining the game's shadow/occlusion renderer.
use crate::engine::{G, V, asset_id, boolean, is_struct, real, truthy};
use bitcode::{Decode, Encode};

#[derive(Clone, Debug, Default, PartialEq, Encode, Decode)]
pub struct Light {
    pub slot: u8,
    pub sprite: i32,
    pub x: f32,
    pub y: f32,
    pub angle: f32,
    pub xs: f32,
    pub ys: f32,
    pub alpha: f32,
    pub image: f32,
    pub blend: u32,
    pub shadows: bool,
    pub visible: bool,
}
impl Light {
    pub fn sane(&self) -> bool {
        self.slot < 6
            && (0..32768).contains(&self.sprite)
            && self.blend <= 0xffffff
            && [
                self.x, self.y, self.angle, self.xs, self.ys, self.alpha, self.image,
            ]
            .iter()
            .all(|n| n.is_finite() && n.abs() < 1.0e7)
    }
}
const FIELDS: [&str; 6] = [
    "flashlight",
    "body_light",
    "eyes_light",
    "muzzle_flash_light",
    "muzzle_flash_light_core",
    "laser_sight_light",
];

pub fn capture(g: G, player: V) -> Vec<Light> {
    let gun = g.get(player, "Gun");
    FIELDS
        .iter()
        .enumerate()
        .filter_map(|(slot, name)| {
            if slot >= 3 && !g.exists(gun) {
                return None;
            }
            let light = g.get(if slot < 3 { player } else { gun }, name);
            if !is_struct(&light) || truthy(&g.get(light, "__destroyed")) {
                return None;
            }
            let f = |name| g.num(light, name) as f32;
            Some(Light {
                slot: slot as u8,
                sprite: asset_id(g.get(light, "sprite")),
                x: f("x"),
                y: f("y"),
                angle: f("angle"),
                xs: f("xscale"),
                ys: f("yscale"),
                alpha: f("alpha"),
                image: f("image"),
                blend: g.num(light, "blend") as u32,
                shadows: truthy(&g.get(light, "castShadows")),
                visible: truthy(&g.get(light, "visible")),
            })
        })
        .filter(Light::sane)
        .collect()
}

pub fn apply(g: G, anchor: V, states: &[Light], dx: f64, dy: f64) {
    let Some(player) = g.first("objPlayer") else {
        return;
    };
    let Some(controller) = g.first("objLightController") else {
        return;
    };
    for slot in 0..6 {
        let name = format!("focmp_light_{slot}");
        let mut light = g.get(anchor, &name);
        let state = states.iter().find(|s| s.slot == slot);
        let Some(state) = state else {
            if is_struct(&light) {
                g.set(light, "visible", boolean(false));
            }
            continue;
        };
        if !is_struct(&light) {
            // Flash/muzzle/laser lights share BulbLight; sprite overlays retain
            // their own subclass and renderer registration method.
            let template = g.get(
                player,
                if slot == 1 {
                    "body_light"
                } else if slot == 2 {
                    "eyes_light"
                } else {
                    "flashlight"
                },
            );
            if !is_struct(&template) {
                continue;
            }
            light = g.call("variable_clone", &[template]);
            g.set(light, "__destroyed", boolean(false));
            g.set(light, "surf", real(-4.0));
            g.set(light, "__oldSprite", real(-4.0));
            g.set(anchor, &name, light); // strong root; renderer holds only weak refs
            let add = g.get(light, "AddToRenderer");
            let method = g.call("method", &[light, add]);
            let args = g.call("array_create", &[real(1.0), g.get(controller, "renderer")]);
            g.call("method_call", &[method, args]);
        }
        g.set(light, "sprite", real(state.sprite as f64));
        for (name, value) in [
            ("x", state.x as f64 + dx),
            ("y", state.y as f64 + dy),
            ("angle", state.angle as f64),
            ("xscale", state.xs as f64),
            ("yscale", state.ys as f64),
            ("alpha", state.alpha as f64),
            ("image", state.image as f64),
            ("blend", state.blend as f64),
        ] {
            g.set(light, name, real(value));
        }
        g.set(light, "castShadows", boolean(state.shadows));
        g.set(light, "visible", boolean(state.visible));
    }
}
pub fn destroy(g: G, anchor: V) {
    for slot in 0..6 {
        let light = g.get(anchor, &format!("focmp_light_{slot}"));
        if is_struct(&light) {
            g.set(light, "visible", boolean(false));
            g.set(light, "__destroyed", boolean(true));
        }
    }
}
