//! Quiet-time work: weapon pickups, cubes, crates, NPC drones, supply drops.
//! Each candidate has a value (from [`Tuning`](crate::config::Tuning)) divided by the
//! time it costs; the best one becomes the current task, with hysteresis.
use crate::brain::*;
use crate::mathx::*;
use crate::model::*;
use crate::mover::Drive;
use crate::proto::*;
use world::max_climb;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TaskKind {
    Pickup(i64),
    Cube(i64),
    Crate(i64),
    Npc(i64),
    Drop(i64),
}

#[derive(Clone, Copy, Debug)]
pub struct Task {
    pub kind: TaskKind,
    pub pos: V3,
    pub score: f32,
    /// Needs a climb (roof, deck, enclosed yard).
    pub climb: bool,
}

impl Bot {
    /// How much we want a weapon kind, given what the enemies we know about carry
    /// (the weapon triangle: laser > mortar > lightning > laser).
    pub(crate) fn weapon_value(&self, bit: u8, sit: &Sit) -> f32 {
        let t = &self.tune;
        let mut seen = 0u8;
        for foe in sit.foes.iter().filter(|x| x.age < 20.0) {
            seen |= foe.weapons;
        }
        match bit {
            W_LASER => t.w_laser + if seen & W_MORTAR != 0 { 1.0 } else { 0.0 } - if seen & W_LIGHTNING != 0 { 0.5 } else { 0.0 },
            W_LIGHTNING => t.w_lightning + if seen & W_LASER != 0 { 1.5 } else { 0.0 } - if seen & W_MORTAR != 0 { 0.5 } else { 0.0 },
            // A carried mortar makes enemy lasers hit 60% harder (cook-off).
            W_MORTAR => t.w_mortar + if seen & W_LIGHTNING != 0 { 1.0 } else { 0.0 } - if seen & W_LASER != 0 { 1.5 } else { 0.0 },
            W_GUN => 8.0,
            _ => 0.0,
        }
    }

    /// Value of taking pickup kind `bit` now (0 = not wanted), and the slot to drop for it.
    fn pickup_gain(&self, f: &Frame, sit: &Sit, bit: u8) -> (f32, Option<u32>) {
        if bit == 0 || f.has(bit) {
            return (0.0, None);
        }
        if f.slots.iter().any(|s| s.is_none()) {
            return (self.weapon_value(bit, sit), None);
        }
        if bit == W_GUN {
            return (0.0, None);
        }
        // Both slots full: swap out the weaker special (never the gun).
        let mut worst: Option<(f32, usize)> = None;
        for (i, s) in f.slots.iter().enumerate() {
            if let Some(s) = s {
                if s.bit != W_GUN && !(s.bit == W_LASER && s.charge > 0.0) {
                    let v = self.weapon_value(s.bit, sit);
                    if worst.map_or(true, |(wv, _)| v < wv) {
                        worst = Some((v, i));
                    }
                }
            }
        }
        match worst {
            Some((v, i)) => ((self.weapon_value(bit, sit) - v - 1.0).max(0.0), Some(i as u32)),
            None => (0.0, None),
        }
    }

    /// Score of going to `pos` for `value`: value per second, or None if out of reach.
    fn eval_task(&self, f: &Frame, obs: &Obs, sit: &Sit, pos: V3, value: f32, extra_s: f32) -> Option<(f32, bool)> {
        self.eval_task_in(f, obs, sit, pos, value, extra_s, self.tune.loot_range, self.tune.avoid_range * 0.9)
    }

    /// As [`Self::eval_task`], with the search radius and the distance to keep from
    /// mechs we are not fighting given explicitly (a laser is worth a longer, bolder trip).
    #[allow(clippy::too_many_arguments)]
    fn eval_task_in(&self, f: &Frame, obs: &Obs, sit: &Sit, pos: V3, value: f32, extra_s: f32, range: f32, keep: f32) -> Option<(f32, bool)> {
        let d = dist2d(pos, f.pos);
        if d > range || value <= 0.0 {
            return None;
        }
        // not next to a mech we are not fighting
        let fighting = if sit.fight { sit.target.map(|x| x.id) } else { None };
        if d > 12.0 && sit.foes.iter().any(|x| x.age < 20.0 && Some(x.id) != fighting && dist2d(x.pos, pos) < keep) {
            return None;
        }
        let me = &obs.me;
        let speed = me.stats.walk_speed.max(1.0);
        let mut time = d * 1.3 / speed + extra_s + 3.0;
        let mut value = value;
        let mut climb = false;
        let nav = self.nav(f.nav_idx);
        if d > 3.0 {
            let n = nav.nearest_node(pos)?;
            let np = nav.node_pos(n);
            if dist2d(np, pos) > 6.0 {
                return None; // nowhere to stand near it
            }
            if !nav.is_main(n) {
                // on a roof, a deck or in an enclosed yard: needs a hop
                let here = nav.nearest_node(f.pos).map(|m| nav.comp_of(m));
                if here != Some(nav.comp_of(n)) {
                    let reg = nav.region_of(n)?;
                    let reach = 0.8 * max_climb(me.max_energy, &f.jet);
                    let a = reg.access.iter().filter(|a| a.climb < reach).map(|a| a.climb).fold(f32::INFINITY, f32::min);
                    if !a.is_finite() || a > self.tune.max_loot_climb {
                        return None;
                    }
                    time += a / 5.0 + 5.0;
                    value *= 0.85;
                    climb = true;
                }
            }
        }
        let p2 = [pos[0], pos[1]];
        if !f.zone.inside_at(p2, f.t + time + 4.0, 6.0) {
            return None;
        }
        // Outside the next circle: only while the closing circle is still well away from
        // it when we get there. (Judged at the thing, never at where we stand: a rule
        // that flips as we walk has us pacing up and down across the line.)
        if !f.zone.inside_next(p2, 5.0) && f.zone.leaves_at(p2, 6.0).map_or(false, |tl| tl - f.t < time + self.tune.zone_margin_s) {
            return None;
        }
        Some((value / time, climb))
    }

    /// The laser lying on the ground that we want and can go for (we hold none): its
    /// place and whether it needs a climb.
    pub(crate) fn wanted_laser(&self, f: &Frame, obs: &Obs, sit: &Sit) -> Option<(V3, bool)> {
        if f.has(W_LASER) {
            return None;
        }
        let (gain, _) = self.pickup_gain(f, sit, W_LASER);
        if gain <= 0.5 {
            return None;
        }
        let mut best: Option<(f32, V3, bool)> = None;
        for p in self.model.pickups.iter().filter(|p| p.kind == "laser" && !self.model.bad_pickups.contains(&p.id)) {
            if let Some((score, climb)) = self.eval_task_in(f, obs, sit, p.pos, gain, 0.5, self.tune.laser_range, self.tune.laser_keep) {
                if best.map_or(true, |b| score > b.0) {
                    best = Some((score, p.pos, climb));
                }
            }
        }
        best.map(|b| (b.1, b.2))
    }

    /// Debug aid: why are we not holding the laser we know about?
    pub(crate) fn explain_laser(&self, f: &Frame, obs: &Obs, sit: &Sit) -> String {
        let mut out = String::new();
        for p in self.model.pickups.iter().filter(|p| p.kind == "laser") {
            let (gain, _) = self.pickup_gain(f, sit, W_LASER);
            let nav = self.nav(f.nav_idx);
            let main = nav.nearest_node(p.pos).map(|n| nav.is_main(n));
            let foe = sit.foes.iter().filter(|x| x.age < 20.0 && dist2d(x.pos, p.pos) < self.tune.avoid_range * 0.9).map(|x| x.id).collect::<Vec<_>>();
            out += &format!(" [laser {} d={:.0} dz={:.0} gain={:.1} eval={:?} main={:?} foes_near={:?} bad={}]", p.id, dist2d(p.pos, f.pos), p.pos[2] - f.pos[2], gain, self.eval_task(f, obs, sit, p.pos, gain, 0.5), main, foe, self.model.bad_pickups.contains(&p.id));
        }
        out
    }

    fn task_valid(&self, task: &Task, obs: &Obs) -> bool {
        match task.kind {
            TaskKind::Pickup(id) => self.model.pickups.iter().any(|p| p.id == id) && !self.model.bad_pickups.contains(&id),
            TaskKind::Cube(id) => self.model.cubes.iter().any(|c| c.id == id),
            TaskKind::Crate(id) => self.model.crates.iter().any(|c| c.id == id),
            TaskKind::Npc(id) => self.model.tracks.iter().any(|t| t.id == id && self.model.t - t.seen_t < 6.0),
            TaskKind::Drop(id) => obs.drops.iter().any(|d| d.id == id),
        }
    }

    fn choose_task(&mut self, f: &Frame, obs: &Obs, sit: &Sit) {
        let t = f.t;
        let tune = &self.tune;
        let me = &obs.me;
        self.task_black.retain(|&(_, until)| until > t);
        let farm_ok = f.hp_frac >= tune.farm_min_hp && !sit.threat_near;
        let want_cubes = me.cubes < tune.max_cubes;
        let gun_dps = f.slot(W_GUN).map_or(100.0, |(_, s)| s.stats.damage / s.stats.cooldown.max(0.05) * 0.75 * f.dmg_scale);
        let mut best: Option<Task> = None;
        let mut offer = |kind: TaskKind, pos: V3, r: Option<(f32, bool)>| {
            // The task we are on is judged leniently (a mech drifting to just inside the
            // keep-away distance of our goal must not send us back and forth).
            let current = self.task.map_or(false, |c| c.kind == kind);
            let r = if r.is_none() && current {
                let prize = matches!(kind, TaskKind::Pickup(id) if self.model.pickups.iter().any(|p| p.id == id && p.kind == "laser"));
                let (range, keep) = if prize { (tune.laser_range, tune.laser_keep) } else { (tune.loot_range, tune.avoid_range * 0.9) };
                self.eval_task_in(f, obs, sit, pos, self.task.map_or(1.0, |c| c.score.max(0.05)), 0.0, range * 1.25, keep * 0.55).map(|(_, climb)| (self.task.map_or(0.1, |c| c.score / 1.35), climb))
            } else {
                r
            };
            if let Some((score, climb)) = r {
                if self.task_black.iter().any(|(k, _)| *k == kind) {
                    return;
                }
                let score = if current { score * 1.35 } else { score };
                if best.map_or(true, |b| score > b.score) {
                    best = Some(Task { kind, pos, score, climb });
                }
            }
        };
        for p in &self.model.pickups {
            if self.model.bad_pickups.contains(&p.id) {
                continue;
            }
            let (gain, _) = self.pickup_gain(f, sit, weapon_bit(&p.kind));
            if gain > 0.5 {
                let r = if p.kind == "laser" { self.eval_task_in(f, obs, sit, p.pos, gain, 0.5, tune.laser_range, tune.laser_keep) } else { self.eval_task(f, obs, sit, p.pos, gain, 0.5) };
                offer(TaskKind::Pickup(p.id), p.pos, r);
            }
        }
        if want_cubes {
            for c in &self.model.cubes {
                offer(TaskKind::Cube(c.id), c.pos, self.eval_task(f, obs, sit, c.pos, tune.v_cube, 0.0));
            }
        }
        // No laser and none known: looking for one may be worth more than farming.
        let searching = !f.has(W_LASER) && !self.model.pickups.iter().any(|p| p.kind == "laser");
        let farm_mult = if searching { tune.nolaser_farm_mult } else { 1.0 };
        if farm_ok {
            let special_full = f.slots.iter().all(|s| s.is_some());
            for c in &self.model.crates {
                let v = farm_mult * tune.v_crate * if want_cubes { 1.0 } else if special_full { 0.0 } else { 0.5 };
                offer(TaskKind::Crate(c.id), c.pos, self.eval_task(f, obs, sit, c.pos, v, c.hp / gun_dps + 2.0));
            }
            if want_cubes {
                for tr in self.model.tracks.iter().filter(|x| x.npc && t - x.seen_t < 4.0) {
                    offer(TaskKind::Npc(tr.id), tr.pos, self.eval_task(f, obs, sit, tr.pos, farm_mult * tune.v_npc, tr.hp / gun_dps + 3.0));
                }
            }
        }
        for d in &obs.drops {
            let travel = dist2d(d.pos, f.pos) * 1.3 / me.stats.walk_speed.max(1.0);
            let contested = sit.foes.iter().any(|x| x.age < 5.0 && dist2d(x.pos, d.pos) < 60.0);
            let v = tune.v_drop * (1.0 + (1.0 - f.hp_frac)) * if contested { 0.3 } else { 1.0 };
            let wait = (d.lands_in - travel).max(0.0) + d.capture_s;
            offer(TaskKind::Drop(d.id), d.pos, self.eval_task(f, obs, sit, d.pos, v, wait));
        }
        if best.map(|b| b.kind) != self.task.map(|b| b.kind) {
            self.task_started = t;
        }
        self.task = best;
    }

    /// Movement for the current task, or None when there is nothing worth doing.
    pub(crate) fn plan_task(&mut self, f: &Frame, obs: &Obs, sit: &Sit) -> Option<Intent> {
        let t = f.t;
        let valid = self.task.map_or(false, |k| self.task_valid(&k, obs));
        if !valid || t - self.task_t > 0.5 {
            self.task_t = t;
            if !valid {
                self.task = None;
            }
            self.choose_task(f, obs, sit);
        }
        let task = self.task?;
        // A task that drags on is out of reach in a way the map did not show: drop it for a while.
        if t - self.task_started > 40.0 + dist2d(task.pos, f.pos) / 8.0 && !matches!(task.kind, TaskKind::Drop(_)) {
            self.task_black.push((task.kind, t + 60.0));
            self.task = None;
            return None;
        }
        let world = self.world.clone();
        let mut it = match task.kind {
            TaskKind::Pickup(_) => Intent::new(Mode::Loot, self.drive_to_task(f, obs, task.pos, 1.0, task.climb), 2.0),
            TaskKind::Cube(_) => Intent::new(Mode::Loot, self.drive_to_task(f, obs, task.pos, 0.4, task.climb), 2.0),
            TaskKind::Crate(id) => {
                let c = self.model.crates.iter().find(|c| c.id == id)?.pos;
                let aim_pt = [c[0], c[1], c[2] + self.cfg.crates.height * 0.5];
                let d = dist(c, f.pos);
                let los = world.los(f.muzzle, aim_pt);
                let close = if f.has(W_LIGHTNING) { 30.0 } else { 45.0 };
                let mut it = if los && d < close {
                    Intent::new(Mode::Loot, Drive { arrived: true, ..Default::default() }, 0.0)
                } else {
                    Intent::new(Mode::Loot, self.drive_to_task(f, obs, c, 5.0, task.climb), 2.0)
                };
                if los && d < 110.0 {
                    it.farm = Some(Farm::Crate(id));
                }
                it
            }
            TaskKind::Npc(id) => {
                let tr = self.model.tracks.iter().find(|x| x.id == id)?;
                let (p, los) = (tr.pos, tr.los);
                let d = dist(p, f.pos);
                let want = if f.has(W_LIGHTNING) { 26.0 } else { self.cfg.npc.aggro_range + 12.0 };
                let mut it = if los && d < want + 6.0 {
                    let mut it = Intent::new(Mode::Loot, Drive::default(), 0.0);
                    let bearing = norm2([p[0] - f.xy[0], p[1] - f.xy[1]]);
                    it.strafe = Some((perp(bearing), 0.6));
                    if d < want - 14.0 {
                        it.away = Some(([-bearing[0], -bearing[1]], 0.6));
                    }
                    it
                } else {
                    Intent::new(Mode::Loot, self.drive_to_task(f, obs, p, want * 0.8, false), 2.0)
                };
                if los && d < 130.0 {
                    it.farm = Some(Farm::Npc(id));
                }
                it
            }
            TaskKind::Drop(id) => {
                let d = obs.drops.iter().find(|d| d.id == id)?;
                let r = if d.radius > 0.0 { d.radius } else { self.cfg.drops.radius };
                Intent::new(Mode::Loot, self.drive_to_task(f, obs, d.pos, r * 0.4, false), 2.0)
            }
        };
        // Shot at by someone we cannot see: weave across the line of fire.
        if self.model.dmg_recent > 40.0 && t - self.model.last_hurt_t < 2.0 {
            if let Some(b) = self.model.last_hurt_bearing {
                it.strafe = Some((perp([b.cos(), b.sin()]), 0.6));
            }
        }
        Some(it)
    }

    fn drive_to_task(&mut self, f: &Frame, obs: &Obs, goal: V3, arrive_r: f32, climb: bool) -> Drive {
        let nav = &self.navs[f.nav_idx].1;
        self.mover.drive(nav, &obs.me, goal, arrive_r, f.t, &f.jet, climb, f.can_plan())
    }

    /// `take` / `drop` for a wanted pickup in reach.
    pub(crate) fn loot_actions(&mut self, f: &Frame, _obs: &Obs, sit: &Sit) -> (Option<i64>, Option<u32>) {
        let t = f.t;
        if t - self.take_t < 0.25 {
            return (None, None);
        }
        let reach = (self.cfg.loot.pickup_radius - 0.3).max(1.0);
        let mut best: Option<(f32, i64, Option<u32>)> = None;
        for p in &self.model.pickups {
            if self.model.bad_pickups.contains(&p.id) || dist2d(p.pos, f.pos) > reach || (p.pos[2] - f.pos[2]).abs() > 3.5 {
                continue;
            }
            let (gain, drop) = self.pickup_gain(f, sit, weapon_bit(&p.kind));
            if gain > 0.5 && best.map_or(true, |b| gain > b.0) {
                best = Some((gain, p.id, drop));
            }
        }
        let Some((_, id, drop)) = best else { return (None, None) };
        self.take_t = t;
        if self.take_tries.0 == id {
            self.take_tries.1 += 1;
            if self.take_tries.1 > 8 {
                self.model.bad_pickups.push(id);
            }
        } else {
            self.take_tries = (id, 1);
        }
        (Some(id), drop)
    }
}
