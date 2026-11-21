//! Wire types for protocol `mechfight/1` (protocol.md §6). Every field has a default so
//! a missing or added field never fails a parse; the documented defaults are used when
//! the server omits a number.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type V2 = [f32; 2];
pub type V3 = [f32; 3];

// ------------------------------------------------------------------ hello / config

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Hello {
    pub protocol: String,
    pub slot: i64,
    pub seed: u64,
    pub map: MapRef,
    pub config: Config,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct MapRef {
    pub dir: String,
    pub files: Vec<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Config {
    #[serde(rename = "match")]
    pub match_: MatchCfg,
    pub bot: BotCfg,
    pub mech: MechCfg,
    pub vision: VisionCfg,
    pub sound: SoundCfg,
    pub weapons: WeaponsCfg,
    pub cubes: CubesCfg,
    pub crates: CratesCfg,
    pub loot: LootCfg,
    pub npc: NpcCfg,
    pub drops: DropsCfg,
    pub zone: ZoneCfg,
    pub upgrades: Vec<UpgradeDef>,
}

macro_rules! cfg_struct {
    ($name:ident { $($field:ident : $ty:ty = $def:expr),* $(,)? }) => {
        #[derive(Deserialize, Debug, Clone)]
        #[serde(default)]
        pub struct $name { $(pub $field: $ty),* }
        impl Default for $name {
            fn default() -> Self { $name { $($field: $def),* } }
        }
    };
}

cfg_struct!(MatchCfg { tick_hz: f32 = 20.0, decide_every: f32 = 2.0, duration_s: f32 = 480.0, mechs: f32 = 16.0 });
cfg_struct!(BotCfg { tick_budget_ms: f32 = 50.0, hello_budget_ms: f32 = 20000.0, coast_s: f32 = 1.0, offer_timeout_s: f32 = 5.0, max_line_bytes: f32 = 65536.0 });
cfg_struct!(MechCfg {
    radius: f32 = 2.5, height: f32 = 9.0, eye_height: f32 = 8.0, muzzle_height: f32 = 7.0, hp: f32 = 1400.0,
    walk_speed: f32 = 11.7, boost_speed: f32 = 19.5, ground_accel: f32 = 58.5, air_accel: f32 = 12.0, gravity: f32 = 22.0,
    jump_speed: f32 = 11.0, jet_accel: f32 = 34.0, max_rise_speed: f32 = 10.0, step_up: f32 = 1.2, max_slope_deg: f32 = 50.0,
    energy: f32 = 100.0, energy_regen: f32 = 18.0, energy_regen_delay: f32 = 0.6, boost_drain: f32 = 17.0, jet_drain: f32 = 15.0,
    turn_rate: f32 = 4.0, pitch_rate: f32 = 3.0, min_pitch: f32 = -0.7, max_pitch: f32 = 1.3, regen_delay: f32 = 5.0,
    regen_per_s: f32 = 45.0, charge_move_mult: f32 = 0.5, self_damage_mult: f32 = 0.5, armor: f32 = 0.0, rear_mult: f32 = 1.5,
    front_arc_deg: f32 = 180.0, near_range_bonus: f32 = 0.0, vision_bonus: f32 = 0.0, hearing: f32 = 1.0,
});
cfg_struct!(VisionCfg { range: f32 = 200.0, fov_deg: f32 = 120.0, near_range: f32 = 45.0, canopy_blocks: bool = true, show_enemy_hp: bool = true, memory_s: f32 = 5.0 });
cfg_struct!(SoundCfg { bearing_noise_deg: f32 = 7.0, near: f32 = 60.0, mid: f32 = 160.0, step_speed: f32 = 5.0 });
cfg_struct!(CubesCfg { pickup_radius: f32 = 3.5, hp_per_cube: f32 = 0.04, damage_per_cube: f32 = 0.04, stat_cap: f32 = 10.0, death_drop_extra: f32 = 2.0, size_per_cube: f32 = 0.05 });
cfg_struct!(CratesCfg { count: f32 = 36.0, hp: f32 = 300.0, radius: f32 = 1.4, height: f32 = 2.4, cubes: f32 = 1.0 });
cfg_struct!(LootCfg { pickup_radius: f32 = 4.0, equip_s: f32 = 1.0, crate_weapon_chance: f32 = 0.5 });
cfg_struct!(NpcCfg {
    count: f32 = 12.0, hp: f32 = 350.0, radius: f32 = 1.8, height: f32 = 3.5, speed: f32 = 5.0, leash: f32 = 30.0, chase_leash: f32 = 60.0,
    aggro_range: f32 = 45.0, deaggro_range: f32 = 90.0, damage: f32 = 25.0, cooldown: f32 = 1.0, projectile_speed: f32 = 90.0, cubes: f32 = 2.0,
});
cfg_struct!(DropsCfg { fall_s: f32 = 25.0, radius: f32 = 8.0, capture_s: f32 = 4.0, cubes: f32 = 3.0 });
cfg_struct!(ZoneCfg { start_center: V2 = [41.3, -72.8], start_radius: f32 = 560.0, phases: Vec<ZonePhase> = Vec::new() });
cfg_struct!(ZonePhase { hold_s: f32 = 0.0, shrink_s: f32 = 0.0, radius: f32 = 0.0, dps: f32 = 0.0 });

/// Weapon numbers. One flat struct for all four kinds; fields a kind does not use are 0.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct WStats {
    pub damage: f32,
    pub cooldown: f32,
    pub speed: f32,
    pub range: f32,
    pub spread_deg: f32,
    pub mag: f32,
    pub reload: f32,
    pub splash: f32,
    pub splash_radius: f32,
    pub splash_edge: f32,
    pub gravity: f32,
    pub ttl: f32,
    pub charge: f32,
    pub min_charge: f32,
    pub min_frac: f32,
    pub hold_s: f32,
    pub cookoff: f32,
    pub cone_deg: f32,
    pub chain: f32,
    pub chain_range: f32,
    pub energy: f32,
    pub short_s: f32,
    pub drain: f32,
}

impl WStats {
    /// Documented defaults (protocol.md §7), used only if the server sends nothing.
    pub fn default_for(kind: &str) -> WStats {
        let z = WStats::default();
        match kind {
            "gun" => WStats { damage: 45.0, cooldown: 0.24, speed: 252.0, range: 230.0, spread_deg: 0.6, mag: 12.0, reload: 2.4, ..z },
            "mortar" => WStats { damage: 200.0, splash: 160.0, splash_radius: 7.0, splash_edge: 0.3, cooldown: 2.6, speed: 72.0, gravity: 22.0, ttl: 7.0, drain: 45.0, ..z },
            "laser" => WStats { damage: 315.0, charge: 0.75, min_charge: 0.15, min_frac: 0.35, hold_s: 2.0, cooldown: 1.65, range: 280.0, cookoff: 0.6, ..z },
            "lightning" => WStats { damage: 60.0, cooldown: 0.25, range: 45.0, cone_deg: 30.0, chain: 2.0, chain_range: 14.0, energy: 3.0, short_s: 2.0, ..z },
            _ => z,
        }
    }
    /// Fill in defaults when the essential numbers are missing.
    pub fn or_default(self, kind: &str) -> WStats {
        let bad = match kind {
            "gun" => self.speed <= 0.0 || self.damage <= 0.0,
            "mortar" => self.speed <= 0.0 || self.gravity <= 0.0,
            "laser" => self.range <= 0.0 || self.damage <= 0.0,
            "lightning" => self.range <= 0.0 || self.damage <= 0.0,
            _ => false,
        };
        if bad { WStats::default_for(kind) } else { self }
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct WeaponsCfg {
    pub gun: WStats,
    pub mortar: WStats,
    pub laser: WStats,
    pub lightning: WStats,
}
impl WeaponsCfg {
    pub fn get(&self, kind: &str) -> WStats {
        match kind {
            "gun" => self.gun.clone().or_default(kind),
            "mortar" => self.mortar.clone().or_default(kind),
            "laser" => self.laser.clone().or_default(kind),
            "lightning" => self.lightning.clone().or_default(kind),
            _ => WStats::default(),
        }
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct UpgradeDef {
    pub id: String,
    pub requires: Option<String>,
    pub max: f32,
}

// ------------------------------------------------------------------ observation

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Obs {
    pub tick: f64,
    pub t: f32,
    pub alive: i32,
    #[serde(rename = "self")]
    pub me: SelfObs,
    pub mechs: Vec<ActorObs>,
    pub projectiles: Vec<ProjObs>,
    pub crates: Vec<CrateObs>,
    pub cubes: Vec<ItemObs>,
    pub pickups: Vec<PickupObs>,
    pub sounds: Vec<SoundObs>,
    pub beams: Vec<BeamObs>,
    pub events: Vec<Event>,
    pub drops: Vec<DropObs>,
    pub zone: ZoneObs,
    pub offer: Option<Offer>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct SelfObs {
    pub id: i64,
    pub pos: V3,
    pub vel: V3,
    pub aim: V2,
    pub hp: f32,
    pub max_hp: f32,
    pub energy: f32,
    pub max_energy: f32,
    pub grounded: bool,
    pub cubes: f32,
    pub in_zone: bool,
    pub weapons: Vec<Option<SelfWeapon>>,
    pub stats: MechCfg,
}
impl Default for SelfObs {
    fn default() -> Self {
        SelfObs {
            id: -1, pos: [0.0; 3], vel: [0.0; 3], aim: [0.0; 2], hp: 1400.0, max_hp: 1400.0, energy: 100.0, max_energy: 100.0,
            grounded: true, cubes: 0.0, in_zone: true, weapons: Vec::new(), stats: MechCfg::default(),
        }
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct SelfWeapon {
    pub kind: String,
    pub cooldown: f32,
    pub ammo: Option<f32>,
    pub reload: f32,
    pub charge: f32,
    pub stats: WStats,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct ActorObs {
    pub id: i64,
    pub kind: String,
    pub pos: V3,
    pub vel: V3,
    pub aim: V2,
    pub hp: Option<f32>,
    pub max_hp: Option<f32>,
    pub cubes: f32,
    pub weapons: Vec<String>,
    pub charging: bool,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct ProjObs {
    pub id: i64,
    pub kind: String,
    pub pos: V3,
    pub vel: V3,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CrateObs {
    pub id: i64,
    pub pos: V3,
    pub hp: f32,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct ItemObs {
    pub id: i64,
    pub pos: V3,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct PickupObs {
    pub id: i64,
    pub kind: String,
    pub pos: V3,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct SoundObs {
    pub kind: String,
    pub bearing: f32,
    pub dist: String,
    pub age: f32,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct BeamObs {
    pub by: Option<i64>,
    pub kind: String,
    pub path: Vec<V3>,
    pub age: f32,
}

/// All event kinds share one struct; unused fields stay `None`.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Event {
    #[serde(rename = "type")]
    pub kind: String,
    pub amount: Option<f32>,
    pub weapon: Option<String>,
    pub bearing: Option<f32>,
    pub by: Option<i64>,
    pub rear: Option<bool>,
    pub target: Option<i64>,
    pub effect: Option<String>,
    pub id: Option<Value>,
    pub cubes: Option<f32>,
    pub reason: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct DropObs {
    pub id: i64,
    pub pos: V3,
    pub radius: f32,
    pub lands_in: f32,
    pub capture_s: f32,
    pub progress: f32,
    pub holder: Option<i64>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct ZoneObs {
    pub center: V2,
    pub radius: f32,
    pub next_center: V2,
    pub next_radius: f32,
    pub shrink_start: f32,
    pub shrink_end: f32,
    pub dps: f32,
}
impl Default for ZoneObs {
    fn default() -> Self {
        ZoneObs { center: [41.3, -72.8], radius: 560.0, next_center: [41.3, -72.8], next_radius: 560.0, shrink_start: 1e9, shrink_end: 1e9, dps: 0.0 }
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Offer {
    pub id: f64,
    pub expires: f32,
    pub choices: Vec<OfferChoice>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct OfferChoice {
    pub id: String,
}

/// The few fields the I/O loop needs before the full parse.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Envelope {
    #[serde(rename = "type")]
    pub kind: String,
    pub tick: Option<f64>,
}

// ------------------------------------------------------------------ action

#[derive(Serialize, Debug, Clone, Default)]
pub struct Action {
    pub tick: i64,
    #[serde(rename = "move")]
    pub mv: V2,
    pub boost: bool,
    pub jump: bool,
    pub aim: V2,
    pub fire: [bool; 2],
    pub reload: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pick: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub take: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drop: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<DebugDraw>,
}

#[derive(Serialize, Debug, Clone, Default)]
pub struct DebugDraw {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<DebugLine>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<DebugPoint>,
}
#[derive(Serialize, Debug, Clone)]
pub struct DebugLine {
    pub a: V3,
    pub b: V3,
    pub color: &'static str,
}
#[derive(Serialize, Debug, Clone)]
pub struct DebugPoint {
    pub at: V3,
    pub color: &'static str,
    pub size: f32,
}

impl Action {
    /// Replace anything non-finite and clamp the move, so the line is always schema-valid.
    pub fn sanitize(&mut self) {
        for v in self.mv.iter_mut().chain(self.aim.iter_mut()) {
            if !v.is_finite() {
                *v = 0.0;
            }
        }
        let l = (self.mv[0] * self.mv[0] + self.mv[1] * self.mv[1]).sqrt();
        if l > 1.0 {
            self.mv = [self.mv[0] / l, self.mv[1] / l];
        }
        let r = |v: f32| (v * 10000.0).round() / 10000.0;
        self.mv = [r(self.mv[0]), r(self.mv[1])];
        self.aim = [r(self.aim[0]), r(self.aim[1])];
    }
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| format!("{{\"tick\":{}}}", self.tick))
    }
}
