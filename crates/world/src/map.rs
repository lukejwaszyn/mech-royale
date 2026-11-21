//! Static map: loading and geometry queries (protocol.md §5, §7, §8, §11).
//!
//! Conventions: x east, y north, z up, meters. Positions are `[f32; 3]`,
//! horizontal points `[f32; 2]`. A mech position is its FEET (bottom centre).

use serde::Deserialize;
use std::fmt;
use std::path::Path;

/// Numerical tolerance for height comparisons (m).
pub const EPS: f32 = 1e-3;
/// `Floor::layer` value meaning "terrain". Prism `k` is layer `k + 1`.
pub const LAYER_TERRAIN: u32 = 0;

#[derive(Debug)]
pub struct LoadError(pub String);
impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "world load error: {}", self.0)
    }
}
impl std::error::Error for LoadError {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min_x: f32,
    pub max_x: f32,
    pub min_y: f32,
    pub max_y: f32,
}
impl Bounds {
    #[inline]
    pub fn contains(&self, x: f32, y: f32, margin: f32) -> bool {
        x >= self.min_x + margin && x <= self.max_x - margin && y >= self.min_y + margin && y <= self.max_y - margin
    }
    /// Clamp a point into the bounds shrunk by `margin`.
    #[inline]
    pub fn clamp(&self, x: f32, y: f32, margin: f32) -> [f32; 2] {
        [x.clamp(self.min_x + margin, self.max_x - margin), y.clamp(self.min_y + margin, self.max_y - margin)]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrismKind {
    Building,
    Stadium,
    Bridge,
    Other,
}

/// A solid: footprint polygon (`ring` minus `holes`) extruded from `base` to `top`.
#[derive(Clone, Debug)]
pub struct Prism {
    pub kind: PrismKind,
    pub base: f32,
    pub top: f32,
    /// Index into world.json `blocks` (named building), -1 if none.
    pub block: i32,
    pub ring: Vec<[f32; 2]>,
    pub holes: Vec<Vec<[f32; 2]>>,
    /// [min_x, min_y, max_x, max_y]
    pub bbox: [f32; 4],
    /// All boundary edges (ring and holes) as [x0, y0, x1, y1].
    pub edges: Vec<[f32; 4]>,
}

impl Prism {
    /// Is the horizontal point inside the footprint (inside ring, outside holes)?
    #[inline]
    pub fn contains_xy(&self, x: f32, y: f32) -> bool {
        if x < self.bbox[0] || x > self.bbox[2] || y < self.bbox[1] || y > self.bbox[3] {
            return false;
        }
        let mut inside = false;
        for e in &self.edges {
            if (e[1] > y) != (e[3] > y) && x < (e[2] - e[0]) * (y - e[1]) / (e[3] - e[1]) + e[0] {
                inside = !inside;
            }
        }
        inside
    }

    /// Horizontal distance from the point to the footprint (0 if inside).
    /// Returns `cap` early when the point is farther than `cap` from the bbox.
    pub fn dist_xy(&self, x: f32, y: f32, cap: f32) -> f32 {
        let bx = (self.bbox[0] - x).max(x - self.bbox[2]).max(0.0);
        let by = (self.bbox[1] - y).max(y - self.bbox[3]).max(0.0);
        if bx >= cap || by >= cap {
            return cap;
        }
        if self.contains_xy(x, y) {
            return 0.0;
        }
        let mut best = cap * cap;
        for e in &self.edges {
            let d = seg_dist2(x, y, e[0], e[1], e[2], e[3]);
            if d < best {
                best = d;
            }
        }
        best.sqrt()
    }

    /// Footprint area (ring minus holes), m².
    pub fn area(&self) -> f32 {
        fn a(r: &[[f32; 2]]) -> f32 {
            let mut s = 0.0;
            for i in 0..r.len() {
                let p = r[i];
                let q = r[(i + 1) % r.len()];
                s += p[0] * q[1] - q[0] * p[1];
            }
            (s * 0.5).abs()
        }
        a(&self.ring) - self.holes.iter().map(|h| a(h)).sum::<f32>()
    }
}

#[inline]
fn seg_dist2(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((px - x0) * dx + (py - y0) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    let cx = x0 + dx * t - px;
    let cy = y0 + dy * t - py;
    cx * cx + cy * cy
}

/// A tree. Trees block vision only (trunk hard, crown soft); never shots or movement.
#[derive(Clone, Copy, Debug)]
pub struct Tree {
    pub x: f32,
    pub y: f32,
    /// Ground height at the trunk.
    pub z: f32,
    pub height: f32,
    pub crown_r: f32,
    /// Height above `z` where the crown starts.
    pub crown_base: f32,
    pub trunk_r: f32,
    pub crown_x: f32,
    pub crown_y: f32,
}

/// Heightfield, bilinear over a regular vertex grid.
pub struct Terrain {
    pub min_x: f32,
    pub min_y: f32,
    pub step: f32,
    pub nx: usize,
    pub ny: usize,
    /// Row-major heights in meters, `h[j * nx + i]`.
    pub h: Vec<f32>,
    pub min_h: f32,
    pub max_h: f32,
    inv_step: f32,
    // 8-vertex tiles, each holding the max height of itself and its 8 neighbours.
    tnx: usize,
    tny: usize,
    tile_max: Vec<f32>,
}

const TILE: usize = 8;

impl Terrain {
    /// Ground height at (x, y), bilinear; clamped at the grid border.
    #[inline]
    pub fn height(&self, x: f32, y: f32) -> f32 {
        let fx = ((x - self.min_x) * self.inv_step).clamp(0.0, (self.nx - 1) as f32);
        let fy = ((y - self.min_y) * self.inv_step).clamp(0.0, (self.ny - 1) as f32);
        let i = (fx as usize).min(self.nx - 2);
        let j = (fy as usize).min(self.ny - 2);
        let a = fx - i as f32;
        let b = fy - j as f32;
        let k = j * self.nx + i;
        let h00 = self.h[k];
        let h10 = self.h[k + 1];
        let h01 = self.h[k + self.nx];
        let h11 = self.h[k + self.nx + 1];
        (h00 * (1.0 - a) + h10 * a) * (1.0 - b) + (h01 * (1.0 - a) + h11 * a) * b
    }

    /// Terrain gradient (dz/dx, dz/dy) at (x, y).
    #[inline]
    pub fn gradient(&self, x: f32, y: f32) -> [f32; 2] {
        let fx = ((x - self.min_x) * self.inv_step).clamp(0.0, (self.nx - 1) as f32);
        let fy = ((y - self.min_y) * self.inv_step).clamp(0.0, (self.ny - 1) as f32);
        let i = (fx as usize).min(self.nx - 2);
        let j = (fy as usize).min(self.ny - 2);
        let a = fx - i as f32;
        let b = fy - j as f32;
        let k = j * self.nx + i;
        let h00 = self.h[k];
        let h10 = self.h[k + 1];
        let h01 = self.h[k + self.nx];
        let h11 = self.h[k + self.nx + 1];
        [
            ((h10 - h00) * (1.0 - b) + (h11 - h01) * b) * self.inv_step,
            ((h01 - h00) * (1.0 - a) + (h11 - h10) * a) * self.inv_step,
        ]
    }

    /// Upper bound of the terrain height anywhere within 8 m (horizontally) of (x, y).
    #[inline]
    fn max_near(&self, x: f32, y: f32) -> f32 {
        let i = (((x - self.min_x) * self.inv_step).max(0.0) as usize / TILE).min(self.tnx - 1);
        let j = (((y - self.min_y) * self.inv_step).max(0.0) as usize / TILE).min(self.tny - 1);
        self.tile_max[j * self.tnx + i]
    }

    fn new(min_x: f32, min_y: f32, step: f32, nx: usize, ny: usize, h: Vec<f32>) -> Terrain {
        let tnx = (nx - 1 + TILE - 1) / TILE;
        let tny = (ny - 1 + TILE - 1) / TILE;
        let mut raw = vec![f32::NEG_INFINITY; tnx * tny];
        for tj in 0..tny {
            for ti in 0..tnx {
                let mut m = f32::NEG_INFINITY;
                for j in tj * TILE..=((tj + 1) * TILE).min(ny - 1) {
                    for i in ti * TILE..=((ti + 1) * TILE).min(nx - 1) {
                        m = m.max(h[j * nx + i]);
                    }
                }
                raw[tj * tnx + ti] = m;
            }
        }
        let mut tile_max = vec![f32::NEG_INFINITY; tnx * tny];
        for tj in 0..tny {
            for ti in 0..tnx {
                let mut m = f32::NEG_INFINITY;
                for j in tj.saturating_sub(1)..=(tj + 1).min(tny - 1) {
                    for i in ti.saturating_sub(1)..=(ti + 1).min(tnx - 1) {
                        m = m.max(raw[j * tnx + i]);
                    }
                }
                tile_max[tj * tnx + ti] = m;
            }
        }
        let min_h = h.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_h = h.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        Terrain { min_x, min_y, step, nx, ny, h, min_h, max_h, inv_step: 1.0 / step, tnx, tny, tile_max }
    }
}

/// Uniform grid of item lists (CSR).
struct Grid {
    x0: f32,
    y0: f32,
    cs: f32,
    inv: f32,
    nx: i32,
    ny: i32,
    start: Vec<u32>,
    items: Vec<u32>,
}

impl Grid {
    fn build(x0: f32, y0: f32, x1: f32, y1: f32, cs: f32, lists: impl Fn(i32, i32, &mut Vec<u32>)) -> Grid {
        let nx = (((x1 - x0) / cs).ceil() as i32).max(1);
        let ny = (((y1 - y0) / cs).ceil() as i32).max(1);
        let mut start = Vec::with_capacity((nx * ny + 1) as usize);
        let mut items = Vec::new();
        for j in 0..ny {
            for i in 0..nx {
                start.push(items.len() as u32);
                lists(i, j, &mut items);
            }
        }
        start.push(items.len() as u32);
        Grid { x0, y0, cs, inv: 1.0 / cs, nx, ny, start, items }
    }
    #[inline]
    fn cell(&self, i: i32, j: i32) -> &[u32] {
        let k = (j * self.nx + i) as usize;
        &self.items[self.start[k] as usize..self.start[k + 1] as usize]
    }
    #[inline]
    fn cell_at(&self, x: f32, y: f32) -> &[u32] {
        let i = ((x - self.x0) * self.inv).floor() as i32;
        let j = ((y - self.y0) * self.inv).floor() as i32;
        if i < 0 || j < 0 || i >= self.nx || j >= self.ny {
            return &[];
        }
        self.cell(i, j)
    }
    /// Cell index range overlapped by an axis-aligned box.
    #[inline]
    fn range(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> (i32, i32, i32, i32) {
        let i0 = (((x0 - self.x0) * self.inv).floor() as i32).max(0);
        let j0 = (((y0 - self.y0) * self.inv).floor() as i32).max(0);
        let i1 = (((x1 - self.x0) * self.inv).floor() as i32).min(self.nx - 1);
        let j1 = (((y1 - self.y0) * self.inv).floor() as i32).min(self.ny - 1);
        (i0, j0, i1, j1)
    }
    /// Visit the cells crossed by segment a→b in order. `f(items, t_exit)` returns false to stop.
    fn walk<F: FnMut(&[u32], f32) -> bool>(&self, ax: f32, ay: f32, bx: f32, by: f32, mut f: F) {
        let dx = bx - ax;
        let dy = by - ay;
        let xm = self.x0 + self.nx as f32 * self.cs;
        let ym = self.y0 + self.ny as f32 * self.cs;
        let mut t0 = 0.0f32;
        let mut t1 = 1.0f32;
        for (p, d, lo, hi) in [(ax, dx, self.x0, xm), (ay, dy, self.y0, ym)] {
            if d.abs() < 1e-9 {
                if p < lo || p > hi {
                    return;
                }
            } else {
                let ta = (lo - p) / d;
                let tb = (hi - p) / d;
                let (ta, tb) = if ta < tb { (ta, tb) } else { (tb, ta) };
                t0 = t0.max(ta);
                t1 = t1.min(tb);
                if t0 > t1 {
                    return;
                }
            }
        }
        let sx = ax + dx * t0;
        let sy = ay + dy * t0;
        let mut ix = (((sx - self.x0) * self.inv).floor() as i32).clamp(0, self.nx - 1);
        let mut iy = (((sy - self.y0) * self.inv).floor() as i32).clamp(0, self.ny - 1);
        let (stx, mut tmx, tdx) = if dx.abs() < 1e-9 {
            (0, f32::INFINITY, f32::INFINITY)
        } else if dx > 0.0 {
            (1, (self.x0 + (ix + 1) as f32 * self.cs - ax) / dx, self.cs / dx)
        } else {
            (-1, (self.x0 + ix as f32 * self.cs - ax) / dx, -self.cs / dx)
        };
        let (sty, mut tmy, tdy) = if dy.abs() < 1e-9 {
            (0, f32::INFINITY, f32::INFINITY)
        } else if dy > 0.0 {
            (1, (self.y0 + (iy + 1) as f32 * self.cs - ay) / dy, self.cs / dy)
        } else {
            (-1, (self.y0 + iy as f32 * self.cs - ay) / dy, -self.cs / dy)
        };
        loop {
            let t_exit = tmx.min(tmy).min(t1);
            if !f(self.cell(ix, iy), t_exit) {
                return;
            }
            if t_exit >= t1 {
                return;
            }
            if tmx < tmy {
                ix += stx;
                tmx += tdx;
                if ix < 0 || ix >= self.nx {
                    return;
                }
            } else {
                iy += sty;
                tmy += tdy;
                if iy < 0 || iy >= self.ny {
                    return;
                }
            }
        }
    }
}

/// The surface a mech stands on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Floor {
    pub z: f32,
    /// `LAYER_TERRAIN` (0) or prism index + 1.
    pub layer: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HitKind {
    Terrain,
    Prism(u32),
}

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    /// Fraction along the segment, 0..=1.
    pub t: f32,
    pub pos: [f32; 3],
    pub kind: HitKind,
}

/// Body + walking rules of a mech. Fill from `hello.config.mech` / `self.stats`
/// (radius and height grow 5% per cube).
#[derive(Clone, Copy, Debug)]
pub struct Body {
    pub radius: f32,
    pub height: f32,
    pub step_up: f32,
    pub max_slope_deg: f32,
}
impl Default for Body {
    fn default() -> Self {
        Body { radius: 2.5, height: 9.0, step_up: 1.2, max_slope_deg: 50.0 }
    }
}

/// Vision rules (protocol.md §8). Fill from `hello.config.vision` plus the mech's bonuses.
#[derive(Clone, Copy, Debug)]
pub struct VisionParams {
    pub range: f32,
    pub fov_deg: f32,
    pub near_range: f32,
    pub canopy_blocks: bool,
}
impl Default for VisionParams {
    fn default() -> Self {
        VisionParams { range: 200.0, fov_deg: 120.0, near_range: 45.0, canopy_blocks: true }
    }
}

pub struct World {
    pub bounds: Bounds,
    pub terrain: Terrain,
    pub prisms: Vec<Prism>,
    pub trees: Vec<Tree>,
    /// world.json `rules.canopyDensity` (per metre of crown crossed).
    pub canopy_density: f32,
    /// world.json `rules.canopyBlockT`: a sight line is blocked when its transmittance drops below this.
    pub canopy_block_t: f32,
    /// world.json `rules.maxStep` / `maxSlopeDeg` (viewer rules; the server uses config.mech).
    pub rules_max_step: f32,
    pub rules_max_slope_deg: f32,
    pgrid: Grid,
    tgrid: Grid,
}

#[derive(Deserialize)]
struct RawBounds {
    #[serde(rename = "minX")]
    min_x: f32,
    #[serde(rename = "maxX")]
    max_x: f32,
    #[serde(rename = "minY")]
    min_y: f32,
    #[serde(rename = "maxY")]
    max_y: f32,
}
#[derive(Deserialize, Default)]
struct RawRules {
    #[serde(rename = "maxStep")]
    max_step: Option<f32>,
    #[serde(rename = "maxSlopeDeg")]
    max_slope_deg: Option<f32>,
    #[serde(rename = "canopyDensity")]
    canopy_density: Option<f32>,
    #[serde(rename = "canopyBlockT")]
    canopy_block_t: Option<f32>,
}
#[derive(Deserialize)]
struct RawTerrain {
    file: Option<String>,
    #[serde(rename = "minX")]
    min_x: f32,
    #[serde(rename = "minY")]
    min_y: f32,
    step: f32,
    nx: usize,
    ny: usize,
    scale: Option<f32>,
}
#[derive(Deserialize)]
struct RawPrism {
    kind: Option<String>,
    block: Option<i32>,
    base: f32,
    top: f32,
    ring: Vec<f32>,
    #[serde(default)]
    holes: Vec<Vec<f32>>,
}
#[derive(Deserialize, Default)]
struct RawTrees {
    #[serde(default)]
    fields: Vec<String>,
    #[serde(default)]
    data: Vec<f32>,
}
#[derive(Deserialize)]
struct RawWorld {
    bounds: RawBounds,
    #[serde(default)]
    rules: RawRules,
    terrain: RawTerrain,
    #[serde(default)]
    prisms: Vec<RawPrism>,
    #[serde(default)]
    trees: RawTrees,
}

const PRISM_CELL: f32 = 8.0;
const TREE_CELL: f32 = 16.0;

impl World {
    /// Load the map from `hello.map.dir` (the directory holding `v2/world.json`).
    /// Also accepts the `v2` directory itself.
    pub fn load(map_dir: &Path) -> Result<World, LoadError> {
        let mut dir = map_dir.join("v2");
        if !dir.join("world.json").exists() {
            dir = map_dir.to_path_buf();
        }
        let wj = dir.join("world.json");
        let bytes = std::fs::read(&wj).map_err(|e| LoadError(format!("{}: {}", wj.display(), e)))?;
        let raw: RawWorld = serde_json::from_slice(&bytes).map_err(|e| LoadError(format!("{}: {}", wj.display(), e)))?;
        let tname = raw.terrain.file.clone().unwrap_or_else(|| "terrain.i16".to_string());
        let tp = dir.join(&tname);
        let tb = std::fs::read(&tp).map_err(|e| LoadError(format!("{}: {}", tp.display(), e)))?;
        Self::from_parts(raw, &tb)
    }

    /// Build from in-memory file contents (world.json text, terrain.i16 bytes).
    pub fn from_bytes(world_json: &[u8], terrain_i16: &[u8]) -> Result<World, LoadError> {
        let raw: RawWorld = serde_json::from_slice(world_json).map_err(|e| LoadError(e.to_string()))?;
        Self::from_parts(raw, terrain_i16)
    }

    fn from_parts(raw: RawWorld, tb: &[u8]) -> Result<World, LoadError> {
        let (nx, ny) = (raw.terrain.nx, raw.terrain.ny);
        if nx < 2 || ny < 2 || tb.len() < nx * ny * 2 {
            return Err(LoadError(format!("terrain size mismatch: {} bytes for {}x{}", tb.len(), nx, ny)));
        }
        let scale = raw.terrain.scale.unwrap_or(0.01);
        let h: Vec<f32> = tb[..nx * ny * 2].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 * scale).collect();
        let terrain = Terrain::new(raw.terrain.min_x, raw.terrain.min_y, raw.terrain.step, nx, ny, h);
        let bounds = Bounds { min_x: raw.bounds.min_x, max_x: raw.bounds.max_x, min_y: raw.bounds.min_y, max_y: raw.bounds.max_y };

        let to_pts = |f: &[f32]| -> Vec<[f32; 2]> { f.chunks_exact(2).map(|c| [c[0], c[1]]).collect() };
        let mut prisms = Vec::with_capacity(raw.prisms.len());
        for rp in &raw.prisms {
            let ring = to_pts(&rp.ring);
            if ring.len() < 3 {
                continue;
            }
            let holes: Vec<Vec<[f32; 2]>> = rp.holes.iter().map(|h| to_pts(h)).filter(|h| h.len() >= 3).collect();
            let mut edges = Vec::new();
            let mut bbox = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
            for poly in std::iter::once(&ring).chain(holes.iter()) {
                for i in 0..poly.len() {
                    let p = poly[i];
                    let q = poly[(i + 1) % poly.len()];
                    if p != q {
                        edges.push([p[0], p[1], q[0], q[1]]);
                    }
                }
            }
            for p in &ring {
                bbox[0] = bbox[0].min(p[0]);
                bbox[1] = bbox[1].min(p[1]);
                bbox[2] = bbox[2].max(p[0]);
                bbox[3] = bbox[3].max(p[1]);
            }
            let kind = match rp.kind.as_deref() {
                Some("building") => PrismKind::Building,
                Some("stadium") => PrismKind::Stadium,
                Some("bridge") => PrismKind::Bridge,
                _ => PrismKind::Other,
            };
            prisms.push(Prism { kind, base: rp.base.min(rp.top), top: rp.top.max(rp.base), block: rp.block.unwrap_or(-1), ring, holes, bbox, edges });
        }

        let col = |name: &str| raw.trees.fields.iter().position(|f| f == name);
        let nf = raw.trees.fields.len().max(1);
        let mut trees = Vec::new();
        if let (Some(cx), Some(cy), Some(cz), Some(ch)) = (col("x"), col("y"), col("z"), col("height")) {
            let (ccr, ccb, ctr, ccx, ccy) = (col("crownR"), col("crownBase"), col("trunkR"), col("crownX"), col("crownY"));
            for r in raw.trees.data.chunks_exact(nf) {
                let g = |c: Option<usize>, d: f32| c.map(|c| r[c]).unwrap_or(d);
                let height = r[ch];
                trees.push(Tree {
                    x: r[cx],
                    y: r[cy],
                    z: r[cz],
                    height,
                    crown_r: g(ccr, 3.0),
                    crown_base: g(ccb, height * 0.3).min(height),
                    trunk_r: g(ctr, 0.3),
                    crown_x: g(ccx, r[cx]),
                    crown_y: g(ccy, r[cy]),
                });
            }
        }

        // Grids cover the terrain extent (a superset of the bounds).
        let gx0 = terrain.min_x.min(bounds.min_x) - 1.0;
        let gy0 = terrain.min_y.min(bounds.min_y) - 1.0;
        let gx1 = (terrain.min_x + (nx - 1) as f32 * terrain.step).max(bounds.max_x) + 1.0;
        let gy1 = (terrain.min_y + (ny - 1) as f32 * terrain.step).max(bounds.max_y) + 1.0;
        let half_diag = PRISM_CELL * 0.5 * std::f32::consts::SQRT_2 + 0.05;
        // Per-prism candidate lists first, to avoid testing every prism per cell.
        let pnx = (((gx1 - gx0) / PRISM_CELL).ceil() as i32).max(1);
        let pny = (((gy1 - gy0) / PRISM_CELL).ceil() as i32).max(1);
        let mut pcells: Vec<Vec<u32>> = vec![Vec::new(); (pnx * pny) as usize];
        for (pi, p) in prisms.iter().enumerate() {
            let i0 = (((p.bbox[0] - gx0) / PRISM_CELL).floor() as i32).clamp(0, pnx - 1);
            let i1 = (((p.bbox[2] - gx0) / PRISM_CELL).floor() as i32).clamp(0, pnx - 1);
            let j0 = (((p.bbox[1] - gy0) / PRISM_CELL).floor() as i32).clamp(0, pny - 1);
            let j1 = (((p.bbox[3] - gy0) / PRISM_CELL).floor() as i32).clamp(0, pny - 1);
            for j in j0..=j1 {
                for i in i0..=i1 {
                    let cx = gx0 + (i as f32 + 0.5) * PRISM_CELL;
                    let cy = gy0 + (j as f32 + 0.5) * PRISM_CELL;
                    if p.dist_xy(cx, cy, half_diag + 1.0) <= half_diag {
                        pcells[(j * pnx + i) as usize].push(pi as u32);
                    }
                }
            }
        }
        let pgrid = Grid::build(gx0, gy0, gx1, gy1, PRISM_CELL, |i, j, out| out.extend_from_slice(&pcells[(j * pnx + i) as usize]));

        let tnx = (((gx1 - gx0) / TREE_CELL).ceil() as i32).max(1);
        let tny = (((gy1 - gy0) / TREE_CELL).ceil() as i32).max(1);
        let mut tcells: Vec<Vec<u32>> = vec![Vec::new(); (tnx * tny) as usize];
        for (ti, t) in trees.iter().enumerate() {
            let x0 = (t.crown_x - t.crown_r).min(t.x - t.trunk_r) - 0.05;
            let x1 = (t.crown_x + t.crown_r).max(t.x + t.trunk_r) + 0.05;
            let y0 = (t.crown_y - t.crown_r).min(t.y - t.trunk_r) - 0.05;
            let y1 = (t.crown_y + t.crown_r).max(t.y + t.trunk_r) + 0.05;
            let i0 = (((x0 - gx0) / TREE_CELL).floor() as i32).clamp(0, tnx - 1);
            let i1 = (((x1 - gx0) / TREE_CELL).floor() as i32).clamp(0, tnx - 1);
            let j0 = (((y0 - gy0) / TREE_CELL).floor() as i32).clamp(0, tny - 1);
            let j1 = (((y1 - gy0) / TREE_CELL).floor() as i32).clamp(0, tny - 1);
            for j in j0..=j1 {
                for i in i0..=i1 {
                    tcells[(j * tnx + i) as usize].push(ti as u32);
                }
            }
        }
        let tgrid = Grid::build(gx0, gy0, gx1, gy1, TREE_CELL, |i, j, out| out.extend_from_slice(&tcells[(j * tnx + i) as usize]));

        Ok(World {
            bounds,
            terrain,
            prisms,
            trees,
            canopy_density: raw.rules.canopy_density.unwrap_or(0.35),
            canopy_block_t: raw.rules.canopy_block_t.unwrap_or(0.25),
            rules_max_step: raw.rules.max_step.unwrap_or(1.5),
            rules_max_slope_deg: raw.rules.max_slope_deg.unwrap_or(50.0),
            pgrid,
            tgrid,
        })
    }

    // ------------------------------------------------------------------ heights

    /// Bare terrain height at (x, y) (ignores prisms).
    #[inline]
    pub fn terrain_height(&self, x: f32, y: f32) -> f32 {
        self.terrain.height(x, y)
    }

    /// Call `f(prism_index, prism)` for every prism whose footprint contains (x, y).
    #[inline]
    pub fn prisms_at<F: FnMut(u32, &Prism)>(&self, x: f32, y: f32, mut f: F) {
        for &pi in self.pgrid.cell_at(x, y) {
            let p = &self.prisms[pi as usize];
            if p.contains_xy(x, y) {
                f(pi, p);
            }
        }
    }

    /// Call `f(prism_index, prism)` for prisms that MAY be within `r` of (x, y)
    /// (conservative superset; a prism can be reported more than once).
    #[inline]
    pub fn prisms_near<F: FnMut(u32, &Prism)>(&self, x: f32, y: f32, r: f32, mut f: F) {
        let (i0, j0, i1, j1) = self.pgrid.range(x - r, y - r, x + r, y + r);
        for j in j0..=j1 {
            for i in i0..=i1 {
                for &pi in self.pgrid.cell(i, j) {
                    f(pi, &self.prisms[pi as usize]);
                }
            }
        }
    }

    /// True if no prism can be within `r` of (x, y) (cheap pre-check).
    #[inline]
    pub fn no_prism_near(&self, x: f32, y: f32, r: f32) -> bool {
        let (i0, j0, i1, j1) = self.pgrid.range(x - r, y - r, x + r, y + r);
        for j in j0..=j1 {
            for i in i0..=i1 {
                if !self.pgrid.cell(i, j).is_empty() {
                    return false;
                }
            }
        }
        true
    }

    /// The surface a mech whose feet are at height `feet_z` stands on / steps onto at (x, y):
    /// the highest surface (terrain or prism top) not above `feet_z + step_up`.
    /// `None` if that spot is inside solid (inside a building, or under ground).
    ///
    /// * walking: `floor_at(x, y, feet_z, step_up)`
    /// * where would I land when falling from z: `floor_at(x, y, z, 0.0)`
    /// * under a bridge with feet at ground level you get the terrain; with feet at
    ///   deck level you get the deck.
    pub fn floor_at(&self, x: f32, y: f32, feet_z: f32, step_up: f32) -> Option<Floor> {
        let g = self.terrain.height(x, y);
        let lim = feet_z + step_up + EPS;
        if g > lim {
            return None;
        }
        let mut best = Floor { z: g, layer: LAYER_TERRAIN };
        let mut cont = [(0.0f32, 0.0f32); 12];
        let mut n = 0;
        for &pi in self.pgrid.cell_at(x, y) {
            let p = &self.prisms[pi as usize];
            if p.contains_xy(x, y) {
                if p.top <= lim && p.top > best.z {
                    best = Floor { z: p.top, layer: pi + 1 };
                }
                if n < cont.len() {
                    cont[n] = (p.base, p.top);
                    n += 1;
                }
            }
        }
        for &(base, top) in &cont[..n] {
            if base < best.z + 0.01 && top > best.z + 0.01 {
                return None;
            }
        }
        Some(best)
    }

    /// Highest surface at (x, y): the roof if there is one, else the terrain.
    pub fn top_surface(&self, x: f32, y: f32) -> Floor {
        let mut best = Floor { z: self.terrain.height(x, y), layer: LAYER_TERRAIN };
        self.prisms_at(x, y, |pi, p| {
            if p.top > best.z {
                best = Floor { z: p.top, layer: pi + 1 };
            }
        });
        best
    }

    /// Is the point inside solid (below terrain or inside a prism)?
    pub fn point_solid(&self, p: [f32; 3]) -> bool {
        if p[2] < self.terrain.height(p[0], p[1]) - EPS {
            return true;
        }
        let mut solid = false;
        self.prisms_at(p[0], p[1], |_, pr| {
            if p[2] > pr.base + EPS && p[2] < pr.top - EPS {
                solid = true;
            }
        });
        solid
    }

    /// Lowest overhang (prism underside) above `z` at (x, y), i.e. the ceiling a jet would hit.
    pub fn ceiling_at(&self, x: f32, y: f32, z: f32) -> Option<f32> {
        let mut c: Option<f32> = None;
        self.prisms_at(x, y, |_, p| {
            if p.base >= z - EPS && c.map_or(true, |v| p.base < v) {
                c = Some(p.base);
            }
        });
        c
    }

    // ---------------------------------------------------------------- collision

    /// Horizontal distance from (x, y) to the nearest prism that is a wall for a mech
    /// with feet at `z`: a prism that overlaps the body vertically (`base < z + height`)
    /// and is too tall to step onto (`top > z + step_up`). Capped at `cap`.
    /// 0 means the point is inside such a prism's footprint.
    pub fn clearance(&self, x: f32, y: f32, z: f32, height: f32, step_up: f32, cap: f32) -> f32 {
        let mut best = cap;
        let (i0, j0, i1, j1) = self.pgrid.range(x - cap, y - cap, x + cap, y + cap);
        for j in j0..=j1 {
            for i in i0..=i1 {
                for &pi in self.pgrid.cell(i, j) {
                    let p = &self.prisms[pi as usize];
                    if p.top > z + step_up + EPS && p.base < z + height - EPS {
                        let d = p.dist_xy(x, y, best);
                        if d < best {
                            best = d;
                        }
                    }
                }
            }
        }
        best
    }

    /// Would a mech cylinder (`body.radius`, `body.height`) with feet at `pos` intersect
    /// the world? True if it overlaps a wall/overhang prism, is under ground, or pokes
    /// outside the map bounds. A ledge up to `step_up` above the feet does not block.
    pub fn mech_blocked(&self, pos: [f32; 3], body: &Body) -> bool {
        let (x, y, z) = (pos[0], pos[1], pos[2]);
        if !self.bounds.contains(x, y, 0.0) {
            return true;
        }
        if self.terrain.height(x, y) > z + body.step_up + EPS {
            return true;
        }
        self.clearance(x, y, z, body.height, body.step_up, body.radius) < body.radius
    }

    /// Simulate walking in a straight line from `from` (feet) to `to` (x, y) with the
    /// step/slope rules. Returns the feet height at the end, or `None` if the walk is
    /// stopped: a wall or overhang within `body.radius`, a ledge higher than `step_up`,
    /// an uphill slope steeper than `max_slope_deg`, or a drop larger than `max_drop`
    /// (pass `f32::INFINITY` to allow walking off roofs).
    ///
    /// Use `radius: 0.0` to trace the centre only.
    pub fn walk_clear(&self, from: [f32; 3], to: [f32; 2], body: &Body, max_drop: f32) -> Option<f32> {
        let mut cur = self.floor_at(from[0], from[1], from[2], body.step_up)?;
        if body.radius > 0.0 && self.mech_blocked([from[0], from[1], cur.z], body) {
            return None;
        }
        let dx = to[0] - from[0];
        let dy = to[1] - from[1];
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-4 {
            return Some(cur.z);
        }
        let n = (len / 0.5).ceil() as usize;
        let ds = len / n as f32;
        let max_rise = body.max_slope_deg.to_radians().tan() * ds + 0.01;
        for k in 1..=n {
            let t = k as f32 / n as f32;
            let x = from[0] + dx * t;
            let y = from[1] + dy * t;
            let f = self.floor_at(x, y, cur.z, body.step_up)?;
            let rise = f.z - cur.z;
            if rise > 0.0 {
                if f.layer == LAYER_TERRAIN && cur.layer == LAYER_TERRAIN && rise > max_rise {
                    return None;
                }
            } else if -rise > max_drop.max(max_rise) {
                return None;
            }
            if body.radius > 0.0 && self.mech_blocked([x, y, f.z], body) {
                return None;
            }
            cur = f;
        }
        Some(cur.z)
    }

    // ------------------------------------------------------------------ raycast

    fn prism_ray(&self, p: &Prism, a: [f32; 3], d: [f32; 3]) -> Option<f32> {
        let mut t0 = 0.0f32;
        let mut t1 = 1.0f32;
        if d[2].abs() < 1e-9 {
            if a[2] < p.base || a[2] > p.top {
                return None;
            }
        } else {
            let ta = (p.base - a[2]) / d[2];
            let tb = (p.top - a[2]) / d[2];
            let (ta, tb) = if ta < tb { (ta, tb) } else { (tb, ta) };
            t0 = t0.max(ta);
            t1 = t1.min(tb);
            if t0 > t1 {
                return None;
            }
        }
        let (dx, dy) = (d[0], d[1]);
        let ox = a[0] + dx * t0;
        let oy = a[1] + dy * t0;
        let ex = a[0] + dx * t1;
        let ey = a[1] + dy * t1;
        if ox.max(ex) < p.bbox[0] || ox.min(ex) > p.bbox[2] || oy.max(ey) < p.bbox[1] || oy.min(ey) > p.bbox[3] {
            return None;
        }
        if dx * dx + dy * dy < 1e-10 {
            return if p.contains_xy(ox, oy) { Some(t0) } else { None };
        }
        let mut cnt = 0u32;
        let mut best = f32::INFINITY;
        for e in &p.edges {
            let q0x = e[0] - a[0];
            let q0y = e[1] - a[1];
            let q1x = e[2] - a[0];
            let q1y = e[3] - a[1];
            let s0 = dx * q0y - dy * q0x;
            let s1 = dx * q1y - dy * q1x;
            if (s0 > 0.0) != (s1 > 0.0) {
                let sx = e[2] - e[0];
                let sy = e[3] - e[1];
                let den = dx * sy - dy * sx;
                if den.abs() < 1e-12 {
                    continue;
                }
                let t = (q0x * sy - q0y * sx) / den;
                if t > t0 {
                    cnt += 1;
                    if t < best {
                        best = t;
                    }
                }
            }
        }
        if cnt & 1 == 1 {
            Some(t0)
        } else if best <= t1 {
            Some(best)
        } else {
            None
        }
    }

    fn prisms_ray(&self, a: [f32; 3], b: [f32; 3], any: bool) -> Option<(f32, u32)> {
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let mut best: Option<(f32, u32)> = None;
        let mut seen = [0u64; 16];
        let mailbox = self.prisms.len() <= 1024;
        self.pgrid.walk(a[0], a[1], b[0], b[1], |items, t_exit| {
            for &pi in items {
                if mailbox {
                    let w = &mut seen[(pi >> 6) as usize];
                    let bit = 1u64 << (pi & 63);
                    if *w & bit != 0 {
                        continue;
                    }
                    *w |= bit;
                }
                if let Some(t) = self.prism_ray(&self.prisms[pi as usize], a, d) {
                    if best.map_or(true, |(bt, _)| t < bt) {
                        best = Some((t, pi));
                    }
                }
            }
            match best {
                Some((bt, _)) => !(any || bt <= t_exit),
                None => true,
            }
        });
        best
    }

    fn terrain_ray(&self, a: [f32; 3], b: [f32; 3]) -> Option<f32> {
        let tr = &self.terrain;
        if a[2].min(b[2]) > tr.max_h {
            return None;
        }
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let lh = (d[0] * d[0] + d[1] * d[1]).sqrt();
        if a[2] < tr.height(a[0], a[1]) {
            return Some(0.0);
        }
        if lh < 1e-4 {
            let g = tr.height(a[0], a[1]);
            return if b[2] < g { Some(((a[2] - g) / (a[2] - b[2])).clamp(0.0, 1.0)) } else { None };
        }
        let dt1 = (tr.step / lh).min(1.0);
        let dt8 = (TILE as f32 * tr.step) / lh;
        let mut t = 0.0f32;
        while t < 1.0 {
            let x = a[0] + d[0] * t;
            let y = a[1] + d[1] * t;
            let z = a[2] + d[2] * t;
            let t8 = (t + dt8).min(1.0);
            let z8 = a[2] + d[2] * t8;
            if z.min(z8) > tr.max_near(x, y) {
                t = t8;
                continue;
            }
            let tn = (t + dt1).min(1.0);
            let zn = a[2] + d[2] * tn;
            if zn < tr.height(a[0] + d[0] * tn, a[1] + d[1] * tn) {
                let (mut lo, mut hi) = (t, tn);
                for _ in 0..8 {
                    let m = 0.5 * (lo + hi);
                    if a[2] + d[2] * m < tr.height(a[0] + d[0] * m, a[1] + d[1] * m) {
                        hi = m;
                    } else {
                        lo = m;
                    }
                }
                return Some(hi);
            }
            t = tn;
        }
        None
    }

    /// Brute-force reference for `raycast` (tests only; slow).
    #[doc(hidden)]
    pub fn raycast_brute(&self, a: [f32; 3], b: [f32; 3]) -> Option<f32> {
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let mut best: Option<f32> = None;
        for p in &self.prisms {
            if let Some(t) = self.prism_ray(p, a, d) {
                if best.map_or(true, |b| t < b) {
                    best = Some(t);
                }
            }
        }
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let n = (len / 0.1).ceil().max(1.0) as usize;
        for s in 0..=n {
            let t = s as f32 / n as f32;
            if best.map_or(false, |b| t >= b) {
                break;
            }
            if a[2] + d[2] * t < self.terrain.height(a[0] + d[0] * t, a[1] + d[1] * t) {
                best = Some(t);
                break;
            }
        }
        best
    }

    /// First thing a shot (gun bullet, laser, mortar segment) from `a` to `b` hits:
    /// prisms (buildings, stadium, bridges) and terrain. Trees never stop shots.
    pub fn raycast(&self, a: [f32; 3], b: [f32; 3]) -> Option<Hit> {
        let ph = self.prisms_ray(a, b, false);
        let th = self.terrain_ray(a, b);
        let (t, kind) = match (ph, th) {
            (Some((tp, pi)), Some(tt)) => {
                if tp <= tt {
                    (tp, HitKind::Prism(pi))
                } else {
                    (tt, HitKind::Terrain)
                }
            }
            (Some((tp, pi)), None) => (tp, HitKind::Prism(pi)),
            (None, Some(tt)) => (tt, HitKind::Terrain),
            (None, None) => return None,
        };
        Some(Hit { t, pos: [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t], kind })
    }

    /// True if nothing solid (prism or terrain) lies between `a` and `b`. This is the
    /// line of fire test; trees are ignored.
    #[inline]
    pub fn los(&self, a: [f32; 3], b: [f32; 3]) -> bool {
        self.prisms_ray(a, b, true).is_none() && self.terrain_ray(a, b).is_none()
    }

    /// Foliage on the sight line a→b: `(trunk_hit, crown_metres)`.
    pub fn foliage(&self, a: [f32; 3], b: [f32; 3]) -> (bool, f32) {
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let dh2 = d[0] * d[0] + d[1] * d[1];
        let mut crown = 0.0f32;
        let mut trunk = false;
        let mut seen = [u32::MAX; 96];
        let mut ns = 0usize;
        self.tgrid.walk(a[0], a[1], b[0], b[1], |items, _| {
            'tree: for &ti in items {
                for s in &seen[..ns] {
                    if *s == ti {
                        continue 'tree;
                    }
                }
                if ns < seen.len() {
                    seen[ns] = ti;
                    ns += 1;
                }
                let t = &self.trees[ti as usize];
                // Trunk: vertical cylinder from the ground to the crown base.
                if dh2 > 1e-10 {
                    let ox = a[0] - t.x;
                    let oy = a[1] - t.y;
                    let bq = ox * d[0] + oy * d[1];
                    let cq = ox * ox + oy * oy - t.trunk_r * t.trunk_r;
                    let disc = bq * bq - dh2 * cq;
                    if disc > 0.0 {
                        let sq = disc.sqrt();
                        let ta = ((-bq - sq) / dh2).max(0.0);
                        let tb = ((-bq + sq) / dh2).min(1.0);
                        if ta < tb {
                            let za = a[2] + d[2] * ta;
                            let zb = a[2] + d[2] * tb;
                            let (zl, zh) = if za < zb { (za, zb) } else { (zb, za) };
                            if zh > t.z && zl < t.z + t.crown_base {
                                trunk = true;
                                return false;
                            }
                        }
                    }
                }
                // Crown: ellipsoid, horizontal radius crown_r, spanning crown_base..height.
                let rz = ((t.height - t.crown_base) * 0.5).max(0.1);
                let cz = t.z + (t.height + t.crown_base) * 0.5;
                let k = t.crown_r / rz;
                let ox = a[0] - t.crown_x;
                let oy = a[1] - t.crown_y;
                let oz = (a[2] - cz) * k;
                let dz = d[2] * k;
                let aa = dh2 + dz * dz;
                if aa > 1e-10 {
                    let bq = ox * d[0] + oy * d[1] + oz * dz;
                    let cq = ox * ox + oy * oy + oz * oz - t.crown_r * t.crown_r;
                    let disc = bq * bq - aa * cq;
                    if disc > 0.0 {
                        let sq = disc.sqrt();
                        let ta = ((-bq - sq) / aa).max(0.0);
                        let tb = ((-bq + sq) / aa).min(1.0);
                        if tb > ta {
                            crown += (tb - ta) * len;
                        }
                    }
                }
            }
            true
        });
        (trunk, crown)
    }

    /// Sight line test a→b: solid geometry blocks, tree trunks block, and (if `canopy`)
    /// tree crowns block once `exp(-canopy_density * metres_of_crown) < canopy_block_t`.
    pub fn los_vision(&self, a: [f32; 3], b: [f32; 3], canopy: bool) -> bool {
        if !self.los(a, b) {
            return false;
        }
        let (trunk, crown) = self.foliage(a, b);
        if trunk {
            return false;
        }
        !(canopy && (-self.canopy_density * crown).exp() < self.canopy_block_t)
    }

    /// Protocol §8 visibility: can an eye at `eye` whose torso yaw is `eye_yaw` see an
    /// object standing at `target_feet` with the given height? (Line of sight to its
    /// centre or top; within `near_range` any direction with canopy ignored, or within
    /// `range` inside the yaw wedge with canopy.)
    pub fn can_see(&self, eye: [f32; 3], eye_yaw: f32, target_feet: [f32; 3], target_height: f32, vp: &VisionParams) -> bool {
        let center = [target_feet[0], target_feet[1], target_feet[2] + target_height * 0.5];
        let top = [target_feet[0], target_feet[1], target_feet[2] + target_height];
        let dx = center[0] - eye[0];
        let dy = center[1] - eye[1];
        let dz = center[2] - eye[2];
        let dist = (dx * dx + dy * dy + dz * dz).sqrt();
        let near = dist <= vp.near_range;
        let mut far = false;
        if !near && dist <= vp.range {
            let mut da = dy.atan2(dx) - eye_yaw;
            while da > std::f32::consts::PI {
                da -= 2.0 * std::f32::consts::PI;
            }
            while da < -std::f32::consts::PI {
                da += 2.0 * std::f32::consts::PI;
            }
            far = da.abs() <= vp.fov_deg.to_radians() * 0.5;
        }
        if !near && !far {
            return false;
        }
        let canopy = vp.canopy_blocks && !near;
        self.los_vision(eye, center, canopy) || self.los_vision(eye, top, canopy)
    }
}
