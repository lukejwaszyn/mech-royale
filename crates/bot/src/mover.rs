//! Getting from A to B: nav paths, roof hops (jump + jet through a region access),
//! stuck detection and recovery. The brain decides *where*; this decides *how*.
use crate::mathx::*;
use crate::proto::*;
use world::{max_climb, Access, JetParams, Nav};

/// What the legs should do this decision.
#[derive(Clone, Copy, Debug, Default)]
pub struct Drive {
    /// Unit direction to walk (zero = stand).
    pub dir: V2,
    pub jump: bool,
    /// Hold boost to jet (airborne).
    pub jet: bool,
    /// The mover needs exact control (a hop or a recovery): do not blend dodging into it.
    pub hard: bool,
    pub arrived: bool,
    /// Walking distance left on the planned path (straight distance if no path).
    pub remaining: f32,
    /// Waiting for energy before a climb.
    pub waiting: bool,
}

#[derive(Clone, Copy, Debug)]
struct Hop {
    to: V3,
    crest: f32,
    t0: f32,
    need_jump: bool,
    jumped_t: f32,
    left_ground: bool,
    /// Horizontal length of the hop when it started.
    span: f32,
    /// Where the hop started, the ground end of its access, and whether it goes up.
    from: V3,
    ground: V3,
    up: bool,
}

#[derive(Default)]
pub struct Mover {
    path: Vec<V3>,
    path_goal: V3,
    path_t: f32,
    path_len: f32,
    hop: Option<Hop>,
    // stuck recovery
    slow_t: f32,
    level: u32,
    recover_until: f32,
    recover_dir: V2,
    recover_jump: bool,
    recover_jet: bool,
    last_stuck_t: f32,
    pub stuck_count: u32,
    /// Hops that did not get us there, per access (its ground point): count of failures.
    /// An access that failed twice is not used again this match - the map (or the
    /// rules) are not what the nav grid assumed there.
    failed_hops: Vec<(V3, u32)>,
    /// Hops that ended without arriving (diagnostics).
    pub hop_failures: u32,
}

/// Failed hops after which an access is given up.
const HOP_TRIES: u32 = 2;

/// Energy to jet `climb` metres up, with a margin for hovering over the edge.
pub fn climb_energy(climb: f32, jp: &JetParams) -> f32 {
    jp.jet_drain * (climb.max(0.0) / jp.max_rise_speed.max(1.0)) * 1.15 + 14.0
}

impl Mover {
    pub fn new() -> Mover {
        Mover { path_t: -1e9, last_stuck_t: -1e9, ..Default::default() }
    }

    pub fn hopping(&self) -> bool {
        self.hop.is_some()
    }
    pub fn recovering(&self, t: f32) -> bool {
        t < self.recover_until
    }
    pub fn clear_path(&mut self) {
        self.path.clear();
        self.path_t = -1e9;
    }
    pub fn path_points(&self) -> &[V3] {
        &self.path
    }
    /// Has hopping through this access failed too often?
    pub fn access_bad(&self, a: &Access) -> bool {
        self.failed_hops.iter().any(|(g, n)| *n >= HOP_TRIES && dist2d(*g, a.ground) < 1.0 && (g[2] - a.ground[2]).abs() < 2.0)
    }
    fn note_hop_end(&mut self, h: &Hop, me: &SelfObs) {
        // Arrived = standing near the far end, at its height.
        let ok = dist2d(me.pos, h.to) < h.span.max(3.0) * 0.8 + 2.5 && (me.pos[2] - h.to[2]).abs() < 2.5 && (!h.up || me.pos[2] > h.from[2] + 0.5 * (h.to[2] - h.from[2]));
        if ok {
            return;
        }
        self.hop_failures += 1;
        if std::env::var("MECH_VERBOSE").is_ok() {
            eprintln!("[hop-fail] up={} from=({:.1},{:.1},{:.1}) to=({:.1},{:.1},{:.1}) crest={:.1} ended at=({:.1},{:.1},{:.1}) grounded={} e={:.0} left_ground={} need_jump={}", h.up, h.from[0], h.from[1], h.from[2], h.to[0], h.to[1], h.to[2], h.crest, me.pos[0], me.pos[1], me.pos[2], me.grounded, me.energy, h.left_ground, h.need_jump);
        }
        let key = h.ground;
        match self.failed_hops.iter_mut().find(|(g, _)| dist2d(*g, key) < 1.0 && (g[2] - key[2]).abs() < 2.0) {
            Some((_, n)) => *n += 1,
            None => self.failed_hops.push((key, 1)),
        }
    }

    /// Walk (and hop, when `allow_climb`) towards `goal`. `can_plan` = there is time left
    /// in this decision for a path search.
    #[allow(clippy::too_many_arguments)]
    pub fn drive(&mut self, nav: &Nav, me: &SelfObs, goal: V3, arrive_r: f32, t: f32, jp: &JetParams, allow_climb: bool, can_plan: bool) -> Drive {
        let pos = me.pos;
        if let Some(h) = self.hop {
            if let Some(d) = self.do_hop(me, t) {
                return d;
            }
            self.note_hop_end(&h, me);
        }
        let straight = dist2d(pos, goal);
        let to_goal = norm2([goal[0] - pos[0], goal[1] - pos[1]]);
        if !me.grounded {
            // Falling or thrown: keep steering at the current waypoint.
            let dir = match self.path.first() {
                Some(w) => norm2([w[0] - pos[0], w[1] - pos[1]]),
                None => to_goal,
            };
            return Drive { dir, remaining: straight, ..Default::default() };
        }
        let cur = match nav.nearest_node(pos) {
            Some(n) => n,
            None => return Drive { dir: to_goal, remaining: straight, ..Default::default() },
        };
        let goal_node = nav.nearest_node(goal).unwrap_or(cur);
        let (cc, gc) = (nav.comp_of(cur), nav.comp_of(goal_node));
        if cc == gc {
            if straight < arrive_r && (goal[2] - pos[2]).abs() < 6.0 {
                return Drive { arrived: true, remaining: straight, ..Default::default() };
            }
            return self.walk(nav, pos, goal, t, can_plan);
        }
        if !nav.is_main(cur) {
            // We stand on a roof / deck / in an enclosed region and the goal is elsewhere: leave.
            if let Some(region) = nav.region_of(cur) {
                let best = region
                    .access
                    .iter()
                    .min_by(|a, b| {
                        let ca = dist(pos, a.top) + 0.3 * dist2d(a.ground, goal) + a.climb_out * 4.0;
                        let cb = dist(pos, b.top) + 0.3 * dist2d(b.ground, goal) + b.climb_out * 4.0;
                        ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .copied();
                if let Some(a) = best {
                    return self.via_access(nav, me, a, false, t, jp, can_plan);
                }
            }
            // No access data: walk off towards the goal.
            return Drive { dir: to_goal, remaining: straight, ..Default::default() };
        }
        // We are on the ground and the goal is on a region.
        if allow_climb {
            if let Some(region) = nav.region_of(goal_node) {
                let reach = 0.8 * max_climb(me.max_energy, jp);
                let best = region
                    .access
                    .iter()
                    .filter(|a| a.climb < reach && !self.access_bad(a))
                    .min_by(|a, b| {
                        let ca = dist2d(pos, a.ground) + dist(a.top, goal) + a.climb * 3.0;
                        let cb = dist2d(pos, b.ground) + dist(b.top, goal) + b.climb * 3.0;
                        ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .copied();
                if let Some(a) = best {
                    return self.via_access(nav, me, a, true, t, jp, can_plan);
                }
            }
        }
        // Not climbing: go as close as the ground allows.
        let near = nav.nearest_walkable(goal[0], goal[1]).unwrap_or(goal);
        if dist2d(pos, near) < arrive_r.max(2.0) {
            return Drive { arrived: true, remaining: 0.0, ..Default::default() };
        }
        self.walk(nav, pos, near, t, can_plan)
    }

    /// Walk to one end of an access and hop through it. `up` = ground -> top.
    #[allow(clippy::too_many_arguments)]
    fn via_access(&mut self, nav: &Nav, me: &SelfObs, a: Access, up: bool, t: f32, jp: &JetParams, can_plan: bool) -> Drive {
        let (from, to) = if up { (a.ground, a.top) } else { (a.top, a.ground) };
        let d = dist2d(me.pos, from);
        if d > 1.6 {
            let mut dr = self.walk(nav, me.pos, from, t, can_plan);
            dr.remaining += dist2d(from, to) + if up { a.climb * 2.0 } else { 0.0 };
            return dr;
        }
        let climb = if up { a.climb } else { a.climb_out };
        let need = if climb > me.stats.step_up { climb_energy(climb, jp) } else { 0.0 };
        if me.energy < need.min(me.max_energy - 1.0) {
            return Drive { waiting: true, hard: true, remaining: d + climb, ..Default::default() };
        }
        self.hop = Some(Hop { to, crest: a.crest, t0: t, need_jump: climb > me.stats.step_up, jumped_t: -1e9, left_ground: false, span: dist2d(me.pos, to), from: me.pos, ground: a.ground, up });
        self.path.clear();
        self.do_hop(me, t).unwrap_or_default()
    }

    fn do_hop(&mut self, me: &SelfObs, t: f32) -> Option<Drive> {
        let mut h = self.hop?;
        let pos = me.pos;
        let feet = pos[2];
        let d = dist2d(pos, h.to);
        let dir = norm2([h.to[0] - pos[0], h.to[1] - pos[1]]);
        if t - h.t0 > 8.0 {
            self.hop = None;
            return None;
        }
        if me.grounded {
            if h.need_jump && h.jumped_t < -1e8 {
                h.jumped_t = t;
                self.hop = Some(h);
                return Some(Drive { dir: [dir[0] * 0.3, dir[1] * 0.3], jump: true, jet: false, hard: true, remaining: d, ..Default::default() });
            }
            // Landed (after leaving the ground), or a plain walk-off that has arrived.
            let done = if h.need_jump { h.left_ground || t - h.jumped_t > 0.4 } else { h.left_ground || d < 1.5 || t - h.t0 > 4.0 };
            if done {
                self.hop = None;
                return None;
            }
            return Some(Drive { dir, hard: true, remaining: d, ..Default::default() });
        }
        h.left_ground = true;
        self.hop = Some(h);
        if !h.need_jump {
            return Some(Drive { dir, hard: true, remaining: d, ..Default::default() });
        }
        let clear = feet >= h.crest + 0.5;
        let going_up = h.to[2] > feet - 3.0; // target surface is not far below us
        let jet = if !clear {
            me.energy > 0.5
        } else {
            // hover over the edge until we are above the landing spot
            // (going up), or until we are well past the edge (coming down over a parapet)
            d > 1.2 && (going_up || d > h.span * 0.45) && feet < h.crest + 1.6 && me.vel[2] < 1.0 && me.energy > 0.5
        };
        let scale = if clear { 1.0 } else { 0.3 };
        Some(Drive { dir: [dir[0] * scale, dir[1] * scale], jet, hard: true, remaining: d, ..Default::default() })
    }

    fn walk(&mut self, nav: &Nav, pos: V3, goal: V3, t: f32, can_plan: bool) -> Drive {
        let moved = dist2d(goal, self.path_goal);
        let straight = dist2d(pos, goal);
        let stale = self.path.is_empty() || t - self.path_t > 0.6 || moved > (0.08 * straight).max(2.5) || t < self.path_t;
        if stale && can_plan {
            match nav.find_path(pos, goal) {
                Some(p) => {
                    self.path = p.points;
                    self.path_len = p.length;
                }
                None => {
                    self.path.clear();
                    self.path_len = straight;
                }
            }
            self.path_goal = goal;
            self.path_t = t;
        }
        // Drop waypoints we have reached (keep the last).
        while self.path.len() > 1 && dist2d(pos, self.path[0]) < 1.8 {
            self.path.remove(0);
        }
        let target = self.path.first().copied().unwrap_or(goal);
        let mut remaining = dist2d(pos, target);
        for w in self.path.windows(2) {
            remaining += dist2d(w[0], w[1]);
        }
        let dir = norm2([target[0] - pos[0], target[1] - pos[1]]);
        Drive { dir, remaining, ..Default::default() }
    }

    /// Stuck detection on the final leg command. Call once per decision with what the
    /// legs were told to do; returns an override while a recovery manoeuvre runs.
    pub fn unstick(&mut self, me: &SelfObs, cmd: V2, hard: bool, t: f32, dt: f32, stuck_time: f32, rng: &mut Rng) -> Option<Drive> {
        if t < self.recover_until {
            return Some(Drive { dir: self.recover_dir, jump: self.recover_jump && me.grounded, jet: self.recover_jet && !me.grounded && me.energy > 20.0, hard: true, ..Default::default() });
        }
        let speed = me.vel[0].hypot(me.vel[1]);
        let trying = len2(cmd) > 0.5 && me.grounded && !hard;
        if trying && speed < 1.5 {
            self.slow_t += dt;
        } else {
            self.slow_t = (self.slow_t - dt * 2.0).max(0.0);
            if speed > 5.0 && t - self.last_stuck_t > 3.0 {
                self.level = 0;
            }
        }
        if self.slow_t < stuck_time {
            return None;
        }
        // Stuck: escalate. 0 = jump and re-plan, 1 = sidestep, 2 = jump + jet over it, 3 = back off.
        self.slow_t = 0.0;
        self.stuck_count += 1;
        self.last_stuck_t = t;
        self.clear_path();
        let fwd = norm2(cmd);
        let side = perp(fwd);
        let s = rng.sign();
        let (dir, dur, jump, jet) = match self.level % 4 {
            0 => (fwd, 0.3, true, false),
            1 => ([side[0] * s + fwd[0] * 0.2, side[1] * s + fwd[1] * 0.2], 0.7, false, false),
            2 => (fwd, 1.0, true, true),
            _ => ([-fwd[0] + side[0] * s * 0.7, -fwd[1] + side[1] * s * 0.7], 0.9, false, false),
        };
        self.level += 1;
        self.recover_dir = norm2(dir);
        self.recover_until = t + dur;
        self.recover_jump = jump;
        self.recover_jet = jet;
        Some(Drive { dir: self.recover_dir, jump, jet: false, hard: true, ..Default::default() })
    }
}
