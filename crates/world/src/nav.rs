//! Walking navigation: a layered grid graph over the map.
//!
//! Every grid cell holds zero or more *nodes*: one for the terrain (if a mech can
//! stand there) and one per prism top above it (roofs, bridge decks). Nodes of
//! neighbouring cells are linked when a mech can WALK between them in both
//! directions (step/slope rules). Linked nodes form components; the largest one is
//! the "main" ground component. Roofs are separate components ("regions") that can
//! only be reached by jumping/jetting.

use crate::map::{Body, World, LAYER_TERRAIN};
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;

pub type NodeId = u32;
pub const NO_NODE: NodeId = u32::MAX;

/// Neighbour directions, index 0..8: E, NE, N, NW, W, SW, S, SE. Opposite = (d + 4) % 8.
pub const DIRS: [(i32, i32); 8] = [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)];
// (di + 1) + (dj + 1) * 3 -> direction index (4 = centre, unused)
const DIR_OF: [usize; 9] = [5, 6, 7, 4, 8, 0, 3, 2, 1];
/// Longest straight leg the path smoother produces (bounds the smoothing cost).
const MAX_LEG: f32 = 160.0;
/// `node_clearance` value of nodes with no prism anywhere near them.
pub const OPEN_CLR: f32 = 1.0e6;

#[derive(Clone, Copy, Debug)]
pub struct NavParams {
    /// Mech radius (config.mech.radius, scaled by cubes if you want a grid for a bigger mech).
    pub radius: f32,
    /// Mech height: overhangs lower than this above the feet are walls.
    pub height: f32,
    pub step_up: f32,
    pub max_slope_deg: f32,
    /// Extra distance kept from walls by every node (on top of `radius`).
    pub margin: f32,
    /// Grid cell size in metres.
    pub cell: f32,
    /// Slopes within this many degrees of the limit are treated as too steep.
    pub slope_safety_deg: f32,
    /// Number of ALT landmarks for the A* heuristic (0 disables).
    pub landmarks: usize,
}

impl Default for NavParams {
    fn default() -> Self {
        NavParams { radius: 2.5, height: 9.0, step_up: 1.2, max_slope_deg: 50.0, margin: 0.6, cell: 2.0, slope_safety_deg: 2.0, landmarks: 8 }
    }
}

/// A smoothed walking path.
#[derive(Clone, Debug, Default)]
pub struct NavPath {
    /// Waypoints to walk through in order, NOT including the start position.
    /// The last point is the goal (snapped to the nearest reachable spot).
    pub points: Vec<[f32; 3]>,
    /// Horizontal length from the start position along the waypoints, metres.
    pub length: f32,
    /// Horizontal distance between the requested goal and the last waypoint.
    pub goal_error: f32,
}

/// Walking distance (metres) from every node to a goal set. `f32::INFINITY` = unreachable
/// (or beyond the `max_dist` the field was built with).
#[derive(Clone, Debug)]
pub struct DistField {
    pub dist: Vec<f32>,
}

#[derive(Clone, Copy, Debug)]
pub struct CoverPoint {
    pub pos: [f32; 3],
    pub node: NodeId,
    /// Walking distance from the query position.
    pub path_dist: f32,
}

/// A place where a roof (or enclosed courtyard) can be entered from the main ground
/// component by jumping/jetting.
#[derive(Clone, Copy, Debug)]
pub struct Access {
    /// Stand here (main ground component).
    pub ground: [f32; 3],
    pub ground_node: NodeId,
    /// Land here (node of the region).
    pub top: [f32; 3],
    pub top_node: NodeId,
    /// Highest surface (roof edge, parapet, wall top) on the straight hop between the two.
    pub crest: f32,
    /// Height to gain to get in: `crest - ground.z`.
    pub climb: f32,
    /// Height to gain to get out again: `crest - top.z` (0 for a plain roof edge: walk off).
    pub climb_out: f32,
}

/// A walkable component that is not the main ground component: a roof, a bridge deck
/// that does not connect to the ground, or an enclosed courtyard.
#[derive(Clone, Debug)]
pub struct Region {
    /// Component id (`Nav::comp_of(node)`).
    pub comp: u32,
    /// True if most of its nodes are prism tops (roof / deck), false for enclosed ground.
    pub is_roof: bool,
    pub nodes: u32,
    /// Approximate walkable area, m².
    pub area: f32,
    pub z_min: f32,
    pub z_max: f32,
    pub centroid: [f32; 3],
    /// Entry points from the main component, lowest climb first (up to 12, spread out).
    pub access: Vec<Access>,
    /// Smallest climb among `access` (`f32::INFINITY` if none found).
    pub min_climb: f32,
}

/// Jump + jet physics (config.mech).
#[derive(Clone, Copy, Debug)]
pub struct JetParams {
    pub jump_speed: f32,
    pub jet_accel: f32,
    pub max_rise_speed: f32,
    pub gravity: f32,
    pub jet_drain: f32,
}
impl Default for JetParams {
    fn default() -> Self {
        JetParams { jump_speed: 11.0, jet_accel: 34.0, max_rise_speed: 10.0, gravity: 22.0, jet_drain: 15.0 }
    }
}

/// Estimated height gained by jumping and then jetting until `energy` runs out
/// (20 Hz simulation; assumes `jet_accel` is applied against gravity and the rise
/// speed is capped at `max_rise_speed`). Protocol: "about 60 m with 100 energy" -
/// this estimate is an upper bound, keep a safety factor (e.g. x0.85).
pub fn max_climb(energy: f32, jp: &JetParams) -> f32 {
    let dt = 0.05f32;
    let mut v = jp.jump_speed;
    let mut z = 0.0f32;
    let mut e = energy;
    for _ in 0..4000 {
        if e > 0.0 {
            v = (v + (jp.jet_accel - jp.gravity) * dt).min(jp.max_rise_speed);
            e -= jp.jet_drain * dt;
        } else {
            v -= jp.gravity * dt;
        }
        if v <= 0.0 {
            break;
        }
        z += v * dt;
    }
    z
}

#[derive(Default)]
struct Scratch {
    g: Vec<f32>,
    parent: Vec<u32>,
    stamp: Vec<u32>,
    closed: Vec<u32>,
    cur: u32,
    heap: BinaryHeap<Reverse<(u32, u32)>>,
}

impl Scratch {
    fn begin(&mut self, n: usize) {
        if self.g.len() != n {
            self.g = vec![0.0; n];
            self.parent = vec![NO_NODE; n];
            self.stamp = vec![0; n];
            self.closed = vec![0; n];
            self.cur = 0;
        }
        self.cur = self.cur.wrapping_add(1);
        if self.cur == 0 {
            self.stamp.iter_mut().for_each(|s| *s = 0);
            self.closed.iter_mut().for_each(|s| *s = 0);
            self.cur = 1;
        }
        self.heap.clear();
    }
}

pub struct Nav {
    pub params: NavParams,
    world: Arc<World>,
    /// Grid origin (south-west corner of cell (0, 0)).
    pub x0: f32,
    pub y0: f32,
    pub cell: f32,
    pub nx: i32,
    pub ny: i32,
    // CSR: nodes of cell k are cell_first[k]..cell_first[k + 1]
    cell_first: Vec<u32>,
    node_cell: Vec<u32>,
    node_z: Vec<f32>,
    node_clr: Vec<f32>,
    // conservative wall distance valid for every feet height reachable inside the cell
    node_cons: Vec<f32>,
    node_layer: Vec<u32>,
    nbr: Vec<[u32; 8]>,
    comp: Vec<u32>,
    comp_size: Vec<u32>,
    main: u32,
    lm_k: usize,
    lm: Vec<f32>,
    regions: Vec<Region>,
    region_of_comp: Vec<u32>,
    clr_cap: f32,
    scratch: RefCell<Scratch>,
}

#[inline]
fn fkey(f: f32) -> u32 {
    f.max(0.0).to_bits()
}

/// Clearance every point of a link / smoothed line keeps from walls.
fn edge_need(p: &NavParams) -> f32 {
    // Two linked nodes each keep `radius + margin`; between diagonal neighbours the
    // guaranteed clearance is the lens bound below.
    let full = p.radius + p.margin;
    let lens = (full * full - p.cell * p.cell * 0.5).max(0.0).sqrt();
    (p.radius + 0.2).min(lens - 0.02).max(p.radius * 0.5)
}

impl Nav {
    /// Build the navigation grid for one mech size. Takes roughly 0.1-0.3 s.
    pub fn build(world: Arc<World>, params: NavParams) -> Nav {
        let w = &*world;
        let b = w.bounds;
        let cell = params.cell;
        let nx = (((b.max_x - b.min_x) / cell).ceil() as i32).max(1);
        let ny = (((b.max_y - b.min_y) / cell).ceil() as i32).max(1);
        let need = params.radius + params.margin;
        let clr_cap = need + 2.5;
        let tan_lim = (params.max_slope_deg - params.slope_safety_deg).to_radians().tan();

        let mut cell_first = Vec::with_capacity((nx * ny + 1) as usize);
        let mut node_cell: Vec<u32> = Vec::new();
        let mut node_z: Vec<f32> = Vec::new();
        let mut node_clr: Vec<f32> = Vec::new();
        let mut node_cons: Vec<f32> = Vec::new();
        // Height change possible within one cell (half diagonal at the slope limit).
        let z_slack = params.max_slope_deg.to_radians().tan() * cell * 0.75 + 0.1;
        let mut node_layer: Vec<u32> = Vec::new();
        let mut near: Vec<(u32, f32)> = Vec::new();
        let mut cands: Vec<(f32, u32)> = Vec::new();
        for j in 0..ny {
            for i in 0..nx {
                cell_first.push(node_z.len() as u32);
                let x = b.min_x + (i as f32 + 0.5) * cell;
                let y = b.min_y + (j as f32 + 0.5) * cell;
                if !b.contains(x, y, need) {
                    continue;
                }
                let k = (j * nx + i) as u32;
                let g = w.terrain_height(x, y);
                if w.no_prism_near(x, y, clr_cap) {
                    node_cell.push(k);
                    node_z.push(g);
                    node_clr.push(OPEN_CLR);
                    node_cons.push(OPEN_CLR);
                    node_layer.push(LAYER_TERRAIN);
                    continue;
                }
                near.clear();
                w.prisms_near(x, y, clr_cap, |pi, p| {
                    if !near.iter().any(|n| n.0 == pi) {
                        near.push((pi, p.dist_xy(x, y, clr_cap)));
                    }
                });
                cands.clear();
                cands.push((g, LAYER_TERRAIN));
                for &(pi, d) in &near {
                    let top = w.prisms[pi as usize].top;
                    if d == 0.0 && top > g + 0.01 {
                        cands.push((top, pi + 1));
                    }
                }
                cands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                let mut last = f32::NEG_INFINITY;
                for &(c, layer) in &cands {
                    if c - last < 0.05 {
                        continue;
                    }
                    let mut clr = clr_cap;
                    let mut cons = clr_cap;
                    let mut ok = true;
                    for &(pi, d) in &near {
                        let p = &w.prisms[pi as usize];
                        if d == 0.0 && p.base < c + 0.01 && p.top > c + 0.01 {
                            ok = false; // inside solid
                            break;
                        }
                        if p.top > c + params.step_up + 1e-3 && p.base < c + params.height - 1e-3 && d < clr {
                            clr = d;
                        }
                        // Walls for any feet height the terrain can reach inside this cell.
                        if p.top > c - z_slack + params.step_up && p.base < c + z_slack + params.height && d < cons {
                            cons = d;
                        }
                    }
                    if !ok || clr < need {
                        continue;
                    }
                    last = c;
                    node_cell.push(k);
                    node_z.push(c);
                    node_clr.push(clr);
                    node_cons.push(cons);
                    node_layer.push(layer);
                }
            }
        }
        cell_first.push(node_z.len() as u32);
        let n = node_z.len();

        // ---- links
        let mut nbr = vec![[NO_NODE; 8]; n];
        let edge_need = edge_need(&params);
        let trace_body = Body { radius: edge_need, height: params.height, step_up: params.step_up, max_slope_deg: params.max_slope_deg - params.slope_safety_deg };
        for a in 0..n {
            let k = node_cell[a] as i32;
            let (i, j) = (k % nx, k / nx);
            let ax = b.min_x + (i as f32 + 0.5) * cell;
            let ay = b.min_y + (j as f32 + 0.5) * cell;
            for d in 0..4 {
                let (ni, nj) = (i + DIRS[d].0, j + DIRS[d].1);
                if ni < 0 || nj < 0 || ni >= nx || nj >= ny {
                    continue;
                }
                let nk = (nj * nx + ni) as usize;
                let bx = b.min_x + (ni as f32 + 0.5) * cell;
                let by = b.min_y + (nj as f32 + 0.5) * cell;
                for bn in cell_first[nk] as usize..cell_first[nk + 1] as usize {
                    if nbr[bn][d + 4] != NO_NODE {
                        continue;
                    }
                    let (la, lb) = (node_layer[a], node_layer[bn]);
                    let ok = if la == LAYER_TERRAIN && lb == LAYER_TERRAIN {
                        let len = ((bx - ax) * (bx - ax) + (by - ay) * (by - ay)).sqrt();
                        let m = (len / 0.5).ceil() as usize;
                        let lim = tan_lim * len / m as f32;
                        let mut prev = node_z[a];
                        let mut good = true;
                        for s in 1..=m {
                            let t = s as f32 / m as f32;
                            let h = w.terrain_height(ax + (bx - ax) * t, ay + (by - ay) * t);
                            if (h - prev).abs() > lim {
                                good = false;
                                break;
                            }
                            prev = h;
                        }
                        // Near prisms the set of walls depends on the feet height (a low deck
                        // stops being a wall once you are level with it), so check the middle too.
                        if good && (node_clr[a] < OPEN_CLR || node_clr[bn] < OPEN_CLR) {
                            for s in 1..4 {
                                let t = s as f32 * 0.25;
                                let (x, y) = (ax + (bx - ax) * t, ay + (by - ay) * t);
                                if w.clearance(x, y, w.terrain_height(x, y), params.height, params.step_up, edge_need) < edge_need {
                                    good = false;
                                    break;
                                }
                            }
                        }
                        good
                    } else if la == lb {
                        true
                    } else {
                        let f = w.walk_clear([ax, ay, node_z[a]], [bx, by], &trace_body, params.step_up);
                        let r = w.walk_clear([bx, by, node_z[bn]], [ax, ay], &trace_body, params.step_up);
                        matches!(f, Some(z) if (z - node_z[bn]).abs() < 0.05) && matches!(r, Some(z) if (z - node_z[a]).abs() < 0.05)
                    };
                    if ok {
                        nbr[a][d] = bn as u32;
                        nbr[bn][d + 4] = a as u32;
                        break;
                    }
                }
            }
        }

        // ---- components
        let mut comp = vec![u32::MAX; n];
        let mut comp_size: Vec<u32> = Vec::new();
        let mut stack: Vec<u32> = Vec::new();
        for s in 0..n {
            if comp[s] != u32::MAX {
                continue;
            }
            let c = comp_size.len() as u32;
            let mut size = 0u32;
            comp[s] = c;
            stack.push(s as u32);
            while let Some(u) = stack.pop() {
                size += 1;
                for &v in &nbr[u as usize] {
                    if v != NO_NODE && comp[v as usize] == u32::MAX {
                        comp[v as usize] = c;
                        stack.push(v);
                    }
                }
            }
            comp_size.push(size);
        }
        // Main = the largest component made of terrain nodes.
        let mut main = 0u32;
        let mut best = 0u32;
        for (c, &s) in comp_size.iter().enumerate() {
            if s > best {
                best = s;
                main = c as u32;
            }
        }

        let mut nav = Nav {
            params,
            world,
            x0: b.min_x,
            y0: b.min_y,
            cell,
            nx,
            ny,
            cell_first,
            node_cell,
            node_z,
            node_clr,
            node_cons,
            node_layer,
            nbr,
            comp,
            comp_size,
            main,
            lm_k: 0,
            lm: Vec::new(),
            regions: Vec::new(),
            region_of_comp: Vec::new(),
            clr_cap,
            scratch: RefCell::new(Scratch::default()),
        };
        nav.build_landmarks();
        nav.build_regions();
        nav
    }

    fn build_landmarks(&mut self) {
        let k = self.params.landmarks;
        let n = self.node_count();
        if k == 0 || n == 0 {
            return;
        }
        let seed = match (0..n as u32).find(|&u| self.comp[u as usize] == self.main) {
            Some(s) => s,
            None => return,
        };
        let mut mind = vec![f32::INFINITY; n];
        let mut d = Vec::new();
        self.dijkstra(&[seed], f32::INFINITY, &mut d);
        let far = |d: &[f32]| -> u32 {
            let mut b = (0u32, -1.0f32);
            for (u, &v) in d.iter().enumerate() {
                if v.is_finite() && v > b.1 {
                    b = (u as u32, v);
                }
            }
            b.0
        };
        let mut next = far(&d);
        let mut lm = vec![f32::INFINITY; n * k];
        for l in 0..k {
            self.dijkstra(&[next], f32::INFINITY, &mut d);
            for u in 0..n {
                lm[u * k + l] = d[u];
                if d[u] < mind[u] {
                    mind[u] = d[u];
                }
            }
            next = far(&mind);
        }
        self.lm_k = k;
        self.lm = lm;
    }

    fn build_regions(&mut self) {
        let n = self.node_count();
        let nc = self.comp_size.len();
        self.region_of_comp = vec![u32::MAX; nc];
        let mut members: Vec<Vec<u32>> = vec![Vec::new(); nc];
        for u in 0..n {
            let c = self.comp[u] as usize;
            if c as u32 != self.main && self.comp_size[c] >= 3 {
                members[c].push(u as u32);
            }
        }
        let reach = (((self.params.radius + self.params.margin) * 2.0 / self.cell).ceil() as i32 + 1).max(2);
        for c in 0..nc {
            if members[c].is_empty() {
                continue;
            }
            let ms = &members[c];
            let mut roof = 0u32;
            let (mut zmin, mut zmax) = (f32::INFINITY, f32::NEG_INFINITY);
            let mut cen = [0.0f64; 3];
            let mut acc: Vec<Access> = Vec::new();
            for &u in ms {
                let p = self.node_pos(u);
                if self.node_layer[u as usize] != LAYER_TERRAIN {
                    roof += 1;
                }
                zmin = zmin.min(p[2]);
                zmax = zmax.max(p[2]);
                cen[0] += p[0] as f64;
                cen[1] += p[1] as f64;
                cen[2] += p[2] as f64;
                // Only region-border nodes can be entry points.
                if self.nbr[u as usize].iter().all(|&v| v != NO_NODE) {
                    continue;
                }
                let (i, j) = self.node_ij(u);
                let mut bestg: Option<(f32, u32)> = None;
                for dj in -reach..=reach {
                    for di in -reach..=reach {
                        let (ci, cj) = (i + di, j + dj);
                        if ci < 0 || cj < 0 || ci >= self.nx || cj >= self.ny {
                            continue;
                        }
                        for v in self.cell_nodes(ci, cj) {
                            if self.comp[v as usize] == self.main {
                                let d2 = (di * di + dj * dj) as f32;
                                if bestg.map_or(true, |(bd, _)| d2 < bd) {
                                    bestg = Some((d2, v));
                                }
                            }
                        }
                    }
                }
                if let Some((_, v)) = bestg {
                    let gp = self.node_pos(v);
                    let mut crest = p[2].max(gp[2]);
                    let len = (p[0] - gp[0]).hypot(p[1] - gp[1]);
                    let m = (len / 0.5).ceil().max(1.0) as usize;
                    for s in 0..=m {
                        let t = s as f32 / m as f32;
                        crest = crest.max(self.world.top_surface(gp[0] + (p[0] - gp[0]) * t, gp[1] + (p[1] - gp[1]) * t).z);
                    }
                    acc.push(Access { ground: gp, ground_node: v, top: p, top_node: u, crest, climb: crest - gp[2], climb_out: crest - p[2] });
                }
            }
            acc.sort_by(|a, b| a.climb.partial_cmp(&b.climb).unwrap());
            let mut pick: Vec<Access> = Vec::new();
            for a in &acc {
                if pick.len() >= 12 {
                    break;
                }
                if pick.iter().all(|p| (p.top[0] - a.top[0]).hypot(p.top[1] - a.top[1]) >= 12.0) {
                    pick.push(*a);
                }
            }
            let m = ms.len() as f64;
            let min_climb = pick.first().map_or(f32::INFINITY, |a| a.climb);
            self.region_of_comp[c] = self.regions.len() as u32;
            self.regions.push(Region {
                comp: c as u32,
                is_roof: roof * 2 > ms.len() as u32,
                nodes: ms.len() as u32,
                area: ms.len() as f32 * self.cell * self.cell,
                z_min: zmin,
                z_max: zmax,
                centroid: [(cen[0] / m) as f32, (cen[1] / m) as f32, (cen[2] / m) as f32],
                access: pick,
                min_climb,
            });
        }
    }

    // ------------------------------------------------------------ raw graph access

    #[inline]
    pub fn world(&self) -> &Arc<World> {
        &self.world
    }
    #[inline]
    pub fn node_count(&self) -> usize {
        self.node_z.len()
    }
    /// Nodes of grid cell (i, j) as an id range (lowest surface first).
    #[inline]
    pub fn cell_nodes(&self, i: i32, j: i32) -> std::ops::Range<u32> {
        if i < 0 || j < 0 || i >= self.nx || j >= self.ny {
            return 0..0;
        }
        let k = (j * self.nx + i) as usize;
        self.cell_first[k]..self.cell_first[k + 1]
    }
    /// Grid cell containing a horizontal point (may be out of range).
    #[inline]
    pub fn cell_of(&self, x: f32, y: f32) -> (i32, i32) {
        (((x - self.x0) / self.cell).floor() as i32, ((y - self.y0) / self.cell).floor() as i32)
    }
    #[inline]
    pub fn node_ij(&self, n: NodeId) -> (i32, i32) {
        let k = self.node_cell[n as usize] as i32;
        (k % self.nx, k / self.nx)
    }
    /// World position of a node (cell centre, standing height).
    #[inline]
    pub fn node_pos(&self, n: NodeId) -> [f32; 3] {
        let (i, j) = self.node_ij(n);
        [self.x0 + (i as f32 + 0.5) * self.cell, self.y0 + (j as f32 + 0.5) * self.cell, self.node_z[n as usize]]
    }
    /// 0 = terrain, otherwise prism index + 1 (the node is on that prism's top).
    #[inline]
    pub fn node_layer(&self, n: NodeId) -> u32 {
        self.node_layer[n as usize]
    }
    /// Distance from the node centre to the nearest wall (at the node's height), capped at
    /// `clearance_cap()`; `OPEN_CLR` (huge) when no prism at all is near the node.
    #[inline]
    pub fn node_clearance(&self, n: NodeId) -> f32 {
        self.node_clr[n as usize]
    }
    /// Linked neighbours by direction (`DIRS`), `NO_NODE` where there is none.
    #[inline]
    pub fn neighbors(&self, n: NodeId) -> &[NodeId; 8] {
        &self.nbr[n as usize]
    }
    #[inline]
    pub fn comp_of(&self, n: NodeId) -> u32 {
        self.comp[n as usize]
    }
    #[inline]
    pub fn comp_size(&self, comp: u32) -> u32 {
        self.comp_size[comp as usize]
    }
    /// Component id of the main ground network.
    #[inline]
    pub fn main_comp(&self) -> u32 {
        self.main
    }
    #[inline]
    pub fn is_main(&self, n: NodeId) -> bool {
        self.comp[n as usize] == self.main
    }
    /// Roofs / decks / courtyards not connected to the main component by walking.
    #[inline]
    pub fn regions(&self) -> &[Region] {
        &self.regions
    }
    /// Region a node belongs to (None for the main component and tiny fragments).
    pub fn region_of(&self, n: NodeId) -> Option<&Region> {
        let r = self.region_of_comp[self.comp[n as usize] as usize];
        if r == u32::MAX {
            None
        } else {
            Some(&self.regions[r as usize])
        }
    }

    // ------------------------------------------------------------------- snapping

    fn nearest_filtered<F: Fn(NodeId) -> bool>(&self, pos: [f32; 3], use_z: bool, max_dist: f32, ok: F) -> Option<NodeId> {
        let (ci, cj) = self.cell_of(pos[0], pos[1]);
        let ci = ci.clamp(0, self.nx - 1);
        let cj = cj.clamp(0, self.ny - 1);
        let max_r = (max_dist / self.cell).ceil() as i32 + 1;
        let mut best: Option<(f32, NodeId)> = None;
        for r in 0..=max_r {
            if let Some((bd, _)) = best {
                let lb = (r - 1).max(0) as f32 * self.cell;
                if lb * lb > bd {
                    break;
                }
            }
            let mut visit = |i: i32, j: i32| {
                for u in self.cell_nodes(i, j) {
                    if !ok(u) {
                        continue;
                    }
                    let p = self.node_pos(u);
                    let dz = if use_z { p[2] - pos[2] } else { 0.0 };
                    let d = (p[0] - pos[0]).powi(2) + (p[1] - pos[1]).powi(2) + dz * dz;
                    if best.map_or(true, |(bd, _)| d < bd) {
                        best = Some((d, u));
                    }
                }
            };
            if r == 0 {
                visit(ci, cj);
            } else {
                for i in ci - r..=ci + r {
                    visit(i, cj - r);
                    visit(i, cj + r);
                }
                for j in cj - r + 1..=cj + r - 1 {
                    visit(ci - r, j);
                    visit(ci + r, j);
                }
            }
        }
        best.map(|b| b.1)
    }

    /// Nearest node to a 3D position on any layer/component (z matters: a mech on a
    /// roof snaps to the roof node, a mech beside the building to the ground node).
    pub fn nearest_node(&self, pos: [f32; 3]) -> Option<NodeId> {
        self.nearest_filtered(pos, true, 150.0, |_| true)
    }
    /// Nearest node of the main ground component to a horizontal point.
    pub fn nearest_main(&self, x: f32, y: f32) -> Option<NodeId> {
        self.nearest_filtered([x, y, 0.0], false, 300.0, |u| self.comp[u as usize] == self.main)
    }
    /// Nearest node within a given component (3D distance).
    pub fn nearest_in_comp(&self, pos: [f32; 3], comp: u32) -> Option<NodeId> {
        self.nearest_filtered(pos, true, 1500.0, |u| self.comp[u as usize] == comp)
    }
    /// Nearest walkable point of the main ground component to an arbitrary point.
    pub fn nearest_walkable(&self, x: f32, y: f32) -> Option<[f32; 3]> {
        self.nearest_main(x, y).map(|u| self.node_pos(u))
    }
    /// Can a mech stand at this spot according to the grid (a node exists in this cell
    /// within 2 m of height `pos.z`)? Returns the node.
    pub fn node_at(&self, pos: [f32; 3]) -> Option<NodeId> {
        let (i, j) = self.cell_of(pos[0], pos[1]);
        let mut best: Option<(f32, NodeId)> = None;
        for u in self.cell_nodes(i, j) {
            let dz = (self.node_z[u as usize] - pos[2]).abs();
            if dz <= 2.0 && best.map_or(true, |(b, _)| dz < b) {
                best = Some((dz, u));
            }
        }
        best.map(|b| b.1)
    }

    // ------------------------------------------------------------------ searching

    #[inline]
    fn heuristic(&self, u: u32, goal: u32, gi: i32, gj: i32) -> f32 {
        let (i, j) = self.node_ij(u);
        let dx = (i - gi).abs() as f32;
        let dy = (j - gj).abs() as f32;
        let mut h = (dx.max(dy) + (std::f32::consts::SQRT_2 - 1.0) * dx.min(dy)) * self.cell;
        if self.lm_k > 0 {
            let a = &self.lm[u as usize * self.lm_k..(u as usize + 1) * self.lm_k];
            let b = &self.lm[goal as usize * self.lm_k..(goal as usize + 1) * self.lm_k];
            for l in 0..self.lm_k {
                let d = (a[l] - b[l]).abs();
                if d > h && d.is_finite() {
                    h = d;
                }
            }
        }
        h
    }

    /// A* over nodes. `penalty(node)` >= 0 is an extra cost per metre for entering a node
    /// (0 everywhere = shortest path). Returns the node chain start..=goal.
    pub fn astar_nodes<F: Fn(NodeId) -> f32>(&self, start: NodeId, goal: NodeId, penalty: F) -> Option<Vec<NodeId>> {
        if self.comp[start as usize] != self.comp[goal as usize] {
            return None;
        }
        if start == goal {
            return Some(vec![start]);
        }
        let mut sc = self.scratch.borrow_mut();
        let sc = &mut *sc;
        sc.begin(self.node_count());
        let cur = sc.cur;
        let (gi, gj) = self.node_ij(goal);
        let diag = self.cell * std::f32::consts::SQRT_2;
        sc.g[start as usize] = 0.0;
        sc.stamp[start as usize] = cur;
        sc.parent[start as usize] = NO_NODE;
        sc.heap.push(Reverse((fkey(self.heuristic(start, goal, gi, gj)), start)));
        let mut found = false;
        while let Some(Reverse((_, u))) = sc.heap.pop() {
            if sc.closed[u as usize] == cur {
                continue;
            }
            sc.closed[u as usize] = cur;
            if u == goal {
                found = true;
                break;
            }
            let gu = sc.g[u as usize];
            let nb = &self.nbr[u as usize];
            for d in 0..8 {
                let v = nb[d];
                if v == NO_NODE || sc.closed[v as usize] == cur {
                    continue;
                }
                let step = if d & 1 == 1 { diag } else { self.cell };
                let ng = gu + step * (1.0 + penalty(v).max(0.0));
                if sc.stamp[v as usize] != cur || ng < sc.g[v as usize] {
                    sc.stamp[v as usize] = cur;
                    sc.g[v as usize] = ng;
                    sc.parent[v as usize] = u;
                    sc.heap.push(Reverse((fkey(ng + self.heuristic(v, goal, gi, gj)), v)));
                }
            }
        }
        if !found {
            return None;
        }
        let mut path = vec![goal];
        let mut u = goal;
        while sc.parent[u as usize] != NO_NODE {
            u = sc.parent[u as usize];
            path.push(u);
        }
        path.reverse();
        Some(path)
    }

    fn dijkstra(&self, sources: &[NodeId], max_dist: f32, out: &mut Vec<f32>) {
        let n = self.node_count();
        out.clear();
        out.resize(n, f32::INFINITY);
        let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
        for &s in sources {
            if (s as usize) < n {
                out[s as usize] = 0.0;
                heap.push(Reverse((0, s)));
            }
        }
        let diag = self.cell * std::f32::consts::SQRT_2;
        while let Some(Reverse((k, u))) = heap.pop() {
            let du = out[u as usize];
            if f32::from_bits(k) > du {
                continue;
            }
            let nb = &self.nbr[u as usize];
            for d in 0..8 {
                let v = nb[d];
                if v == NO_NODE {
                    continue;
                }
                let nd = du + if d & 1 == 1 { diag } else { self.cell };
                if nd < out[v as usize] && nd <= max_dist {
                    out[v as usize] = nd;
                    heap.push(Reverse((fkey(nd), v)));
                }
            }
        }
    }

    /// Is the straight walk a→b clear according to the grid? `a_node` must be the node
    /// of the cell containing `a`; the walk must end on `b_node`. Keeps about
    /// `radius + 0.1` m from walls everywhere on the line (exact geometry near prisms).
    pub fn line_clear_nodes(&self, a_node: NodeId, a: [f32; 2], b_node: NodeId, b: [f32; 2]) -> bool {
        let (ai, aj) = self.cell_of(a[0], a[1]);
        let (mut ci, mut cj) = self.node_ij(a_node);
        if (ai, aj) != (ci, cj) {
            return false;
        }
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let len = (dx * dx + dy * dy).sqrt();
        let need = edge_need(&self.params);
        let m = ((len / 0.25).ceil() as usize).max(1);
        let mut cur = a_node;
        let w = &*self.world;
        for s in 0..=m {
            let t = s as f32 / m as f32;
            let x = a[0] + dx * t;
            let y = a[1] + dy * t;
            let (pi, pj) = self.cell_of(x, y);
            let (di, dj) = (pi - ci, pj - cj);
            if di != 0 || dj != 0 {
                if di.abs() > 1 || dj.abs() > 1 {
                    return false;
                }
                let v = self.nbr[cur as usize][DIR_OF[((di + 1) + (dj + 1) * 3) as usize]];
                if v == NO_NODE {
                    return false;
                }
                cur = v;
                ci = pi;
                cj = pj;
            }
            let cx = self.x0 + (ci as f32 + 0.5) * self.cell;
            let cy = self.y0 + (cj as f32 + 0.5) * self.cell;
            let off = ((x - cx) * (x - cx) + (y - cy) * (y - cy)).sqrt();
            if self.node_cons[cur as usize] - off < need {
                let z = if self.node_layer[cur as usize] == LAYER_TERRAIN { w.terrain_height(x, y) } else { self.node_z[cur as usize] };
                if w.clearance(x, y, z, self.params.height, self.params.step_up, need) < need {
                    return false;
                }
            }
        }
        cur == b_node
    }

    /// Is a straight walk between two points clear (grid check, both points snapped
    /// to the node of their cell)? Cheap; for an exact geometric answer use
    /// `World::walk_clear`.
    pub fn line_clear(&self, a: [f32; 3], b: [f32; 3]) -> bool {
        match (self.node_at(a), self.node_at(b)) {
            (Some(na), Some(nb)) => self.line_clear_nodes(na, [a[0], a[1]], nb, [b[0], b[1]]),
            _ => false,
        }
    }

    /// Smooth a node chain into waypoints. `from` is the real start position (used as
    /// the first anchor when it lies in the first node's cell).
    fn smooth(&self, chain: &[NodeId], from: [f32; 3]) -> Vec<(NodeId, [f32; 3])> {
        let mut out: Vec<(NodeId, [f32; 3])> = Vec::new();
        if chain.is_empty() {
            return out;
        }
        let last = chain.len() - 1;
        let xy = |u: NodeId| {
            let p = self.node_pos(u);
            [p[0], p[1]]
        };
        let mut anchor = [from[0], from[1]];
        let mut i = 0usize;
        // The start position may be off-centre: if it cannot reach the next node in a
        // clear line, go through the first node's centre.
        let probe0 = chain[last.min(1)];
        let c0 = xy(chain[0]);
        if (c0[0] - anchor[0]).hypot(c0[1] - anchor[1]) > 0.05 && !self.line_clear_nodes(chain[0], anchor, probe0, xy(probe0)) {
            out.push((chain[0], self.node_pos(chain[0])));
            anchor = xy(chain[0]);
        }
        while i < last {
            let mut best = i + 1;
            let mut step = 1usize;
            let mut fail = last + 1;
            loop {
                let probe = best + step;
                if probe <= last {
                    let q = xy(chain[probe]);
                    if (q[0] - anchor[0]).hypot(q[1] - anchor[1]) > MAX_LEG {
                        fail = probe;
                        break;
                    }
                }
                if probe > last {
                    if best < last && fail > last && self.line_clear_nodes(chain[i], anchor, chain[last], xy(chain[last])) {
                        best = last;
                    } else {
                        fail = fail.min(last + 1);
                    }
                    break;
                }
                if self.line_clear_nodes(chain[i], anchor, chain[probe], xy(chain[probe])) {
                    best = probe;
                    step *= 2;
                } else {
                    fail = probe;
                    break;
                }
            }
            // binary search between best (clear) and fail (not clear)
            let mut hi = fail.min(last + 1);
            while hi > best + 1 {
                let mid = (best + hi) / 2;
                if self.line_clear_nodes(chain[i], anchor, chain[mid], xy(chain[mid])) {
                    best = mid;
                } else {
                    hi = mid;
                }
            }
            out.push((chain[best], self.node_pos(chain[best])));
            anchor = xy(chain[best]);
            i = best;
        }
        if out.is_empty() {
            out.push((chain[last], self.node_pos(chain[last])));
        }
        out
    }

    fn finish_path(&self, chain: &[NodeId], from: [f32; 3], to: [f32; 3]) -> NavPath {
        let mut wp = self.smooth(chain, from);
        let goal = *chain.last().unwrap();
        let gp = self.node_pos(goal);
        // Use the exact goal point when it is clear from the previous waypoint / goal node.
        if (gp[0] - to[0]).hypot(gp[1] - to[1]) > 0.05 && self.cell_of(to[0], to[1]) == self.node_ij(goal) {
            let exact = [to[0], to[1], self.world.floor_at(to[0], to[1], gp[2] + 0.5, self.params.step_up).map_or(gp[2], |f| f.z)];
            let (pn, pp) = if wp.len() >= 2 { let q = wp[wp.len() - 2]; (q.0, [q.1[0], q.1[1]]) } else { (chain[0], [from[0], from[1]]) };
            if self.line_clear_nodes(pn, pp, goal, [to[0], to[1]]) {
                let l = wp.len() - 1;
                wp[l] = (goal, exact);
            } else if self.line_clear_nodes(goal, [gp[0], gp[1]], goal, [to[0], to[1]]) {
                wp.push((goal, exact));
            }
        }
        let mut length = 0.0;
        let mut prev = [from[0], from[1]];
        let mut points = Vec::with_capacity(wp.len());
        for (_, p) in &wp {
            length += (p[0] - prev[0]).hypot(p[1] - prev[1]);
            prev = [p[0], p[1]];
            points.push(*p);
        }
        let lastp = points[points.len() - 1];
        NavPath { points, length, goal_error: (lastp[0] - to[0]).hypot(lastp[1] - to[1]) }
    }

    /// Shortest walking path from `from` to `to` (3D positions, feet). Both ends are
    /// snapped: `from` to the nearest node, `to` to the nearest node of the same
    /// component (so a goal inside a building yields a path to the closest reachable
    /// spot; check `goal_error`). `None` only if `from` cannot be snapped.
    pub fn find_path(&self, from: [f32; 3], to: [f32; 3]) -> Option<NavPath> {
        self.find_path_with(from, to, |_| 0.0)
    }

    /// Like `find_path` with an extra cost per metre for entering each node
    /// (`penalty(node) >= 0`), e.g. to avoid exposed or dangerous ground.
    pub fn find_path_with<F: Fn(NodeId) -> f32>(&self, from: [f32; 3], to: [f32; 3], penalty: F) -> Option<NavPath> {
        let s = self.nearest_node(from)?;
        let g = self.nearest_in_comp(to, self.comp[s as usize])?;
        let chain = self.astar_nodes(s, g, penalty)?;
        Some(self.finish_path(&chain, from, to))
    }

    /// Walking distance from every node to the nearest of `goals` (positions snapped to
    /// main-component nodes when possible). Costs one Dijkstra (~10-20 ms for the whole
    /// map; bound it with `max_dist` to make it cheaper), then each lookup is O(1).
    pub fn dist_field(&self, goals: &[[f32; 3]], max_dist: f32) -> DistField {
        let src: Vec<NodeId> = goals.iter().filter_map(|g| self.nearest_main(g[0], g[1]).or_else(|| self.nearest_node(*g))).collect();
        self.dist_field_nodes(&src, max_dist)
    }

    pub fn dist_field_nodes(&self, goals: &[NodeId], max_dist: f32) -> DistField {
        let mut dist = Vec::new();
        self.dijkstra(goals, max_dist, &mut dist);
        DistField { dist }
    }

    /// Distance field to a disc: every main-component node within `radius` of
    /// (cx, cy) is a goal (e.g. "inside the next zone circle").
    pub fn dist_field_disc(&self, cx: f32, cy: f32, radius: f32, max_dist: f32) -> DistField {
        let mut src = Vec::new();
        let (i0, j0) = self.cell_of(cx - radius, cy - radius);
        let (i1, j1) = self.cell_of(cx + radius, cy + radius);
        for j in j0.max(0)..=j1.min(self.ny - 1) {
            for i in i0.max(0)..=i1.min(self.nx - 1) {
                let x = self.x0 + (i as f32 + 0.5) * self.cell - cx;
                let y = self.y0 + (j as f32 + 0.5) * self.cell - cy;
                if x * x + y * y <= radius * radius {
                    for u in self.cell_nodes(i, j) {
                        if self.comp[u as usize] == self.main {
                            src.push(u);
                        }
                    }
                }
            }
        }
        if src.is_empty() {
            if let Some(u) = self.nearest_main(cx, cy) {
                src.push(u);
            }
        }
        self.dist_field_nodes(&src, max_dist)
    }

    /// Field value at a position (distance of the nearest node, plus the offset to it).
    pub fn field_dist(&self, field: &DistField, pos: [f32; 3]) -> f32 {
        match self.nearest_node(pos) {
            Some(u) => {
                let p = self.node_pos(u);
                field.dist[u as usize] + (p[0] - pos[0]).hypot(p[1] - pos[1])
            }
            None => f32::INFINITY,
        }
    }

    /// Next waypoint when walking down a distance field from `pos`: follows the field
    /// for up to `lookahead` metres and returns the farthest point on that descent
    /// reachable in a clear straight line. `None` if already at a goal or unreachable.
    pub fn field_next(&self, field: &DistField, pos: [f32; 3], lookahead: f32) -> Option<[f32; 3]> {
        let s = self.nearest_node(pos)?;
        if !field.dist[s as usize].is_finite() {
            return None;
        }
        let mut chain = vec![s];
        let mut u = s;
        let start_d = field.dist[s as usize];
        while field.dist[u as usize] > 0.0 && start_d - field.dist[u as usize] < lookahead {
            let mut best = (field.dist[u as usize], NO_NODE);
            for &v in &self.nbr[u as usize] {
                if v != NO_NODE && field.dist[v as usize] < best.0 {
                    best = (field.dist[v as usize], v);
                }
            }
            if best.1 == NO_NODE {
                break;
            }
            u = best.1;
            chain.push(u);
        }
        if chain.len() == 1 {
            return None;
        }
        self.smooth(&chain, pos).first().map(|w| w.1)
    }

    /// Walkable spots near `from` (within `max_path_dist` metres of walking) where a
    /// mech of height `body_height` is hidden from `threat` (the enemy's eye or muzzle
    /// point): no line of fire to the mech's centre nor to its top. Trees are ignored
    /// (they do not stop shots). Nearest first, at most `max_results`.
    /// Cost: roughly 1-2 us per examined node, ~1 ms for a 40 m radius.
    pub fn cover_points(&self, from: [f32; 3], threat: [f32; 3], max_path_dist: f32, body_height: f32, max_results: usize) -> Vec<CoverPoint> {
        let mut out = Vec::new();
        let s = match self.nearest_node(from) {
            Some(s) => s,
            None => return out,
        };
        let w = &*self.world;
        let mut sc = self.scratch.borrow_mut();
        let sc = &mut *sc;
        sc.begin(self.node_count());
        let cur = sc.cur;
        let diag = self.cell * std::f32::consts::SQRT_2;
        sc.g[s as usize] = 0.0;
        sc.stamp[s as usize] = cur;
        sc.heap.push(Reverse((0, s)));
        while let Some(Reverse((_, u))) = sc.heap.pop() {
            if sc.closed[u as usize] == cur {
                continue;
            }
            sc.closed[u as usize] = cur;
            let gu = sc.g[u as usize];
            let p = self.node_pos(u);
            if !w.los(threat, [p[0], p[1], p[2] + body_height * 0.5]) && !w.los(threat, [p[0], p[1], p[2] + body_height]) {
                out.push(CoverPoint { pos: p, node: u, path_dist: gu });
                if out.len() >= max_results {
                    break;
                }
            }
            let nb = &self.nbr[u as usize];
            for d in 0..8 {
                let v = nb[d];
                if v == NO_NODE || sc.closed[v as usize] == cur {
                    continue;
                }
                let ng = gu + if d & 1 == 1 { diag } else { self.cell };
                if ng <= max_path_dist && (sc.stamp[v as usize] != cur || ng < sc.g[v as usize]) {
                    sc.stamp[v as usize] = cur;
                    sc.g[v as usize] = ng;
                    sc.heap.push(Reverse((fkey(ng), v)));
                }
            }
        }
        out
    }

    /// Is a mech of `body_height` standing on node `n` exposed to a shot from `threat`?
    pub fn exposed(&self, n: NodeId, threat: [f32; 3], body_height: f32) -> bool {
        let p = self.node_pos(n);
        let w = &*self.world;
        w.los(threat, [p[0], p[1], p[2] + body_height * 0.5]) || w.los(threat, [p[0], p[1], p[2] + body_height])
    }

    /// Wall clearance cap used for `node_clearance`.
    pub fn clearance_cap(&self) -> f32 {
        self.clr_cap
    }
}
