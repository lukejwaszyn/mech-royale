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
    /// Hops out of a region (off a roof, out of a yard or the stadium bowl) that failed
    /// in a row, and until when we stop hopping and simply walk at the goal, so that
    /// stuck recovery (jump, sidestep, jump and jet) gets a turn.
    exit_fails: u32,
    exit_pause_until: f32,
    /// Waiting for jet energy at an access: (its ground point, since, give up at, last seen waiting).
    wait: Option<(V3, f32, f32, f32)>,
}

/// Failed hops after which an access is given up.
const HOP_TRIES: u32 = 2;
/// Failed exits in a row after which hopping pauses for `EXIT_PAUSE_S`.
const EXIT_TRIES: u32 = 3;
const EXIT_PAUSE_S: f32 = 4.0;

/// Energy to jet `climb` metres up, with a margin for hovering over the edge.
pub fn climb_energy(climb: f32, jp: &JetParams) -> f32 {
    jp.jet_drain * (climb.max(0.0) / jp.max_rise_speed.max(1.0)) * 1.15 + 14.0
}

impl Mover {
    pub fn new() -> Mover {
        Mover { path_t: -1e9, last_stuck_t: -1e9, exit_pause_until: -1e9, ..Default::default() }
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
    fn note_hop_end(&mut self, h: &Hop, me: &SelfObs, t: f32) {
        // Arrived = standing near the far end, at its height.
        let ok = dist2d(me.pos, h.to) < h.span.max(3.0) * 0.8 + 2.5 && (me.pos[2] - h.to[2]).abs() < 2.5 && (!h.up || me.pos[2] > h.from[2] + 0.5 * (h.to[2] - h.from[2]));
        if ok {
            if !h.up {
                self.exit_fails = 0;
            }
            return;
        }
        self.hop_failures += 1;
        if !h.up {
            self.exit_fails += 1;
            if self.exit_fails >= EXIT_TRIES {
                self.exit_fails = 0;
                self.exit_pause_until = t + EXIT_PAUSE_S;
            }
        }
        if std::env::var("MECH_VERBOSE").is_ok() {
            crate::vlogln!("[hop-fail] up={} from=({:.1},{:.1},{:.1}) to=({:.1},{:.1},{:.1}) crest={:.1} ended at=({:.1},{:.1},{:.1}) grounded={} e={:.0} left_ground={} need_jump={}", h.up, h.from[0], h.from[1], h.from[2], h.to[0], h.to[1], h.to[2], h.crest, me.pos[0], me.pos[1], me.pos[2], me.grounded, me.energy, h.left_ground, h.need_jump);
        }
        self.fail_access(h.ground);
    }
    fn fail_access(&mut self, key: V3) {
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
            self.note_hop_end(&h, me, t);
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
            // A hop out that keeps failing (an eave stops the jet, a parapet is higher
            // than the map says, the server holds us on the edge) is not repeated for
            // ever: after a few failures walk straight at the goal for a moment - a
            // plain drive, so that stuck recovery can jump and jet us over.
            if t < self.exit_pause_until {
                return Drive { dir: to_goal, remaining: straight.max(10.0), ..Default::default() };
            }
            if let Some(region) = nav.region_of(cur) {
                if let Some(a) = self.exit_access(&region.access, pos, goal) {
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

    /// The access to leave a region by: the cheapest one whose hops have not failed, or
    /// (when all of them have) the cheapest of all.
    fn exit_access(&self, access: &[Access], pos: V3, goal: V3) -> Option<Access> {
        let cost = |a: &Access| dist(pos, a.top) + 0.3 * dist2d(a.ground, goal) + a.climb_out * 4.0;
        let pick = |all: bool| access.iter().filter(|a| all || !self.access_bad(a)).min_by(|a, b| cost(a).partial_cmp(&cost(b)).unwrap_or(std::cmp::Ordering::Equal)).copied();
        pick(false).or_else(|| pick(true))
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
            // Wait for the energy - but not as a `hard` drive: the brain may strafe and
            // dodge around this spot meanwhile (a mech standing still under fire at the
            // foot of a wall is a free target), and not for ever: energy that does not
            // come (lightning spends it as fast as it regenerates) counts as a failed hop.
            let fresh = self.wait.map_or(true, |(g, _, _, last)| dist(g, a.ground) > 1.0 || t - last > 1.0 || t < last);
            if fresh {
                let lack = need.min(me.max_energy - 1.0) - me.energy;
                self.wait = Some((a.ground, t, t + 3.0 + lack / 10.0, t));
            }
            if let Some(w) = self.wait.as_mut() {
                w.3 = t;
                if t > w.2 {
                    self.wait = None;
                    self.hop_failures += 1;
                    self.fail_access(a.ground);
                    if !up {
                        self.exit_fails += 1;
                    }
                }
            }
            return Drive { waiting: true, remaining: d + climb, ..Default::default() };
        }
        self.wait = None;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn access() -> Access {
        Access { ground: [10.0, 0.0, 0.0], ground_node: 0, top: [16.0, 0.0, 20.0], top_node: 1, crest: 20.0, climb: 20.0, climb_out: 0.0 }
    }
    fn hop(a: &Access, from: V3) -> Hop {
        Hop { to: a.top, crest: a.crest, t0: 0.0, need_jump: true, jumped_t: 0.0, left_ground: false, span: dist2d(from, a.top), from, ground: a.ground, up: true }
    }

    /// A hop that ends where it began (the jump never left the ground, or the jet was
    /// stopped) is a failure; two of them and the access is not used again. One that
    /// ends on the roof is not.
    #[test]
    fn failed_hops_ban_the_access() {
        let a = access();
        let mut m = Mover::new();
        let start = [11.2, 0.5, 0.0];
        let mut me = SelfObs { pos: start, ..Default::default() };
        m.note_hop_end(&hop(&a, start), &me, 0.0);
        assert!(!m.access_bad(&a), "one failure is not proof");
        m.note_hop_end(&hop(&a, start), &me, 0.0);
        assert!(m.access_bad(&a), "two failures ban it");
        assert_eq!(m.hop_failures, 2);
        // another access of the same roof is untouched
        let b = Access { ground: [10.0, 30.0, 0.0], ..a };
        assert!(!m.access_bad(&b));
        // a hop that arrives is not counted
        let mut m = Mover::new();
        me.pos = [15.4, 0.2, 20.0];
        m.note_hop_end(&hop(&a, start), &me, 0.0);
        m.note_hop_end(&hop(&a, start), &me, 0.0);
        assert!(!m.access_bad(&a));
        assert_eq!(m.hop_failures, 0);
    }

    /// A flat map with one 12 m building (x 20..60, y -30..10) whose roof is a region.
    fn roof_nav() -> Nav {
        let map = r#"{"bounds":{"minX":-200,"maxX":200,"minY":-200,"maxY":200},"terrain":{"minX":-210,"minY":-210,"step":420,"nx":2,"ny":2},
            "prisms":[{"kind":"building","base":-5,"top":12,"ring":[20,-30,60,-30,60,10,20,10]}]}"#;
        let world = std::sync::Arc::new(world::World::from_bytes(map.as_bytes(), &[0u8; 8]).unwrap());
        Nav::build(world, world::NavParams::default())
    }

    /// Leaving a roof: an access whose hops failed is not chosen again while another is
    /// left (the bot used to pick the same exit for ever and die on the roof), and after
    /// a few failed exits in a row the mover stops hopping for a moment and returns a
    /// plain drive at the goal, which stuck recovery may override.
    #[test]
    fn leaving_a_region_skips_failed_exits_and_pauses() {
        let nav = roof_nav();
        let roof = nav.regions().iter().find(|r| r.is_roof).expect("the roof is a region").clone();
        assert!(roof.access.len() >= 2, "the roof has several accesses");
        let pos = [40.0, -10.0, 12.0];
        let goal = [150.0, -10.0, 0.0];
        let me = SelfObs { pos, ..Default::default() };
        let jp = JetParams::default();
        let mut m = Mover::new();
        let first = m.exit_access(&roof.access, pos, goal).unwrap();
        // two failed hops through it (we are still on the roof, far from its ground end)
        let down = |a: &Access| Hop { to: a.ground, crest: a.crest, t0: 0.0, need_jump: false, jumped_t: -1e9, left_ground: false, span: 6.0, from: a.top, ground: a.ground, up: false };
        m.note_hop_end(&down(&first), &me, 1.0);
        m.note_hop_end(&down(&first), &me, 2.0);
        assert!(m.access_bad(&first));
        let second = m.exit_access(&roof.access, pos, goal).unwrap();
        assert!(dist(second.ground, first.ground) > 1.0, "another exit is tried");
        // every exit failed: still an answer (the cheapest), never None
        for a in &roof.access {
            m.failed_hops.push((a.ground, HOP_TRIES));
        }
        assert!(m.exit_access(&roof.access, pos, goal).is_some());
        // third failure in a row: no hop for a while, a plain drive straight at the goal
        m.note_hop_end(&down(&second), &me, 3.0);
        let d = m.drive(&nav, &me, goal, 1.0, 3.1, &jp, false, true);
        assert!(!d.hard && !m.hopping(), "no hop while paused");
        assert!(d.dir[0] > 0.9 && d.remaining >= 2.4, "walks at the goal so that stuck recovery can act: {d:?}");
        // after the pause the exits are tried again
        let d = m.drive(&nav, &me, goal, 1.0, 3.1 + EXIT_PAUSE_S + 0.1, &jp, false, true);
        assert!(d.dir != [0.0, 0.0] || d.hard, "moving again: {d:?}");
        assert!(m.hopping() || dot2(d.dir, [1.0, 0.0]) < 0.999, "heading for an exit, not blindly at the goal: {d:?}");
    }

    /// Waiting for jet energy at the foot of a roof is not a `hard` drive (the brain may
    /// strafe and dodge meanwhile), and energy that never comes ends the wait: it counts
    /// as a failed hop, twice and the access is given up.
    #[test]
    fn waiting_for_energy_is_soft_and_times_out() {
        let nav = roof_nav();
        let roof = nav.regions().iter().find(|r| r.is_roof).unwrap().clone();
        let a = roof.access[0];
        let jp = JetParams::default();
        let need = climb_energy(a.climb, &jp);
        let mut me = SelfObs { pos: a.ground, energy: 5.0, ..Default::default() };
        let mut m = Mover::new();
        let d = m.drive(&nav, &me, a.top, 1.0, 0.0, &jp, true, true);
        assert!(d.waiting && !d.hard && !m.hopping(), "waits, softly: {d:?}");
        // energy arrives: the hop starts
        me.energy = need + 5.0;
        let d = m.drive(&nav, &me, a.top, 1.0, 0.5, &jp, true, true);
        assert!(m.hopping() && d.hard && d.jump, "hops once the energy is there: {d:?}");
        // energy never arrives: each wait ends as a failed hop, and the access is given up
        let mut m = Mover::new();
        me.energy = 5.0;
        let mut t = 0.0;
        while t < 40.0 && !m.access_bad(&a) {
            m.drive(&nav, &me, a.top, 1.0, t, &jp, true, true);
            t += 0.1;
        }
        assert!(m.access_bad(&a), "an access we cannot find the energy for is given up");
        assert!(t > 6.0 && t < 30.0, "after a reasonable wait ({t:.1} s)");
    }
}
