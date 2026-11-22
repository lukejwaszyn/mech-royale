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
    #[serde(deserialize_with = "de_i64")]
    pub slot: i64,
    #[serde(deserialize_with = "de_u64")]
    pub seed: u64,
    pub map: MapRef,
    pub config: Config,
}

/// Integers (ids, slot, seed, counts) are read from any JSON number: `3`, `3.0` and
/// `"3"` are the same id. serde's own integer parse rejects the last two, and one such
/// field used to cost the whole message.
fn de_i64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let v = Value::deserialize(d)?;
    num(&v).map(|x| x as i64).ok_or_else(|| serde::de::Error::custom("not a number"))
}
fn de_i32<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i32, D::Error> {
    de_i64(d).map(|x| x.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
}
fn de_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let v = Value::deserialize(d)?;
    v.as_u64().or_else(|| num(&v).map(|x| x.abs() as u64)).ok_or_else(|| serde::de::Error::custom("not a number"))
}
/// An optional id: null, a missing field or anything that is not a number is `None`.
fn de_opt_i64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(num(&v).map(|x| x as i64))
}

/// A JSON number as f64, also when it arrives as a numeric string.
fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok())).filter(|x| x.is_finite())
}

/// Turn numbers that arrive as strings ("12", "0.24") into numbers, everywhere except
/// under the keys that are text by definition. Used only after the strict parse failed.
fn numify(v: &mut Value) {
    match v {
        Value::Array(a) => a.iter_mut().for_each(numify),
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                if !matches!(k.as_str(), "dir" | "files" | "name" | "desc" | "text" | "protocol" | "label") {
                    numify(x);
                }
            }
        }
        Value::String(t) => {
            if let Some(n) = t.trim().parse::<f64>().ok().filter(|x| x.is_finite()).and_then(serde_json::Number::from_f64) {
                *v = Value::Number(n);
            }
        }
        _ => {}
    }
}

/// One section of a message; the documented defaults if it is missing or malformed.
fn section<T: serde::de::DeserializeOwned + Default>(v: &Value) -> T {
    if v.is_null() {
        return T::default();
    }
    serde_json::from_value(v.clone()).unwrap_or_default()
}

/// A list parsed element by element: a malformed element is dropped, not the list.
fn list<T: serde::de::DeserializeOwned>(v: &Value) -> Vec<T> {
    v.as_array().map(|a| a.iter().filter_map(|x| serde_json::from_value(x.clone()).ok()).collect()).unwrap_or_default()
}

impl Hello {
    /// Parse a `hello` line. The strict parse first; if anything in it has an
    /// unexpected type (a float `slot`, a negative `seed`, a number sent as a string),
    /// every part is read on its own so that one bad field costs that part only - above
    /// all the map directory survives. The flag is false when nothing could be read.
    pub fn parse(line: &str) -> (Hello, bool) {
        if let Ok(h) = serde_json::from_str::<Hello>(line) {
            return (h, true);
        }
        let mut v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return (Hello::default(), false),
        };
        if !v.is_object() {
            return (Hello::default(), false);
        }
        numify(&mut v);
        if let Ok(h) = serde_json::from_value::<Hello>(v.clone()) {
            return (h, true);
        }
        let c = &v["config"];
        let hello = Hello {
            protocol: v["protocol"].as_str().unwrap_or("").to_string(),
            slot: num(&v["slot"]).map_or(0, |x| x as i64),
            seed: num(&v["seed"]).map_or(0, |x| x.abs() as u64),
            map: MapRef { dir: v["map"]["dir"].as_str().unwrap_or("").to_string(), files: list(&v["map"]["files"]) },
            config: Config {
                match_: section(&c["match"]),
                bot: section(&c["bot"]),
                mech: section(&c["mech"]),
                vision: section(&c["vision"]),
                sound: section(&c["sound"]),
                weapons: WeaponsCfg { gun: section(&c["weapons"]["gun"]), mortar: section(&c["weapons"]["mortar"]), laser: section(&c["weapons"]["laser"]), lightning: section(&c["weapons"]["lightning"]) },
                cubes: section(&c["cubes"]),
                crates: section(&c["crates"]),
                loot: section(&c["loot"]),
                npc: section(&c["npc"]),
                drops: section(&c["drops"]),
                zone: section(&c["zone"]),
                upgrades: list(&c["upgrades"]),
            },
        };
        (hello, true)
    }
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
cfg_struct!(CubesCfg { pickup_radius: f32 = 3.5, hp_per_cube: f32 = 0.04, damage_per_cube: f32 = 0.04, stat_cap: f32 = 10.0, death_drop_extra: f32 = 2.0, size_per_cube: f32 = 0.05, upgrade_thresholds: Vec<f32> = vec![1.0, 3.0, 6.0, 10.0, 15.0, 21.0] });
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
    #[serde(deserialize_with = "de_i32")]
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

/// Coordinates beyond this (m) are not on any map; speeds beyond `MAX_SPEED` are not
/// motion. Values like 1e9 or 1e30 once hung the ray marches for good.
const MAX_COORD: f32 = 1.0e4;
const MAX_SPEED: f32 = 1.0e3;

fn sane_pos(p: &V3) -> bool {
    p.iter().all(|v| v.is_finite() && v.abs() <= MAX_COORD)
}
fn tame(v: &mut [f32], lim: f32) {
    for x in v.iter_mut() {
        *x = if x.is_finite() { x.clamp(-lim, lim) } else { 0.0 };
    }
}
fn tame_aim(a: &mut V2) {
    for x in a.iter_mut() {
        if !x.is_finite() {
            *x = 0.0;
        }
    }
    let two_pi = 2.0 * std::f32::consts::PI;
    if a[0].abs() > two_pi {
        a[0] = a[0].rem_euclid(two_pi);
    }
    a[1] = a[1].clamp(-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2);
}
fn tame_num(x: &mut f32, lo: f32, hi: f32, default: f32) {
    *x = if x.is_finite() { x.clamp(lo, hi) } else { default };
}

impl Obs {
    /// Parse an observation line. The strict parse first; if one field has an
    /// unexpected type, every section is read on its own and lists element by element,
    /// so a single odd value costs that element instead of the whole decision. `None`
    /// when there is no tick or no usable `self`.
    pub fn parse(line: &str) -> Option<Obs> {
        if let Ok(o) = serde_json::from_str::<Obs>(line) {
            return Some(o);
        }
        let mut v: Value = serde_json::from_str(line).ok()?;
        numify(&mut v);
        if let Ok(o) = serde_json::from_value::<Obs>(v.clone()) {
            return Some(o);
        }
        let tick = num(&v["tick"])?;
        let me: SelfObs = serde_json::from_value(v["self"].clone()).ok()?;
        Some(Obs {
            tick,
            t: num(&v["t"]).map_or(tick as f32 * 0.05, |x| x as f32),
            alive: num(&v["alive"]).map_or(16, |x| x as i32),
            me,
            mechs: list(&v["mechs"]),
            projectiles: list(&v["projectiles"]),
            crates: list(&v["crates"]),
            cubes: list(&v["cubes"]),
            pickups: list(&v["pickups"]),
            sounds: list(&v["sounds"]),
            beams: list(&v["beams"]),
            events: list(&v["events"]),
            drops: list(&v["drops"]),
            zone: section(&v["zone"]),
            offer: serde_json::from_value(v["offer"].clone()).ok().flatten(),
        })
    }

    /// Make every number safe to compute with: things with non-finite or absurd
    /// coordinates are dropped, velocities and our own state are clamped. A sane
    /// server never sends such values; this is what keeps a broken one from hanging
    /// or crashing the decision.
    pub fn sanitize(&mut self) {
        if !self.t.is_finite() {
            self.t = (self.tick as f32 * 0.05).max(0.0);
        }
        let me = &mut self.me;
        tame(&mut me.pos, MAX_COORD);
        tame(&mut me.vel, MAX_SPEED);
        tame_aim(&mut me.aim);
        tame_num(&mut me.hp, 0.0, 1.0e7, 1.0);
        tame_num(&mut me.max_hp, 1.0, 1.0e7, 1400.0);
        tame_num(&mut me.energy, 0.0, 1.0e6, 0.0);
        tame_num(&mut me.max_energy, 1.0, 1.0e6, 100.0);
        tame_num(&mut me.cubes, 0.0, 1.0e4, 0.0);
        let d = MechCfg::default();
        let st = &mut me.stats;
        tame_num(&mut st.radius, 0.3, 200.0, d.radius);
        tame_num(&mut st.height, 0.5, 600.0, d.height);
        tame_num(&mut st.eye_height, 0.1, 600.0, d.eye_height);
        tame_num(&mut st.muzzle_height, 0.1, 600.0, d.muzzle_height);
        tame_num(&mut st.walk_speed, 0.1, MAX_SPEED, d.walk_speed);
        tame_num(&mut st.boost_speed, 0.1, MAX_SPEED, d.boost_speed);
        tame_num(&mut st.ground_accel, 0.1, 1.0e5, d.ground_accel);
        tame_num(&mut st.turn_rate, 0.01, 1.0e3, d.turn_rate);
        tame_num(&mut st.pitch_rate, 0.01, 1.0e3, d.pitch_rate);
        tame_num(&mut st.min_pitch, -1.57, 1.57, d.min_pitch);
        tame_num(&mut st.max_pitch, -1.57, 1.57, d.max_pitch);
        tame_num(&mut st.step_up, 0.0, 100.0, d.step_up);
        tame_num(&mut st.max_slope_deg, 1.0, 89.0, d.max_slope_deg);
        tame_num(&mut st.gravity, 0.1, 1.0e3, d.gravity);
        tame_num(&mut st.jump_speed, 0.0, MAX_SPEED, d.jump_speed);
        tame_num(&mut st.jet_accel, 0.0, 1.0e4, d.jet_accel);
        tame_num(&mut st.max_rise_speed, 0.1, MAX_SPEED, d.max_rise_speed);
        tame_num(&mut st.jet_drain, 0.01, 1.0e4, d.jet_drain);
        tame_num(&mut st.vision_bonus, -1.0e3, 1.0e3, 0.0);
        tame_num(&mut st.near_range_bonus, -1.0e3, 1.0e3, 0.0);
        self.mechs.retain(|a| sane_pos(&a.pos));
        for a in self.mechs.iter_mut() {
            tame(&mut a.vel, MAX_SPEED);
            tame_aim(&mut a.aim);
            tame_num(&mut a.cubes, 0.0, 1.0e4, 0.0);
            a.hp = a.hp.filter(|x| x.is_finite());
            a.max_hp = a.max_hp.filter(|x| x.is_finite());
        }
        self.projectiles.retain(|p| sane_pos(&p.pos) && p.vel.iter().all(|v| v.is_finite() && v.abs() <= 2.0 * MAX_SPEED));
        self.crates.retain(|c| sane_pos(&c.pos) && c.hp.is_finite());
        self.cubes.retain(|c| sane_pos(&c.pos));
        self.pickups.retain(|c| sane_pos(&c.pos));
        self.drops.retain(|d| sane_pos(&d.pos) && d.lands_in.is_finite() && d.capture_s.is_finite());
        self.sounds.retain(|s| s.bearing.is_finite() && s.age.is_finite());
        for e in self.events.iter_mut() {
            e.amount = e.amount.filter(|x| x.is_finite());
            e.bearing = e.bearing.filter(|x| x.is_finite());
        }
        let z = &mut self.zone;
        let dz = ZoneObs::default();
        tame(&mut z.center, MAX_COORD);
        tame(&mut z.next_center, MAX_COORD);
        tame_num(&mut z.radius, 0.0, 1.0e5, dz.radius);
        tame_num(&mut z.next_radius, 0.0, 1.0e5, dz.next_radius);
        tame_num(&mut z.shrink_start, -1.0e9, 1.0e9, dz.shrink_start);
        tame_num(&mut z.shrink_end, -1.0e9, 1.0e9, dz.shrink_end);
        tame_num(&mut z.dps, 0.0, 1.0e6, 0.0);
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct SelfObs {
    #[serde(deserialize_with = "de_i64")]
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
    #[serde(deserialize_with = "de_i64")]
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
    #[serde(deserialize_with = "de_i64")]
    pub id: i64,
    pub kind: String,
    pub pos: V3,
    pub vel: V3,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CrateObs {
    #[serde(deserialize_with = "de_i64")]
    pub id: i64,
    pub pos: V3,
    pub hp: f32,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct ItemObs {
    #[serde(deserialize_with = "de_i64")]
    pub id: i64,
    pub pos: V3,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct PickupObs {
    #[serde(deserialize_with = "de_i64")]
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
    #[serde(deserialize_with = "de_opt_i64")]
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
    #[serde(deserialize_with = "de_opt_i64")]
    pub by: Option<i64>,
    pub rear: Option<bool>,
    #[serde(deserialize_with = "de_opt_i64")]
    pub target: Option<i64>,
    pub effect: Option<String>,
    pub id: Option<Value>,
    pub cubes: Option<f32>,
    pub reason: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct DropObs {
    #[serde(deserialize_with = "de_i64")]
    pub id: i64,
    pub pos: V3,
    pub radius: f32,
    pub lands_in: f32,
    pub capture_s: f32,
    pub progress: f32,
    #[serde(deserialize_with = "de_opt_i64")]
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Ids and counts parse from 3, 3.0 and "3"; a negative or float seed is still a seed.
    #[test]
    fn integers_parse_from_any_number() {
        let h: Hello = serde_json::from_value(json!({"slot": 3.0, "seed": -17.5, "map": {"dir": "/arena"}})).unwrap();
        assert_eq!((h.slot, h.seed, h.map.dir.as_str()), (3, 17, "/arena"));
        let o: Obs = serde_json::from_value(json!({"tick": 8, "alive": 14.0, "mechs": [{"id": 5.0, "pos": [1, 2, 3]}, {"id": "7"}],
            "events": [{"type": "damaged", "by": 5.0, "amount": 45}, {"type": "damaged", "by": null}], "drops": [{"id": 20000.0, "holder": 2.0}]}))
        .unwrap();
        assert_eq!(o.alive, 14);
        assert_eq!(o.mechs.iter().map(|m| m.id).collect::<Vec<_>>(), vec![5, 7]);
        assert_eq!((o.events[0].by, o.events[1].by), (Some(5), None));
        assert_eq!((o.drops[0].id, o.drops[0].holder), (20000, Some(2)));
    }

    /// One field of the wrong type in hello costs that part only: the map directory and
    /// the other sections survive. (It used to throw the whole hello away - no map, flat
    /// world, default rules.)
    #[test]
    fn hello_survives_a_mistyped_field() {
        // a number as a string: read as the number
        let (h, ok) = Hello::parse(r#"{"type":"hello","slot":"4","seed":9,"map":{"dir":"/arena"},"config":{"mech":{"walk_speed":"13.5"},"bot":{"tick_budget_ms":40}}}"#);
        assert!(ok);
        assert_eq!((h.slot, h.map.dir.as_str(), h.config.mech.walk_speed, h.config.bot.tick_budget_ms), (4, "/arena", 13.5, 40.0));
        // a section that is nonsense: that section takes the documented defaults
        let (h, ok) = Hello::parse(r#"{"type":"hello","slot":2,"map":{"dir":"/arena","files":7},"config":{"mech":[1,2],"zone":{"start_radius":{}},"cubes":{"stat_cap":12}}}"#);
        assert!(ok);
        assert_eq!((h.slot, h.map.dir.as_str(), h.config.mech.walk_speed, h.config.cubes.stat_cap), (2, "/arena", 11.7, 12.0));
        // not JSON at all
        assert!(!Hello::parse("hello?").1);
    }

    /// One odd value in an observation costs that element, not the decision.
    #[test]
    fn obs_survives_a_mistyped_field() {
        let line = r#"{"type":"obs","tick":40,"t":"2.0","alive":16,"self":{"id":3,"pos":[1,2,3],"hp":"900"},
            "mechs":[{"id":5,"pos":[10,0,0]},{"id":6,"pos":"nowhere"}],"cubes":{"oops":1},"zone":{"radius":[]},"offer":17}"#;
        let o = Obs::parse(line).expect("parsed leniently");
        assert_eq!((o.tick, o.t, o.me.hp), (40.0, 2.0, 900.0));
        assert_eq!(o.mechs.len(), 1, "the malformed mech is dropped, the other kept");
        assert!(o.cubes.is_empty() && o.offer.is_none());
        assert_eq!(o.zone.radius, ZoneObs::default().radius);
        assert!(Obs::parse("{\"type\":\"obs\",").is_none(), "not JSON: no observation");
    }

    /// Absurd numbers are dropped or clamped before anything computes with them.
    #[test]
    fn sanitize_removes_absurd_numbers() {
        let mut o: Obs = serde_json::from_value(json!({"tick": 2, "t": 0.1,
            "self": {"pos": [1e9, 0, 0], "vel": [1e30, 0, 0], "aim": [1e9, 7.0], "stats": {"min_pitch": 2.0e9}},
            "mechs": [{"id": 1, "pos": [1e9, 0, 0]}, {"id": 2, "pos": [50, 0, 0], "vel": [1e9, 0, 0], "aim": [-1e12, 0]}],
            "projectiles": [{"id": 1, "kind": "mortar", "pos": [0, 0, 5], "vel": [1e9, 0, 0]}, {"id": 2, "kind": "gun", "pos": [0, 0, 5], "vel": [200, 0, 0]}],
            "cubes": [{"id": 5000, "pos": [0, 1e20, 0]}], "zone": {"radius": 1e30}}))
        .unwrap();
        o.sanitize();
        assert!(o.me.pos[0].abs() <= MAX_COORD && o.me.vel[0].abs() <= MAX_SPEED);
        assert!(o.me.aim[0].abs() <= 7.0 && o.me.aim[1].abs() <= 1.6);
        assert!(o.me.stats.min_pitch <= o.me.stats.max_pitch + 3.2);
        assert_eq!(o.mechs.iter().map(|m| m.id).collect::<Vec<_>>(), vec![2]);
        assert!(o.mechs[0].vel[0] <= MAX_SPEED && o.mechs[0].aim[0].abs() <= 7.0);
        assert_eq!(o.projectiles.iter().map(|p| p.id).collect::<Vec<_>>(), vec![2]);
        assert!(o.cubes.is_empty());
        assert!(o.zone.radius <= 1.0e5);
    }
}
