//! What the bot believes about the match: enemy tracks with memory, remembered loot,
//! heard sounds, damage history. Updated once per observation.
use crate::mathx::*;
use crate::proto::*;
use world::World;

pub const W_GUN: u8 = 1;
pub const W_MORTAR: u8 = 2;
pub const W_LASER: u8 = 4;
pub const W_LIGHTNING: u8 = 8;

pub fn weapon_bit(kind: &str) -> u8 {
    match kind {
        "gun" => W_GUN,
        "mortar" => W_MORTAR,
        "laser" => W_LASER,
        "lightning" => W_LIGHTNING,
        _ => 0,
    }
}

#[derive(Clone, Debug)]
pub struct Track {
    pub id: i64,
    pub npc: bool,
    pub pos: V3,
    pub vel: V3,
    /// Exponentially smoothed velocity (jinking averages out).
    pub vel_s: V3,
    pub aim: V2,
    pub hp: f32,
    pub max_hp: f32,
    pub cubes: f32,
    pub weapons: u8,
    pub charging: bool,
    /// Last time it was listed in an observation (seen or still tracked by the server).
    pub seen_t: f32,
    /// Last time we had a clear line from our eye to it.
    pub los_t: f32,
    pub in_obs: bool,
    /// Clear line of fire between our muzzle and its body right now.
    pub los: bool,
    pub hit_us_t: f32,
    /// Damage it dealt to us, decaying with a 3 s time constant.
    pub dmg_to_us: f32,
    pub we_hit_t: f32,
    /// Duel record: all damage it dealt to us / we dealt to it, and the seconds it was
    /// in the open in front of us. Their ratios measure how well each side shoots.
    pub dmg_us_total: f32,
    pub dmg_them_total: f32,
    /// Seconds it spent shooting at us with a clear line / we spent shooting at it.
    pub their_time: f32,
    pub our_time: f32,
}

impl Track {
    /// Its damage per second on us while it can see us (prior: a competent gunner,
    /// more with a laser; the evidence takes over after a few seconds in the open).
    pub fn their_dps(&self) -> f32 {
        let prior = 85.0 + if self.weapons & W_LASER != 0 { 60.0 } else { 0.0 } + if self.weapons & W_LIGHTNING != 0 { 15.0 } else { 0.0 };
        (self.dmg_us_total + prior * 3.0) / (self.their_time + 3.0)
    }
    /// Our damage per second on it while we can see it (prior 85).
    pub fn our_dps(&self) -> f32 {
        (self.dmg_them_total + 85.0 * 3.0) / (self.our_time + 3.0)
    }
}

#[derive(Clone, Debug)]
pub struct Pickup {
    pub id: i64,
    pub kind: String,
    pub pos: V3,
    pub seen_t: f32,
    /// Seconds it has been missing from the observations although we should see it.
    pub missing: f32,
}
#[derive(Clone, Debug)]
pub struct Crate {
    pub id: i64,
    pub pos: V3,
    pub hp: f32,
    pub seen_t: f32,
    pub missing: f32,
}
#[derive(Clone, Debug)]
pub struct Cube {
    pub id: i64,
    pub pos: V3,
    pub seen_t: f32,
    pub missing: f32,
}
#[derive(Clone, Debug)]
pub struct Heard {
    pub kind: String,
    pub bearing: f32,
    /// Rough source position from the bearing and the distance class.
    pub pos: V2,
    pub t: f32,
    pub near: bool,
}

#[derive(Default)]
pub struct Model {
    pub t: f32,
    pub tracks: Vec<Track>,
    pub pickups: Vec<Pickup>,
    pub crates: Vec<Crate>,
    pub cubes: Vec<Cube>,
    pub heard: Vec<Heard>,
    /// Last time we took weapon damage (zone damage excluded).
    pub last_hurt_t: f32,
    pub last_hurt_bearing: Option<f32>,
    pub last_hurt_by: Option<i64>,
    /// Last time we dealt or took any damage (regeneration clock).
    pub last_combat_t: f32,
    /// Weapon damage taken, decaying with a 3 s time constant (about 3 x the incoming DPS).
    pub dmg_recent: f32,
    pub rear_hits: u32,
    pub kills: u32,
    /// Pickup ids whose `take` failed for good.
    pub bad_pickups: Vec<i64>,
    pub last_took_t: f32,
    last_alive: i32,
    /// Last time the count of living mechs went down.
    pub death_t: f32,
    /// Loot we stood in front of and did not see: (id, ignore re-sightings from afar until).
    /// Without this a thing the server shows from far away but not from close by would
    /// have us walk to it and away from it for ever.
    pub ghosts: Vec<(i64, f32)>,
}

/// Seconds a remembered thing may be missing from view (while we should see it) before we forget it.
const MISSING_S: f32 = 0.6;
/// A forgotten thing is not believed again from further than this for a while.
const GHOST_S: f32 = 30.0;
const GHOST_TRUST_DIST: f32 = 25.0;

impl Model {
    pub fn new() -> Model {
        Model { last_hurt_t: -1e9, last_combat_t: -1e9, last_took_t: -1e9, death_t: -1e9, last_alive: i32::MAX, ..Default::default() }
    }

    pub fn track(&self, id: i64) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// Fold the events of an observation we are not going to answer (a newer one is queued).
    pub fn absorb_events(&mut self, obs: &Obs) {
        self.events(obs);
    }

    fn events(&mut self, obs: &Obs) {
        let t = obs.t;
        for e in &obs.events {
            match e.kind.as_str() {
                "damaged" => {
                    self.last_combat_t = t;
                    let zone = e.weapon.as_deref() == Some("zone");
                    if !zone {
                        let amount = e.amount.unwrap_or(0.0);
                        self.last_hurt_t = t;
                        self.last_hurt_bearing = e.bearing.or(self.last_hurt_bearing);
                        self.last_hurt_by = e.by;
                        self.dmg_recent += amount;
                        if e.rear == Some(true) {
                            self.rear_hits += 1;
                        }
                        if let Some(by) = e.by {
                            if let Some(tr) = self.tracks.iter_mut().find(|x| x.id == by) {
                                tr.hit_us_t = t;
                                tr.dmg_to_us += amount;
                                tr.dmg_us_total += amount;
                            }
                        }
                    }
                }
                "hit" => {
                    self.last_combat_t = t;
                    if let Some(id) = e.target {
                        if let Some(tr) = self.tracks.iter_mut().find(|x| x.id == id) {
                            tr.we_hit_t = t;
                            tr.dmg_them_total += e.amount.unwrap_or(0.0);
                        }
                    }
                }
                "kill" => {
                    self.kills += 1;
                    if let Some(id) = e.target {
                        self.tracks.retain(|x| x.id != id);
                    }
                }
                "took" => {
                    self.last_took_t = t;
                    if let Some(id) = e.id.as_ref().and_then(|v| v.as_i64()) {
                        self.pickups.retain(|p| p.id != id);
                    }
                }
                "take_failed" => {
                    if let Some(id) = e.id.as_ref().and_then(|v| v.as_i64()) {
                        match e.reason.as_deref() {
                            Some("gone") => self.pickups.retain(|p| p.id != id),
                            Some("have") | Some("full") | Some("busy") => {
                                if !self.bad_pickups.contains(&id) {
                                    self.bad_pickups.push(id);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Full update from the observation we are about to answer.
    pub fn update(&mut self, obs: &Obs, world: &World, cfg: &Config) {
        let dt = (obs.t - self.t).clamp(0.0, 1.0);
        self.t = obs.t;
        let t = obs.t;
        let me = &obs.me;
        let decay = (-dt / 3.0).exp();
        self.dmg_recent *= decay;
        self.events(obs);
        if obs.alive < self.last_alive && self.last_alive != i32::MAX {
            self.death_t = t;
        }
        self.last_alive = obs.alive;
        let someone_died = t - self.death_t < 0.6;

        let eye = [me.pos[0], me.pos[1], me.pos[2] + me.stats.eye_height];
        let muzzle = [me.pos[0], me.pos[1], me.pos[2] + me.stats.muzzle_height];

        let vp = world::VisionParams {
            range: cfg.vision.range + me.stats.vision_bonus,
            fov_deg: cfg.vision.fov_deg,
            near_range: cfg.vision.near_range + me.stats.near_range_bonus,
            canopy_blocks: cfg.vision.canopy_blocks,
        };

        // ---- tracks
        for tr in self.tracks.iter_mut() {
            tr.in_obs = false;
            tr.los = false;
            tr.dmg_to_us *= decay;
        }
        for a in &obs.mechs {
            let npc = a.kind == "npc" || a.id >= 100;
            let mut w = 0u8;
            for k in &a.weapons {
                w |= weapon_bit(k);
            }
            let height = if npc { cfg.npc.height } else { cfg.mech.height * (1.0 + cfg.cubes.size_per_cube * a.cubes) };
            let mid = [a.pos[0], a.pos[1], a.pos[2] + height * 0.5];
            let top = [a.pos[0], a.pos[1], a.pos[2] + height * 0.9];
            let los = world.los(muzzle, mid) || world.los(muzzle, top);
            // "Seen" by the protocol's rules (range, view wedge, canopy), not just a clear line.
            let eye_los = world.can_see(eye, me.aim[0], a.pos, height, &vp);
            let default_hp = if npc { cfg.npc.hp } else { cfg.mech.hp };
            match self.tracks.iter_mut().find(|x| x.id == a.id) {
                Some(tr) => {
                    let k = if t - tr.seen_t > 0.5 { 1.0 } else { (dt / 0.35).clamp(0.05, 1.0) };
                    for i in 0..3 {
                        tr.vel_s[i] = lerp(tr.vel_s[i], a.vel[i], k);
                    }
                    tr.pos = a.pos;
                    tr.vel = a.vel;
                    tr.aim = a.aim;
                    if let Some(hp) = a.hp {
                        tr.hp = hp;
                    }
                    if let Some(m) = a.max_hp {
                        tr.max_hp = m;
                    }
                    tr.cubes = a.cubes;
                    if !npc {
                        tr.weapons = w;
                    }
                    tr.charging = a.charging;
                    tr.seen_t = t;
                    tr.in_obs = true;
                    tr.los = los;
                    if eye_los {
                        tr.los_t = t;
                    }
                }
                None => self.tracks.push(Track {
                    id: a.id,
                    npc,
                    pos: a.pos,
                    vel: a.vel,
                    vel_s: a.vel,
                    aim: a.aim,
                    hp: a.hp.unwrap_or(default_hp),
                    max_hp: a.max_hp.unwrap_or(default_hp),
                    cubes: a.cubes,
                    weapons: if npc { 0 } else { w },
                    charging: a.charging,
                    seen_t: t,
                    los_t: if eye_los { t } else { -1e9 },
                    in_obs: true,
                    los,
                    hit_us_t: -1e9,
                    dmg_to_us: 0.0,
                    we_hit_t: -1e9,
                    dmg_us_total: 0.0,
                    dmg_them_total: 0.0,
                    their_time: 0.0,
                    our_time: 0.0,
                }),
            }
        }
        // The server keeps listing a mech for memory_s after we lose sight of it. One that
        // vanishes right after we saw it, just as the alive count drops, is dead. Stale
        // tracks expire.
        let memory = cfg.vision.memory_s;
        self.tracks.retain(|tr| {
            if tr.in_obs {
                return true;
            }
            let gone_for = t - tr.seen_t;
            // (Drones are not counted in `alive`: one that drops out of the list while the
            // server would still be tracking it has been destroyed.)
            if (tr.npc || someone_died) && gone_for < 0.3 && tr.seen_t - tr.los_t < memory - 1.0 {
                return false; // died (or left for good): do not chase a ghost
            }
            gone_for < if tr.npc { 20.0 } else { 30.0 }
        });

        // ---- loot memory
        // Things on the ground are only listed while we see them. A remembered thing is
        // forgotten when it stays unlisted although we certainly should see it: close by
        // (the proximity sensors look in every direction and through foliage) with a
        // clear line - no building, no tree trunk - to its foot, middle and top.
        let near = (cfg.vision.near_range + me.stats.near_range_bonus) * 0.8;
        let should_see = |p: V3, h: f32| {
            dist(p, eye) < near && [0.2, 0.5 * h, h].iter().all(|dz| {
                let q = [p[0], p[1], p[2] + dz];
                world.los(eye, q) && !world.foliage(eye, q).0
            })
        };
        self.ghosts.retain(|g| g.1 > t);
        let ghosts = &mut self.ghosts;
        let believe = |id: i64, p: V3, ghosts: &Vec<(i64, f32)>| !ghosts.iter().any(|g| g.0 == id) || dist(p, eye) < GHOST_TRUST_DIST;
        let forget = |seen_t: f32, missing: &mut f32, id: i64, p: V3, h: f32, keep_s: f32, ghosts: &mut Vec<(i64, f32)>| -> bool {
            if seen_t == t {
                *missing = 0.0;
                return true;
            }
            if t - seen_t > keep_s {
                return false;
            }
            // (Standing on the spot counts too: had it been there we would have picked it up
            // or would see it, whatever the ground does to the sight line.)
            let at_feet = dist2d(p, eye) < 2.5 && (p[2] + me.stats.eye_height - eye[2]).abs() < 3.0;
            if at_feet || should_see(p, h) {
                *missing += dt;
                if *missing > MISSING_S {
                    ghosts.push((id, t + GHOST_S));
                    return false;
                }
            }
            true
        };
        for p in &obs.pickups {
            match self.pickups.iter_mut().find(|x| x.id == p.id) {
                Some(x) => {
                    x.pos = p.pos;
                    x.seen_t = t;
                }
                None if believe(p.id, p.pos, ghosts) => self.pickups.push(Pickup { id: p.id, kind: p.kind.clone(), pos: p.pos, seen_t: t, missing: 0.0 }),
                None => {}
            }
        }
        self.pickups.retain_mut(|x| forget(x.seen_t, &mut x.missing, x.id, x.pos, 1.0, 240.0, ghosts));
        for c in &obs.crates {
            match self.crates.iter_mut().find(|x| x.id == c.id) {
                Some(x) => {
                    x.pos = c.pos;
                    x.hp = c.hp;
                    x.seen_t = t;
                }
                None if believe(c.id, c.pos, ghosts) => self.crates.push(Crate { id: c.id, pos: c.pos, hp: c.hp, seen_t: t, missing: 0.0 }),
                None => {}
            }
        }
        let crate_h = cfg.crates.height.max(0.5);
        self.crates.retain_mut(|x| forget(x.seen_t, &mut x.missing, x.id, x.pos, crate_h, 240.0, ghosts));
        for c in &obs.cubes {
            match self.cubes.iter_mut().find(|x| x.id == c.id) {
                Some(x) => {
                    x.pos = c.pos;
                    x.seen_t = t;
                }
                None if believe(c.id, c.pos, ghosts) => self.cubes.push(Cube { id: c.id, pos: c.pos, seen_t: t, missing: 0.0 }),
                None => {}
            }
        }
        self.cubes.retain_mut(|x| forget(x.seen_t, &mut x.missing, x.id, x.pos, 1.0, 90.0, ghosts));

        // ---- sounds
        for s in &obs.sounds {
            let d = match s.dist.as_str() {
                "near" => cfg.sound.near * 0.55,
                "mid" => (cfg.sound.near + cfg.sound.mid) * 0.5,
                _ => cfg.sound.mid * 1.5,
            };
            self.heard.push(Heard {
                kind: s.kind.clone(),
                bearing: s.bearing,
                pos: [me.pos[0] + d * s.bearing.cos(), me.pos[1] + d * s.bearing.sin()],
                t: t - s.age,
                near: s.dist == "near",
            });
        }
        self.heard.retain(|h| t - h.t < 8.0);
        if self.heard.len() > 64 {
            let cut = self.heard.len() - 64;
            self.heard.drain(0..cut);
        }
    }

    /// Seconds since we last dealt or took damage.
    pub fn quiet_for(&self) -> f32 {
        self.t - self.last_combat_t
    }
}
