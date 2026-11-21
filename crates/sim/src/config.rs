//! `hello.config` as typed structs. The raw JSON is kept and sent to bots unchanged.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The live config recorded in the kit's `check/stream.jsonl.gz` hello.
pub const DEFAULT_CONFIG_JSON: &str = include_str!("../config/default.json");

#[derive(Deserialize, Clone, Debug)]
pub struct MatchCfg {
    pub tick_hz: f32,
    pub decide_every: u32,
    pub duration_s: f32,
    pub mechs: usize,
    pub spawn_min_separation: f32,
    pub spawn_zone_fraction: f32,
    #[serde(default)]
    pub arena_margin: f32,
}
#[derive(Deserialize, Clone, Debug)]
pub struct BotCfg {
    pub tick_budget_ms: f64,
    pub hello_budget_ms: f64,
    pub coast_s: f32,
    pub offer_timeout_s: f32,
    pub max_line_bytes: usize,
}
#[derive(Deserialize, Clone, Debug)]
pub struct VisionCfg {
    pub range: f32,
    pub fov_deg: f32,
    pub near_range: f32,
    pub canopy_blocks: bool,
    pub show_enemy_hp: bool,
    pub memory_s: f32,
}
#[derive(Deserialize, Clone, Debug)]
pub struct SoundCfg {
    pub bearing_noise_deg: f32,
    pub near: f32,
    pub mid: f32,
    pub step_speed: f32,
    pub step_every: f32,
    pub range: BTreeMap<String, f32>,
}
#[derive(Deserialize, Clone, Debug)]
pub struct ProjectileCfg {
    pub gun_radius: f32,
    pub npc_radius: f32,
}
#[derive(Deserialize, Clone, Debug)]
pub struct CubesCfg {
    pub pickup_radius: f32,
    pub hp_per_cube: f32,
    pub damage_per_cube: f32,
    pub stat_cap: u32,
    pub death_drop_extra: u32,
    pub size_per_cube: f32,
    pub upgrade_thresholds: Vec<u32>,
}
#[derive(Deserialize, Clone, Debug)]
pub struct CratesCfg {
    pub count: usize,
    pub roof_fraction: f32,
    pub hp: f32,
    pub radius: f32,
    pub height: f32,
    pub cubes: u32,
}
#[derive(Deserialize, Clone, Debug)]
pub struct LootCfg {
    pub pickup_radius: f32,
    pub equip_s: f32,
    pub floor_count: usize,
    pub floor_roof_fraction: f32,
    pub crate_weapon_chance: f32,
    #[serde(default = "one")]
    pub death_drop: f32,
    pub weights: BTreeMap<String, f32>,
}
fn one() -> f32 {
    1.0
}
#[derive(Deserialize, Clone, Debug)]
pub struct NpcCfg {
    pub count: usize,
    pub hp: f32,
    pub radius: f32,
    pub height: f32,
    pub speed: f32,
    pub leash: f32,
    pub chase_leash: f32,
    pub aggro_range: f32,
    pub deaggro_range: f32,
    pub damage: f32,
    pub cooldown: f32,
    pub projectile_speed: f32,
    pub spread_deg: f32,
    pub cubes: u32,
}
#[derive(Deserialize, Clone, Debug)]
pub struct DropsCfg {
    pub times: Vec<f32>,
    pub fall_s: f32,
    pub radius: f32,
    pub capture_s: f32,
    pub cubes: u32,
    #[serde(default = "one")]
    pub repair: f32,
    #[serde(default = "one")]
    pub spread: f32,
}
#[derive(Deserialize, Clone, Debug)]
pub struct ZonePhase {
    pub hold_s: f32,
    pub shrink_s: f32,
    pub radius: f32,
    pub dps: f32,
}
#[derive(Deserialize, Clone, Debug)]
pub struct ZoneCfg {
    pub start_center: [f32; 2],
    pub start_radius: f32,
    pub phases: Vec<ZonePhase>,
}
#[derive(Deserialize, Clone, Debug)]
pub struct Effect {
    pub path: String,
    pub mul: Option<f64>,
    pub add: Option<f64>,
}
#[derive(Deserialize, Clone, Debug)]
pub struct UpgradeDef {
    pub id: String,
    pub name: String,
    pub desc: String,
    pub requires: Option<String>,
    pub max: u32,
    pub effects: Vec<Effect>,
}

/// `[mech]` numbers (after upgrades; radius/height/eye/muzzle after cube scaling).
#[derive(Deserialize, Serialize, Clone, Copy, Debug)]
pub struct MechStats {
    pub radius: f32,
    pub height: f32,
    pub eye_height: f32,
    pub muzzle_height: f32,
    pub hp: f32,
    pub walk_speed: f32,
    pub boost_speed: f32,
    pub ground_accel: f32,
    pub air_accel: f32,
    pub gravity: f32,
    pub jump_speed: f32,
    pub jet_accel: f32,
    pub max_rise_speed: f32,
    pub step_up: f32,
    pub max_slope_deg: f32,
    pub energy: f32,
    pub energy_regen: f32,
    pub energy_regen_delay: f32,
    pub boost_drain: f32,
    pub jet_drain: f32,
    pub turn_rate: f32,
    pub pitch_rate: f32,
    pub min_pitch: f32,
    pub max_pitch: f32,
    pub regen_delay: f32,
    pub regen_per_s: f32,
    pub charge_move_mult: f32,
    pub self_damage_mult: f32,
    pub armor: f32,
    pub rear_mult: f32,
    pub front_arc_deg: f32,
    pub near_range_bonus: f32,
    pub vision_bonus: f32,
    pub hearing: f32,
}

#[derive(Deserialize, Clone, Copy, Debug)]
pub struct GunStats {
    pub damage: f32,
    pub cooldown: f32,
    pub speed: f32,
    pub range: f32,
    pub spread_deg: f32,
    pub mag: f32,
    pub reload: f32,
}
#[derive(Deserialize, Clone, Copy, Debug)]
pub struct MortarStats {
    pub damage: f32,
    pub splash: f32,
    pub splash_radius: f32,
    pub splash_edge: f32,
    pub cooldown: f32,
    pub speed: f32,
    pub gravity: f32,
    pub ttl: f32,
    pub knock: f32,
    pub knock_up: f32,
    pub drain: f32,
}
#[derive(Deserialize, Clone, Copy, Debug)]
pub struct LaserStats {
    pub damage: f32,
    pub charge: f32,
    pub min_charge: f32,
    pub min_frac: f32,
    pub hold_s: f32,
    pub cooldown: f32,
    pub range: f32,
    #[serde(default)]
    pub pierce: f32,
    #[serde(default = "one")]
    pub charge_turn_mult: f32,
    pub cookoff: f32,
}
#[derive(Deserialize, Clone, Copy, Debug)]
pub struct LightningStats {
    pub damage: f32,
    pub cooldown: f32,
    pub range: f32,
    pub cone_deg: f32,
    pub chain: f32,
    pub chain_range: f32,
    pub chain_falloff: f32,
    pub energy: f32,
    pub short_s: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WKind {
    Gun = 0,
    Mortar = 1,
    Laser = 2,
    Lightning = 3,
}
pub const WKINDS: [WKind; 4] = [WKind::Gun, WKind::Mortar, WKind::Laser, WKind::Lightning];
impl WKind {
    pub fn name(self) -> &'static str {
        match self {
            WKind::Gun => "gun",
            WKind::Mortar => "mortar",
            WKind::Laser => "laser",
            WKind::Lightning => "lightning",
        }
    }
    pub fn from_name(s: &str) -> Option<WKind> {
        WKINDS.iter().copied().find(|k| k.name() == s)
    }
}

/// Per-mech weapon numbers (upgrades belong to the mech, not the item).
#[derive(Clone, Debug)]
pub struct WeaponStats {
    pub maps: [Map<String, Value>; 4],
    pub gun: GunStats,
    pub mortar: MortarStats,
    pub laser: LaserStats,
    pub lightning: LightningStats,
}
impl WeaponStats {
    pub fn from_maps(maps: [Map<String, Value>; 4]) -> Result<WeaponStats, String> {
        let get = |i: usize| Value::Object(maps[i].clone());
        Ok(WeaponStats {
            gun: serde_json::from_value(get(0)).map_err(|e| format!("weapons.gun: {e}"))?,
            mortar: serde_json::from_value(get(1)).map_err(|e| format!("weapons.mortar: {e}"))?,
            laser: serde_json::from_value(get(2)).map_err(|e| format!("weapons.laser: {e}"))?,
            lightning: serde_json::from_value(get(3)).map_err(|e| format!("weapons.lightning: {e}"))?,
            maps,
        })
    }
    pub fn refresh(&mut self) {
        if let Ok(n) = WeaponStats::from_maps(self.maps.clone()) {
            *self = n;
        }
    }
    pub fn cooldown(&self, k: WKind) -> f32 {
        match k {
            WKind::Gun => self.gun.cooldown,
            WKind::Mortar => self.mortar.cooldown,
            WKind::Laser => self.laser.cooldown,
            WKind::Lightning => self.lightning.cooldown,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Exactly what goes into `hello.config`.
    pub raw: Value,
    pub match_: MatchCfg,
    pub bot: BotCfg,
    pub mech_map: Map<String, Value>,
    pub mech: MechStats,
    pub vision: VisionCfg,
    pub sound: SoundCfg,
    pub weapons: WeaponStats,
    pub projectile: ProjectileCfg,
    pub cubes: CubesCfg,
    pub crates: CratesCfg,
    pub loot: LootCfg,
    pub npc: NpcCfg,
    pub drops: DropsCfg,
    pub zone: ZoneCfg,
    pub upgrades: Vec<UpgradeDef>,
    pub replay_frame_every: u32,
}

impl Config {
    pub fn default_config() -> Config {
        Config::from_json(DEFAULT_CONFIG_JSON).expect("embedded config is valid")
    }
    pub fn from_json(text: &str) -> Result<Config, String> {
        let raw: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        Config::from_value(raw)
    }
    pub fn from_value(raw: Value) -> Result<Config, String> {
        fn sec<T: for<'a> Deserialize<'a>>(raw: &Value, k: &str) -> Result<T, String> {
            serde_json::from_value(raw.get(k).cloned().ok_or_else(|| format!("config.{k} missing"))?).map_err(|e| format!("config.{k}: {e}"))
        }
        let mech_map: Map<String, Value> = sec(&raw, "mech")?;
        let wraw = raw.get("weapons").ok_or("config.weapons missing")?;
        let wm = |k: &str| -> Result<Map<String, Value>, String> { sec(wraw, k) };
        Ok(Config {
            match_: sec(&raw, "match")?,
            bot: sec(&raw, "bot")?,
            mech: serde_json::from_value(Value::Object(mech_map.clone())).map_err(|e| format!("config.mech: {e}"))?,
            mech_map,
            vision: sec(&raw, "vision")?,
            sound: sec(&raw, "sound")?,
            weapons: WeaponStats::from_maps([wm("gun")?, wm("mortar")?, wm("laser")?, wm("lightning")?])?,
            projectile: sec(&raw, "projectile")?,
            cubes: sec(&raw, "cubes")?,
            crates: sec(&raw, "crates")?,
            loot: sec(&raw, "loot")?,
            npc: sec(&raw, "npc")?,
            drops: sec(&raw, "drops")?,
            zone: sec(&raw, "zone")?,
            upgrades: sec(&raw, "upgrades")?,
            replay_frame_every: raw.pointer("/replay/frame_every").and_then(|v| v.as_u64()).unwrap_or(2) as u32,
            raw,
        })
    }
    pub fn sound_range(&self, kind: &str) -> f32 {
        self.sound.range.get(kind).copied().unwrap_or(0.0)
    }
}
