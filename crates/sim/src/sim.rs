//! The match state and one-tick update. An APPROXIMATION of the real server built from
//! protocol.md; see README.md for the list of guesses.
use crate::config::{Config, MechStats, MortarStats, WKind, WeaponStats, WKINDS};
use crate::physics::{step_kin, Kin, MoveIn, MoveParams};
use crate::util::*;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use world::{Body, Nav, VisionParams, World};

/// One decision's worth of intentions (protocol "action").
#[derive(Clone, Debug, Default)]
pub struct Cmd {
    pub mv: V2,
    pub boost: bool,
    pub jump: bool,
    /// `None` keeps the previously commanded aim.
    pub aim: Option<V2>,
    pub fire: [bool; 2],
    pub reload: bool,
    pub pick: Option<usize>,
    pub take: Option<u32>,
    pub drop: Option<usize>,
}
impl Cmd {
    /// The continuous parts only (what a mech keeps while coasting).
    pub fn coast(&self) -> Cmd {
        Cmd { mv: self.mv, boost: self.boost, aim: None, ..Cmd::default() }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Weapon {
    pub kind: WKind,
    /// Seconds until it can fire (may be slightly negative between shots so the fire rate is exact).
    pub cooldown: f32,
    pub ammo: u32,
    pub reload: f32,
    pub charge: f32,
    pub charging: bool,
    pub release: bool,
}
impl Weapon {
    pub fn new(kind: WKind, mag: u32) -> Weapon {
        Weapon { kind, cooldown: 0.0, ammo: if kind == WKind::Gun { mag } else { 0 }, reload: 0.0, charge: 0.0, charging: false, release: false }
    }
}

#[derive(Clone, Debug)]
pub struct Offer {
    pub id: u32,
    pub expires: f32,
    /// Indices into `cfg.upgrades`.
    pub choices: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct Death {
    pub t: f32,
    pub cause: &'static str,
    pub by: Option<i32>,
    pub pos: V3,
}

#[derive(Clone, Copy, Debug)]
pub struct Hurt {
    pub t: f32,
    pub by: Option<i32>,
    pub weapon: &'static str,
    pub from: V3,
}

pub struct Mech {
    pub id: usize,
    pub alive: bool,
    /// 0 = no team. Scripted team mates never target each other (damage still applies).
    pub team: u8,
    pub kin: Kin,
    pub aim: V2,
    pub cmd_aim: V2,
    pub cmd: Cmd,
    pub jump_used: bool,
    pub hp: f32,
    pub max_hp: f32,
    pub energy: f32,
    pub cubes: u32,
    pub upgrades: Vec<String>,
    pub stats_map: Map<String, Value>,
    pub st: MechStats,
    pub wst: WeaponStats,
    pub weapons: [Option<Weapon>; 2],
    pub last_combat_t: f32,
    pub last_spend_t: f32,
    pub short_until: f32,
    pub in_zone: bool,
    pub offer: Option<Offer>,
    pub pending_offers: u32,
    pub next_offer_id: u32,
    /// Record protocol events for this mech (only needed for process bots).
    pub rec: bool,
    pub events: Vec<Value>,
    pub zone_acc: f32,
    pub kills: u32,
    pub place: u32,
    pub death: Option<Death>,
    pub killer: Option<(&'static str, Option<i32>)>,
    pub last_hurt: Option<Hurt>,
    /// Rubbed a wall / was blocked during the last tick.
    pub wall: bool,
    pub step_timer: f32,
    pub dmg_dealt: f32,
    pub dmg_taken: f32,
    /// Damage taken per attacking mech id (statistics only).
    pub dmg_from: Vec<f32>,
    /// Damage taken per weapon name (statistics only).
    pub dmg_by_weapon: Vec<(&'static str, f32)>,
    /// Gun bullets fired / gun bullets that hit a mech (statistics only).
    pub gun_shots: u32,
    pub gun_hits: u32,
    /// Damage dealt to mechs per weapon name (statistics only).
    pub dealt_by_weapon: Vec<(&'static str, f32)>,
}

impl Mech {
    pub fn eye(&self) -> V3 {
        up(self.kin.pos, self.st.eye_height)
    }
    pub fn center(&self) -> V3 {
        up(self.kin.pos, self.st.height * 0.5)
    }
    pub fn has(&self, k: WKind) -> bool {
        self.weapons.iter().flatten().any(|w| w.kind == k)
    }
    pub fn slot_of(&self, k: WKind) -> Option<usize> {
        self.weapons.iter().position(|w| w.map_or(false, |w| w.kind == k))
    }
    pub fn charging(&self) -> bool {
        self.weapons.iter().flatten().any(|w| w.charging)
    }
    pub fn body(&self) -> Body {
        Body { radius: self.st.radius, height: self.st.height, step_up: self.st.step_up, max_slope_deg: self.st.max_slope_deg }
    }
}

pub struct Npc {
    pub id: i32,
    pub alive: bool,
    pub kin: Kin,
    pub aim: V2,
    pub hp: f32,
    pub home: V3,
    pub target: Option<usize>,
    pub cooldown: f32,
    pub wander: V2,
    pub wander_until: f32,
}
pub struct Crate {
    pub id: u32,
    pub alive: bool,
    pub pos: V3,
    pub hp: f32,
}
pub struct Cube {
    pub id: u32,
    pub pos: V3,
}
pub struct Pickup {
    pub id: u32,
    pub pos: V3,
    pub w: Weapon,
}
pub struct SupplyDrop {
    pub id: u32,
    pub pos: V3,
    pub lands_at: f32,
    pub landed: bool,
    pub holder: Option<usize>,
    pub progress: f32,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PKind {
    Gun,
    Mortar,
    Npc,
}
impl PKind {
    pub fn name(self) -> &'static str {
        match self {
            PKind::Gun => "gun",
            PKind::Mortar => "mortar",
            PKind::Npc => "npc",
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Proj {
    pub id: u32,
    pub kind: PKind,
    /// Mech id (0..) or NPC id (100+).
    pub owner: i32,
    pub pos: V3,
    pub vel: V3,
    /// Gun / NPC: metres of range left. Mortar: seconds of fuse left.
    pub left: f32,
    pub damage: f32,
    pub mult: f32,
    pub mortar: Option<MortarStats>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tgt {
    Mech(usize),
    Npc(usize),
    Crate(usize),
}
#[derive(Clone, Copy, Debug)]
pub struct ZoneNow {
    pub center: V2,
    pub radius: f32,
    pub next_center: V2,
    pub next_radius: f32,
    pub shrink_start: f32,
    pub shrink_end: f32,
    pub dps: f32,
}
pub struct SoundRec {
    pub kind: &'static str,
    pub pos: V3,
    pub t: f32,
    pub src: Option<usize>,
}
pub struct BeamRec {
    pub by: i32,
    pub kind: &'static str,
    pub path: Vec<V3>,
    pub t: f32,
}

/// Places where things can spawn, built once from the nav grid.
pub struct SpawnTable {
    pub main: Vec<V3>,
    pub roof: Vec<V3>,
}
impl SpawnTable {
    pub fn build(nav: &Nav) -> SpawnTable {
        let mut main = Vec::new();
        let mut roof = Vec::new();
        for n in 0..nav.node_count() as u32 {
            if nav.is_main(n) {
                if nav.node_clearance(n) >= 4.5 {
                    main.push(nav.node_pos(n));
                }
            } else if let Some(r) = nav.region_of(n) {
                if r.is_roof && r.min_climb < 45.0 && r.area >= 150.0 && nav.node_clearance(n) >= 3.5 {
                    roof.push(nav.node_pos(n));
                }
            }
        }
        SpawnTable { main, roof }
    }
}

pub struct Sim {
    pub world: Arc<World>,
    pub cfg: Arc<Config>,
    pub spawn: Option<Arc<SpawnTable>>,
    pub rng: Rng,
    pub tick: u32,
    pub t: f32,
    pub dt: f32,
    pub mechs: Vec<Mech>,
    pub npcs: Vec<Npc>,
    pub crates: Vec<Crate>,
    pub cubes: Vec<Cube>,
    pub pickups: Vec<Pickup>,
    pub drops: Vec<SupplyDrop>,
    pub projs: Vec<Proj>,
    pub zone: ZoneNow,
    pub zone_circles: Vec<(V2, f32)>,
    pub alive_count: usize,
    pub sounds: Vec<SoundRec>,
    pub beams: Vec<BeamRec>,
    /// `last_seen[observer * stride + actor]`, actor = mech id, or `mechs.len() + npc index`.
    pub last_seen: Vec<f32>,
    pub seen_stride: usize,
    /// Human-readable log of notable things (deaths, drops) for replays and debugging.
    pub log: Vec<Value>,
    next_cube: u32,
    next_pickup: u32,
    next_crate: u32,
    next_proj: u32,
    next_drop: usize,
    finalized: bool,
}

/// Segment a->b against a vertical cylinder (feet `c`, radius `r`, height `h`): entry t in [0, 1].
pub fn seg_cyl(a: V3, b: V3, c: V3, r: f32, h: f32) -> Option<f32> {
    let d = sub(b, a);
    let (fx, fy) = (a[0] - c[0], a[1] - c[1]);
    let aa = d[0] * d[0] + d[1] * d[1];
    let (mut t0, mut t1);
    if aa < 1e-12 {
        if fx * fx + fy * fy > r * r {
            return None;
        }
        t0 = 0.0;
        t1 = 1.0;
    } else {
        let bb = fx * d[0] + fy * d[1];
        let cc = fx * fx + fy * fy - r * r;
        let disc = bb * bb - aa * cc;
        if disc < 0.0 {
            return None;
        }
        let s = disc.sqrt();
        t0 = (-bb - s) / aa;
        t1 = (-bb + s) / aa;
    }
    let (z0, z1) = (c[2], c[2] + h);
    if d[2].abs() < 1e-9 {
        if a[2] < z0 || a[2] > z1 {
            return None;
        }
    } else {
        let u0 = (z0 - a[2]) / d[2];
        let u1 = (z1 - a[2]) / d[2];
        let (lo, hi) = if u0 < u1 { (u0, u1) } else { (u1, u0) };
        t0 = t0.max(lo);
        t1 = t1.min(hi);
    }
    t0 = t0.max(0.0);
    t1 = t1.min(1.0);
    if t0 <= t1 {
        Some(t0)
    } else {
        None
    }
}

impl Sim {
    pub fn new(world: Arc<World>, cfg: Arc<Config>, seed: u64) -> Sim {
        let dt = 1.0 / cfg.match_.tick_hz;
        let mut s = Sim {
            world,
            spawn: None,
            rng: Rng::new(seed),
            tick: 0,
            t: 0.0,
            dt,
            mechs: Vec::new(),
            npcs: Vec::new(),
            crates: Vec::new(),
            cubes: Vec::new(),
            pickups: Vec::new(),
            drops: Vec::new(),
            projs: Vec::new(),
            zone: ZoneNow { center: cfg.zone.start_center, radius: cfg.zone.start_radius, next_center: cfg.zone.start_center, next_radius: cfg.zone.start_radius, shrink_start: 0.0, shrink_end: 0.0, dps: 0.0 },
            zone_circles: Vec::new(),
            alive_count: 0,
            sounds: Vec::new(),
            beams: Vec::new(),
            last_seen: Vec::new(),
            seen_stride: 0,
            log: Vec::new(),
            next_cube: 5000,
            next_pickup: 8000,
            next_crate: 1000,
            next_proj: 1,
            next_drop: 0,
            finalized: false,
            cfg,
        };
        s.roll_zone();
        s
    }

    // ------------------------------------------------------------------ setup

    /// Pick the zone circles: each next circle lies entirely inside the previous one.
    pub fn roll_zone(&mut self) {
        let cfg = self.cfg.clone();
        let b = self.world.bounds;
        let mut c = cfg.zone.start_center;
        let mut r = cfg.zone.start_radius;
        self.zone_circles = vec![(c, r)];
        for ph in &cfg.zone.phases {
            let slack = (r - ph.radius).max(0.0);
            let o = self.rng.disc();
            let mut n = [c[0] + o[0] * slack, c[1] + o[1] * slack];
            // keep the centre on the map (cannot leave the previous circle: it only moves towards the middle)
            let m = cfg.match_.arena_margin + ph.radius.min(60.0);
            let cl = [n[0].clamp(b.min_x + m, (b.max_x - m).max(b.min_x + m)), n[1].clamp(b.min_y + m, (b.max_y - m).max(b.min_y + m))];
            let dc = ((cl[0] - c[0]).powi(2) + (cl[1] - c[1]).powi(2)).sqrt();
            if dc <= slack + 1e-3 {
                n = cl;
            }
            c = n;
            r = ph.radius;
            self.zone_circles.push((c, r));
        }
        self.zone = self.zone_at(0.0);
    }

    /// Force every zone circle to one centre (tests).
    pub fn set_zone_center(&mut self, c: V2) {
        for z in self.zone_circles.iter_mut() {
            z.0 = c;
        }
        self.zone = self.zone_at(self.t);
    }

    pub fn zone_at(&self, t: f32) -> ZoneNow {
        let ph = &self.cfg.zone.phases;
        let mut t0 = 0.0;
        for (i, p) in ph.iter().enumerate() {
            let hold_end = t0 + p.hold_s;
            let end = hold_end + p.shrink_s;
            if t < end || i + 1 == ph.len() {
                let (c0, r0) = self.zone_circles[i];
                let (c1, r1) = self.zone_circles[i + 1];
                let f = if p.shrink_s > 0.0 { ((t - hold_end) / p.shrink_s).clamp(0.0, 1.0) } else if t >= hold_end { 1.0 } else { 0.0 };
                return ZoneNow {
                    center: [c0[0] + (c1[0] - c0[0]) * f, c0[1] + (c1[1] - c0[1]) * f],
                    radius: r0 + (r1 - r0) * f,
                    next_center: c1,
                    next_radius: r1,
                    shrink_start: hold_end,
                    shrink_end: end,
                    dps: p.dps,
                };
            }
            t0 = end;
        }
        let (c, r) = *self.zone_circles.last().unwrap();
        ZoneNow { center: c, radius: r, next_center: c, next_radius: r, shrink_start: 0.0, shrink_end: 0.0, dps: 0.0 }
    }

    pub fn add_mech(&mut self, pos: V3, yaw: f32) -> usize {
        let cfg = self.cfg.clone();
        let id = self.mechs.len();
        let z = self.world.floor_at(pos[0], pos[1], pos[2] + 0.5, 0.0).map(|f| f.z).unwrap_or(pos[2]);
        let grounded = (z - pos[2]).abs() < 0.6;
        let mut m = Mech {
            id,
            alive: true,
            team: 0,
            kin: Kin { pos: if grounded { [pos[0], pos[1], z] } else { pos }, vel: [0.0; 3], grounded },
            aim: [yaw, 0.0],
            cmd_aim: [yaw, 0.0],
            cmd: Cmd::default(),
            jump_used: false,
            hp: cfg.mech.hp,
            max_hp: cfg.mech.hp,
            energy: cfg.mech.energy,
            cubes: 0,
            upgrades: Vec::new(),
            stats_map: cfg.mech_map.clone(),
            st: cfg.mech,
            wst: cfg.weapons.clone(),
            weapons: [None, None],
            last_combat_t: -1e9,
            last_spend_t: -1e9,
            short_until: -1e9,
            in_zone: true,
            offer: None,
            pending_offers: 0,
            next_offer_id: 1,
            rec: false,
            events: Vec::new(),
            zone_acc: 0.0,
            kills: 0,
            place: 0,
            death: None,
            killer: None,
            last_hurt: None,
            wall: false,
            step_timer: 0.0,
            dmg_dealt: 0.0,
            dmg_taken: 0.0,
            dmg_from: Vec::new(),
            dmg_by_weapon: Vec::new(),
            gun_shots: 0,
            gun_hits: 0,
            dealt_by_weapon: Vec::new(),
        };
        m.weapons[0] = Some(Weapon::new(WKind::Gun, m.wst.gun.mag.round() as u32));
        self.mechs.push(m);
        self.alive_count += 1;
        self.resize_seen();
        id
    }

    fn resize_seen(&mut self) {
        self.seen_stride = self.mechs.len() + self.npcs.len();
        self.last_seen = vec![-1e9; self.mechs.len() * self.seen_stride];
    }

    pub fn give_weapon(&mut self, i: usize, kind: WKind) {
        let mag = self.mechs[i].wst.gun.mag.round() as u32;
        let m = &mut self.mechs[i];
        if m.has(kind) {
            return;
        }
        let slot = if m.weapons[0].is_none() { 0 } else { 1 };
        m.weapons[slot] = Some(Weapon::new(kind, mag));
    }
    pub fn add_crate(&mut self, pos: V3) -> usize {
        self.crates.push(Crate { id: self.next_crate, alive: true, pos, hp: self.cfg.crates.hp });
        self.next_crate += 1;
        self.crates.len() - 1
    }
    pub fn add_cube(&mut self, pos: V3) -> u32 {
        self.cubes.push(Cube { id: self.next_cube, pos });
        self.next_cube += 1;
        self.next_cube - 1
    }
    pub fn add_pickup(&mut self, kind: WKind, pos: V3) -> u32 {
        let mag = self.cfg.weapons.gun.mag.round() as u32;
        self.add_pickup_w(Weapon::new(kind, mag), pos)
    }
    fn add_pickup_w(&mut self, w: Weapon, pos: V3) -> u32 {
        self.pickups.push(Pickup { id: self.next_pickup, pos, w });
        self.next_pickup += 1;
        self.next_pickup - 1
    }
    pub fn add_npc(&mut self, pos: V3) -> usize {
        let id = 100 + self.npcs.len() as i32;
        self.npcs.push(Npc { id, alive: true, kin: Kin { pos, vel: [0.0; 3], grounded: true }, aim: [0.0, 0.0], hp: self.cfg.npc.hp, home: pos, target: None, cooldown: 0.0, wander: [pos[0], pos[1]], wander_until: 0.0 });
        self.resize_seen();
        self.npcs.len() - 1
    }

    /// Random match setup: mech spawns, crates, floor loot, drones. Needs a spawn table.
    pub fn setup_random(&mut self, table: Arc<SpawnTable>) {
        let cfg = self.cfg.clone();
        self.spawn = Some(table.clone());
        let zc = cfg.zone.start_center;
        let zr = cfg.zone.start_radius * cfg.match_.spawn_zone_fraction;
        let mut spots: Vec<V3> = Vec::new();
        let mut sep = cfg.match_.spawn_min_separation;
        let mut tries = 0;
        while spots.len() < cfg.match_.mechs && !table.main.is_empty() {
            let p = table.main[self.rng.usize(table.main.len())];
            tries += 1;
            if tries % 400 == 0 {
                sep *= 0.9;
            }
            if (p[0] - zc[0]).hypot(p[1] - zc[1]) > zr && tries < 20000 {
                continue;
            }
            if spots.iter().any(|q| dist2d(*q, p) < sep) {
                continue;
            }
            spots.push(p);
        }
        for p in &spots {
            let yaw = (zc[1] - p[1]).atan2(zc[0] - p[0]);
            self.add_mech(*p, yaw);
        }
        let pick = |rng: &mut Rng, roof: bool| -> V3 {
            let v = if roof && !table.roof.is_empty() { &table.roof } else { &table.main };
            v[rng.usize(v.len())]
        };
        if table.main.is_empty() {
            return;
        }
        for _ in 0..cfg.crates.count {
            let roof = self.rng.chance(cfg.crates.roof_fraction);
            let p = pick(&mut self.rng, roof);
            self.add_crate(p);
        }
        let wsum: f32 = WKINDS.iter().map(|k| cfg.loot.weights.get(k.name()).copied().unwrap_or(0.0)).sum();
        for _ in 0..cfg.loot.floor_count {
            let roof = self.rng.chance(cfg.loot.floor_roof_fraction);
            let p = pick(&mut self.rng, roof);
            let k = self.random_loot_kind(wsum);
            self.add_pickup(k, p);
        }
        for _ in 0..cfg.npc.count {
            let mut p = pick(&mut self.rng, false);
            for _ in 0..30 {
                if spots.iter().all(|q| dist2d(*q, p) > 70.0) {
                    break;
                }
                p = pick(&mut self.rng, false);
            }
            self.add_npc(p);
        }
    }

    fn random_loot_kind(&mut self, wsum: f32) -> WKind {
        let cfg = self.cfg.clone();
        let mut x = self.rng.f32() * wsum.max(1e-6);
        for k in WKINDS {
            let w = cfg.loot.weights.get(k.name()).copied().unwrap_or(0.0);
            if w > 0.0 {
                if x < w {
                    return k;
                }
                x -= w;
            }
        }
        WKind::Mortar
    }

    // ------------------------------------------------------------------ helpers

    pub fn dmg_mult(&self, i: usize) -> f32 {
        1.0 + self.cfg.cubes.damage_per_cube * self.mechs[i].cubes.min(self.cfg.cubes.stat_cap) as f32
    }

    pub fn sound(&mut self, kind: &'static str, pos: V3, src: Option<usize>) {
        self.sounds.push(SoundRec { kind, pos, t: self.t, src });
    }

    /// Shot geometry of mech `i`: (pivot point on the mech axis, shot origin, direction).
    pub fn muzzle(&self, i: usize, mortar: bool) -> (V3, V3, V3) {
        let m = &self.mechs[i];
        let h = if mortar { m.st.height + 0.5 } else { m.st.muzzle_height };
        let base = up(m.kin.pos, h);
        let dir = aim_dir(m.aim[0], m.aim[1]);
        let reach = m.st.radius + 0.5;
        let mut origin = add(base, scale(dir, reach));
        if let Some(hit) = self.world.raycast(base, origin) {
            origin = add(base, scale(dir, (reach * hit.t - 0.05).max(0.0)));
        }
        (base, origin, dir)
    }

    pub fn tgt_center(&self, tg: Tgt) -> V3 {
        match tg {
            Tgt::Mech(j) => self.mechs[j].center(),
            Tgt::Npc(k) => up(self.npcs[k].kin.pos, self.cfg.npc.height * 0.5),
            Tgt::Crate(c) => up(self.crates[c].pos, self.cfg.crates.height * 0.5),
        }
    }
    fn tgt_alive(&self, tg: Tgt) -> bool {
        match tg {
            Tgt::Mech(j) => self.mechs[j].alive && self.mechs[j].hp > 0.0,
            Tgt::Npc(k) => self.npcs[k].alive,
            Tgt::Crate(c) => self.crates[c].alive,
        }
    }

    /// First mech / drone / crate hit by segment a->b (cylinders padded by `pad`).
    pub fn first_entity_hit(&self, a: V3, b: V3, pad: f32, owner: i32, skip: &[Tgt]) -> Option<(Tgt, f32)> {
        let mut best: Option<(Tgt, f32)> = None;
        let (x0, x1) = (a[0].min(b[0]) - 12.0, a[0].max(b[0]) + 12.0);
        let (y0, y1) = (a[1].min(b[1]) - 12.0, a[1].max(b[1]) + 12.0);
        let mut test = |tg: Tgt, c: V3, r: f32, h: f32| {
            if c[0] < x0 || c[0] > x1 || c[1] < y0 || c[1] > y1 || skip.contains(&tg) {
                return;
            }
            if let Some(t) = seg_cyl(a, b, up(c, -pad), r + pad, h + 2.0 * pad) {
                if best.map_or(true, |(_, bt)| t < bt) {
                    best = Some((tg, t));
                }
            }
        };
        for (j, m) in self.mechs.iter().enumerate() {
            if m.alive && j as i32 != owner {
                test(Tgt::Mech(j), m.kin.pos, m.st.radius, m.st.height);
            }
        }
        for (k, n) in self.npcs.iter().enumerate() {
            if n.alive && n.id != owner {
                test(Tgt::Npc(k), n.kin.pos, self.cfg.npc.radius, self.cfg.npc.height);
            }
        }
        for (c, cr) in self.crates.iter().enumerate() {
            if cr.alive {
                test(Tgt::Crate(c), cr.pos, self.cfg.crates.radius, self.cfg.crates.height);
            }
        }
        best
    }

    fn vision_params(&self, m: &Mech) -> VisionParams {
        let v = &self.cfg.vision;
        VisionParams { range: v.range + m.st.vision_bonus, fov_deg: v.fov_deg, near_range: v.near_range + m.st.near_range_bonus, canopy_blocks: v.canopy_blocks }
    }

    /// Protocol section 8 visibility of a thing standing at `feet` for mech `i`, right now.
    pub fn can_see(&self, i: usize, feet: V3, height: f32) -> bool {
        let m = &self.mechs[i];
        let vp = self.vision_params(m);
        let d2 = (feet[0] - m.kin.pos[0]).powi(2) + (feet[1] - m.kin.pos[1]).powi(2);
        if d2 > (vp.range + 1.0) * (vp.range + 1.0) {
            return false;
        }
        self.world.can_see(m.eye(), m.aim[0], feet, height, &vp)
    }

    /// Refresh who sees whom (called once per decision). Seen actors stay tracked for `memory_s`.
    pub fn update_vision(&mut self) {
        let n = self.mechs.len();
        let stride = self.seen_stride;
        for i in 0..n {
            if !self.mechs[i].alive {
                continue;
            }
            for j in 0..n {
                if i != j && self.mechs[j].alive && self.can_see(i, self.mechs[j].kin.pos, self.mechs[j].st.height) {
                    self.last_seen[i * stride + j] = self.t;
                }
            }
            for k in 0..self.npcs.len() {
                if self.npcs[k].alive && self.can_see(i, self.npcs[k].kin.pos, self.cfg.npc.height) {
                    self.last_seen[i * stride + n + k] = self.t;
                }
            }
        }
    }
    /// Is mech `j` listed in mech `i`'s observation (seen now or within `memory_s`)?
    pub fn sees(&self, i: usize, j: usize) -> bool {
        self.mechs[j].alive && self.t - self.last_seen[i * self.seen_stride + j] <= self.cfg.vision.memory_s
    }
    /// Does mech `i` see mech `j` at this very decision (no memory)?
    pub fn sees_now(&self, i: usize, j: usize) -> bool {
        self.mechs[j].alive && self.t - self.last_seen[i * self.seen_stride + j] <= 1e-4
    }
    pub fn sees_npc(&self, i: usize, k: usize) -> bool {
        self.npcs[k].alive && self.t - self.last_seen[i * self.seen_stride + self.mechs.len() + k] <= self.cfg.vision.memory_s
    }

    pub fn over(&self) -> bool {
        self.alive_count <= 1 || self.t >= self.cfg.match_.duration_s
    }

    /// Give the survivors their places (winner, or ranked by HP when the clock ran out).
    pub fn finalize(&mut self) {
        if self.finalized {
            return;
        }
        self.finalized = true;
        let mut surv: Vec<usize> = (0..self.mechs.len()).filter(|&i| self.mechs[i].alive).collect();
        surv.sort_by(|&a, &b| self.mechs[b].hp.partial_cmp(&self.mechs[a].hp).unwrap_or(std::cmp::Ordering::Equal));
        let mut place = 1;
        for (k, &i) in surv.iter().enumerate() {
            if k > 0 && self.mechs[i].hp < self.mechs[surv[k - 1]].hp {
                place = k as u32 + 1;
            }
            self.mechs[i].place = place;
        }
    }

    // ------------------------------------------------------------------ actions

    /// Apply this decision's actions (call once per decision, before the ticks).
    pub fn apply_cmds(&mut self, cmds: Vec<(usize, Cmd)>) {
        let mut takes: Vec<(usize, u32, Option<usize>)> = Vec::new();
        for (i, mut c) in cmds {
            if i >= self.mechs.len() || !self.mechs[i].alive {
                continue;
            }
            if !c.mv[0].is_finite() || !c.mv[1].is_finite() {
                c.mv = [0.0, 0.0];
            }
            let pick = c.pick;
            let take = c.take;
            let drop = c.drop.filter(|&s| s < 2);
            {
                let m = &mut self.mechs[i];
                if let Some(a) = c.aim {
                    if a[0].is_finite() && a[1].is_finite() {
                        m.cmd_aim = [wrap_pi(a[0]), a[1].clamp(m.st.min_pitch, m.st.max_pitch)];
                    }
                }
                m.jump_used = false;
                m.cmd = c;
            }
            if let Some(p) = pick {
                let ch = self.mechs[i].offer.as_ref().and_then(|o| o.choices.get(p).copied());
                if let Some(u) = ch {
                    self.apply_upgrade(i, u);
                }
            }
            match (take, drop) {
                (Some(id), d) => takes.push((i, id, d)),
                (None, Some(s)) => self.drop_weapon(i, s),
                _ => {}
            }
        }
        self.resolve_takes(takes);
    }

    fn event(&mut self, i: usize, v: Value) {
        if self.mechs[i].rec {
            self.mechs[i].events.push(v);
        }
    }

    fn drop_weapon(&mut self, i: usize, slot: usize) {
        let Some(w) = self.mechs[i].weapons[slot] else { return };
        if w.charging {
            return;
        }
        self.mechs[i].weapons[slot] = None;
        let pos = self.mechs[i].kin.pos;
        let mut w = w;
        w.cooldown = w.cooldown.max(0.0);
        let id = self.add_pickup_w(w, pos);
        self.event(i, json!({"type":"dropped","id":id,"kind":w.kind.name(),"slot":slot}));
    }

    fn resolve_takes(&mut self, takes: Vec<(usize, u32, Option<usize>)>) {
        if takes.is_empty() {
            return;
        }
        let reach = self.cfg.loot.pickup_radius;
        // (mech, pickup id, slot, distance)
        let mut ok: Vec<(usize, u32, usize, f32)> = Vec::new();
        for (i, id, drop) in takes {
            let fail = |s: &mut Sim, reason: &str| s.event(i, json!({"type":"take_failed","id":id,"reason":reason}));
            let Some(p) = self.pickups.iter().find(|p| p.id == id) else {
                fail(self, "gone");
                continue;
            };
            let m = &self.mechs[i];
            let d = dist2d(m.kin.pos, p.pos);
            if d > reach || (m.kin.pos[2] - p.pos[2]).abs() > 4.0 {
                fail(self, "range");
                continue;
            }
            let kind = p.w.kind;
            if m.weapons.iter().enumerate().any(|(s, w)| w.map_or(false, |w| w.kind == kind) && Some(s) != drop) {
                fail(self, "have");
                continue;
            }
            let slot = match drop {
                Some(s) => {
                    if m.weapons[s].map_or(false, |w| w.charging) {
                        fail(self, "busy");
                        continue;
                    }
                    s
                }
                None => match m.weapons.iter().position(|w| w.is_none()) {
                    Some(s) => s,
                    None => {
                        fail(self, "full");
                        continue;
                    }
                },
            };
            ok.push((i, id, slot, d));
        }
        ok.sort_by(|a, b| a.1.cmp(&b.1).then(a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal)).then(a.0.cmp(&b.0)));
        let mut last = u32::MAX;
        for (i, id, slot, _) in ok {
            if id == last {
                self.event(i, json!({"type":"take_failed","id":id,"reason":"gone"}));
                continue;
            }
            last = id;
            let idx = self.pickups.iter().position(|p| p.id == id).unwrap();
            let mut w = self.pickups.remove(idx).w;
            if self.mechs[i].weapons[slot].is_some() {
                self.drop_weapon(i, slot);
            }
            let equip = self.cfg.loot.equip_s.max(self.mechs[i].wst.cooldown(w.kind));
            w.cooldown = w.cooldown.max(equip);
            w.charge = 0.0;
            w.charging = false;
            self.mechs[i].weapons[slot] = Some(w);
            self.event(i, json!({"type":"took","id":id,"kind":w.kind.name(),"slot":slot}));
        }
    }

    // ------------------------------------------------------------------ cubes / upgrades

    fn recompute(&mut self, i: usize) {
        let cfg = self.cfg.clone();
        let m = &mut self.mechs[i];
        if let Ok(mut st) = serde_json::from_value::<MechStats>(Value::Object(m.stats_map.clone())) {
            let size = 1.0 + cfg.cubes.size_per_cube * m.cubes as f32;
            st.radius *= size;
            st.height *= size;
            st.eye_height *= size;
            st.muzzle_height *= size;
            m.st = st;
        }
        let new_max = m.st.hp * (1.0 + cfg.cubes.hp_per_cube * m.cubes.min(cfg.cubes.stat_cap) as f32);
        if new_max > m.max_hp {
            m.hp += new_max - m.max_hp;
        }
        m.max_hp = new_max;
        m.hp = m.hp.min(m.max_hp);
        m.energy = m.energy.min(m.st.energy);
    }

    pub fn gain_cubes(&mut self, i: usize, n: u32) {
        let cfg = self.cfg.clone();
        let old = self.mechs[i].cubes;
        self.mechs[i].cubes += n;
        let new = old + n;
        let hit = cfg.cubes.upgrade_thresholds.iter().filter(|&&th| th > old && th <= new).count() as u32;
        self.mechs[i].pending_offers += hit;
        self.recompute(i);
    }

    fn draw_offer(&mut self, i: usize) {
        let cfg = self.cfg.clone();
        let mut elig: Vec<usize> = Vec::new();
        {
            let m = &self.mechs[i];
            for (u, def) in cfg.upgrades.iter().enumerate() {
                let have = m.upgrades.iter().filter(|x| **x == def.id).count() as u32;
                if have >= def.max {
                    continue;
                }
                if let Some(req) = &def.requires {
                    if !WKind::from_name(req).map_or(false, |k| m.has(k)) {
                        continue;
                    }
                }
                elig.push(u);
            }
        }
        self.mechs[i].pending_offers -= 1;
        if elig.is_empty() {
            return;
        }
        self.rng.shuffle(&mut elig);
        elig.truncate(3);
        let m = &mut self.mechs[i];
        m.offer = Some(Offer { id: m.next_offer_id, expires: self.t + cfg.bot.offer_timeout_s, choices: elig });
        m.next_offer_id += 1;
    }

    pub fn apply_upgrade(&mut self, i: usize, u: usize) {
        let cfg = self.cfg.clone();
        let def = &cfg.upgrades[u];
        {
            let m = &mut self.mechs[i];
            for e in &def.effects {
                let parts: Vec<&str> = e.path.split('.').collect();
                let slot: Option<&mut Value> = match parts.as_slice() {
                    ["mech", key] => m.stats_map.get_mut(*key),
                    ["weapons", kind, key] => WKind::from_name(kind).and_then(|k| m.wst.maps[k as usize].get_mut(*key)),
                    _ => None,
                };
                if let Some(v) = slot {
                    let x = v.as_f64().unwrap_or(0.0) * e.mul.unwrap_or(1.0) + e.add.unwrap_or(0.0);
                    *v = json!((x * 1e6).round() / 1e6);
                }
            }
            m.wst.refresh();
            m.upgrades.push(def.id.clone());
            m.offer = None;
        }
        self.recompute(i);
        self.event(i, json!({"type":"upgrade","id":def.id}));
    }

    // ------------------------------------------------------------------ damage

    /// Deal `amount` (already scaled by the attacker's cubes) to a target. `from` is where
    /// it came from (rear armor and the reported bearing).
    pub fn hurt(&mut self, tg: Tgt, amount: f32, weapon: &'static str, by: Option<i32>, from: V3, effect: Option<&'static str>) {
        let t = self.t;
        let attacker = by.filter(|&b| b >= 0 && (b as usize) < self.mechs.len()).map(|b| b as usize);
        let mut dealt = amount;
        let mut rear = false;
        let target_id: i64;
        match tg {
            Tgt::Mech(j) => {
                target_id = j as i64;
                let self_hit = attacker == Some(j);
                let m = &mut self.mechs[j];
                if !m.alive {
                    return;
                }
                let bearing = yaw_to(m.kin.pos, from);
                rear = !self_hit && wrap_pi(bearing - m.aim[0]).abs() > m.st.front_arc_deg.to_radians() * 0.5;
                if rear {
                    dealt *= m.st.rear_mult;
                }
                dealt *= (1.0 - m.st.armor).max(0.0);
                let was = m.hp;
                m.hp -= dealt;
                m.last_combat_t = t;
                m.dmg_taken += dealt;
                if let Some(a) = attacker {
                    if m.dmg_from.len() <= a {
                        m.dmg_from.resize(a + 1, 0.0);
                    }
                    m.dmg_from[a] += dealt;
                }
                match m.dmg_by_weapon.iter_mut().find(|(w, _)| *w == weapon) {
                    Some((_, x)) => *x += dealt,
                    None => m.dmg_by_weapon.push((weapon, dealt)),
                }
                m.last_hurt = Some(Hurt { t, by, weapon, from });
                if was > 0.0 && m.hp <= 0.0 {
                    m.killer = Some((weapon, by));
                }
                if m.rec {
                    let mut ev = json!({"type":"damaged","amount":r2(dealt),"weapon":weapon,"bearing":r3(bearing),"by":by});
                    if rear {
                        ev["rear"] = json!(true);
                    }
                    m.events.push(ev);
                    if let Some(e) = effect {
                        m.events.push(json!({"type":"effect","effect":e,"by":by}));
                    }
                }
                if self_hit {
                    return;
                }
            }
            Tgt::Npc(k) => {
                let cubes = self.cfg.npc.cubes;
                let n = &mut self.npcs[k];
                if !n.alive {
                    return;
                }
                target_id = n.id as i64;
                n.hp -= dealt;
                if let Some(a) = attacker {
                    n.target = Some(a);
                }
                if n.hp <= 0.0 {
                    n.alive = false;
                    let pos = n.kin.pos;
                    for _ in 0..cubes {
                        let o = self.rng.disc();
                        self.add_cube([pos[0] + o[0] * 2.0, pos[1] + o[1] * 2.0, pos[2]]);
                    }
                }
            }
            Tgt::Crate(c) => {
                let cr = &mut self.crates[c];
                if !cr.alive {
                    return;
                }
                target_id = cr.id as i64;
                cr.hp -= dealt;
                if cr.hp <= 0.0 {
                    cr.alive = false;
                    let pos = cr.pos;
                    for _ in 0..self.cfg.crates.cubes {
                        self.add_cube(pos);
                    }
                    if self.rng.chance(self.cfg.loot.crate_weapon_chance) {
                        let wsum: f32 = self.cfg.loot.weights.values().sum();
                        let k = self.random_loot_kind(wsum);
                        self.add_pickup(k, pos);
                    }
                    self.sound("crate", pos, None);
                }
            }
        }
        if let Some(a) = attacker {
            let am = &mut self.mechs[a];
            if !matches!(tg, Tgt::Crate(_)) {
                am.last_combat_t = t;
            }
            am.dmg_dealt += dealt;
            if matches!(tg, Tgt::Mech(_)) {
                if weapon == "gun" {
                    am.gun_hits += 1;
                }
                match am.dealt_by_weapon.iter_mut().find(|(w, _)| *w == weapon) {
                    Some((_, x)) => *x += dealt,
                    None => am.dealt_by_weapon.push((weapon, dealt)),
                }
            }
            if am.rec {
                let mut ev = json!({"type":"hit","target":target_id,"amount":r2(dealt),"weapon":weapon});
                if let Some(e) = effect {
                    ev["effect"] = json!(e);
                }
                if rear {
                    ev["rear"] = json!(true);
                }
                am.events.push(ev);
            }
        }
    }

    fn explode(&mut self, p: V3, owner: i32, ms: MortarStats, mult: f32, direct: Option<Tgt>) {
        let own = if owner >= 0 && (owner as usize) < self.mechs.len() { Some(owner as usize) } else { None };
        self.sound("explosion", p, None);
        if let Some(tg) = direct {
            self.hurt(tg, ms.damage * mult, "mortar", Some(owner), p, None);
        }
        let pu = up(p, 0.3);
        let rr = ms.splash_radius.max(0.01);
        let fall = |d: f32| 1.0 - (1.0 - ms.splash_edge) * (d / rr).clamp(0.0, 1.0);
        for j in 0..self.mechs.len() {
            let (pos, h, alive) = (self.mechs[j].kin.pos, self.mechs[j].st.height, self.mechs[j].alive);
            if !alive {
                continue;
            }
            let q = [pos[0], pos[1], p[2].clamp(pos[2], pos[2] + h)];
            let d = dist(p, q);
            if d > rr {
                continue;
            }
            let hit_direct = direct == Some(Tgt::Mech(j));
            if !hit_direct && !self.world.los(pu, up(pos, h * 0.5)) && !self.world.los(pu, up(pos, h)) {
                continue;
            }
            let f = fall(d);
            let self_mult = if own == Some(j) { self.mechs[j].st.self_damage_mult } else { 1.0 };
            self.hurt(Tgt::Mech(j), ms.splash * mult * f * self_mult, "mortar", Some(owner), p, Some("concuss"));
            let mut dir = [pos[0] - p[0], pos[1] - p[1]];
            let dl = dir[0].hypot(dir[1]);
            if dl > 0.05 {
                dir = [dir[0] / dl, dir[1] / dl];
            } else {
                let a = self.rng.range(0.0, std::f32::consts::TAU);
                dir = [a.cos(), a.sin()];
            }
            let m = &mut self.mechs[j];
            m.kin.vel[0] += dir[0] * ms.knock * f;
            m.kin.vel[1] += dir[1] * ms.knock * f;
            m.kin.vel[2] = m.kin.vel[2].max(0.0) + ms.knock_up * f;
            m.kin.grounded = false;
            m.energy = (m.energy - ms.drain * f).max(0.0);
        }
        for k in 0..self.npcs.len() {
            let c = up(self.npcs[k].kin.pos, self.cfg.npc.height * 0.5);
            let d = dist(p, c);
            if self.npcs[k].alive && d <= rr && direct != Some(Tgt::Npc(k)) && self.world.los(pu, c) {
                self.hurt(Tgt::Npc(k), ms.splash * mult * fall(d), "mortar", Some(owner), p, None);
            }
        }
        for c in 0..self.crates.len() {
            let cc = up(self.crates[c].pos, self.cfg.crates.height * 0.5);
            let d = dist(p, cc);
            if self.crates[c].alive && d <= rr && direct != Some(Tgt::Crate(c)) && self.world.los(pu, cc) {
                self.hurt(Tgt::Crate(c), ms.splash * mult * fall(d), "mortar", Some(owner), p, None);
            }
        }
    }

    // ------------------------------------------------------------------ weapons

    fn fire_gun(&mut self, i: usize) {
        let (base, origin, _) = self.muzzle(i, false);
        let g = self.mechs[i].wst.gun;
        let s = g.spread_deg.to_radians();
        let yaw = self.mechs[i].aim[0] + self.rng.gauss() * s;
        let pitch = self.mechs[i].aim[1] + self.rng.gauss() * s;
        let dir = aim_dir(yaw, pitch);
        let mult = self.dmg_mult(i);
        self.projs.push(Proj { id: self.next_proj, kind: PKind::Gun, owner: i as i32, pos: origin, vel: scale(dir, g.speed), left: g.range, damage: g.damage * mult, mult, mortar: None });
        self.next_proj += 1;
        self.mechs[i].gun_shots += 1;
        self.sound("gun", base, Some(i));
    }

    fn fire_mortar(&mut self, i: usize) {
        let (base, origin, dir) = self.muzzle(i, true);
        let ms = self.mechs[i].wst.mortar;
        let mult = self.dmg_mult(i);
        self.projs.push(Proj { id: self.next_proj, kind: PKind::Mortar, owner: i as i32, pos: origin, vel: scale(dir, ms.speed), left: ms.ttl, damage: ms.damage * mult, mult, mortar: Some(ms) });
        self.next_proj += 1;
        self.sound("mortar", base, Some(i));
    }

    fn fire_laser(&mut self, i: usize, frac: f32) {
        let (base, origin, dir) = self.muzzle(i, false);
        let ls = self.mechs[i].wst.laser;
        let mut end = add(origin, scale(dir, ls.range));
        if let Some(h) = self.world.raycast(origin, end) {
            end = h.pos;
        }
        let dmg = ls.damage * (ls.min_frac + (1.0 - ls.min_frac) * frac.clamp(0.0, 1.0)) * self.dmg_mult(i);
        let max_hits = ls.pierce.round().max(0.0) as usize + 1;
        let mut skip: Vec<Tgt> = Vec::new();
        let mut last_pt = end;
        while skip.len() < max_hits {
            let Some((tg, t)) = self.first_entity_hit(origin, end, 0.0, i as i32, &skip) else { break };
            skip.push(tg);
            last_pt = add(origin, scale(sub(end, origin), t));
            let cook = matches!(tg, Tgt::Mech(j) if self.mechs[j].has(WKind::Mortar));
            if cook {
                self.hurt(tg, dmg * (1.0 + ls.cookoff), "laser", Some(i as i32), origin, Some("cookoff"));
            } else {
                self.hurt(tg, dmg, "laser", Some(i as i32), origin, None);
            }
        }
        if skip.len() == max_hits {
            end = last_pt;
        }
        self.beams.push(BeamRec { by: i as i32, kind: "laser", path: vec![origin, end], t: self.t });
        self.sound("laser", base, Some(i));
    }

    /// Candidates for a lightning arc from `from`: nearest valid target not in `skip`.
    fn arc_target(&self, from: V3, range: f32, cone: Option<(f32, f32)>, shooter: usize, skip: &[Tgt]) -> Option<Tgt> {
        let mut best: Option<(f32, Tgt)> = None;
        let mut consider = |tg: Tgt, c: V3, top: V3| {
            if skip.contains(&tg) {
                return;
            }
            let d = dist(from, c);
            if d > range || best.map_or(false, |(bd, _)| d >= bd) {
                return;
            }
            if let Some((yaw, half)) = cone {
                if wrap_pi(yaw_to(from, c) - yaw).abs() > half {
                    return;
                }
            }
            if self.world.los(from, c) || self.world.los(from, top) {
                best = Some((d, tg));
            }
        };
        for (j, m) in self.mechs.iter().enumerate() {
            if m.alive && j != shooter {
                consider(Tgt::Mech(j), m.center(), up(m.kin.pos, m.st.height));
            }
        }
        for (k, n) in self.npcs.iter().enumerate() {
            if n.alive {
                consider(Tgt::Npc(k), up(n.kin.pos, self.cfg.npc.height * 0.5), up(n.kin.pos, self.cfg.npc.height));
            }
        }
        for (c, cr) in self.crates.iter().enumerate() {
            if cr.alive {
                consider(Tgt::Crate(c), up(cr.pos, self.cfg.crates.height * 0.5), up(cr.pos, self.cfg.crates.height));
            }
        }
        best.map(|b| b.1)
    }

    fn fire_lightning(&mut self, i: usize) {
        let (base, origin, dir) = self.muzzle(i, false);
        let ls = self.mechs[i].wst.lightning;
        let yaw = self.mechs[i].aim[0];
        let mut dmg = ls.damage * self.dmg_mult(i);
        let mut path = vec![origin];
        let mut hit: Vec<Tgt> = Vec::new();
        let mut from = base;
        let jumps = ls.chain.round().max(0.0) as usize + 1;
        for n in 0..jumps {
            let tg = if n == 0 { self.arc_target(from, ls.range, Some((yaw, ls.cone_deg.to_radians())), i, &hit) } else { self.arc_target(from, ls.chain_range, None, i, &hit) };
            let Some(tg) = tg else { break };
            let c = self.tgt_center(tg);
            let mut effect = None;
            if let Tgt::Mech(j) = tg {
                if self.mechs[j].has(WKind::Laser) {
                    effect = Some("short");
                    let until = self.t + ls.short_s;
                    let m = &mut self.mechs[j];
                    m.short_until = until;
                    for w in m.weapons.iter_mut().flatten() {
                        if w.kind == WKind::Laser {
                            w.charging = false;
                            w.charge = 0.0;
                            w.release = false;
                        }
                    }
                }
            }
            self.hurt(tg, dmg, "lightning", Some(i as i32), from, effect);
            hit.push(tg);
            path.push(c);
            from = c;
            dmg *= ls.chain_falloff;
        }
        if hit.is_empty() {
            let mut end = add(origin, scale(dir, ls.range));
            if let Some(h) = self.world.raycast(origin, end) {
                end = h.pos;
            }
            path.push(end);
        }
        self.beams.push(BeamRec { by: i as i32, kind: "lightning", path, t: self.t });
        self.sound("lightning", base, Some(i));
    }

    fn step_weapons(&mut self, i: usize) {
        let dt = self.dt;
        let t = self.t;
        #[derive(Clone, Copy)]
        enum Shot {
            Gun,
            Mortar,
            Laser(f32),
            Lightning,
            Snd(&'static str),
        }
        let mut shots: Vec<Shot> = Vec::new();
        {
            let m = &mut self.mechs[i];
            let fire = m.cmd.fire;
            let want_reload = std::mem::replace(&mut m.cmd.reload, false);
            let shorted = t < m.short_until;
            let wst = &m.wst;
            let mut energy = m.energy;
            let mut spent = false;
            for s in 0..2 {
                let Some(w) = m.weapons[s].as_mut() else { continue };
                // `cooldown` is the time left at the start of this tick. A small negative
                // value is carried into the next shot so a held trigger fires at exactly
                // one shot per cooldown on average.
                let ready = w.cooldown <= 1e-5;
                let mut fired = false;
                match w.kind {
                    WKind::Gun => {
                        let mag = wst.gun.mag.round().max(1.0) as u32;
                        if w.reload > 0.0 {
                            w.reload -= dt;
                            if w.reload <= 1e-5 {
                                w.reload = 0.0;
                                w.ammo = mag;
                            }
                        } else if want_reload && w.ammo < mag {
                            w.reload = wst.gun.reload;
                            shots.push(Shot::Snd("reload"));
                        }
                        if fire[s] && ready && w.reload <= 0.0 && w.ammo > 0 {
                            w.ammo -= 1;
                            w.cooldown = w.cooldown.min(0.0) + wst.gun.cooldown;
                            fired = true;
                            shots.push(Shot::Gun);
                            if w.ammo == 0 {
                                w.reload = wst.gun.reload;
                                shots.push(Shot::Snd("reload"));
                            }
                        }
                    }
                    WKind::Mortar => {
                        if fire[s] && ready {
                            w.cooldown = wst.mortar.cooldown;
                            fired = true;
                            shots.push(Shot::Mortar);
                        }
                    }
                    WKind::Laser => {
                        let ls = wst.laser;
                        if w.charging {
                            w.charge += dt;
                            if !fire[s] {
                                w.release = true;
                            }
                            if (w.release && w.charge >= ls.min_charge - 1e-5) || w.charge >= ls.charge + ls.hold_s {
                                let frac = if ls.charge > 0.0 { (w.charge / ls.charge).min(1.0) } else { 1.0 };
                                w.charging = false;
                                w.charge = 0.0;
                                w.release = false;
                                w.cooldown = ls.cooldown;
                                fired = true;
                                shots.push(Shot::Laser(frac));
                            }
                        } else if fire[s] && ready && !shorted {
                            w.charging = true;
                            w.charge = 0.0;
                            w.release = false;
                            shots.push(Shot::Snd("laser_charge"));
                        }
                    }
                    WKind::Lightning => {
                        if fire[s] && ready && energy >= wst.lightning.energy {
                            energy -= wst.lightning.energy;
                            spent = true;
                            w.cooldown = w.cooldown.min(0.0) + wst.lightning.cooldown;
                            fired = true;
                            shots.push(Shot::Lightning);
                        }
                    }
                }
                if !fired && ready {
                    w.cooldown = 0.0;
                }
                if w.cooldown > 0.0 {
                    w.cooldown -= dt;
                }
            }
            m.energy = energy;
            if spent {
                m.last_spend_t = t;
            }
        }
        for s in shots {
            match s {
                Shot::Gun => self.fire_gun(i),
                Shot::Mortar => self.fire_mortar(i),
                Shot::Laser(f) => self.fire_laser(i, f),
                Shot::Lightning => self.fire_lightning(i),
                Shot::Snd(k) => {
                    let p = self.mechs[i].center();
                    self.sound(k, p, Some(i));
                }
            }
        }
    }

    fn step_projectiles(&mut self) {
        let dt = self.dt;
        let mut idx = 0;
        while idx < self.projs.len() {
            let mut p = self.projs[idx];
            if let Some(ms) = p.mortar {
                p.vel[2] -= ms.gravity * dt;
            }
            let a = p.pos;
            let b = add(a, scale(p.vel, dt));
            let wt = self.world.raycast(a, b).map(|h| h.t);
            let pad = match p.kind {
                PKind::Gun => self.cfg.projectile.gun_radius,
                PKind::Npc => self.cfg.projectile.npc_radius,
                PKind::Mortar => 0.3,
            };
            let eh = self.first_entity_hit(a, b, pad, p.owner, &[]).filter(|&(_, te)| te <= wt.unwrap_or(2.0));
            let mut done = false;
            if let Some((tg, te)) = eh {
                let pt = add(a, scale(sub(b, a), te));
                match p.kind {
                    PKind::Mortar => self.explode(pt, p.owner, p.mortar.unwrap(), p.mult, Some(tg)),
                    PKind::Gun => self.hurt(tg, p.damage, "gun", Some(p.owner), sub(pt, scale(norm(p.vel), 30.0)), None),
                    PKind::Npc => self.hurt(tg, p.damage, "npc", Some(p.owner), sub(pt, scale(norm(p.vel), 30.0)), None),
                }
                done = true;
            } else if let Some(t) = wt {
                if let Some(ms) = p.mortar {
                    let pt = add(a, scale(sub(b, a), (t - 0.02).max(0.0)));
                    self.explode(pt, p.owner, ms, p.mult, None);
                }
                done = true;
            } else {
                p.pos = b;
                p.left -= if p.kind == PKind::Mortar { dt } else { len(p.vel) * dt };
                if p.left <= 0.0 || !self.world.bounds.contains(b[0], b[1], -50.0) {
                    if let Some(ms) = p.mortar {
                        self.explode(b, p.owner, ms, p.mult, None);
                    }
                    done = true;
                }
            }
            if done {
                self.projs.swap_remove(idx);
            } else {
                self.projs[idx] = p;
                idx += 1;
            }
        }
    }

    // ------------------------------------------------------------------ per-tick pieces

    fn step_mech(&mut self, i: usize) {
        let dt = self.dt;
        let t = self.t;
        let step_every = self.cfg.sound.step_every;
        let step_speed = self.cfg.sound.step_speed;
        let mut snd: Option<&'static str> = None;
        {
            let world = &*self.world;
            let m = &mut self.mechs[i];
            let charging = m.charging();
            let st = m.st;
            let turn = if charging { m.wst.laser.charge_turn_mult } else { 1.0 };
            let dy = wrap_pi(m.cmd_aim[0] - m.aim[0]);
            let my = st.turn_rate * turn * dt;
            m.aim[0] = wrap_pi(m.aim[0] + dy.clamp(-my, my));
            let dp = m.cmd_aim[1].clamp(st.min_pitch, st.max_pitch) - m.aim[1];
            let mp = st.pitch_rate * turn * dt;
            m.aim[1] = (m.aim[1] + dp.clamp(-mp, mp)).clamp(st.min_pitch, st.max_pitch);

            let inp = MoveIn { mv: m.cmd.mv, boost: m.cmd.boost, jump: m.cmd.jump && !m.jump_used, speed_mult: if charging { st.charge_move_mult } else { 1.0 } };
            let params = MoveParams::from_stats(&st);
            let e0 = m.energy;
            let out = step_kin(world, &mut m.kin, &params, &inp, &mut m.energy, dt);
            if out.jumped {
                m.jump_used = true;
            }
            if m.energy < e0 {
                m.last_spend_t = t;
            } else if t - m.last_spend_t >= st.energy_regen_delay {
                m.energy = (m.energy + st.energy_regen * dt).min(st.energy);
            }
            m.wall = out.wall || out.blocked;
            m.step_timer -= dt;
            let sp = m.kin.vel[0].hypot(m.kin.vel[1]);
            if m.step_timer <= 0.0 {
                if out.jetted {
                    snd = Some("jet");
                } else if out.boosted {
                    snd = Some("boost");
                } else if m.kin.grounded && sp > step_speed {
                    snd = Some("step");
                }
                if snd.is_some() {
                    m.step_timer = step_every;
                }
            }
        }
        if let Some(k) = snd {
            let p = self.mechs[i].kin.pos;
            self.sound(k, p, Some(i));
        }
    }

    fn separate_mechs(&mut self) {
        let n = self.mechs.len();
        for a in 0..n {
            for b in a + 1..n {
                if !self.mechs[a].alive || !self.mechs[b].alive {
                    continue;
                }
                let (pa, pb) = (self.mechs[a].kin.pos, self.mechs[b].kin.pos);
                let rr = self.mechs[a].st.radius + self.mechs[b].st.radius;
                let (dx, dy) = (pb[0] - pa[0], pb[1] - pa[1]);
                let d2 = dx * dx + dy * dy;
                if d2 >= rr * rr || pa[2] >= pb[2] + self.mechs[b].st.height || pb[2] >= pa[2] + self.mechs[a].st.height {
                    continue;
                }
                let d = d2.sqrt();
                let (nx, ny) = if d > 1e-4 { (dx / d, dy / d) } else { (1.0, 0.0) };
                let push = (rr - d) * 0.5 + 0.01;
                for (idx, s) in [(a, -1.0f32), (b, 1.0f32)] {
                    let m = &self.mechs[idx];
                    let np = [m.kin.pos[0] + nx * push * s, m.kin.pos[1] + ny * push * s, m.kin.pos[2]];
                    let mut body = m.body();
                    if !m.kin.grounded {
                        body.step_up = 0.0;
                    }
                    if !self.world.mech_blocked(np, &body) && self.world.bounds.contains(np[0], np[1], body.radius) {
                        self.mechs[idx].kin.pos = np;
                    }
                }
            }
        }
    }

    fn step_npcs(&mut self) {
        let cfg = self.cfg.clone();
        let nc = &cfg.npc;
        let dt = self.dt;
        let t = self.t;
        let params = MoveParams {
            radius: nc.radius,
            height: nc.height,
            step_up: cfg.mech.step_up,
            max_slope_deg: cfg.mech.max_slope_deg,
            walk_speed: nc.speed,
            boost_speed: nc.speed,
            ground_accel: 30.0,
            air_accel: 5.0,
            gravity: cfg.mech.gravity,
            jump_speed: 0.0,
            jet_accel: 0.0,
            max_rise_speed: 0.0,
            boost_drain: 0.0,
            jet_drain: 0.0,
        };
        for k in 0..self.npcs.len() {
            if !self.npcs[k].alive {
                continue;
            }
            let pos = self.npcs[k].kin.pos;
            let home = self.npcs[k].home;
            let home_d = dist2d(pos, home);
            let eye = up(pos, nc.height * 0.8);
            if let Some(tj) = self.npcs[k].target {
                let tm = &self.mechs[tj];
                if !tm.alive || dist(pos, tm.kin.pos) >= nc.deaggro_range || home_d >= nc.chase_leash {
                    self.npcs[k].target = None;
                    self.npcs[k].wander = [home[0], home[1]];
                    self.npcs[k].wander_until = t + 4.0;
                }
            }
            if self.npcs[k].target.is_none() && home_d <= nc.leash {
                let mut best: Option<(f32, usize)> = None;
                for (j, m) in self.mechs.iter().enumerate() {
                    if !m.alive {
                        continue;
                    }
                    let d = dist(pos, m.kin.pos);
                    if d <= nc.aggro_range && best.map_or(true, |(bd, _)| d < bd) && self.world.los(eye, m.center()) {
                        best = Some((d, j));
                    }
                }
                self.npcs[k].target = best.map(|b| b.1);
            }
            let mut mv = [0.0f32, 0.0];
            let mut shot: Option<V3> = None;
            if let Some(tj) = self.npcs[k].target {
                let tc = self.mechs[tj].center();
                let d = dist2d(pos, tc);
                if d > 28.0 {
                    mv = [(tc[0] - pos[0]) / d, (tc[1] - pos[1]) / d];
                }
                let n = &mut self.npcs[k];
                n.aim = [yaw_to(eye, tc), pitch_to(eye, tc)];
                n.cooldown -= dt;
                if n.cooldown <= 0.0 && self.world.los(eye, tc) {
                    n.cooldown = nc.cooldown;
                    shot = Some(tc);
                }
            } else {
                let n = &mut self.npcs[k];
                n.cooldown = (n.cooldown - dt).max(0.0);
                if t >= n.wander_until || (pos[0] - n.wander[0]).hypot(pos[1] - n.wander[1]) < 1.5 {
                    let o = self.rng.disc();
                    n.wander = [home[0] + o[0] * nc.leash * 0.8, home[1] + o[1] * nc.leash * 0.8];
                    n.wander_until = t + self.rng.range(4.0, 9.0);
                }
                let (dx, dy) = (n.wander[0] - pos[0], n.wander[1] - pos[1]);
                let d = dx.hypot(dy);
                if d > 1.5 {
                    mv = [dx / d, dy / d];
                    n.aim = [dy.atan2(dx), 0.0];
                }
            }
            let mut e = 0.0;
            let world = &*self.world;
            let out = step_kin(world, &mut self.npcs[k].kin, &params, &MoveIn { mv, boost: false, jump: false, speed_mult: 1.0 }, &mut e, dt);
            if out.blocked && self.npcs[k].target.is_none() {
                self.npcs[k].wander_until = 0.0;
            }
            if let Some(tc) = shot {
                let s = nc.spread_deg.to_radians();
                let yaw = yaw_to(eye, tc) + self.rng.gauss() * s;
                let pitch = pitch_to(eye, tc) + self.rng.gauss() * s;
                let dir = aim_dir(yaw, pitch);
                let id = self.npcs[k].id;
                self.projs.push(Proj { id: self.next_proj, kind: PKind::Npc, owner: id, pos: add(eye, scale(dir, nc.radius + 0.3)), vel: scale(dir, nc.projectile_speed), left: nc.deaggro_range + 30.0, damage: nc.damage, mult: 1.0, mortar: None });
                self.next_proj += 1;
                self.sound("npc_gun", eye, None);
            }
        }
    }

    fn step_cubes(&mut self) {
        if self.cubes.is_empty() {
            return;
        }
        let r = self.cfg.cubes.pickup_radius;
        let mut c = 0;
        while c < self.cubes.len() {
            let p = self.cubes[c].pos;
            let mut best: Option<(f32, usize)> = None;
            for (j, m) in self.mechs.iter().enumerate() {
                if !m.alive || m.hp <= 0.0 {
                    continue;
                }
                let d = dist2d(m.kin.pos, p);
                if d <= r && (m.kin.pos[2] - p[2]).abs() <= 4.0 && best.map_or(true, |(bd, _)| d < bd) {
                    best = Some((d, j));
                }
            }
            if let Some((_, j)) = best {
                self.cubes.swap_remove(c);
                self.gain_cubes(j, 1);
                let total = self.mechs[j].cubes;
                self.event(j, json!({"type":"pickup","cubes":total}));
            } else {
                c += 1;
            }
        }
    }

    fn step_drops(&mut self) {
        let cfg = self.cfg.clone();
        let t = self.t;
        while self.next_drop < cfg.drops.times.len() && t >= cfg.drops.times[self.next_drop] {
            let z = self.zone;
            let rad = (z.next_radius * cfg.drops.spread).max(0.0);
            let mut pos = [z.next_center[0], z.next_center[1], self.world.terrain_height(z.next_center[0], z.next_center[1])];
            if let Some(tab) = self.spawn.clone() {
                let mut best = f32::INFINITY;
                for n in 0..400 {
                    let p = tab.main[self.rng.usize(tab.main.len())];
                    let d = (p[0] - z.next_center[0]).hypot(p[1] - z.next_center[1]);
                    if d <= rad && n < 300 {
                        pos = p;
                        break;
                    }
                    if d < best {
                        best = d;
                        pos = p;
                    }
                }
            }
            let id = 20000 + self.next_drop as u32;
            self.drops.push(SupplyDrop { id, pos, lands_at: t + cfg.drops.fall_s, landed: false, holder: None, progress: 0.0 });
            self.log.push(json!({"t":r2(t),"ev":"drop_called","id":id,"pos":j3(pos)}));
            self.next_drop += 1;
        }
        let mut d = 0;
        while d < self.drops.len() {
            if !self.drops[d].landed {
                if t >= self.drops[d].lands_at {
                    self.drops[d].landed = true;
                    let p = self.drops[d].pos;
                    self.sound("drop", p, None);
                }
                d += 1;
                continue;
            }
            let p = self.drops[d].pos;
            let mut inside: Vec<usize> = Vec::new();
            for (j, m) in self.mechs.iter().enumerate() {
                if m.alive && m.hp > 0.0 && dist2d(m.kin.pos, p) <= cfg.drops.radius && (m.kin.pos[2] - p[2]).abs() <= 6.0 {
                    inside.push(j);
                }
            }
            let dr = &mut self.drops[d];
            match inside.len() {
                0 => {
                    dr.holder = None;
                    dr.progress = 0.0;
                }
                1 => {
                    if dr.holder == Some(inside[0]) {
                        dr.progress += self.dt;
                    } else {
                        dr.holder = Some(inside[0]);
                        dr.progress = 0.0;
                    }
                }
                _ => {}
            }
            if dr.progress >= cfg.drops.capture_s {
                let j = dr.holder.unwrap();
                let id = dr.id;
                self.drops.remove(d);
                self.gain_cubes(j, cfg.drops.cubes);
                let m = &mut self.mechs[j];
                m.hp = m.hp.max(m.max_hp * cfg.drops.repair.clamp(0.0, 1.0));
                m.energy = m.st.energy;
                let total = m.cubes;
                self.event(j, json!({"type":"drop","id":id,"cubes":total}));
                self.log.push(json!({"t":r2(t),"ev":"drop_taken","id":id,"by":j}));
            } else {
                d += 1;
            }
        }
    }

    fn step_health(&mut self) {
        let dt = self.dt;
        let t = self.t;
        let z = self.zone;
        for i in 0..self.mechs.len() {
            if !self.mechs[i].alive {
                continue;
            }
            {
                let m = &mut self.mechs[i];
                let inside = (m.kin.pos[0] - z.center[0]).hypot(m.kin.pos[1] - z.center[1]) <= z.radius;
                m.in_zone = inside;
                if !inside {
                    let a = z.dps * dt;
                    let was = m.hp;
                    m.hp -= a;
                    m.zone_acc += a;
                    m.dmg_taken += a;
                    m.last_combat_t = t;
                    if was > 0.0 && m.hp <= 0.0 {
                        m.killer = Some(("zone", None));
                    }
                } else if m.hp > 0.0 && m.hp < m.max_hp && t - m.last_combat_t >= m.st.regen_delay {
                    m.hp = (m.hp + m.st.regen_per_s * dt).min(m.max_hp);
                }
            }
            if self.mechs[i].offer.is_none() && self.mechs[i].pending_offers > 0 {
                self.draw_offer(i);
            }
            let expired = self.mechs[i].offer.as_ref().filter(|o| t >= o.expires).map(|o| o.choices[0]);
            if let Some(u) = expired {
                self.apply_upgrade(i, u);
            }
        }
    }

    fn process_deaths(&mut self) {
        let dying: Vec<usize> = (0..self.mechs.len()).filter(|&i| self.mechs[i].alive && self.mechs[i].hp <= 0.0).collect();
        if dying.is_empty() {
            return;
        }
        let cfg = self.cfg.clone();
        let place = (self.alive_count + 1 - dying.len()) as u32;
        for &j in &dying {
            let (cause, by) = self.mechs[j].killer.unwrap_or(("zone", None));
            let pos = self.mechs[j].kin.pos;
            let (cubes, weapons) = {
                let m = &mut self.mechs[j];
                m.alive = false;
                m.hp = 0.0;
                m.place = place;
                m.death = Some(Death { t: self.t, cause, by, pos });
                (m.cubes, std::mem::replace(&mut m.weapons, [None, None]))
            };
            if let Some(b) = by.filter(|&b| b >= 0 && (b as usize) < self.mechs.len() && b as usize != j) {
                self.mechs[b as usize].kills += 1;
                self.event(b as usize, json!({"type":"kill","target":j}));
            }
            for _ in 0..cubes + cfg.cubes.death_drop_extra {
                let o = self.rng.disc();
                self.add_cube([pos[0] + o[0] * 3.0, pos[1] + o[1] * 3.0, pos[2]]);
            }
            for w in weapons.into_iter().flatten() {
                if w.kind != WKind::Gun && self.rng.chance(cfg.loot.death_drop) {
                    let o = self.rng.disc();
                    let mut w = w;
                    w.charging = false;
                    w.charge = 0.0;
                    w.cooldown = 0.0;
                    self.add_pickup_w(w, [pos[0] + o[0] * 2.0, pos[1] + o[1] * 2.0, pos[2]]);
                }
            }
            self.sound("death", pos, Some(j));
            self.log.push(json!({"t":r2(self.t),"ev":"death","id":j,"place":place,"cause":cause,"by":by,"pos":j3(pos)}));
        }
        self.alive_count -= dying.len();
    }

    /// Advance the match by one sim tick.
    pub fn step(&mut self) {
        self.zone = self.zone_at(self.t);
        for i in 0..self.mechs.len() {
            if self.mechs[i].alive {
                self.step_mech(i);
            }
        }
        self.separate_mechs();
        for i in 0..self.mechs.len() {
            if self.mechs[i].alive {
                self.step_weapons(i);
            }
        }
        self.step_projectiles();
        self.step_npcs();
        self.step_cubes();
        self.step_drops();
        self.step_health();
        self.process_deaths();
        if self.crates.iter().any(|c| !c.alive) {
            self.crates.retain(|c| c.alive);
        }
        self.tick += 1;
        self.t = self.tick as f32 * self.dt;
        if self.over() {
            self.finalize();
        }
    }

    /// Forget the per-decision sound and beam buffers (after all observations are built).
    pub fn end_decision(&mut self) {
        self.sounds.clear();
        self.beams.clear();
    }

    pub fn is_tgt_alive(&self, tg: Tgt) -> bool {
        self.tgt_alive(tg)
    }
}
