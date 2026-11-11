use bitcode::{Decode, Encode};
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transport {
    Tcp,
    Steam,
}

#[derive(Clone, Copy, Debug, Encode, Decode)]
pub struct RunRules {
    pub players: u8,
    pub enemy_health_multiplier: f32,
    pub enemy_health_per_extra_player: f32,
    pub enemy_damage_multiplier: f32,
    pub enemy_damage_per_extra_player: f32,
}
impl Default for RunRules {
    fn default() -> Self {
        Self {
            players: 1,
            enemy_health_multiplier: 1.0,
            enemy_health_per_extra_player: 0.5,
            enemy_damage_multiplier: 1.0,
            enemy_damage_per_extra_player: 0.1,
        }
    }
}
impl RunRules {
    pub fn valid(&self) -> bool {
        (1..=crate::net::MAX_PLAYERS as u8).contains(&self.players)
            && self.enemy_health_multiplier.is_finite()
            && (0.1..=100.0).contains(&self.enemy_health_multiplier)
            && self.enemy_damage_multiplier.is_finite()
            && (0.1..=100.0).contains(&self.enemy_damage_multiplier)
            && self.enemy_damage_per_extra_player.is_finite()
            && (0.0..=10.0).contains(&self.enemy_damage_per_extra_player)
            && self.enemy_health_per_extra_player.is_finite()
            && (0.0..=10.0).contains(&self.enemy_health_per_extra_player)
    }
    pub fn damage_scale(&self) -> f64 {
        if self.players < 2 {
            1.0
        } else {
            (self.enemy_damage_multiplier
                * (1.0 + self.enemy_damage_per_extra_player * (self.players - 1) as f32))
                as f64
        }
    }
    pub fn health_scale(&self) -> f64 {
        if self.players < 2 {
            1.0
        } else {
            (self.enemy_health_multiplier
                * (1.0 + self.enemy_health_per_extra_player * (self.players - 1) as f32))
                as f64
        }
    }
}
pub static RUN_RULES: Mutex<RunRules> = Mutex::new(RunRules {
    players: 1,
    enemy_health_multiplier: 1.0,
    enemy_health_per_extra_player: 0.5,
    enemy_damage_multiplier: 1.0,
    enemy_damage_per_extra_player: 0.1,
});

pub struct Config {
    pub transport: Transport,
    pub rules: RunRules,
    /// The development console on 127.0.0.1 (`[debug] console = true`). Off by default: any local program, or a
    /// web page posting to it, could run its commands.
    pub debug_console: bool,
}
// One immutable snapshot for the process: menu ticks must not perform file I/O,
// and transport selection must agree with the hooks installed at startup.
static CONFIG: LazyLock<Config> = LazyLock::new(load);

pub fn get() -> &'static Config {
    &CONFIG
}

fn load() -> Config {
    let mut c = Config {
        transport: Transport::Tcp,
        rules: RunRules::default(),
        debug_console: false,
    };
    let path = format!("{}focmp.ini", crate::win::exe_dir());
    let input = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            crate::log(&format!("config: {path}: {e}; using defaults"));
            String::new()
        }
    };
    let mut section = String::new();
    for line in input.lines() {
        let line = line.split([';', '#']).next().unwrap_or("").trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        let valid = match (section.as_str(), key.as_str()) {
            ("network", "transport") => match value.to_ascii_lowercase().as_str() {
                "tcp" => {
                    c.transport = Transport::Tcp;
                    true
                }
                "steam" => {
                    c.transport = Transport::Steam;
                    true
                }
                _ => false,
            },
            ("debug", "console") => match value.to_ascii_lowercase().as_str() {
                "true" | "1" | "on" | "yes" => {
                    c.debug_console = true;
                    true
                }
                "false" | "0" | "off" | "no" => {
                    c.debug_console = false;
                    true
                }
                _ => false,
            },
            ("gameplay", "enemy_health_multiplier") => {
                set_number(value, &mut c.rules.enemy_health_multiplier, 0.1, 100.0)
            }
            ("gameplay", "enemy_health_per_extra_player") => {
                set_number(value, &mut c.rules.enemy_health_per_extra_player, 0.0, 10.0)
            }
            ("gameplay", "enemy_damage_multiplier") => {
                set_number(value, &mut c.rules.enemy_damage_multiplier, 0.1, 100.0)
            }
            ("gameplay", "enemy_damage_per_extra_player") => {
                set_number(value, &mut c.rules.enemy_damage_per_extra_player, 0.0, 10.0)
            }
            _ => false,
        };
        if !valid {
            crate::log(&format!(
                "config: ignored invalid/unknown {section}.{key}={value}"
            ));
        }
    }
    if let Ok(value) = std::env::var("FOCMP_TRANSPORT") {
        match value.as_str() {
            "tcp" => c.transport = Transport::Tcp,
            "steam" => c.transport = Transport::Steam,
            _ => crate::log("config: invalid FOCMP_TRANSPORT"),
        }
    }
    c
}
fn set_number(value: &str, target: &mut f32, min: f32, max: f32) -> bool {
    if let Ok(n) = value.parse::<f32>() {
        if n.is_finite() && (min..=max).contains(&n) {
            *target = n;
            return true;
        }
    }
    false
}
