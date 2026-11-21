//! Every tunable number of the bot's behaviour lives in [`Tuning`]. Rule numbers
//! (speeds, damage, ranges of weapons, ...) are never here: those come from `hello.config`.
//!
//! Override at run time without rebuilding:
//! `MECH_TUNE="aggression=0.6,retreat_hp=0.5" MECH_MODE=ffa ./bot`

macro_rules! tuning {
    ($( $(#[$doc:meta])* $name:ident = $def:expr ),* $(,)?) => {
        #[derive(Clone, Debug)]
        pub struct Tuning {
            /// Free-for-all mode for the final (other teams' bots): less aggression, no third-party fights.
            pub ffa: bool,
            $( $(#[$doc])* pub $name: f32, )*
        }
        impl Default for Tuning {
            fn default() -> Self { Tuning { ffa: false, $( $name: $def, )* } }
        }
        impl Tuning {
            /// Set one field by name. Returns false for an unknown name.
            pub fn set(&mut self, key: &str, value: f32) -> bool {
                match key {
                    $( stringify!($name) => { self.$name = value; true } )*
                    "ffa" => { self.ffa = value > 0.5; true }
                    _ => false,
                }
            }
            pub fn names() -> &'static [&'static str] { &[ $( stringify!($name), )* ] }
        }
    };
}

tuning! {
    // ---- fight or flight
    /// 0 = never start a fight, 1 = hunt any mech we know about while healthy.
    aggression = 1.0,
    /// HP fraction needed to start hunting a mech that is not attacking us.
    hunt_min_hp = 0.8,
    /// Do not walk further than this (straight line, m) to start a fight.
    hunt_range = 170.0,
    /// Below this HP fraction: break line of sight and regenerate.
    retreat_hp = 0.0,
    /// Retreat threshold when two or more enemies are shooting at us.
    retreat_hp_outnumbered = 0.0,
    /// Engaged by more than this many enemies: retreat whatever our HP.
    max_enemies = 99.0,
    /// A target counts as alone when no other mech is within this distance of it (m).
    isolation_radius = 85.0,
    /// Duel estimate (our survival time / the target's): below this we break off even at
    /// high HP, above `press_margin` we keep fighting even at low HP.
    flee_margin = 0.0,
    press_margin = 1.3,
    /// Start a fight with a healthy mech only if our power (cubes, second weapon) is at
    /// least this multiple of its power, or it has shown poor aim.
    hunt_power_ratio = 1.15,
    /// Leave cover and fight again above this HP fraction.
    reengage_hp = 0.86,
    /// Keep fighting below `retreat_hp` while the only enemy has less than this x our HP.
    finish_ratio = 0.6,
    /// A target below this HP fraction is chased down (boost, close range) before it regenerates.
    finish_hp = 0.3,
    /// Shot continuously for this long while retreating: stop running, fight the weakest.
    last_stand_s = 2.5,
    /// A mech counts as attacking us for this long after its last hit on us (s).
    engaged_memory_s = 4.0,
    /// Give up on a target we could not shoot for this long (s).
    target_patience_s = 14.0,
    /// A mech in plain sight within this distance that a few hits will finish is always attacked (0 = off).
    kill_shot_range = 160.0,
    /// Practice mode: a mech that walks towards us or aims at us inside this distance is
    /// attacked before it opens fire (0 = off), while our HP fraction is at least `first_strike_hp`.
    first_strike_range = 130.0,
    first_strike_hp = 0.5,
    /// Having opened fire on a mech we stay on it at least this long (s).
    hunt_commit_s = 6.0,

    // ---- ranges (m)
    range_gun_min = 70.0,
    range_gun_max = 120.0,
    /// With lightning (and energy) close to this distance.
    range_lightning = 28.0,
    /// With lightning, rush a target only when it is already this close.
    lightning_rush_dist = 65.0,
    /// Without lightning, stay at least this far from a lightning carrier.
    range_vs_lightning = 62.0,
    /// A mech known within this distance stops looting and farming.
    threat_range = 150.0,
    /// Keep at least this far from mechs we are not fighting (they defend their space,
    /// and a group is never worth walking into); loot inside this radius of them is ignored.
    avoid_range = 100.0,

    // ---- movement
    /// Search radius (walking m) for cover when retreating.
    cover_search = 70.0,
    /// Step behind cover while the gun reloads if cover is within this walking distance (0 = off).
    tuck_reload_dist = 14.0,
    /// Weight of staying out of the arc of lightning carriers (0 = off).
    arc_avoid = 3.0,
    strafe_period_min = 0.45,
    strafe_period_max = 1.25,
    /// Weight of dodging visible projectiles against following the path.
    dodge_weight = 4.0,
    strafe_weight = 1.0,
    /// Start moving this many seconds before the closing circle would reach us.
    zone_margin_s = 9.0,
    /// Aim to stand this far inside the next circle (capped at 35% of its radius).
    zone_inner_margin = 22.0,
    /// Energy kept back for emergencies when boosting for convenience.
    energy_reserve = 30.0,
    boost_travel_start = 92.0,
    boost_travel_stop = 58.0,
    stuck_time_s = 0.6,

    // ---- gunnery
    /// Seconds between the observation and the shot leaving (decision latency).
    fire_delay = 0.075,
    /// Fire when the aim error is below this fraction of the target's angular half-size.
    aim_tol_scale = 0.8,
    /// 0 = lead with the instantaneous velocity, 1 = with the smoothed one (long shots).
    lead_smooth = 0.5,
    /// Mortar: no shots closer than this (own splash).
    mortar_min_dist = 17.0,
    /// Mortar at a visible target only if its smoothed speed is below this (m/s) ...
    mortar_max_target_speed = 5.0,
    /// ... or the shell flies less than this (s).
    mortar_max_flight_s = 1.1,
    /// Keep this much energy when firing lightning (unless the target is nearly dead).
    lightning_reserve = 8.0,
    scan_period_s = 4.0,

    // ---- loot priorities (value per item; divided by the time it costs)
    w_laser = 20.0,
    w_lightning = 4.0,
    w_mortar = 9.0,
    v_cube = 6.0,
    v_crate = 5.0,
    v_npc = 6.5,
    v_drop = 24.0,
    /// Only consider loot within this straight-line distance (m).
    loot_range = 170.0,
    /// Shooting crates and drones resets regeneration: only above this HP fraction.
    farm_min_hp = 0.8,
    /// Cubes stop adding HP and damage at the stat cap but keep growing the mech: stop collecting here.
    max_cubes = 10.0,
    /// Highest roof (m above the ground) we climb for loot.
    max_loot_climb = 22.0,
    /// With nothing else to do, walk around looking for loot until we have this many cubes
    /// (and a second weapon). Negative = never explore.
    explore_until_cubes = 6.0,
}

/// Upgrade preference (higher = pick first). Survivability first: max HP also counts in
/// the final zone race where armor does not; then damage, then mobility.
pub const UPGRADE_WEIGHTS: &[(&str, f32)] = &[
    ("plating", 10.0),
    ("armor", 9.0),
    ("nanites", 7.6),
    ("gun_damage", 7.2),
    ("gun_rate", 7.0),
    ("laser_damage", 6.8),
    ("laser_charge", 6.6),
    ("lightning_efficiency", 6.4),
    ("servos", 6.0),
    ("gun_velocity", 5.8),
    ("lightning_range", 5.6),
    ("gun_mag", 5.2),
    ("gyros", 5.0),
    ("reactor", 4.6),
    ("capacitor", 4.2),
    ("lightning_chain", 3.8),
    ("mortar_reload", 3.6),
    ("mortar_splash", 3.4),
    ("mortar_velocity", 3.2),
    ("jets", 3.0),
    ("sensors", 2.8),
    ("laser_pierce", 2.0),
];

pub fn upgrade_weight(id: &str) -> f32 {
    UPGRADE_WEIGHTS.iter().find(|(k, _)| *k == id).map(|(_, w)| *w).unwrap_or(1.0)
}

impl Tuning {
    /// Defaults, then `MECH_MODE` (`ffa` | `practice`) and `MECH_TUNE` (`key=value,...`).
    pub fn from_env() -> Tuning {
        let mut t = Tuning::default();
        if let Ok(m) = std::env::var("MECH_MODE") {
            if m.eq_ignore_ascii_case("ffa") {
                t.apply_ffa();
            }
        }
        if let Ok(s) = std::env::var("MECH_TUNE") {
            for bad in t.apply_overrides(&s) {
                eprintln!("[bot] MECH_TUNE: ignored '{bad}'");
            }
        }
        t
    }

    /// The free-for-all preset: survive first, fight only when attacked or when the
    /// target is already weak, never join a fight between others at full strength.
    pub fn apply_ffa(&mut self) {
        self.ffa = true;
        self.aggression = 0.35;
        self.hunt_min_hp = 0.9;
        self.hunt_range = 130.0;
        self.retreat_hp = 0.55;
        self.retreat_hp_outnumbered = 0.9;
        self.max_enemies = 1.0;
        self.flee_margin = 0.7;
        self.first_strike_range = 0.0;
        self.reengage_hp = 0.92;
        self.range_gun_min = 80.0;
        self.range_gun_max = 135.0;
    }

    /// Apply `key=value,key=value`; returns the entries that could not be applied.
    pub fn apply_overrides(&mut self, s: &str) -> Vec<String> {
        let mut bad = Vec::new();
        for part in s.split(|c| c == ',' || c == ';').map(str::trim).filter(|p| !p.is_empty()) {
            let ok = match part.split_once('=') {
                Some((k, v)) => match v.trim().parse::<f32>() {
                    Ok(x) if x.is_finite() => self.set(k.trim(), x),
                    _ => false,
                },
                None => false,
            };
            if !ok {
                bad.push(part.to_string());
            }
        }
        bad
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overrides_apply_and_report_bad_keys() {
        let mut t = Tuning::default();
        let bad = t.apply_overrides("aggression=0.25, retreat_hp=0.5,nope=1,ffa=1,x");
        assert_eq!(t.aggression, 0.25);
        assert_eq!(t.retreat_hp, 0.5);
        assert!(t.ffa);
        assert_eq!(bad, vec!["nope=1".to_string(), "x".to_string()]);
        assert!(Tuning::names().contains(&"zone_margin_s"));
    }
}
