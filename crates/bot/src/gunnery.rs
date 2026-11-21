//! Aim, fire and reload. Shots leave along the *actual* torso aim, which slews towards
//! the commanded aim at `turn_rate` / `pitch_rate`: we command the solution and pull a
//! trigger only when the actual aim will be on it.
use crate::ballistics::*;
use crate::brain::*;
use crate::mathx::*;
use crate::model::*;
use crate::proto::*;
use world::World;

/// Something to shoot at.
#[derive(Clone, Copy, Debug)]
pub struct Tgt {
    pub id: i64,
    pub feet: V3,
    pub vel: V3,
    pub vel_s: V3,
    pub height: f32,
    pub radius: f32,
    pub hp: f32,
    pub mech: bool,
    pub weapons: u8,
    /// Clear line of fire now.
    pub los: bool,
    /// Its position is current (listed by the server this tick).
    pub live: bool,
    /// Last time we had a clear line of fire to it.
    pub fire_los_t: f32,
}

/// Is the actual aim close enough to `want`, allowing for the slew that happens before
/// the shot leaves? `tol` = (yaw, pitch) tolerances in radians.
pub fn aim_settled(actual: V2, want: V2, tol: V2, turn_rate: f32, pitch_rate: f32, slew_s: f32) -> bool {
    let ey = (ang_diff(want[0], actual[0]).abs() - turn_rate * slew_s).max(0.0);
    let ep = ((want[1] - actual[1]).abs() - pitch_rate * slew_s).max(0.0);
    ey <= tol[0] && ep <= tol[1]
}

/// Angular half-size of a target at distance `d`, scaled.
pub fn aim_tolerance(radius: f32, half_height: f32, d: f32, scale: f32) -> V2 {
    let d = d.max(1.0);
    [(radius * scale / d).atan().clamp(0.004, 0.3), (half_height * scale / d).atan().clamp(0.004, 0.3)]
}

pub struct GunSolution {
    pub aim: V2,
    pub dist: f32,
    pub flight: f32,
    pub clear: bool,
    pub in_range: bool,
    pub tol: V2,
}

/// Lead solution for a straight projectile against a moving target. Aims at the centre
/// of mass, or near the top if only the top is visible. `shooter` is our muzzle point on
/// the mech axis, already advanced by our own motion over `delay`.
#[allow(clippy::too_many_arguments)]
pub fn gun_solution(world: &World, shooter: V3, tg: &Tgt, speed: f32, range: f32, delay: f32, offset: f32, lead_smooth: f32, tol_scale: f32) -> Option<GunSolution> {
    let centre = [tg.feet[0], tg.feet[1], tg.feet[2] + tg.height * 0.5];
    let top = [tg.feet[0], tg.feet[1], tg.feet[2] + tg.height * 0.85];
    let (p, half) = if world.los(shooter, centre) || !world.los(shooter, top) { (centre, tg.height * 0.5) } else { (top, tg.height * 0.15) };
    let (_, t0) = lead_point(shooter, p, tg.vel, speed, delay, offset)?;
    // Long shots: a jinking target is better predicted by its smoothed velocity.
    let k = (lead_smooth * (t0 / 0.6).clamp(0.0, 1.0)).clamp(0.0, 1.0);
    let v = [lerp(tg.vel[0], tg.vel_s[0], k), lerp(tg.vel[1], tg.vel_s[1], k), lerp(tg.vel[2], tg.vel_s[2], k) * 0.5];
    let (lp, flight) = lead_point(shooter, p, v, speed, delay, offset)?;
    let d = dist(lp, shooter);
    Some(GunSolution { aim: aim_at(shooter, lp), dist: d, flight, clear: world.los(shooter, lp), in_range: d <= range - 1.0, tol: aim_tolerance(tg.radius, half, d, tol_scale) })
}

/// Is a mortar arc clear of the world from launch to just short of the target?
pub fn arc_clear(world: &World, shot: &MortarShot, v: f32, g: f32) -> bool {
    let n = 14;
    let mut prev = shot.origin;
    for i in 1..=n {
        // stop 4% short: the last bit is the ground at the target itself
        let t = shot.time * (i as f32 / n as f32) * 0.96;
        let p = mortar_point(shot.origin, shot.aim, v, g, t);
        if !world.los(prev, p) {
            return false;
        }
        prev = p;
    }
    true
}

impl Bot {
    fn tgt_of_track(&self, tr: &Track) -> Tgt {
        let c = &self.cfg;
        let (height, radius) = if tr.npc {
            (c.npc.height, c.npc.radius)
        } else {
            let s = 1.0 + c.cubes.size_per_cube * tr.cubes;
            (c.mech.height * s, c.mech.radius * s)
        };
        Tgt { id: tr.id, feet: tr.pos, vel: tr.vel, vel_s: tr.vel_s, height, radius, hp: tr.hp, mech: !tr.npc, weapons: tr.weapons, los: tr.los, live: tr.in_obs, fire_los_t: tr.fire_los_t }
    }

    /// Returns (aim, fire, reload).
    pub(crate) fn gunnery(&mut self, f: &Frame, obs: &Obs, sit: &Sit, intent: &Intent, mv: V2) -> (V2, [bool; 2], bool) {
        let me = &obs.me;
        let t = f.t;
        let tune = self.tune.clone();
        let world = self.world.clone();
        let st = &me.stats;
        let delay = tune.fire_delay;
        let offset = st.radius + 0.5;
        let shooter = add(f.muzzle, mul(f.vel, delay));
        let mut fire = [false, false];

        // ---- pick what to shoot
        // Mechs that attack us, and the one we decided to fight. Nobody else: a shot at a
        // bystander makes one more enemy.
        let may_shoot = |foe: &Foe| foe.engaged || (sit.fight && Some(foe.id) == sit.target.map(|x| x.id));
        let mut tgt: Option<Tgt> = None;
        if let Some(tg) = sit.target {
            if tg.age < 0.3 && may_shoot(&tg) {
                tgt = Some(self.tgt_of_track(&self.model.tracks[tg.ti]));
            }
        }
        if tgt.map_or(true, |x| !x.los) {
            // the chosen target is hidden: any other mech we can hit right now?
            let other = sit
                .foes
                .iter()
                .filter(|x| x.los && may_shoot(x))
                .min_by(|a, b| (a.dist * (0.5 + a.hp_frac)).partial_cmp(&(b.dist * (0.5 + b.hp_frac))).unwrap_or(std::cmp::Ordering::Equal));
            if let Some(o) = other {
                tgt = Some(self.tgt_of_track(&self.model.tracks[o.ti]));
            }
        }
        // Hidden and healing: firing would restart the regeneration clock for nothing.
        if intent.hold_fire && tgt.map_or(false, |x| !x.los) {
            tgt = None;
        }
        let unseen_attacker = t - self.model.last_hurt_t < 2.0 && self.model.last_hurt_by.map_or(true, |id| id < 100 && !sit.foes.iter().any(|x| x.id == id && x.age < 1.0));
        // Shot by a mech the server does not show us (it is behind us, or we have never
        // seen it): turn to its shots. Until we face it we cannot fight it, every hit
        // counts as a rear hit, and whatever we were shooting at matters less than
        // that - unless the target in front dies to what is already loaded.
        let mut face_blind: Option<f32> = None;
        if let Some((id, bearing, dmg)) = sit.blind {
            if self.blind_face.0 != id {
                self.blind_face = (id, t, -1e9);
            }
            // looked that way for a while and it did not show (out of sight range, or
            // hidden): stop staring for a few seconds
            let facing = ang_diff(me.aim[0], bearing).abs() < 0.35;
            if facing && t - self.blind_face.1 > 1.2 {
                self.blind_face.2 = t + 3.0;
            }
            if !facing {
                self.blind_face.1 = t;
            }
            let finishing = tgt.map_or(false, |x| x.mech && x.los && x.live && x.hp <= self.burst_now(f));
            if dmg >= tune.blind_turn_dmg && tune.blind_turn_dmg > 0.0 && t >= self.blind_face.2 && !finishing {
                face_blind = Some(bearing);
                tgt = None;
            }
        }
        // A drone that is shooting at us, when no mech can be shot right now: it is a
        // few seconds of gun fire, it stops the damage (which also blocks regeneration)
        // and it drops cubes. Ignoring drones let them kill a hiding bot.
        if tgt.map_or(true, |x| !x.los) && face_blind.is_none() {
            let drone = self
                .model
                .tracks
                .iter()
                .filter(|tr| tr.npc && tr.in_obs && tr.los && t - tr.hit_us_t < 4.0)
                .min_by(|a, b| (a.hp + dist(a.pos, f.pos)).partial_cmp(&(b.hp + dist(b.pos, f.pos))).unwrap_or(std::cmp::Ordering::Equal));
            if let Some(tr) = drone {
                tgt = Some(self.tgt_of_track(tr));
            }
        }
        if tgt.is_none() && !unseen_attacker && face_blind.is_none() {
            match intent.farm {
                Some(Farm::Npc(id)) => {
                    if let Some(tr) = self.model.track(id) {
                        if tr.in_obs {
                            tgt = Some(self.tgt_of_track(tr));
                        }
                    }
                }
                Some(Farm::Crate(id)) => {
                    if let Some(c) = self.model.crates.iter().find(|c| c.id == id) {
                        let cc = &self.cfg.crates;
                        let mid = [c.pos[0], c.pos[1], c.pos[2] + cc.height * 0.5];
                        tgt = Some(Tgt { id, feet: c.pos, vel: [0.0; 3], vel_s: [0.0; 3], height: cc.height, radius: cc.radius, hp: c.hp, mech: false, weapons: 0, los: world.los(f.muzzle, mid), live: true, fire_los_t: t });
                    }
                }
                None => {}
            }
        }

        self.shot_at = tgt.filter(|x| x.mech && x.los).map(|x| x.id);

        // ---- solutions per weapon
        let mut aim = self.look(f, obs, sit, mv, unseen_attacker);
        if let Some(b) = face_blind {
            aim = [b, 0.0];
        }
        let mut reload = false;
        let mut gun_busy = false; // the gun has a shot to take this decision
        if let Some(tg) = tgt {
            let centre = [tg.feet[0], tg.feet[1], tg.feet[2] + tg.height * 0.5];
            aim = aim_at(shooter, centre);

            // gun
            if let Some((i, s)) = f.slot(W_GUN) {
                if let Some(sol) = gun_solution(&world, shooter, &tg, s.stats.speed.max(1.0), s.stats.range, delay, offset, tune.lead_smooth, tune.aim_tol_scale) {
                    aim = sol.aim;
                    let loaded = s.ammo > 0.0 && s.reload <= 0.0;
                    if tg.los && tg.live && sol.clear && sol.in_range && loaded {
                        gun_busy = true;
                        if aim_settled(me.aim, sol.aim, sol.tol, st.turn_rate, st.pitch_rate, 0.03) {
                            fire[i] = true;
                        }
                    }
                }
            }

            // laser: hold to charge, let go when charged and on target
            if let Some((i, s)) = f.slot(W_LASER) {
                let full = s.stats.charge.max(0.05);
                let latency = 0.06;
                let p = add(centre, mul(tg.vel, latency));
                let la = aim_at(shooter, p);
                let d = dist(p, shooter);
                let in_range = d <= s.stats.range - 2.0;
                let clear = tg.live && world.los(shooter, p);
                let tol = aim_tolerance(tg.radius, tg.height * 0.5, d, tune.aim_tol_scale);
                let on = aim_settled(me.aim, la, tol, st.turn_rate, st.pitch_rate, 0.03);
                let charging = s.charge > 0.0;
                let ready = s.cooldown <= 0.12;
                // start charging on a visible target, or on a mech about to come round a corner
                // (Only one we had in the line of fire a moment ago: charging halves our
                // speed, and a charge held too long fires itself into the wall.)
                let peeking = tg.mech && tg.live && d < 150.0 && !intent.hold_fire && t - tg.fire_los_t <= tune.precharge_s;
                let start = ready && in_range && !intent.no_charge && (clear || peeking);
                if charging || start {
                    let dmg_now = laser_damage(s.stats.damage, s.stats.min_frac, full, s.charge) * f.dmg_scale;
                    let kill_now = charging && clear && on && s.charge >= s.stats.min_charge && dmg_now >= tg.hp;
                    let charged = s.charge >= full - 0.03;
                    let release = kill_now || (charged && clear && on);
                    fire[i] = !release;
                    // Final settle: aim where the beam must go. Before that the gun keeps the aim.
                    if !gun_busy || (charging && s.charge >= full - 0.3) {
                        aim = la;
                    }
                }
            }

            // lightning: instant, hits the nearest target in the cone
            if let Some((i, s)) = f.slot(W_LIGHTNING) {
                let d = dist(centre, f.muzzle);
                let yaw_to = (centre[1] - f.muzzle[1]).atan2(centre[0] - f.muzzle[0]);
                let cone = s.stats.cone_deg.to_radians() * 0.45;
                let yaw_err = (ang_diff(yaw_to, me.aim[0]).abs() - st.turn_rate * 0.05).max(0.0);
                let reserve = if tg.hp < 3.0 * s.stats.damage || !tg.mech { 0.0 } else { tune.lightning_reserve };
                if tg.live && tg.los && s.cooldown <= 0.1 && d <= s.stats.range - 1.5 && yaw_err <= cone && me.energy >= s.stats.energy + reserve {
                    fire[i] = true;
                }
            }

            // mortar: ballistic, also over cover at a mech the server still tracks
            if let Some((i, s)) = f.slot(W_MORTAR) {
                if let Some((shot_aim, settle_tol)) = self.mortar_plan(f, obs, &tg, s, intent, gun_busy) {
                    aim = shot_aim;
                    if s.cooldown <= 0.1 && aim_settled(me.aim, shot_aim, [settle_tol, settle_tol * 0.6], st.turn_rate, st.pitch_rate, 0.0) {
                        fire[i] = true;
                    }
                }
            }
        } else {
            // nothing to shoot: a laser already charging stays held (it may find a target)
            if let Some((i, s)) = f.slot(W_LASER) {
                if s.charge > 0.0 {
                    fire[i] = true;
                }
            }
        }

        // ---- reload discipline: at once when empty; early only when nobody can shoot us
        if let Some((_, s)) = f.slot(W_GUN) {
            if s.reload <= 0.0 && s.ammo < s.stats.mag {
                let fresh_foe = sit.foes.iter().any(|x| x.age < 3.0 && (x.los || x.dist < 150.0));
                let quiet = self.model.quiet_for() > 2.5;
                // with an enemy about, only top up a nearly empty magazine while it cannot see us
                let top_up = !gun_busy && fresh_foe && s.ammo <= s.stats.mag * 0.25 && !sit.foes.iter().any(|x| x.los);
                if s.ammo <= 0.0 || top_up || (!gun_busy && !fresh_foe && (s.ammo <= s.stats.mag * 0.5 || quiet)) {
                    reload = true;
                }
            }
        }

        aim[1] = aim[1].clamp(st.min_pitch, st.max_pitch);
        (aim, fire, reload)
    }

    /// Damage we can put out in the next second or so with what is loaded.
    pub(crate) fn burst_now(&self, f: &Frame) -> f32 {
        f.slots
            .iter()
            .flatten()
            .map(|s| match s.bit {
                W_LASER if s.cooldown < 1.0 => s.stats.damage,
                W_GUN => s.stats.damage * s.ammo.min(4.0),
                _ => 0.0,
            })
            .sum::<f32>()
            * f.dmg_scale
    }

    /// Mortar decision: returns the aim to command and the settle tolerance when a shot is worth it.
    fn mortar_plan(&mut self, f: &Frame, obs: &Obs, tg: &Tgt, s: &Slot, intent: &Intent, gun_busy: bool) -> Option<(V2, f32)> {
        let me = &obs.me;
        let tune = &self.tune;
        if !tg.live || s.cooldown > 0.35 {
            return None;
        }
        if intent.hold_fire && !tg.los {
            return None;
        }
        let (v, g) = (s.stats.speed.max(1.0), s.stats.gravity.max(1.0));
        let tube = [f.pos[0], f.pos[1], f.pos[2] + me.stats.height + 0.5];
        let offset = me.stats.radius + 0.5;
        let speed_s = tg.vel_s[0].hypot(tg.vel_s[1]);
        let world = &*self.world;
        // aim at the ground under the (predicted) target: the splash does the work
        let mut p = tg.feet;
        let mut best: Option<MortarShot> = None;
        for high in [false, true] {
            let mut shot = None;
            for _ in 0..3 {
                let sol = mortar_solve(tube, p, v, g, high, offset, me.stats.min_pitch, me.stats.max_pitch)?;
                let lead = tune.fire_delay + sol.time;
                let q = [tg.feet[0] + tg.vel_s[0] * lead, tg.feet[1] + tg.vel_s[1] * lead];
                let z = world.floor_at(q[0], q[1], tg.feet[2] + 1.0, 1.5).map_or(tg.feet[2], |fl| fl.z);
                p = [q[0], q[1], z + 0.3];
                shot = Some(sol);
            }
            if let Some(sol) = shot {
                if arc_clear(world, &sol, v, g) {
                    best = Some(sol);
                    break;
                }
            }
            p = tg.feet;
        }
        let shot = best?;
        let d = dist2d(p, f.pos);
        if d < tune.mortar_min_dist.max(s.stats.splash_radius + me.stats.radius + 4.0) {
            return None;
        }
        // Worth interrupting the gun for? Slow or close targets, or ones we cannot hit otherwise.
        // ... or it has been walking a steady line (a hunter coming straight at us) and
        // the shell is not in the air too long for that to hold.
        let steady = (tg.vel[0] - tg.vel_s[0]).hypot(tg.vel[1] - tg.vel_s[1]) < 2.0 && shot.time <= tune.mortar_steady_flight_s;
        let likely = speed_s <= tune.mortar_max_target_speed || shot.time <= tune.mortar_max_flight_s || steady;
        if (gun_busy || tg.los) && !likely {
            return None;
        }
        if !tg.los && !tg.mech {
            return None;
        }
        let tol = (s.stats.splash_radius * 0.25 / d.max(1.0)).atan().clamp(0.006, 0.05);
        self.mortar_aim_until = f.t + 0.3;
        Some((shot.aim, tol))
    }

    /// Where to look when there is nothing to shoot.
    fn look(&mut self, f: &Frame, _obs: &Obs, sit: &Sit, mv: V2, unseen_attacker: bool) -> V2 {
        let t = f.t;
        // 1. being shot by someone we do not see: face it (rear hits take x1.5)
        if unseen_attacker {
            if let Some(b) = self.model.last_hurt_bearing {
                return [b, 0.0];
            }
        }
        // 2. the last place we knew an enemy to be
        if let Some(foe) = sit.threat.filter(|x| x.age < 10.0).or_else(|| sit.foes.iter().filter(|x| x.age < 10.0).min_by(|a, b| a.dist.partial_cmp(&b.dist).unwrap_or(std::cmp::Ordering::Equal)).copied()) {
            let p = [foe.pos[0], foe.pos[1], foe.pos[2] + 4.5];
            return aim_at(f.muzzle, p);
        }
        // 3. a recent weapon sound
        let heard = self
            .model
            .heard
            .iter()
            .filter(|h| t - h.t < 3.0 && matches!(h.kind.as_str(), "gun" | "laser" | "laser_charge" | "lightning" | "mortar" | "boost" | "step" | "jet" | "reload"))
            .max_by(|a, b| {
                let sa = if a.near { 10.0 } else { 0.0 } - (t - a.t);
                let sb = if b.near { 10.0 } else { 0.0 } - (t - b.t);
                sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
            });
        if let Some(h) = heard {
            if h.near || matches!(h.kind.as_str(), "gun" | "laser" | "lightning" | "mortar") && (h.pos[0] - f.xy[0]).hypot(h.pos[1] - f.xy[1]) < 200.0 {
                return [h.bearing, 0.0];
            }
        }
        // 4. scan: sweep around the direction of travel so the 120 degree wedge covers everything
        if len2(mv) > 0.2 {
            self.scan_base = mv[1].atan2(mv[0]);
        }
        let period = self.tune.scan_period_s.max(0.5);
        let ph = (t / period).fract();
        let tri = if ph < 0.5 { 4.0 * ph - 1.0 } else { 3.0 - 4.0 * ph };
        [wrap(self.scan_base + 2.3 * tri), 0.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settle_respects_slew() {
        // 0.3 rad off, 4 rad/s, 0.05 s of slew covers 0.2: still 0.1 off
        assert!(!aim_settled([0.0, 0.0], [0.3, 0.0], [0.05, 0.05], 4.0, 3.0, 0.05));
        assert!(aim_settled([0.0, 0.0], [0.24, 0.0], [0.05, 0.05], 4.0, 3.0, 0.05));
        // wraps around +-pi
        assert!(aim_settled([3.13, 0.0], [-3.13, 0.0], [0.05, 0.05], 4.0, 3.0, 0.0));
        // pitch counts too
        assert!(!aim_settled([0.0, 0.0], [0.0, 0.4], [0.05, 0.05], 4.0, 3.0, 0.05));
    }

    #[test]
    fn tolerance_shrinks_with_distance() {
        let near = aim_tolerance(2.5, 4.5, 20.0, 0.8);
        let far = aim_tolerance(2.5, 4.5, 200.0, 0.8);
        assert!(near[0] > far[0] && near[1] > far[1]);
        assert!((far[0] - (2.0f32 / 200.0).atan()).abs() < 1e-4);
    }
}
