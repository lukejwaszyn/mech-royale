//! Built-in scripted opponents (run inside the simulator, no process) and the "squad"
//! coordinator imitating the practice setup. These are stand-ins: the real house bots
//! are unknown and certainly play differently.
use crate::config::WKind;
use crate::sim::{Cmd, Offer, Sim};
use crate::util::*;
use world::Nav;

/// Skill knobs of one level. All tunable from the CLI (`--tune easy.aim_sigma_deg=8,...`).
#[derive(Clone, Debug)]
pub struct LevelParams {
    pub name: &'static str,
    /// Standard deviation of the aim error, degrees (re-drawn about three times a second).
    pub aim_sigma_deg: f32,
    /// Seconds between first seeing a target and the first shot at it.
    pub react_s: f32,
    /// 0 = shoots where the target is, 1 = perfect linear lead.
    pub lead: f32,
    /// Fires when the torso is within this many degrees of where it wants to aim.
    pub fire_tol_deg: f32,
    /// 0..1: how much of the time it strafes in a fight (otherwise it stands still).
    pub strafe: f32,
    /// Runs for cover below this fraction of max HP (0 = never).
    pub retreat_hp: f32,
    /// 0..1: how eagerly it goes for weapons, cubes and crates.
    pub loot: f32,
    /// Uses boost to chase / flee / get into the zone.
    pub boost: f32,
    /// Goes for supply drops.
    pub drops: f32,
    /// Turns toward the last source of damage when it has no target.
    pub face_threat: f32,
    /// A squad member that is not a hunter still fights the outsider inside this range.
    pub defend_range: f32,
    /// Range at which it likes to fight with the gun.
    pub gun_range: f32,
}

pub const LEVEL_NAMES: [&str; 4] = ["easy", "medium", "hard", "expert"];

pub fn default_levels() -> [LevelParams; 4] {
    [
        LevelParams { name: "easy", aim_sigma_deg: 6.0, react_s: 1.0, lead: 0.0, fire_tol_deg: 5.0, strafe: 0.0, retreat_hp: 0.0, loot: 0.3, boost: 0.0, drops: 0.0, face_threat: 0.0, defend_range: 25.0, gun_range: 60.0 },
        LevelParams { name: "medium", aim_sigma_deg: 3.0, react_s: 0.6, lead: 0.6, fire_tol_deg: 3.0, strafe: 0.5, retreat_hp: 0.2, loot: 0.6, boost: 0.0, drops: 0.0, face_threat: 1.0, defend_range: 35.0, gun_range: 75.0 },
        LevelParams { name: "hard", aim_sigma_deg: 1.5, react_s: 0.35, lead: 0.9, fire_tol_deg: 2.0, strafe: 1.0, retreat_hp: 0.3, loot: 0.9, boost: 1.0, drops: 1.0, face_threat: 1.0, defend_range: 45.0, gun_range: 85.0 },
        LevelParams { name: "expert", aim_sigma_deg: 0.7, react_s: 0.2, lead: 1.0, fire_tol_deg: 1.2, strafe: 1.0, retreat_hp: 0.35, loot: 1.0, boost: 1.0, drops: 1.0, face_threat: 1.0, defend_range: 55.0, gun_range: 95.0 },
    ]
}

impl LevelParams {
    /// Set one knob by name. Returns false for an unknown name.
    pub fn set(&mut self, key: &str, v: f32) -> bool {
        match key {
            "aim_sigma_deg" | "aim_deg" => self.aim_sigma_deg = v,
            "react_s" => self.react_s = v,
            "lead" => self.lead = v,
            "fire_tol_deg" => self.fire_tol_deg = v,
            "strafe" => self.strafe = v,
            "retreat_hp" => self.retreat_hp = v,
            "loot" => self.loot = v,
            "boost" => self.boost = v,
            "drops" => self.drops = v,
            "face_threat" => self.face_threat = v,
            "defend_range" => self.defend_range = v,
            "gun_range" => self.gun_range = v,
            _ => return false,
        }
        true
    }
}

/// Squad coordinator: shared vision, and who is hunting the single outsider.
#[derive(Clone, Debug)]
pub struct Squad {
    pub team: u8,
    pub outsider: usize,
    /// Per mech id: is it currently a hunter?
    pub hunter: Vec<bool>,
    /// Per mech id: scripted level index (hunters are drafted weakest-first).
    pub level: Vec<usize>,
    /// Last place any member saw the outsider, and when.
    pub known: Option<(V3, f32)>,
    /// Some member sees the outsider right now (so every member knows where it is).
    pub visible: bool,
    pub initial: usize,
    /// From this match time on everybody hunts.
    pub late_t: f32,
    /// ... or once this few mechs are left in the match.
    pub late_alive: usize,
    /// One more hunter for every this many dead squad members.
    pub deaths_per_hunter: f32,
    /// Cheat: the squad always knows where the outsider is.
    pub omniscient: bool,
}

impl Squad {
    pub fn new(team: u8, outsider: usize, level: Vec<usize>) -> Squad {
        let n = level.len();
        Squad { team, outsider, hunter: vec![false; n], level, known: None, visible: false, initial: 0, late_t: 240.0, late_alive: 6, deaths_per_hunter: 1.0, omniscient: false }
    }

    pub fn update(&mut self, sim: &Sim) {
        let members: Vec<usize> = (0..sim.mechs.len()).filter(|&i| sim.mechs[i].alive && sim.mechs[i].team == self.team).collect();
        if self.initial == 0 {
            self.initial = members.len();
        }
        let out = &sim.mechs[self.outsider];
        self.visible = false;
        if !out.alive {
            self.hunter.iter_mut().for_each(|h| *h = false);
            self.known = None;
            return;
        }
        if self.omniscient || members.iter().any(|&i| sim.sees_now(i, self.outsider)) {
            self.visible = true;
            self.known = Some((out.kin.pos, sim.t));
        }
        let dead = self.initial.saturating_sub(members.len());
        let want = if sim.t >= self.late_t || sim.alive_count <= self.late_alive { members.len() } else { (1 + (dead as f32 / self.deaths_per_hunter.max(0.01)) as usize).min(members.len()) };
        for i in 0..self.hunter.len() {
            if self.hunter[i] && !sim.mechs[i].alive {
                self.hunter[i] = false;
            }
        }
        let mut have = members.iter().filter(|&&i| self.hunter[i]).count();
        while have < want {
            let reference = self.known.map(|k| k.0).unwrap_or([sim.zone.center[0], sim.zone.center[1], 0.0]);
            let cand = members.iter().copied().filter(|&i| !self.hunter[i]).min_by(|&a, &b| {
                let ka = (self.level[a], dist2d(sim.mechs[a].kin.pos, reference));
                let kb = (self.level[b], dist2d(sim.mechs[b].kin.pos, reference));
                ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
            });
            match cand {
                Some(i) => {
                    self.hunter[i] = true;
                    have += 1;
                }
                None => break,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Loot {
    Pickup(u32),
    Cube(u32),
    Crate(u32),
    Drop(u32),
}

pub struct Scripted {
    pub level: usize,
    pub p: LevelParams,
    rng: Rng,
    path: Vec<V3>,
    path_goal: V3,
    replan_at: f32,
    wander: Option<V3>,
    wander_until: f32,
    zone_spot: Option<(V2, V3)>,
    target: Option<usize>,
    target_since: f32,
    err: V2,
    err_goal: V2,
    err_at: f32,
    strafe_dir: f32,
    strafe_on: bool,
    strafe_until: f32,
    check_pos: V3,
    check_t: f32,
    moved_want: bool,
    unstick_until: f32,
    unstick_dir: V2,
    loot: Option<(Loot, V3)>,
    loot_at: f32,
    bad_loot: Vec<Loot>,
    cover: Option<V3>,
    cover_at: f32,
    last_move_yaw: f32,
}

fn hp_frac(sim: &Sim, i: usize) -> f32 {
    sim.mechs[i].hp / sim.mechs[i].max_hp.max(1.0)
}

/// Low-arc launch pitch to hit a point `dx` metres away and `dz` metres up at muzzle speed `v`.
pub fn ballistic_pitch(dx: f32, dz: f32, v: f32, g: f32) -> Option<f32> {
    let disc = v.powi(4) - g * (g * dx * dx + 2.0 * dz * v * v);
    if disc < 0.0 || dx < 1e-3 {
        return None;
    }
    Some(((v * v - disc.sqrt()) / (g * dx)).atan())
}

impl Scripted {
    pub fn new(level: usize, p: LevelParams, seed: u64) -> Scripted {
        Scripted {
            level,
            p,
            rng: Rng::new(seed),
            path: Vec::new(),
            path_goal: [f32::NAN; 3],
            replan_at: 0.0,
            wander: None,
            wander_until: 0.0,
            zone_spot: None,
            target: None,
            target_since: 0.0,
            err: [0.0; 2],
            err_goal: [0.0; 2],
            err_at: 0.0,
            strafe_dir: 1.0,
            strafe_on: false,
            strafe_until: 0.0,
            check_pos: [0.0; 3],
            check_t: 0.0,
            moved_want: false,
            unstick_until: -1.0,
            unstick_dir: [0.0; 2],
            loot: None,
            loot_at: 0.0,
            bad_loot: Vec::new(),
            cover: None,
            cover_at: -10.0,
            last_move_yaw: 0.0,
        }
    }

    fn choose_upgrade(&self, sim: &Sim, o: &Offer) -> usize {
        if self.level == 0 {
            return 0;
        }
        const PREF: [&str; 10] = ["plating", "armor", "nanites", "gun_damage", "gun_rate", "laser_damage", "servos", "gun_velocity", "reactor", "capacitor"];
        let mut best = (usize::MAX, 0usize);
        for (k, &u) in o.choices.iter().enumerate() {
            let rank = PREF.iter().position(|p| *p == sim.cfg.upgrades[u].id).unwrap_or(50);
            if rank < best.0 {
                best = (rank, k);
            }
        }
        best.1
    }

    /// Walk toward `goal` along the nav grid. Returns the move vector.
    fn steer(&mut self, sim: &Sim, nav: &Nav, me: usize, goal: V3) -> V2 {
        let m = &sim.mechs[me];
        let pos = m.kin.pos;
        let t = sim.t;
        let moved_goal = !self.path_goal[0].is_finite() || dist2d(self.path_goal, goal) > 8.0;
        if m.kin.grounded && (t >= self.replan_at || moved_goal) {
            self.replan_at = t + 1.0 + self.rng.f32() * 0.6;
            self.path_goal = goal;
            self.path.clear();
            let on_main = nav.nearest_node(pos).map_or(false, |n| nav.is_main(n) && dist2d(nav.node_pos(n), pos) < 6.0 && (nav.node_pos(n)[2] - pos[2]).abs() < 3.0);
            if on_main {
                if let Some(p) = nav.find_path(pos, goal) {
                    self.path = p.points;
                }
            } else if let Some(w) = nav.nearest_walkable(goal[0], goal[1]).or_else(|| nav.nearest_walkable(pos[0], pos[1])) {
                // on a roof or in an odd spot: head straight for the goal and drop off the edge
                self.path = vec![w];
            }
        }
        while self.path.len() > 1 && dist2d(self.path[0], pos) < 2.5 {
            self.path.remove(0);
        }
        let next = self.path.first().copied().unwrap_or(goal);
        let (dx, dy) = (next[0] - pos[0], next[1] - pos[1]);
        let d = dx.hypot(dy);
        if d < 0.8 {
            return [0.0, 0.0];
        }
        [dx / d, dy / d]
    }

    fn pick_loot(&mut self, sim: &Sim, me: usize) -> Option<(Loot, V3)> {
        let m = &sim.mechs[me];
        let pos = m.kin.pos;
        let z = sim.zone;
        let in_next = |p: V3| (p[0] - z.next_center[0]).hypot(p[1] - z.next_center[1]) < z.next_radius.max(z.radius * 0.5);
        let mut best: Option<(f32, Loot, V3)> = None;
        let mut consider = |score: f32, l: Loot, p: V3, bad: &Vec<Loot>| {
            if !bad.contains(&l) && best.map_or(true, |b| score < b.0) {
                best = Some((score, l, p));
            }
        };
        let reach = 40.0 + 60.0 * self.p.loot;
        if m.weapons.iter().any(|w| w.is_none()) {
            for p in &sim.pickups {
                let d = dist(pos, p.pos);
                if d < reach && !m.has(p.w.kind) && in_next(p.pos) && (p.pos[2] - pos[2]).abs() < 6.0 && sim.can_see(me, p.pos, 1.0) {
                    consider(d, Loot::Pickup(p.id), p.pos, &self.bad_loot);
                }
            }
        }
        for c in &sim.cubes {
            let d = dist(pos, c.pos);
            if d < reach && in_next(c.pos) && (c.pos[2] - pos[2]).abs() < 6.0 && sim.can_see(me, c.pos, 1.0) {
                consider(d * 0.8, Loot::Cube(c.id), c.pos, &self.bad_loot);
            }
        }
        if self.p.loot >= 0.5 {
            for c in &sim.crates {
                let d = dist(pos, c.pos);
                if c.alive && d < reach * 0.8 && in_next(c.pos) && (c.pos[2] - pos[2]).abs() < 6.0 && sim.can_see(me, c.pos, sim.cfg.crates.height) {
                    consider(d * 1.2 + 15.0, Loot::Crate(c.id), c.pos, &self.bad_loot);
                }
            }
        }
        if self.p.drops > 0.5 {
            for d in &sim.drops {
                let dd = dist(pos, d.pos);
                if dd < 250.0 && d.lands_at - sim.t < dd / 11.0 + 5.0 {
                    consider(dd * 0.5, Loot::Drop(d.id), d.pos, &self.bad_loot);
                }
            }
        }
        best.map(|b| (b.1, b.2))
    }

    fn loot_still_there(&self, sim: &Sim, l: Loot) -> Option<V3> {
        match l {
            Loot::Pickup(id) => sim.pickups.iter().find(|p| p.id == id).map(|p| p.pos),
            Loot::Cube(id) => sim.cubes.iter().find(|c| c.id == id).map(|c| c.pos),
            Loot::Crate(id) => sim.crates.iter().find(|c| c.id == id && c.alive).map(|c| c.pos),
            Loot::Drop(id) => sim.drops.iter().find(|d| d.id == id).map(|d| d.pos),
        }
    }

    /// One decision. `squad` is given to members of the squad team only.
    pub fn decide(&mut self, sim: &Sim, me: usize, nav: &Nav, squad: Option<&Squad>) -> Cmd {
        let m = &sim.mechs[me];
        let p = self.p.clone();
        let t = sim.t;
        let pos = m.kin.pos;
        let z = sim.zone;
        let mut cmd = Cmd::default();
        if let Some(o) = &m.offer {
            cmd.pick = Some(self.choose_upgrade(sim, o));
        }
        let hunting = squad.map_or(false, |s| s.hunter[me] && sim.mechs[s.outsider].alive);

        // ---------------- target selection
        let mut target: Option<usize> = None;
        let mut best = f32::INFINITY;
        for j in 0..sim.mechs.len() {
            let o = &sim.mechs[j];
            if j == me || !o.alive || (m.team != 0 && o.team == m.team) {
                continue;
            }
            let shared = squad.map_or(false, |s| j == s.outsider && s.visible);
            if !sim.sees(me, j) && !(shared && hunting) {
                continue;
            }
            let d = dist(pos, o.kin.pos);
            if let Some(s) = squad {
                if j == s.outsider && !s.hunter[me] {
                    let attacked = m.last_hurt.map_or(false, |h| h.by == Some(j as i32) && t - h.t < 4.0);
                    if d > p.defend_range && !attacked {
                        continue;
                    }
                }
            }
            let mut score = d * (0.5 + 0.5 * hp_frac(sim, j));
            if self.target == Some(j) {
                score *= 0.75;
            }
            if score < best {
                best = score;
                target = Some(j);
            }
        }
        if target != self.target {
            self.target = target;
            self.target_since = t;
        }

        // ---------------- where to go
        let safe_r = (z.next_radius * 0.8 - 5.0).max(0.0);
        let d_next = (pos[0] - z.next_center[0]).hypot(pos[1] - z.next_center[1]);
        let d_cur = (pos[0] - z.center[0]).hypot(pos[1] - z.center[1]);
        let outside_now = d_cur > z.radius - 12.0;
        let need_s = (d_next - safe_r).max(0.0) / m.st.walk_speed * 1.5 + 6.0;
        let must_zone = d_next > safe_r && (outside_now || t >= z.shrink_end - need_s - (z.shrink_end - z.shrink_start) * 0.5);
        if self.zone_spot.map_or(true, |(c, _)| c != z.next_center) {
            let o = self.rng.disc();
            let r = z.next_radius * 0.55;
            let (x, y) = (z.next_center[0] + o[0] * r, z.next_center[1] + o[1] * r);
            let spot = nav.nearest_walkable(x, y).unwrap_or([x, y, sim.world.terrain_height(x, y)]);
            self.zone_spot = Some((z.next_center, spot));
        }
        let zone_spot = self.zone_spot.unwrap().1;

        let mut goal: Option<V3> = None;
        let mut mv: Option<V2> = None;
        let mut boost = false;
        let mut crate_target: Option<V3> = None;
        let my_frac = hp_frac(sim, me);
        let tgt_now = target.filter(|&j| sim.sees_now(me, j));

        if must_zone {
            goal = Some(zone_spot);
            boost = outside_now && (p.boost > 0.5 || my_frac < 0.5);
        } else if let Some(j) = target {
            let tm = &sim.mechs[j];
            let d = dist(pos, tm.kin.pos);
            let want = if m.has(WKind::Lightning) { 28.0 } else if m.has(WKind::Laser) { p.gun_range + 30.0 } else { p.gun_range };
            let retreat = p.retreat_hp > 0.0 && my_frac < p.retreat_hp && hp_frac(sim, j) > my_frac && !hunting;
            if retreat {
                if t - self.cover_at > 1.5 {
                    self.cover_at = t;
                    let threat = up(tm.kin.pos, tm.st.muzzle_height);
                    self.cover = if m.kin.grounded { nav.cover_points(pos, threat, 70.0, m.st.height, 3).first().map(|c| c.pos) } else { None };
                }
                match self.cover {
                    Some(c) => goal = Some(c),
                    None => {
                        let dd = dist2d(pos, tm.kin.pos).max(0.1);
                        mv = Some([(pos[0] - tm.kin.pos[0]) / dd, (pos[1] - tm.kin.pos[1]) / dd]);
                    }
                }
                boost = p.boost > 0.5 && m.energy > 20.0;
            } else if tgt_now.is_none() || d > want + 20.0 {
                goal = Some(tm.kin.pos);
                boost = p.boost > 0.5 && hunting && d > 150.0 && m.energy > 50.0;
            } else if d < want * 0.45 && !m.has(WKind::Lightning) {
                let dd = dist2d(pos, tm.kin.pos).max(0.1);
                mv = Some([(pos[0] - tm.kin.pos[0]) / dd, (pos[1] - tm.kin.pos[1]) / dd]);
            } else {
                if t >= self.strafe_until || m.wall {
                    if m.wall && t < self.strafe_until {
                        self.strafe_dir = -self.strafe_dir;
                    } else if self.rng.chance(0.5) {
                        self.strafe_dir = -self.strafe_dir;
                    }
                    self.strafe_on = self.rng.chance(p.strafe);
                    self.strafe_until = t + self.rng.range(0.7, 2.2);
                }
                if self.strafe_on {
                    let dd = dist2d(pos, tm.kin.pos).max(0.1);
                    let (ux, uy) = ((tm.kin.pos[0] - pos[0]) / dd, (tm.kin.pos[1] - pos[1]) / dd);
                    let inward = ((d - want) / 30.0).clamp(-0.5, 0.5);
                    let v = [-uy * self.strafe_dir + ux * inward, ux * self.strafe_dir + uy * inward];
                    let l = v[0].hypot(v[1]).max(1e-3);
                    mv = Some([v[0] / l, v[1] / l]);
                } else {
                    mv = Some([0.0, 0.0]);
                }
            }
        } else if hunting {
            let s = squad.unwrap();
            goal = match s.known {
                Some((kp, kt)) if t - kt < 25.0 || dist2d(kp, pos) > 20.0 => Some(kp),
                _ => None,
            };
            boost = false;
        }
        if goal.is_none() && mv.is_none() {
            // loot, else wander inside the next circle
            if p.loot > 0.0 && t >= self.loot_at {
                self.loot_at = t + 0.5;
                let keep = self.loot.and_then(|(l, _)| self.loot_still_there(sim, l).map(|pp| (l, pp)));
                self.loot = keep.or_else(|| if self.rng.chance(0.3 + 0.7 * p.loot) { self.pick_loot(sim, me) } else { None });
            }
            if let Some((l, lp)) = self.loot {
                if self.loot_still_there(sim, l).is_none() {
                    self.loot = None;
                } else {
                    match l {
                        Loot::Pickup(id) => {
                            if dist2d(pos, lp) <= sim.cfg.loot.pickup_radius - 0.5 && (pos[2] - lp[2]).abs() < 4.0 {
                                cmd.take = Some(id);
                            }
                            goal = Some(lp);
                        }
                        Loot::Crate(_) => {
                            let c = up(lp, sim.cfg.crates.height * 0.5);
                            if dist(pos, lp) < 45.0 && sim.world.los(up(pos, m.st.muzzle_height), c) {
                                crate_target = Some(c);
                                mv = Some([0.0, 0.0]);
                            } else {
                                goal = Some(lp);
                            }
                        }
                        _ => goal = Some(lp),
                    }
                }
            }
            if goal.is_none() && mv.is_none() {
                let stale = self.wander.map_or(true, |w| dist2d(w, pos) < 10.0 || t >= self.wander_until || (w[0] - z.next_center[0]).hypot(w[1] - z.next_center[1]) > z.next_radius);
                if stale {
                    let o = self.rng.disc();
                    let r = z.next_radius * 0.7;
                    let (x, y) = (z.next_center[0] + o[0] * r, z.next_center[1] + o[1] * r);
                    self.wander = nav.nearest_walkable(x, y);
                    self.wander_until = t + 40.0;
                }
                goal = self.wander.or(Some(zone_spot));
            }
        }

        // ---------------- legs
        let mut jump = false;
        let mut dir = match (mv, goal) {
            (Some(v), _) => v,
            (None, Some(g)) => {
                let v = self.steer(sim, nav, me, g);
                if let Some((l, lp)) = self.loot {
                    // unreachable loot (on a roof, inside a wall): give up on it
                    if lp == g && self.path.last().map_or(false, |e| dist2d(*e, g) > 6.0) {
                        self.bad_loot.push(l);
                        self.loot = None;
                    }
                }
                v
            }
            _ => [0.0, 0.0],
        };
        let wants_move = dir[0].abs() + dir[1].abs() > 0.01;
        if t - self.check_t >= 1.0 {
            if self.moved_want && dist2d(self.check_pos, pos) < 1.2 && goal.is_some() {
                self.unstick_until = t + 0.8;
                let a = self.rng.range(0.0, std::f32::consts::TAU);
                self.unstick_dir = [a.cos(), a.sin()];
                self.replan_at = 0.0;
                if let Some((l, _)) = self.loot.take() {
                    self.bad_loot.push(l);
                }
            }
            self.check_t = t;
            self.check_pos = pos;
            self.moved_want = wants_move && goal.is_some();
        }
        if t < self.unstick_until {
            dir = self.unstick_dir;
            jump = m.kin.grounded;
            boost = !m.kin.grounded && m.energy > 30.0;
        }
        if wants_move {
            self.last_move_yaw = dir[1].atan2(dir[0]);
        }
        cmd.mv = dir;
        cmd.jump = jump;
        cmd.boost = boost;

        // ---------------- torso
        if t >= self.err_at {
            self.err_at = t + 0.35;
            let s = p.aim_sigma_deg.to_radians();
            self.err_goal = [self.rng.gauss() * s, self.rng.gauss() * s * 0.6];
        }
        self.err = [self.err[0] + (self.err_goal[0] - self.err[0]) * 0.35, self.err[1] + (self.err_goal[1] - self.err[1]) * 0.35];
        let (base, _, _) = sim.muzzle(me, false);
        let tol = p.fire_tol_deg.to_radians();
        let aligned = |aim: V2, yaw: f32, pitch: f32, tol: f32| wrap_pi(aim[0] - yaw).abs() < tol && (aim[1] - pitch).abs() < tol;

        if let Some(j) = tgt_now {
            let tm = &sim.mechs[j];
            let tc = tm.center();
            let d = dist(base, tc);
            let reacted = t - self.target_since >= p.react_s;
            let clear = sim.world.los(base, tc);
            let friend_in_line = m.team != 0 && sim.mechs.iter().enumerate().any(|(k, o)| {
                if k == me || k == j || !o.alive || o.team != m.team {
                    return false;
                }
                let c = o.center();
                let ab = sub(tc, base);
                let tt = (dot(sub(c, base), ab) / dot(ab, ab).max(1e-6)).clamp(0.0, 1.0);
                dist(c, add(base, scale(ab, tt))) < o.st.radius + 1.5
            });
            // gun solution (lead)
            let g = m.wst.gun;
            let tt = d / g.speed.max(1.0);
            let lead_pt = add(tc, scale(tm.kin.vel, tt * p.lead));
            let mut yaw = yaw_to(base, lead_pt);
            let mut pitch = pitch_to(base, lead_pt);
            let mut mortar_aim = false;
            let laser_slot = m.slot_of(WKind::Laser);
            let mortar_slot = m.slot_of(WKind::Mortar);
            if let Some(s) = laser_slot {
                let w = m.weapons[s].unwrap();
                if (w.charging || w.cooldown <= 0.3) && d <= m.wst.laser.range {
                    yaw = yaw_to(base, tc);
                    pitch = pitch_to(base, tc);
                }
            }
            if let Some(s) = mortar_slot {
                let w = m.weapons[s].unwrap();
                let ms = m.wst.mortar;
                let tube = up(pos, m.st.height + 0.5);
                let fly = dist2d(tube, tc) / (ms.speed * 0.9).max(1.0);
                let aim_pt = add(tm.kin.pos, scale([tm.kin.vel[0], tm.kin.vel[1], 0.0], fly * p.lead));
                let dx = dist2d(tube, aim_pt);
                if w.cooldown <= 0.25 && dx > 18.0 && laser_slot.map_or(true, |ls| !m.weapons[ls].unwrap().charging) {
                    if let Some(th) = ballistic_pitch(dx, aim_pt[2] - tube[2], ms.speed, ms.gravity) {
                        if th <= m.st.max_pitch {
                            yaw = yaw_to(tube, aim_pt);
                            pitch = th;
                            mortar_aim = true;
                        }
                    }
                }
            }
            yaw += self.err[0];
            pitch += self.err[1];
            cmd.aim = Some([yaw, pitch]);
            let on = aligned(m.aim, yaw, pitch, tol);
            for s in 0..2 {
                let Some(w) = m.weapons[s] else { continue };
                cmd.fire[s] = match w.kind {
                    WKind::Gun => reacted && on && !mortar_aim && clear && !friend_in_line && d <= g.range * 0.95,
                    WKind::Mortar => reacted && mortar_aim && aligned(m.aim, yaw, pitch, tol * 2.0) && !friend_in_line,
                    WKind::Laser => {
                        let ls = m.wst.laser;
                        if w.charging {
                            // hold until fully charged and on target
                            !(w.charge >= ls.charge * 0.95 && (on || w.charge >= ls.charge + ls.hold_s * 0.6))
                        } else {
                            reacted && clear && d <= ls.range && !friend_in_line && t >= m.short_until
                        }
                    }
                    WKind::Lightning => {
                        let ls = m.wst.lightning;
                        reacted && clear && d <= ls.range && wrap_pi(yaw_to(base, tc) - m.aim[0]).abs() < (ls.cone_deg - 4.0).max(2.0).to_radians() && m.energy >= ls.energy
                    }
                };
            }
        } else if let Some(c) = crate_target {
            let (yaw, pitch) = (yaw_to(base, c) + self.err[0] * 0.3, pitch_to(base, c));
            cmd.aim = Some([yaw, pitch]);
            if aligned(m.aim, yaw, pitch, tol.max(0.03)) {
                for s in 0..2 {
                    if let Some(w) = m.weapons[s] {
                        cmd.fire[s] = matches!(w.kind, WKind::Gun) || (w.kind == WKind::Lightning && dist(base, c) < m.wst.lightning.range);
                    }
                }
            }
        } else {
            // nothing to shoot: release a charging laser, look around
            let threat = m.last_hurt.filter(|h| p.face_threat > 0.5 && t - h.t < 3.0 && h.by.is_some());
            let known = squad.filter(|_| hunting).and_then(|s| s.known).filter(|k| dist2d(k.0, pos) < 150.0);
            let yaw = if let Some(h) = threat {
                yaw_to(pos, h.from)
            } else if let Some(j) = target {
                yaw_to(pos, sim.mechs[j].kin.pos)
            } else if let Some((kp, _)) = known {
                yaw_to(pos, kp)
            } else if wants_move {
                self.last_move_yaw + (t * 0.7 + me as f32).sin() * 0.6 * if self.level >= 2 { 1.0 } else { 0.0 }
            } else {
                m.cmd_aim[0] + 0.25
            };
            cmd.aim = Some([yaw, 0.0]);
            if let Some(w) = m.weapons.iter().flatten().find(|w| w.kind == WKind::Gun) {
                if w.reload <= 0.0 && (w.ammo as f32) < m.wst.gun.mag * 0.5 {
                    cmd.reload = true;
                }
            }
        }
        cmd
    }
}
