//! The decision core: one [`Bot`] per match. `decide` turns an observation into an action:
//!
//! 1. update the world model, 2. assess the situation (who threatens us, whom to shoot),
//! 3. choose where the legs go (zone > retreat > fight > loot > hold), 4. steer (path
//! following blended with strafing and projectile dodging, stuck recovery),
//! 5. gunnery (aim, fire, reload), 6. loot actions and upgrade picks.
use crate::config::Tuning;
use crate::mathx::*;
use crate::model::*;
use crate::mover::{Drive, Mover};
use crate::proto::*;
use std::sync::Arc;
use std::time::Instant;
use world::{Body, DistField, JetParams, Nav, NavParams, World};

/// Cube counts the nav grids are built for (a mech grows 5% per cube).
const NAV_TIERS: [f32; 5] = [0.0, 2.0, 5.0, 9.0, 14.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Final,
    Zone,
    Retreat,
    Fight,
    Loot,
    Hold,
}

/// The zone circle now and where it is going.
#[derive(Clone, Copy, Debug)]
pub struct Zone {
    pub c: V2,
    pub r: f32,
    pub nc: V2,
    pub nr: f32,
    pub t0: f32,
    pub t1: f32,
    pub dps: f32,
    /// Time of the observation that carried `c`/`r`.
    pub now: f32,
}

impl Zone {
    pub fn from_obs(z: &ZoneObs, now: f32) -> Zone {
        Zone { c: z.center, r: z.radius, nc: z.next_center, nr: z.next_radius, t0: z.shrink_start, t1: z.shrink_end, dps: z.dps, now }
    }
    /// Circle at time `t >= now`. `c`/`r` are the circle at `now` (already part-way
    /// through a shrink if one is running), moving linearly to the next one by `t1`.
    pub fn at(&self, t: f32) -> (V2, f32) {
        let start = self.t0.max(self.now);
        if t <= start || self.t1 <= start {
            if t >= self.t1 && self.t1 <= start {
                return (self.nc, self.nr);
            }
            return (self.c, self.r);
        }
        let k = ((t - start) / (self.t1 - start)).clamp(0.0, 1.0);
        ([lerp(self.c[0], self.nc[0], k), lerp(self.c[1], self.nc[1], k)], lerp(self.r, self.nr, k))
    }
    pub fn inside_at(&self, p: V2, t: f32, margin: f32) -> bool {
        let (c, r) = self.at(t);
        (p[0] - c[0]).hypot(p[1] - c[1]) <= r - margin
    }
    pub fn inside_next(&self, p: V2, margin: f32) -> bool {
        (p[0] - self.nc[0]).hypot(p[1] - self.nc[1]) <= self.nr - margin
    }
    /// First time at which `p` is no longer `margin` inside the circle; `None` if it
    /// stays inside through the end of this shrink. Circles are nested, so the inside
    /// times form one interval and a binary search finds its end.
    pub fn leaves_at(&self, p: V2, margin: f32) -> Option<f32> {
        if !self.inside_at(p, self.now, margin) {
            return Some(self.now);
        }
        if self.inside_next(p, margin) {
            return None;
        }
        let (mut lo, mut hi) = (self.t0.max(self.now), self.t1.max(self.now));
        for _ in 0..24 {
            let mid = 0.5 * (lo + hi);
            if self.inside_at(p, mid, margin) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Some(hi)
    }
}

#[derive(Clone, Debug)]
pub struct Slot {
    pub bit: u8,
    pub cooldown: f32,
    pub ammo: f32,
    pub reload: f32,
    pub charge: f32,
    pub stats: WStats,
}

/// Per-decision derived facts about ourselves.
pub struct Frame {
    pub t: f32,
    pub dt: f32,
    pub pos: V3,
    pub vel: V3,
    pub xy: V2,
    pub muzzle: V3,
    pub eye: V3,
    pub hp_frac: f32,
    pub body: Body,
    /// Slightly thinner body for local collision probes (we often touch walls).
    pub slim: Body,
    pub nav_idx: usize,
    pub slots: [Option<Slot>; 2],
    pub dmg_scale: f32,
    pub zone: Zone,
    pub jet: JetParams,
    pub started: Instant,
}

impl Frame {
    pub fn slot(&self, bit: u8) -> Option<(usize, &Slot)> {
        self.slots.iter().enumerate().find_map(|(i, s)| s.as_ref().filter(|s| s.bit == bit).map(|s| (i, s)))
    }
    pub fn has(&self, bit: u8) -> bool {
        self.slot(bit).is_some()
    }
    /// Milliseconds spent on this decision so far.
    pub fn spent_ms(&self) -> f32 {
        self.started.elapsed().as_secs_f32() * 1000.0
    }
    /// Is there time for another expensive query (path, cover search)?
    pub fn can_plan(&self) -> bool {
        self.spent_ms() < 12.0
    }
}

/// One enemy mech as seen this decision.
#[derive(Clone, Copy, Debug)]
pub struct Foe {
    /// Index into `model.tracks`.
    pub ti: usize,
    pub id: i64,
    pub pos: V3,
    pub dist: f32,
    /// Clear line of fire right now.
    pub los: bool,
    /// Seconds since the server last listed it.
    pub age: f32,
    pub hp: f32,
    pub hp_frac: f32,
    /// It is shooting at us or aiming at us.
    pub engaged: bool,
    pub weapons: u8,
    pub cubes: f32,
}

#[derive(Default)]
pub struct Sit {
    /// Duel estimate against the target: (seconds we last) / (seconds it lasts). > 1 = we win.
    pub margin: f32,
    pub foes: Vec<Foe>,
    pub n_engaged: usize,
    pub target: Option<Foe>,
    /// The enemy to hide from / face.
    pub threat: Option<Foe>,
    pub threat_near: bool,
    /// We have decided to fight `target` (it attacks us, or it is worth hunting).
    pub fight: bool,
    /// Enemies are around that we neither fight nor are attacked by: stay out of sight.
    pub wary: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Farm {
    Npc(i64),
    Crate(i64),
}

/// What the legs should do and how much freedom the steering has.
#[derive(Clone, Copy, Debug)]
pub struct Intent {
    pub mode: Mode,
    pub drive: Drive,
    /// Weight of following `drive.dir`.
    pub base_w: f32,
    /// Strafe axis (unit) and weight.
    pub strafe: Option<(V2, f32)>,
    /// Direction to back off along, and weight.
    pub away: Option<(V2, f32)>,
    pub boost: bool,
    pub farm: Option<Farm>,
    /// Do not start charging the laser (it halves our speed).
    pub no_charge: bool,
    /// Hidden and regenerating: do not fire at things that cannot see us.
    pub hold_fire: bool,
}

impl Intent {
    pub fn new(mode: Mode, drive: Drive, base_w: f32) -> Intent {
        Intent { mode, drive, base_w, strafe: None, away: None, boost: false, farm: None, no_charge: false, hold_fire: false }
    }
}

#[derive(Default, Clone, Copy)]
pub struct Timing {
    pub n: u32,
    pub sum_ms: f32,
    pub max_ms: f32,
    pub over_5: u32,
    pub over_20: u32,
}

pub struct Bot {
    pub cfg: Config,
    pub tune: Tuning,
    pub slot: i64,
    pub world: Arc<World>,
    pub navs: Vec<(f32, Nav)>,
    pub model: Model,
    pub mover: Mover,
    pub rng: Rng,
    pub map_ok: bool,
    // zone
    zone_key: (i32, i32, i32),
    safe_field: Option<DistField>,
    safe_field_nav: usize,
    pub home: Option<V3>,
    // fight state
    pub mode: Mode,
    pub target: Option<i64>,
    target_blind_since: f32,
    ignore: Vec<(i64, f32)>,
    pub retreating: bool,
    cover: Option<V3>,
    cover_t: f32,
    fight_cover: Option<(V3, f32)>,
    fight_cover_t: f32,
    strafe_sign: f32,
    strafe_until: f32,
    exposed_since: f32,
    stand_until: f32,
    /// The mech our guns were on last decision.
    pub(crate) shot_at: Option<i64>,
    // tasks (tasks.rs)
    pub(crate) task: Option<crate::tasks::Task>,
    pub(crate) task_t: f32,
    pub(crate) task_started: f32,
    pub(crate) task_black: Vec<(crate::tasks::TaskKind, f32)>,
    pub(crate) take_t: f32,
    pub(crate) take_tries: (i64, u32),
    // gunnery (gunnery.rs)
    pub(crate) last_aim: V2,
    pub(crate) scan_base: f32,
    pub(crate) mortar_aim_until: f32,
    // exploring for loot: (goal, since) and where we have been
    explore: Option<(V3, f32)>,
    visited: Vec<V3>,
    // travel boost hysteresis
    boosting: bool,
    last_t: f32,
    last_mode_log: f32,
    pub timing: Timing,
    pub debug: bool,
    pub verbose: bool,
}

impl Bot {
    /// Load the map named by `hello` and build the nav grids. Never fails: with a
    /// missing map the bot plays on a flat empty world (no walls known).
    pub fn new(hello: &Hello, tune: Tuning) -> Bot {
        let started = Instant::now();
        let cfg = &hello.config;
        let (world, map_ok) = match World::load(std::path::Path::new(&hello.map.dir)) {
            Ok(w) => (w, true),
            Err(e) => {
                eprintln!("[bot] map load failed ({e}); playing on a flat world");
                (flat_world(cfg), false)
            }
        };
        let world = Arc::new(world);
        let mut navs = Vec::new();
        for (i, &cubes) in NAV_TIERS.iter().enumerate() {
            // The first grid is mandatory; the fatter ones only while there is time (hello budget).
            if i > 0 && (started.elapsed().as_secs_f32() > 5.0 || !map_ok) {
                break;
            }
            let scale = 1.0 + cfg.cubes.size_per_cube * cubes;
            let nav = Nav::build(
                world.clone(),
                NavParams { radius: cfg.mech.radius * scale, height: cfg.mech.height * scale, step_up: cfg.mech.step_up, max_slope_deg: cfg.mech.max_slope_deg, ..NavParams::default() },
            );
            navs.push((cubes, nav));
        }
        eprintln!("[bot] slot {} map_ok={} navs={} init {:.0} ms ffa={}", hello.slot, map_ok, navs.len(), started.elapsed().as_secs_f32() * 1000.0, tune.ffa);
        Bot::with_parts(hello, tune, world, navs, map_ok)
    }

    /// The map and nav grids of a finished match, for [`Bot::with_parts`] (the local
    /// simulator plays many matches in one process and the map never changes).
    pub fn into_parts(self) -> (Arc<World>, Vec<(f32, Nav)>) {
        (self.world, self.navs)
    }

    /// A bot on an already loaded map with already built nav grids (see [`Bot::new`]).
    pub fn with_parts(hello: &Hello, tune: Tuning, world: Arc<World>, navs: Vec<(f32, Nav)>, map_ok: bool) -> Bot {
        let cfg = hello.config.clone();
        Bot {
            cfg,
            tune,
            slot: hello.slot,
            world,
            navs,
            model: Model::new(),
            mover: Mover::new(),
            rng: Rng::new(hello.seed ^ 0xA5A5_5A5A),
            map_ok,
            zone_key: (i32::MIN, 0, 0),
            safe_field: None,
            safe_field_nav: usize::MAX,
            home: None,
            mode: Mode::Hold,
            target: None,
            target_blind_since: -1e9,
            ignore: Vec::new(),
            retreating: false,
            cover: None,
            cover_t: -1e9,
            fight_cover: None,
            fight_cover_t: -1e9,
            strafe_sign: 1.0,
            strafe_until: -1e9,
            exposed_since: -1.0,
            stand_until: -1e9,
            shot_at: None,
            task: None,
            task_t: -1e9,
            task_started: -1e9,
            task_black: Vec::new(),
            take_t: -1e9,
            take_tries: (-1, 0),
            last_aim: [0.0, 0.0],
            scan_base: 0.0,
            mortar_aim_until: -1e9,
            explore: None,
            visited: Vec::new(),
            boosting: false,
            last_t: 0.0,
            last_mode_log: -1e9,
            timing: Timing::default(),
            verbose: std::env::var("MECH_VERBOSE").map(|v| v != "0" && !v.is_empty()).unwrap_or(false),
            debug: std::env::var("MECH_DEBUG").map(|v| v != "0" && !v.is_empty()).unwrap_or(false),
        }
    }

    pub fn name(&self) -> &'static str {
        "tin-buckeye"
    }

    /// An observation we will not answer (a newer one is queued): keep its events.
    pub fn absorb(&mut self, obs: &Obs) {
        self.model.absorb_events(obs);
    }

    pub fn nav(&self, idx: usize) -> &Nav {
        &self.navs[idx.min(self.navs.len() - 1)].1
    }

    fn frame(&self, obs: &Obs, started: Instant) -> Frame {
        let me = &obs.me;
        let st = &me.stats;
        let mut slots: [Option<Slot>; 2] = [None, None];
        for (i, w) in me.weapons.iter().take(2).enumerate() {
            if let Some(w) = w {
                let stats = if w.stats.damage > 0.0 || w.stats.range > 0.0 || w.stats.speed > 0.0 { w.stats.clone() } else { self.cfg.weapons.get(&w.kind) };
                slots[i] = Some(Slot { bit: weapon_bit(&w.kind), cooldown: w.cooldown, ammo: w.ammo.unwrap_or(0.0), reload: w.reload, charge: w.charge, stats });
            }
        }
        let body = Body { radius: st.radius, height: st.height, step_up: st.step_up, max_slope_deg: st.max_slope_deg };
        let slim = Body { radius: (st.radius - 0.25).max(0.5), ..body };
        let mut nav_idx = self.navs.len() - 1;
        for (i, (c, _)) in self.navs.iter().enumerate() {
            if me.cubes <= *c {
                nav_idx = i;
                break;
            }
        }
        let c = &self.cfg.cubes;
        Frame {
            t: obs.t,
            dt: (obs.t - self.last_t).clamp(0.05, 1.0),
            pos: me.pos,
            vel: me.vel,
            xy: [me.pos[0], me.pos[1]],
            muzzle: [me.pos[0], me.pos[1], me.pos[2] + st.muzzle_height],
            eye: [me.pos[0], me.pos[1], me.pos[2] + st.eye_height],
            hp_frac: if me.max_hp > 0.0 { me.hp / me.max_hp } else { 1.0 },
            body,
            slim,
            nav_idx,
            slots,
            dmg_scale: 1.0 + c.damage_per_cube * me.cubes.min(c.stat_cap),
            zone: Zone::from_obs(&obs.zone, obs.t),
            jet: JetParams { jump_speed: st.jump_speed, jet_accel: st.jet_accel, max_rise_speed: st.max_rise_speed, gravity: st.gravity, jet_drain: st.jet_drain },
            started,
        }
    }

    /// The whole decision. Never panics on purpose; the caller still wraps it.
    pub fn decide(&mut self, obs: &Obs) -> Action {
        let started = Instant::now();
        let world = self.world.clone();
        self.model.update(obs, &world, &self.cfg);
        let f = self.frame(obs, started);
        self.update_zone(&f, obs);
        let sit = self.assess(&f, obs);
        let intent = self.plan_move(&f, obs, &sit);
        self.mode = intent.mode;
        let (mv, boost, jump) = self.steer(&f, obs, &sit, &intent);
        let (aim, fire, reload) = self.gunnery(&f, obs, &sit, &intent, mv);
        let (take, drop) = self.loot_actions(&f, obs, &sit);
        let pick = self.pick_upgrade(&f, obs);
        self.last_aim = aim;
        self.last_t = obs.t;

        let mut act = Action { tick: obs.tick as i64, mv, boost, jump, aim, fire, reload, pick, take, drop, debug: None };
        if self.debug {
            act.debug = Some(self.debug_draw(&f, &sit, &intent));
        }
        act.sanitize();

        let ms = started.elapsed().as_secs_f32() * 1000.0;
        let tm = &mut self.timing;
        tm.n += 1;
        tm.sum_ms += ms;
        tm.max_ms = tm.max_ms.max(ms);
        if ms > 5.0 {
            tm.over_5 += 1;
        }
        if ms > 20.0 {
            tm.over_20 += 1;
            eprintln!("[bot] slow decision {:.1} ms at t={:.1} mode={:?}", ms, obs.t, self.mode);
        }
        if self.verbose && (obs.t * 10.0).round() as i64 % 5 == 0 {
            let tg = sit.target.map(|x| format!("tgt {} d={:.0} los={} hp={:.0} eng={} w={}", x.id, x.dist, x.los, x.hp, x.engaged, x.weapons)).unwrap_or_default();
            let w: Vec<String> = f.slots.iter().flatten().map(|s| format!("{}:{:.0}/{:.1}/{:.1}", s.bit, s.ammo, s.reload, s.cooldown)).collect();
            eprintln!(
                "[v] t={:.1} {:?} hp={:.0} e={:.0} pos=({:.0},{:.0},{:.0}) v={:.1} mv=({:.1},{:.1}) b={} fire={:?} rl={} retreat={} n={} dmg={:.0} {} [{}] rem={:.0}",
                obs.t, intent.mode, obs.me.hp, obs.me.energy, f.pos[0], f.pos[1], f.pos[2], f.vel[0].hypot(f.vel[1]), act.mv[0], act.mv[1], act.boost, act.fire, act.reload,
                self.retreating, sit.n_engaged, self.model.dmg_recent, tg, w.join(" "), intent.drive.remaining
            );
        }
        if obs.t - self.last_mode_log >= 10.0 {
            self.last_mode_log = obs.t;
            eprintln!(
                "[bot] t={:.0} {:?} hp={:.0}/{:.0} cubes={} alive={} foes={} engaged={} kills={} avg={:.2}ms max={:.1}ms stuck={}",
                obs.t, self.mode, obs.me.hp, obs.me.max_hp, obs.me.cubes, obs.alive, sit.foes.len(), sit.n_engaged, self.model.kills,
                tm.sum_ms / tm.n.max(1) as f32, tm.max_ms, self.mover.stuck_count
            );
        }
        act
    }

    // ------------------------------------------------------------------ zone

    fn update_zone(&mut self, f: &Frame, obs: &Obs) {
        let z = &f.zone;
        let key = ((z.nc[0] * 10.0) as i32, (z.nc[1] * 10.0) as i32, (z.nr * 10.0) as i32);
        if key == self.zone_key && self.safe_field_nav == f.nav_idx && self.safe_field.is_some() && self.home.is_some() {
            return;
        }
        let phase_changed = key != self.zone_key;
        self.zone_key = key;
        self.safe_field_nav = f.nav_idx;
        let nav = &self.navs[f.nav_idx].1;
        let inner = self.tune.zone_inner_margin.min(z.nr * 0.35);
        self.safe_field = Some(nav.dist_field_disc(z.nc[0], z.nc[1], (z.nr - inner).max(1.0), f32::INFINITY));
        if phase_changed || self.home.is_none() {
            // A holding spot inside the next circle: near its centre, near us, near a wall.
            let centre = nav.nearest_walkable(z.nc[0], z.nc[1]).unwrap_or([z.nc[0], z.nc[1], obs.me.pos[2]]);
            let mut best = (f32::INFINITY, centre);
            if z.nr > 30.0 {
                for _ in 0..24 {
                    let a = self.rng.range(-3.1416, 3.1416);
                    let r = z.nr * 0.55 * self.rng.f().sqrt();
                    let (x, y) = (z.nc[0] + r * a.cos(), z.nc[1] + r * a.sin());
                    if let Some(n) = nav.nearest_main(x, y) {
                        let p = nav.node_pos(n);
                        if !z.inside_next([p[0], p[1]], inner) {
                            continue;
                        }
                        let wall = if nav.node_clearance(n) < 9.0 { -30.0 } else { 0.0 };
                        let score = 0.5 * dist2d(p, f.pos) + 0.5 * (p[0] - z.nc[0]).hypot(p[1] - z.nc[1]) + wall;
                        if score < best.0 {
                            best = (score, p);
                        }
                    }
                }
            }
            self.home = Some(best.1);
        }
    }

    /// Walking distance to safety (inside the next circle), by the distance field.
    fn safe_dist(&self, f: &Frame) -> f32 {
        let straight = ((f.xy[0] - f.zone.nc[0]).hypot(f.xy[1] - f.zone.nc[1]) - f.zone.nr).max(0.0);
        match &self.safe_field {
            Some(field) => {
                let d = self.navs[f.nav_idx].1.field_dist(field, f.pos);
                if d.is_finite() { d } else { straight * 1.3 + 20.0 }
            }
            None => straight * 1.3,
        }
    }

    /// 0 = fine, 1 = should drift to safety when idle, 2 = go now.
    fn zone_need(&self, f: &Frame, obs: &Obs) -> u8 {
        let z = &f.zone;
        let me = &obs.me;
        if !me.in_zone {
            return 2;
        }
        let inner = self.tune.zone_inner_margin.min(z.nr * 0.35);
        let leaves = match z.leaves_at(f.xy, 5.0) {
            None => return if z.inside_next(f.xy, inner * 0.5) { 0 } else { 1 },
            Some(t) => t,
        };
        let walk = self.safe_dist(f);
        let straight = ((f.xy[0] - z.nc[0]).hypot(f.xy[1] - z.nc[1]) - z.nr).max(0.0);
        let speed = me.stats.walk_speed.max(1.0);
        let detour = (walk - straight).max(0.0) / speed;
        let margin = self.tune.zone_margin_s;
        if leaves - f.t < margin + detour * 1.2 {
            return 2;
        }
        if z.t1 - f.t < walk / speed * 1.25 + margin {
            return 2;
        }
        if leaves - f.t < 2.0 * margin + detour * 1.2 + 12.0 {
            return 1;
        }
        0
    }

    // ------------------------------------------------------------------ situation

    fn assess(&mut self, f: &Frame, obs: &Obs) -> Sit {
        let t = f.t;
        let tune = &self.tune;
        let mut sit = Sit::default();
        self.ignore.retain(|&(_, until)| until > t);
        for (ti, tr) in self.model.tracks.iter().enumerate() {
            if tr.npc {
                continue;
            }
            let age = t - tr.seen_t;
            let d = dist(tr.pos, f.pos);
            let to_us = (f.pos[1] - tr.pos[1]).atan2(f.pos[0] - tr.pos[0]);
            // "Engaged" = it is really attacking: it hit us in the last seconds, or it is
            // charging a laser down our line. A mech that merely looks our way is not an
            // enemy yet, and shooting it first would make it one.
            let aiming = tr.in_obs && tr.los && d < 290.0 && ang_diff(tr.aim[0], to_us).abs() < 0.2;
            let engaged = age < 1.5 && ((aiming && tr.charging) || t - tr.hit_us_t < tune.engaged_memory_s);
            sit.foes.push(Foe { ti, id: tr.id, pos: tr.pos, dist: d, los: tr.los && age < 0.3, age, hp: tr.hp, hp_frac: if tr.max_hp > 0.0 { tr.hp / tr.max_hp } else { 1.0 }, engaged, weapons: tr.weapons, cubes: tr.cubes });
        }
        sit.n_engaged = sit.foes.iter().filter(|x| x.engaged).count();
        // duel clocks: time it spent shooting at us, time we spent shooting at it
        for foe in &sit.foes {
            let tr = &mut self.model.tracks[foe.ti];
            if foe.engaged && foe.los {
                tr.their_time += f.dt;
            }
            if self.shot_at == Some(foe.id) && foe.los {
                tr.our_time += f.dt;
            }
        }
        sit.threat_near = sit.foes.iter().any(|x| x.age < 8.0 && x.dist < tune.threat_range && (x.los || x.engaged || x.dist < tune.avoid_range));

        // ---- target
        let mut best: Option<(f32, Foe)> = None;
        for foe in &sit.foes {
            if foe.age > 6.0 || self.ignore.iter().any(|&(id, _)| id == foe.id) && !foe.engaged {
                continue;
            }
            if tune.ffa && !foe.engaged && foe.hp_frac > 0.5 {
                continue;
            }
            let mut s = 4.0 * (1.0 - foe.hp_frac) - foe.dist / 120.0 - 0.3 * foe.age;
            if foe.hp_frac < tune.finish_hp {
                // a kill within reach beats answering a fresh attacker
                s += 2.0 + 3.0 * (tune.finish_hp - foe.hp_frac) / tune.finish_hp.max(0.05);
            }
            if foe.los {
                s += 1.5;
            }
            if foe.engaged {
                s += 2.5;
            }
            if self.target == Some(foe.id) {
                s += 0.8;
            }
            if best.map_or(true, |(bs, _)| s > bs) {
                best = Some((s, *foe));
            }
        }
        let target = best.map(|b| b.1);
        if target.map(|x| x.id) != self.target {
            self.target = target.map(|x| x.id);
            self.target_blind_since = t;
            self.fight_cover = None;
        }
        if let Some(tg) = target {
            if tg.los {
                self.target_blind_since = t;
            } else if t - self.target_blind_since > tune.target_patience_s && !tg.engaged {
                self.ignore.push((tg.id, t + 12.0));
            }
        }
        sit.target = target;

        // ---- duel estimate: how long do we last under everyone shooting at us, against
        // how long the target lasts under our fire (measured accuracy of both sides).
        let incoming: f32 = sit.foes.iter().filter(|x| x.engaged || Some(x.id) == target.map(|g| g.id)).map(|x| self.model.tracks[x.ti].their_dps()).sum();
        sit.margin = match target {
            Some(tg) => {
                let ours = self.model.tracks[tg.ti].our_dps();
                let ttd = obs.me.hp / incoming.max(5.0);
                let mut ttk = tg.hp.max(1.0) / ours.max(5.0);
                if let Some((_, g)) = f.slot(W_GUN) {
                    // the magazine will not finish it: the reload is dead time
                    if f.slots.iter().flatten().count() == 1 && g.ammo * g.stats.damage * 0.8 < tg.hp {
                        ttk += 0.6 * g.stats.reload;
                    }
                }
                ttd / ttk
            }
            None => 9.0,
        };

        // ---- fight it? Always when it attacks us. Otherwise only a fight we can keep
        // one-on-one and expect to win: it stands alone, we see it, and either it is
        // already weak or we are clearly the stronger mech (cubes, second weapon, or it
        // has shown that it cannot shoot). The squad sends its weakest first, so a mech
        // that is not hunting us is likely a better one: leave it alone.
        if let Some(tg) = target {
            let tr = &self.model.tracks[tg.ti];
            let isolated = !sit.foes.iter().any(|x| x.id != tg.id && x.age < 10.0 && dist(x.pos, tg.pos) < tune.isolation_radius);
            let weak = tg.hp_frac < tune.finish_hp;
            let special = |w: u8| if w & !W_GUN != 0 { 0.35 } else { 0.0 } + if w & W_LASER != 0 { 0.15 } else { 0.0 };
            let mine = f.slots.iter().flatten().fold(0u8, |a, s| a | s.bit);
            let my_power = 1.0 + 0.08 * obs.me.cubes.min(tune.max_cubes) + special(mine);
            let its_power = 1.0 + 0.08 * tg.cubes.min(10.0) + special(tg.weapons);
            let proven_poor = tr.their_time > 5.0 && tr.their_dps() < 45.0;
            let stronger = my_power >= its_power * tune.hunt_power_ratio || proven_poor;
            let hunt = tune.aggression > 0.0
                && isolated
                && tg.dist <= tune.hunt_range * tune.aggression.clamp(0.3, 1.0)
                && ((weak && f.hp_frac >= tune.retreat_hp + 0.1) || (stronger && tg.los && f.hp_frac >= tune.hunt_min_hp && !tune.ffa));
            sit.fight = tg.engaged || hunt;
        }
        sit.wary = !sit.fight && sit.foes.iter().any(|x| x.age < 10.0 && x.dist < tune.avoid_range);

        // ---- whom to hide from / face: whoever hurts us most, else the nearest shooter, else the target
        sit.threat = sit
            .foes
            .iter()
            .filter(|x| x.engaged)
            .max_by(|a, b| {
                let da = self.model.tracks[a.ti].dmg_to_us - a.dist * 0.2;
                let db = self.model.tracks[b.ti].dmg_to_us - b.dist * 0.2;
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied()
            .or(target)
            .or_else(|| sit.foes.iter().filter(|x| x.age < 8.0).min_by(|a, b| a.dist.partial_cmp(&b.dist).unwrap_or(std::cmp::Ordering::Equal)).copied());

        // ---- fight or flight
        let hp = f.hp_frac;
        let n = sit.n_engaged;
        let finishing = target.map_or(false, |tg| {
            (n <= 1 && tg.los && tg.hp < tune.finish_ratio * obs.me.hp) || (n <= 2 && tg.age < 1.0 && tg.hp_frac < tune.finish_hp * 0.7 && tg.dist < 80.0 && hp > 0.45)
        });
        // Retreating but still being shot for seconds: the cover is not working. Turn and
        // take the weakest one with us - in the practice run a kill is a point.
        if self.retreating && self.model.dmg_recent > 120.0 && t - self.model.last_hurt_t < 0.5 {
            if self.exposed_since < 0.0 {
                self.exposed_since = t;
            }
        } else if t - self.model.last_hurt_t > 1.0 {
            self.exposed_since = -1.0;
        }
        if self.retreating && self.exposed_since >= 0.0 && t - self.exposed_since > tune.last_stand_s {
            self.stand_until = t + 5.0;
            self.exposed_since = -1.0;
        }
        let stand = t < self.stand_until && target.is_some();
        let danger_near = sit.foes.iter().any(|x| x.age < 6.0 && x.dist < 280.0);
        let winning = sit.margin > tune.press_margin;
        let losing = sit.margin < tune.flee_margin && n >= 1;
        let outnumbered = n as f32 > tune.max_enemies && sit.margin < 2.0 * tune.press_margin;
        let want = danger_near && !finishing && (losing || (hp < tune.retreat_hp && !winning) || (n >= 2 && hp < tune.retreat_hp_outnumbered && !winning) || outnumbered);
        if stand {
            self.retreating = false;
            sit.fight = true;
        } else if self.retreating {
            let calm = !sit.foes.iter().any(|x| x.age < 6.0 && x.dist < tune.threat_range);
            // back to the fight when healed, when nobody is left, or when the numbers say we
            // win anyway (an enemy that cannot shoot is not a reason to hide)
            if (hp >= tune.reengage_hp && (n as f32) <= tune.max_enemies) || (calm && n == 0) || finishing || (winning && !outnumbered && sit.margin > 1.2 * tune.press_margin) {
                self.retreating = false;
                self.cover = None;
            }
        } else if want {
            self.retreating = true;
            self.cover = None;
            self.cover_t = -1e9;
        }
        sit
    }

    /// Range band (min, max) we want to the target.
    fn range_band(&self, f: &Frame, obs: &Obs, foe: &Foe) -> (f32, f32) {
        let tune = &self.tune;
        let finish = foe.hp_frac < tune.finish_hp && f.hp_frac > 0.35;
        let they_arc = foe.weapons & W_LIGHTNING != 0;
        let we_arc = f.slot(W_LIGHTNING).map_or(false, |(_, s)| obs.me.energy > 25.0 && s.stats.range > 0.0);
        // Lightning never misses inside its range, whoever pilots it: stay well outside
        // an enemy's arc unless we carry one too and have the HP to win the brawl.
        let arc_safe = tune.range_vs_lightning.max(self.cfg.weapons.get("lightning").range * 1.35);
        // Only once it is close anyway (hunters come to us): crossing open ground to reach
        // arc range costs more than the arc gives.
        if we_arc && (foe.dist < tune.lightning_rush_dist || finish) && (!they_arc || obs.me.hp > foe.hp * 1.15) {
            let r = f.slot(W_LIGHTNING).map_or(45.0, |(_, s)| s.stats.range);
            return (10.0, tune.range_lightning.min(r * 0.8));
        }
        if they_arc {
            let lo = tune.range_gun_min.max(arc_safe);
            return if finish { (lo, lo + 20.0) } else { (lo, tune.range_gun_max.max(lo + 45.0)) };
        }
        if finish {
            // finish it: get close enough that it cannot break away and regenerate
            return (12.0, 40.0);
        }
        (tune.range_gun_min, tune.range_gun_max)
    }

    // ------------------------------------------------------------------ movement intent

    fn drive_to(&mut self, f: &Frame, obs: &Obs, goal: V3, arrive_r: f32, climb: bool) -> Drive {
        let nav = &self.navs[f.nav_idx].1;
        self.mover.drive(nav, &obs.me, goal, arrive_r, f.t, &f.jet, climb, f.can_plan())
    }

    fn plan_move(&mut self, f: &Frame, obs: &Obs, sit: &Sit) -> Intent {
        let me = &obs.me;
        let z = f.zone;
        let home = self.home.unwrap_or(f.pos);
        let strafe_axis = sit.threat.map(|th| perp(norm2([th.pos[0] - f.xy[0], th.pos[1] - f.xy[1]])));
        let under_fire = sit.n_engaged > 0 || self.model.dmg_recent > 40.0;

        // 1. Endgame: the circle closes to nothing and every metre from its centre costs
        //    seconds of zone damage. Get to the centre and stay there.
        if z.nr <= 1.0 || z.r <= 30.0 {
            let centre = self.navs[f.nav_idx].1.nearest_walkable(z.nc[0], z.nc[1]).unwrap_or(home);
            let d = self.drive_to(f, obs, centre, 0.6, false);
            let mut it = Intent::new(Mode::Final, d, 3.0);
            it.boost = d.remaining > 12.0 && (!me.in_zone || z.leaves_at(f.xy, 3.0).map_or(false, |t| t - f.t < 4.0));
            it.no_charge = d.remaining > 6.0;
            return it;
        }

        // 2. Zone discipline.
        let need = self.zone_need(f, obs);
        if need == 2 {
            let nav = &self.navs[f.nav_idx].1;
            let step = self.safe_field.as_ref().and_then(|fl| nav.field_next(fl, f.pos, 40.0));
            let d = match step {
                Some(w) if me.grounded && !self.mover.hopping() && nav.nearest_node(f.pos).map_or(false, |n| nav.is_main(n)) => {
                    self.mover.clear_path();
                    Drive { dir: norm2([w[0] - f.xy[0], w[1] - f.xy[1]]), remaining: self.safe_dist(f), ..Default::default() }
                }
                _ => self.drive_to(f, obs, home, 3.0, false),
            };
            let late = !me.in_zone || z.leaves_at(f.xy, 5.0).map_or(false, |t| t - f.t < 3.0);
            let mut it = Intent::new(Mode::Zone, d, 2.5);
            it.boost = late && me.energy > 3.0;
            it.no_charge = true;
            if under_fire {
                it.strafe = strafe_axis.map(|a| (a, 0.4));
            }
            return it;
        }

        // 3. Retreat: break line of sight and regenerate.
        if self.retreating {
            if let Some(th) = sit.threat {
                return self.plan_retreat(f, obs, sit, th, true);
            }
        }

        // 4. Fight.
        if let Some(tg) = sit.target {
            if sit.fight && (tg.engaged || z.inside_at([tg.pos[0], tg.pos[1]], f.t + 6.0, -30.0)) {
                return self.plan_fight(f, obs, sit, tg);
            }
        }

        // 4b. Enemies about that we do not want to fight (a group, or we are not healthy):
        //     get out of their sight and let whoever hunts us come round the corner alone.
        if sit.wary && need == 0 {
            if let Some(g) = self.quick_grab(f, obs, sit, 70.0, 55.0) {
                let d = self.drive_to(f, obs, g, 0.4, false);
                return Intent::new(Mode::Loot, d, 2.0);
            }
            let near = sit.foes.iter().filter(|x| x.age < 10.0).min_by(|a, b| a.dist.partial_cmp(&b.dist).unwrap_or(std::cmp::Ordering::Equal)).copied();
            if let Some(th) = near {
                let mut it = self.plan_retreat(f, obs, sit, th, false);
                it.mode = Mode::Hold;
                it.boost = it.boost && th.dist < 70.0;
                return it;
            }
        }

        // 5. Drift into the next circle early when nothing else is pressing.
        // 6. Loot, farm, supply drops.
        if let Some(it) = self.plan_task(f, obs, sit, need == 1) {
            return it;
        }

        // 7. Nothing known worth doing. While healthy and still weak (no second weapon or
        //    few cubes), walk around inside the next circle to find loot; otherwise hold
        //    a spot near a wall inside the next circle.
        let weak = f.slots.iter().any(|s| s.is_none()) || me.cubes < self.tune.explore_until_cubes;
        let mut goal = home;
        if need == 0 && weak && f.hp_frac >= self.tune.farm_min_hp && !sit.wary && self.tune.explore_until_cubes >= 0.0 {
            let stale = self.explore.map_or(true, |(g, since)| dist2d(g, f.pos) < 6.0 || f.t - since > 25.0 || !z.inside_next([g[0], g[1]], 4.0));
            if stale {
                let nav = &self.navs[f.nav_idx].1;
                let mut best: Option<(f32, V3)> = None;
                for _ in 0..8 {
                    let a = self.rng.range(-3.1416, 3.1416);
                    let r = z.nr * 0.85 * self.rng.f().sqrt();
                    if let Some(p) = nav.nearest_walkable(z.nc[0] + r * a.cos(), z.nc[1] + r * a.sin()) {
                        let d = dist2d(p, f.pos);
                        if !(40.0..=260.0).contains(&d) || !z.inside_next([p[0], p[1]], 6.0) {
                            continue;
                        }
                        if sit.foes.iter().any(|x| x.age < 25.0 && dist2d(x.pos, p) < self.tune.avoid_range) {
                            continue;
                        }
                        // prefer ground we have not walked yet, and not too far
                        let seen = self.visited.iter().filter(|v| dist2d(**v, p) < 70.0).count() as f32;
                        let s = seen * 60.0 + (d - 120.0).abs();
                        if best.map_or(true, |(bs, _)| s < bs) {
                            best = Some((s, p));
                        }
                    }
                }
                self.explore = best.map(|b| (b.1, f.t));
                if self.visited.last().map_or(true, |v| dist2d(*v, f.pos) > 50.0) {
                    self.visited.push(f.pos);
                }
            }
            if let Some((g, _)) = self.explore {
                goal = g;
            }
        }
        let d = self.drive_to(f, obs, goal, 3.0, false);
        let mut it = Intent::new(if need == 1 { Mode::Zone } else { Mode::Hold }, d, 2.0);
        if under_fire {
            it.strafe = strafe_axis.map(|a| (a, 0.8));
        }
        it
    }

    /// A cube (or a wanted weapon) close enough to grab without walking into anyone:
    /// within `reach` of us and at least `clear` from every known mech.
    fn quick_grab(&self, f: &Frame, obs: &Obs, sit: &Sit, reach: f32, clear: f32) -> Option<V3> {
        let safe = |p: V3| !sit.foes.iter().any(|x| x.age < 8.0 && dist2d(x.pos, p) < clear) && f.zone.inside_at([p[0], p[1]], f.t + 4.0, 4.0) && (p[2] - f.pos[2]).abs() < 4.0;
        let mut best: Option<(f32, V3)> = None;
        if obs.me.cubes < self.tune.max_cubes {
            for c in &self.model.cubes {
                let d = dist2d(c.pos, f.pos);
                if d < reach && safe(c.pos) && best.map_or(true, |b| d < b.0) {
                    best = Some((d, c.pos));
                }
            }
        }
        if f.slots.iter().any(|s| s.is_none()) {
            for p in &self.model.pickups {
                let d = dist2d(p.pos, f.pos) + 10.0;
                if d < reach && !f.has(weapon_bit(&p.kind)) && !self.model.bad_pickups.contains(&p.id) && safe(p.pos) && best.map_or(true, |b| d < b.0) {
                    best = Some((d, p.pos));
                }
            }
        }
        best.map(|b| b.1)
    }

    /// Get out of `th`'s sight (and stay out). `allow_stand`: turn and fight when cornered.
    fn plan_retreat(&mut self, f: &Frame, obs: &Obs, sit: &Sit, th: Foe, allow_stand: bool) -> Intent {
        let me = &obs.me;
        let t = f.t;
        let h = f.body.height;
        let muzzle_of = |foe: &Foe| -> V3 { [foe.pos[0], foe.pos[1], foe.pos[2] + self.cfg.mech.muzzle_height] };
        let tm = muzzle_of(&th);
        let world = self.world.clone();
        let hidden_from = |p: V3, m: V3| !world.los(m, [p[0], p[1], p[2] + h * 0.5]) && !world.los(m, [p[0], p[1], p[2] + h]);
        let others: Vec<V3> = sit.foes.iter().filter(|x| x.id != th.id && x.age < 4.0 && x.dist < 250.0).map(muzzle_of).collect();
        let speed = me.stats.walk_speed.max(1.0);

        // Cover the enemy can simply walk up to is no cover: keep a distance, and well
        // outside the arc of a lightning carrier.
        let keep = if th.weapons & W_LIGHTNING != 0 { self.cfg.weapons.get("lightning").range * 1.35 + 12.0 } else { 45.0 };
        let cover_bad = match self.cover {
            Some(c) => !hidden_from(c, tm) || !f.zone.inside_at([c[0], c[1]], t + 4.0, 4.0) || (th.age < 3.0 && dist2d(c, th.pos) < keep),
            None => true,
        };
        if (cover_bad || t - self.cover_t > 1.0) && t - self.cover_t > 0.35 && f.can_plan() {
            self.cover_t = t;
            let nav = &self.navs[f.nav_idx].1;
            let cands = nav.cover_points(f.pos, tm, self.tune.cover_search, h, 40);
            let bearing = norm2([th.pos[0] - f.xy[0], th.pos[1] - f.xy[1]]);
            let side = perp(bearing);
            let mut best: Option<(f32, V3)> = None;
            for c in &cands {
                let p = c.pos;
                if !f.zone.inside_at([p[0], p[1]], t + c.path_dist / speed + 4.0, 6.0) {
                    continue;
                }
                let mut s = c.path_dist - 0.35 * dist2d(p, th.pos);
                if dist2d(p, th.pos) < keep + 8.0 {
                    s += 80.0;
                }
                // Robust cover stays cover when the enemy sidesteps.
                for sgn in [-1.0f32, 1.0] {
                    let shifted = [tm[0] + side[0] * 12.0 * sgn, tm[1] + side[1] * 12.0 * sgn, tm[2]];
                    if !hidden_from(p, shifted) {
                        s += 12.0;
                    }
                }
                for o in &others {
                    if !hidden_from(p, *o) {
                        s += 45.0;
                    }
                }
                if let Some(cur) = self.cover {
                    if dist2d(cur, p) < 4.0 {
                        s -= 12.0;
                    }
                }
                if best.map_or(true, |(bs, _)| s < bs) {
                    best = Some((s, p));
                }
            }
            self.cover = best.map(|b| b.1);
        }

        let exposed = th.los;
        let strafe_axis = perp(norm2([th.pos[0] - f.xy[0], th.pos[1] - f.xy[1]]));
        // Cornered: a close enemy with a clear shot and no cover right here. Running at the
        // same speed never breaks its line of sight; shooting back is the better trade.
        let cornered = allow_stand && exposed && th.dist < 45.0;
        match self.cover {
            Some(c) => {
                let d = self.drive_to(f, obs, c, 1.0, false);
                if cornered && d.remaining > 16.0 {
                    return self.plan_fight(f, obs, sit, th);
                }
                let mut it = Intent::new(Mode::Retreat, d, 2.5);
                it.boost = exposed && me.energy > 4.0 && d.remaining > 6.0;
                it.no_charge = exposed;
                it.hold_fire = !exposed;
                if exposed {
                    it.strafe = Some((strafe_axis, 0.35));
                }
                it
            }
            None => {
                if cornered {
                    return self.plan_fight(f, obs, sit, th);
                }
                // No cover in reach: run away from the threat, staying in the zone.
                let home = self.home.unwrap_or(f.pos);
                let away = norm2([f.xy[0] - th.pos[0], f.xy[1] - th.pos[1]]);
                let to_home = norm2([home[0] - f.xy[0], home[1] - f.xy[1]]);
                let goal = if dot2(away, to_home) > -0.2 && dist2d(home, f.pos) > 25.0 {
                    home
                } else {
                    let mut g = [f.xy[0] + away[0] * 70.0, f.xy[1] + away[1] * 70.0];
                    if !f.zone.inside_at(g, t + 8.0, 10.0) {
                        g = [f.xy[0] + (away[0] + to_home[0]) * 40.0, f.xy[1] + (away[1] + to_home[1]) * 40.0];
                    }
                    self.navs[f.nav_idx].1.nearest_walkable(g[0], g[1]).unwrap_or(home)
                };
                let d = self.drive_to(f, obs, goal, 3.0, false);
                let mut it = Intent::new(Mode::Retreat, d, 2.0);
                it.boost = exposed && me.energy > 4.0;
                it.no_charge = true;
                it.hold_fire = !exposed;
                if exposed {
                    it.strafe = Some((strafe_axis, 0.5));
                }
                it
            }
        }
    }

    fn plan_fight(&mut self, f: &Frame, obs: &Obs, sit: &Sit, tg: Foe) -> Intent {
        let me = &obs.me;
        let t = f.t;
        let (rmin, rmax) = self.range_band(f, obs, &tg);
        let bearing = norm2([tg.pos[0] - f.xy[0], tg.pos[1] - f.xy[1]]);
        let side = perp(bearing);
        let sw = self.tune.strafe_weight;
        let has_lightning = f.has(W_LIGHTNING);

        // Step behind cover while the gun reloads (or the laser recovers and the gun is dry).
        if self.tune.tuck_reload_dist > 0.0 && tg.los {
            let gun_down = f.slot(W_GUN).map_or(true, |(_, s)| s.reload > 0.8 || (s.ammo <= 0.0 && s.reload > 0.0));
            let special_down = f.slots.iter().flatten().filter(|s| s.bit != W_GUN).all(|s| s.cooldown > 0.6 || (s.bit == W_LIGHTNING && tg.dist > s.stats.range));
            if gun_down && special_down {
                if t - self.fight_cover_t > 0.5 && f.can_plan() {
                    self.fight_cover_t = t;
                    let tm = [tg.pos[0], tg.pos[1], tg.pos[2] + self.cfg.mech.muzzle_height];
                    let nav = &self.navs[f.nav_idx].1;
                    self.fight_cover = nav.cover_points(f.pos, tm, self.tune.tuck_reload_dist, f.body.height, 1).first().map(|c| (c.pos, c.path_dist));
                }
                if let Some((c, _)) = self.fight_cover {
                    if f.zone.inside_at([c[0], c[1]], t + 3.0, 4.0) {
                        let d = self.drive_to(f, obs, c, 0.8, false);
                        let mut it = Intent::new(Mode::Fight, d, 2.2);
                        it.strafe = Some((side, 0.3 * sw));
                        return it;
                    }
                }
            }
        }

        let chase = tg.hp_frac < self.tune.finish_hp && f.hp_frac > 0.35 && (tg.dist > rmax || !tg.los) && me.energy > 12.0;
        let close_in = false;
        if !tg.los {
            // Close in on where it is until we have a line of fire.
            let d = self.drive_to(f, obs, tg.pos, (rmin * 0.6).max(8.0), false);
            let mut it = Intent::new(Mode::Fight, d, 1.6);
            if tg.engaged || sit.n_engaged > 0 {
                it.strafe = Some((side, 0.4 * sw));
            }
            it.boost = chase || close_in;
            return it;
        }
        if tg.dist > rmax {
            let d = self.drive_to(f, obs, tg.pos, rmax * 0.8, false);
            let mut it = Intent::new(Mode::Fight, d, 1.0);
            it.strafe = Some((side, 0.8 * sw));
            it.boost = chase || close_in;
            return it;
        }
        let mut it = Intent::new(Mode::Fight, Drive::default(), 0.0);
        it.strafe = Some((side, sw));
        if tg.dist >= rmin {
            // Cubes lying around (from the last kill): drift over them while we fight.
            if let Some(g) = self.quick_grab(f, obs, sit, 45.0, rmin * 0.85) {
                it.drive = self.drive_to(f, obs, g, 0.4, false);
                it.base_w = 1.3;
                it.strafe = Some((side, 0.5 * sw));
                return it;
            }
        }
        if tg.dist < rmin {
            // kite: back off at full speed (the torso keeps facing and firing), harder the
            // deeper it is inside our minimum range
            let deep = tg.dist < rmin * 0.8;
            it.away = Some(([-bearing[0], -bearing[1]], if deep { 2.2 } else { 1.4 }));
            it.strafe = Some((side, if deep { 0.4 * sw } else { 0.7 * sw }));
            it.boost = deep && tg.weapons & W_LIGHTNING != 0 && me.energy > 10.0;
            it.no_charge = deep;
        } else if has_lightning {
            // keep pressing inside arc range
            it.away = Some((bearing, 0.3));
        }
        it
    }

    // ------------------------------------------------------------------ steering

    /// Final leg command: blend the intent with strafing and projectile dodging, check
    /// walls, apply stuck recovery. Returns (move, boost, jump).
    fn steer(&mut self, f: &Frame, obs: &Obs, sit: &Sit, intent: &Intent) -> (V2, bool, bool) {
        let me = &obs.me;
        let t = f.t;
        let d = intent.drive;
        if d.hard {
            // hop or wait for energy: exact control
            let _ = self.mover.unstick(me, [0.0, 0.0], true, t, f.dt, self.tune.stuck_time_s, &mut self.rng);
            return (d.dir, d.jet, d.jump);
        }

        // jink timer
        if t >= self.strafe_until {
            self.strafe_sign = -self.strafe_sign;
            if self.rng.f() < 0.25 {
                self.strafe_sign = -self.strafe_sign; // sometimes keep going: unpredictable
            }
            self.strafe_until = t + self.rng.range(self.tune.strafe_period_min, self.tune.strafe_period_max);
        }

        // strafing into a wall: turn round at once
        if let Some((axis, _)) = intent.strafe {
            if me.grounded {
                let s = self.strafe_sign;
                let probe = [f.xy[0] + axis[0] * s * 5.0, f.xy[1] + axis[1] * s * 5.0];
                if self.world.walk_clear(f.pos, probe, &f.slim, 3.0).is_none() {
                    self.strafe_sign = -s;
                }
            }
        }
        let shells = self.shell_impacts(f, obs);
        let has_threats = !obs.projectiles.is_empty();
        let plain = intent.strafe.is_none() && intent.away.is_none() && !has_threats;
        let mut cmd = d.dir;
        if !plain && me.grounded {
            let speed = if intent.boost { me.stats.boost_speed } else { me.stats.walk_speed };
            let accel = me.stats.ground_accel.max(1.0);
            let world = &*self.world;
            let cur_dir = norm2([f.vel[0], f.vel[1]]);
            let mut best = (f32::NEG_INFINITY, d.dir);
            let n = 16;
            for k in 0..=n {
                let c: V2 = if k == n {
                    [0.0, 0.0]
                } else {
                    let a = k as f32 * std::f32::consts::TAU / n as f32;
                    [a.cos(), a.sin()]
                };
                let mut s = intent.base_w * dot2(c, d.dir) + 0.12 * dot2(c, cur_dir);
                if let Some((axis, w)) = intent.strafe {
                    s += w * self.strafe_sign * dot2(c, axis);
                }
                if let Some((dir, w)) = intent.away {
                    s += w * dot2(c, dir);
                }
                if k == n {
                    // standing still is only right when we have arrived and nothing is incoming
                    s -= if intent.strafe.is_some() || intent.away.is_some() { 0.6 } else { 0.0 };
                } else {
                    // walls and ledges
                    let probe = [f.xy[0] + c[0] * 4.5, f.xy[1] + c[1] * 4.5];
                    if world.walk_clear(f.pos, probe, &f.slim, 3.0).is_none() {
                        s -= 2.5;
                    }
                    // do not dodge out of the zone
                    let ahead = [f.xy[0] + c[0] * 12.0, f.xy[1] + c[1] * 12.0];
                    if !f.zone.inside_at(ahead, t + 2.0, 4.0) {
                        s -= 3.0;
                    }
                }
                if has_threats {
                    s -= self.tune.dodge_weight * incoming_hits(f, obs, c, speed, accel, &shells, &self.cfg);
                }
                if s > best.0 {
                    best = (s, c);
                }
            }
            cmd = best.1;
            // Walking with the path: keep the exact path direction when it scored as well as its neighbours.
            if intent.base_w > 0.0 && dot2(cmd, d.dir) > 0.98 {
                cmd = d.dir;
            }
        }

        // stuck recovery (only when we are trying to get somewhere, not while strafing on the spot)
        let travelling = intent.base_w > 0.0 && len2(d.dir) > 0.5;
        if let Some(r) = self.mover.unstick(me, cmd, !travelling, t, f.dt, self.tune.stuck_time_s, &mut self.rng) {
            return (r.dir, r.jet, r.jump);
        }

        // boost: asked for by the intent, or cruising on spare energy over long quiet legs
        let reserve = if f.has(W_LIGHTNING) { 65.0 } else { self.tune.energy_reserve };
        if self.boosting {
            if me.energy < self.tune.boost_travel_stop.max(reserve) || d.remaining < 30.0 || sit.threat_near {
                self.boosting = false;
            }
        } else if me.energy >= self.tune.boost_travel_start.min(me.max_energy - 1.0) && d.remaining > 60.0 && !sit.threat_near {
            self.boosting = true;
        }
        let boost = me.grounded && len2(cmd) > 0.3 && (intent.boost || self.boosting);
        (cmd, boost, false)
    }

    /// Where and when visible mortar shells will land.
    fn shell_impacts(&self, f: &Frame, obs: &Obs) -> Vec<(V3, f32)> {
        let mut out = Vec::new();
        let g = self.cfg.weapons.get("mortar").gravity;
        for p in &obs.projectiles {
            if p.kind != "mortar" || dist(p.pos, f.pos) > 260.0 {
                continue;
            }
            let (mut pos, mut vel) = (p.pos, p.vel);
            let dt = 0.05;
            let mut tt = 0.0;
            while tt < 6.0 {
                let next = [pos[0] + vel[0] * dt, pos[1] + vel[1] * dt, pos[2] + vel[2] * dt - 0.5 * g * dt * dt];
                if !self.world.los(pos, next) {
                    break;
                }
                pos = next;
                vel[2] -= g * dt;
                tt += dt;
            }
            if out.len() < 8 {
                out.push((pos, tt));
            }
        }
        out
    }

    // ------------------------------------------------------------------ upgrades

    fn pick_upgrade(&mut self, f: &Frame, obs: &Obs) -> Option<u32> {
        let offer = obs.offer.as_ref()?;
        let mut best = (f32::NEG_INFINITY, 0u32);
        for (i, c) in offer.choices.iter().enumerate() {
            let mut w = crate::config::upgrade_weight(&c.id);
            // weapon upgrades only count for a weapon we hold
            if let Some(def) = self.cfg.upgrades.iter().find(|u| u.id == c.id) {
                if let Some(req) = &def.requires {
                    if !f.has(weapon_bit(req)) {
                        w -= 6.0;
                    }
                }
            }
            if w > best.0 {
                best = (w, i as u32);
            }
        }
        Some(best.1)
    }

    fn debug_draw(&self, f: &Frame, sit: &Sit, intent: &Intent) -> DebugDraw {
        let mut d = DebugDraw { label: Some(format!("{:?} hp{:.0}% e{}", intent.mode, f.hp_frac * 100.0, sit.n_engaged)), ..Default::default() };
        let mut prev = f.pos;
        for p in self.mover.path_points().iter().take(20) {
            d.lines.push(DebugLine { a: [prev[0], prev[1], prev[2] + 1.0], b: [p[0], p[1], p[2] + 1.0], color: "#40c0ff" });
            prev = *p;
        }
        if let Some(tg) = sit.target {
            d.lines.push(DebugLine { a: f.muzzle, b: [tg.pos[0], tg.pos[1], tg.pos[2] + 4.5], color: "#ff5040" });
        }
        if let Some(h) = self.home {
            d.points.push(DebugPoint { at: h, color: "#ffd060", size: 6.0 });
        }
        d
    }
}

/// Expected hits (weighted) from visible projectiles if we walk in direction `c` for the
/// next second. Our motion is integrated with the ground acceleration limit.
fn incoming_hits(f: &Frame, obs: &Obs, c: V2, speed: f32, accel: f32, shells: &[(V3, f32)], cfg: &Config) -> f32 {
    let dt = 0.05f32;
    let steps = 20;
    let r = f.body.radius + cfg_proj_radius() + 0.6;
    let h = f.body.height + 0.5;
    let want = [c[0] * speed, c[1] * speed];
    let mut pos = f.pos;
    let mut vel = [f.vel[0], f.vel[1]];
    let mut hits = 0.0f32;
    let mut done = [false; 48];
    let mut landed = [false; 8];
    let splash = cfg.weapons.get("mortar").splash_radius;
    for k in 0..steps {
        let t0 = k as f32 * dt;
        // accelerate towards the wanted velocity
        let dv = [want[0] - vel[0], want[1] - vel[1]];
        let l = len2(dv);
        let step = (accel * dt).min(l);
        if l > 1e-6 {
            vel = [vel[0] + dv[0] / l * step, vel[1] + dv[1] / l * step];
        }
        for (i, p) in obs.projectiles.iter().enumerate().take(48) {
            if done[i] || p.kind == "mortar" {
                continue;
            }
            let pp = [p.pos[0] + p.vel[0] * t0, p.pos[1] + p.vel[1] * t0, p.pos[2] + p.vel[2] * t0];
            let rel = sub(pp, pos);
            if rel[0].abs() > 40.0 || rel[1].abs() > 40.0 {
                continue;
            }
            let rv = [p.vel[0] - vel[0], p.vel[1] - vel[1], p.vel[2]];
            if crate::ballistics::hits_cylinder([rel[0], rel[1], rel[2] + 0.25], rv, r, h, dt).is_some() {
                done[i] = true;
                hits += if p.kind == "npc" { 0.5 } else { 1.0 };
            }
        }
        pos = [pos[0] + vel[0] * dt, pos[1] + vel[1] * dt, pos[2]];
        for (i, (at, when)) in shells.iter().enumerate().take(8) {
            if !landed[i] && *when <= t0 + dt {
                landed[i] = true;
                let d = dist2d(*at, pos);
                let reach = splash + f.body.radius + 1.0;
                if d < reach {
                    hits += 3.0 * (1.0 - 0.6 * d / reach);
                }
            }
        }
    }
    // shells that land after our one-second horizon: stay out of their circle
    for (i, (at, when)) in shells.iter().enumerate().take(8) {
        if !landed[i] && *when < 2.5 {
            let d = dist2d(*at, pos);
            let reach = splash + f.body.radius + 1.0;
            if d < reach {
                hits += 1.5 * (1.0 - 0.6 * d / reach);
            }
        }
    }
    hits
}

fn cfg_proj_radius() -> f32 {
    0.3
}

/// A flat, empty stand-in world for when the map files cannot be read.
fn flat_world(cfg: &Config) -> World {
    let c = cfg.zone.start_center;
    let r = cfg.zone.start_radius.max(100.0) + 60.0;
    let json = format!(
        "{{\"bounds\":{{\"minX\":{},\"maxX\":{},\"minY\":{},\"maxY\":{}}},\"terrain\":{{\"minX\":{},\"minY\":{},\"step\":{},\"nx\":2,\"ny\":2}},\"prisms\":[]}}",
        c[0] - r, c[0] + r, c[1] - r, c[1] + r, c[0] - r - 10.0, c[1] - r - 10.0, 2.0 * r + 20.0
    );
    World::from_bytes(json.as_bytes(), &[0u8; 8]).expect("flat world")
}
