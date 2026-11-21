//! Top-down trace image of one mech's match (for debugging navigation).
use crate::sim::Sim;
use crate::util::{V2, V3};
use std::path::Path;
use world::{PrismKind, World, LAYER_TERRAIN};

#[derive(Clone, Copy)]
pub struct TracePt {
    pub pos: V3,
    pub wall: bool,
    pub in_zone: bool,
    pub airborne: bool,
    pub slow: bool,
}

#[derive(Default)]
pub struct Trace {
    pub slot: usize,
    pub pts: Vec<TracePt>,
    /// Where the traced mech was when it took weapon damage.
    pub hurt: Vec<V3>,
    pub death: Option<V3>,
    pub other_deaths: Vec<V3>,
    pub zones: Vec<(V2, f32)>,
    last_taken: f32,
    last_zone_acc: f32,
}

impl Trace {
    pub fn new(slot: usize, sim: &Sim) -> Trace {
        Trace { slot, zones: sim.zone_circles.clone(), ..Trace::default() }
    }
    /// Call once per tick.
    pub fn sample(&mut self, sim: &Sim) {
        let m = &sim.mechs[self.slot];
        if m.alive {
            let sp = m.kin.vel[0].hypot(m.kin.vel[1]);
            let want = m.cmd.mv[0].hypot(m.cmd.mv[1]).min(1.0);
            self.pts.push(TracePt { pos: m.kin.pos, wall: m.wall, in_zone: m.in_zone, airborne: !m.kin.grounded, slow: want > 0.5 && sp < 0.25 * m.st.walk_speed });
            let weapon_dmg = m.dmg_taken - self.last_taken;
            if weapon_dmg > 0.5 && m.zone_acc <= self.last_zone_acc {
                self.hurt.push(m.kin.pos);
            }
            self.last_taken = m.dmg_taken;
            self.last_zone_acc = m.zone_acc;
        } else if self.death.is_none() {
            self.death = m.death.as_ref().map(|d| d.pos);
        }
    }
    pub fn finish(&mut self, sim: &Sim) {
        self.death = sim.mechs[self.slot].death.as_ref().map(|d| d.pos);
        self.other_deaths = sim.mechs.iter().filter(|m| m.id != self.slot).filter_map(|m| m.death.as_ref().map(|d| d.pos)).collect();
    }
}

struct Img {
    w: usize,
    h: usize,
    px: Vec<u8>,
}
impl Img {
    fn set(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            let k = (y as usize * self.w + x as usize) * 3;
            self.px[k..k + 3].copy_from_slice(&c);
        }
    }
    fn disc(&mut self, x: i32, y: i32, r: i32, c: [u8; 3]) {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r * r {
                    self.set(x + dx, y + dy, c);
                }
            }
        }
    }
    fn line(&mut self, a: (i32, i32), b: (i32, i32), c: [u8; 3]) {
        let n = (b.0 - a.0).abs().max((b.1 - a.1).abs()).max(1);
        if n > 4000 {
            return;
        }
        for k in 0..=n {
            self.set(a.0 + (b.0 - a.0) * k / n, a.1 + (b.1 - a.1) * k / n, c);
        }
    }
    fn circle(&mut self, cx: f32, cy: f32, r: f32, c: [u8; 3]) {
        let n = ((r * 6.3) as usize).clamp(16, 6000);
        for k in 0..n {
            let a = k as f32 / n as f32 * std::f32::consts::TAU;
            self.set((cx + r * a.cos()) as i32, (cy + r * a.sin()) as i32, c);
        }
    }
    fn cross(&mut self, x: i32, y: i32, r: i32, c: [u8; 3]) {
        for k in -r..=r {
            for t in -1..=1 {
                self.set(x + k + t, y + k, c);
                self.set(x + k + t, y - k, c);
            }
        }
    }
}

fn crc32(data: &[u8], mut c: u32) -> u32 {
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    c
}

struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}
impl Bits {
    /// Append `n` bits, least significant first.
    fn put(&mut self, v: u32, n: u32) {
        self.acc |= v << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    /// Append a Huffman code (most significant bit first).
    fn code(&mut self, v: u32, n: u32) {
        let mut r = 0;
        for k in 0..n {
            r |= ((v >> k) & 1) << (n - 1 - k);
        }
        self.put(r, n);
    }
    fn sym(&mut self, s: u32) {
        match s {
            0..=143 => self.code(0x30 + s, 8),
            144..=255 => self.code(0x190 + s - 144, 9),
            256..=279 => self.code(s - 256, 7),
            _ => self.code(0xC0 + s - 280, 8),
        }
    }
}

/// zlib stream with one fixed-Huffman block. Matches only against the pixel to the left
/// (distance 3), which is plenty for a flat-coloured map.
fn deflate_rle3(raw: &[u8]) -> Vec<u8> {
    const BASE: [u32; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
    const EXTRA: [u32; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
    let mut b = Bits { out: vec![0x78, 0x01], acc: 0, n: 0 };
    b.put(1, 1); // final block
    b.put(1, 2); // fixed Huffman
    let mut i = 0;
    while i < raw.len() {
        let mut n = 0;
        if i >= 3 {
            while n < 258 && i + n < raw.len() && raw[i + n] == raw[i + n - 3] {
                n += 1;
            }
        }
        if n >= 3 {
            let k = (0..29).rev().find(|&k| BASE[k] <= n as u32).unwrap();
            b.sym(257 + k as u32);
            b.put(n as u32 - BASE[k], EXTRA[k]);
            b.code(2, 5); // distance 3
            i += n;
        } else {
            b.sym(raw[i] as u32);
            i += 1;
        }
    }
    b.sym(256);
    if b.n > 0 {
        b.put(0, 8 - b.n);
    }
    b.out
}

/// Minimal PNG encoder (fixed-Huffman deflate with run-length matches only).
pub fn png(w: usize, h: usize, rgb: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w * 3 + 1) * h);
    for y in 0..h {
        raw.push(0);
        raw.extend_from_slice(&rgb[y * w * 3..(y + 1) * w * 3]);
    }
    let mut z = deflate_rle3(&raw);
    let (mut a, mut b) = (1u32, 0u32);
    for &v in &raw {
        a = (a + v as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |tag: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(tag);
        out.extend_from_slice(data);
        let c = crc32(data, crc32(tag, 0xFFFF_FFFF)) ^ 0xFFFF_FFFF;
        out.extend_from_slice(&c.to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

/// Draw the trace over the map. `scale` = pixels per metre. `.ppm` or `.png` by extension.
///
/// Legend: grey = ground (lighter = higher), dark = buildings, purple = bridges, blue
/// rings = zone circles, path from green (start) to dark blue (end), cyan = airborne,
/// white = outside the zone, red dots = rubbing a wall, orange dots = commanded to move
/// but almost standing still, magenta dots = took weapon damage, red X = death, small
/// black x = other mechs' deaths.
pub fn render(world: &World, tr: &Trace, scale: f32, crop: bool, path: &Path) -> std::io::Result<()> {
    let mut b = world.bounds;
    let mut scale = scale.clamp(0.25, 8.0);
    if crop && !tr.pts.is_empty() {
        // crop to the path (plus a margin) and zoom in
        let (mut x0, mut x1, mut y0, mut y1) = (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
        for p in &tr.pts {
            x0 = x0.min(p.pos[0]);
            x1 = x1.max(p.pos[0]);
            y0 = y0.min(p.pos[1]);
            y1 = y1.max(p.pos[1]);
        }
        let m = 50.0;
        b.min_x = (x0 - m).max(b.min_x);
        b.max_x = (x1 + m).min(b.max_x);
        b.min_y = (y0 - m).max(b.min_y);
        b.max_y = (y1 + m).min(b.max_y);
        let span = (b.max_x - b.min_x).max(b.max_y - b.min_y).max(1.0);
        scale = scale.max((1100.0 / span).clamp(1.0, 6.0));
    }
    let w = (((b.max_x - b.min_x) * scale).ceil() as usize).max(1);
    let h = (((b.max_y - b.min_y) * scale).ceil() as usize).max(1);
    let mut img = Img { w, h, px: vec![0; w * h * 3] };
    let (lo, hi) = (world.terrain.min_h, world.terrain.max_h.max(world.terrain.min_h + 1.0));
    for py in 0..h {
        for px in 0..w {
            let x = b.min_x + (px as f32 + 0.5) / scale;
            let y = b.max_y - (py as f32 + 0.5) / scale;
            let f = world.top_surface(x, y);
            let c = if f.layer == LAYER_TERRAIN {
                let g = (150.0 + 80.0 * ((world.terrain_height(x, y) - lo) / (hi - lo)).clamp(0.0, 1.0)) as u8 & 0xFC;
                [g, g, g]
            } else {
                match world.prisms[(f.layer - 1) as usize].kind {
                    PrismKind::Bridge => [120, 100, 150],
                    PrismKind::Stadium => [95, 80, 75],
                    _ => [70, 72, 80],
                }
            };
            img.set(px as i32, py as i32, c);
        }
    }
    let to_px = |p: [f32; 2]| -> (i32, i32) { (((p[0] - b.min_x) * scale) as i32, ((b.max_y - p[1]) * scale) as i32) };
    for (c, r) in &tr.zones {
        let (cx, cy) = to_px(*c);
        img.circle(cx as f32, cy as f32, r * scale, [40, 90, 220]);
    }
    for d in &tr.other_deaths {
        let (x, y) = to_px([d[0], d[1]]);
        img.cross(x, y, 2, [0, 0, 0]);
    }
    let n = tr.pts.len().max(1);
    for k in 1..tr.pts.len() {
        let (p0, p1) = (tr.pts[k - 1], tr.pts[k]);
        let f = k as f32 / n as f32;
        let mut c = [(30.0 * (1.0 - f)) as u8, (200.0 * (1.0 - f) + 40.0 * f) as u8, (60.0 * (1.0 - f) + 200.0 * f) as u8];
        if p1.airborne {
            c = [0, 230, 230];
        }
        if !p1.in_zone {
            c = [255, 255, 255];
        }
        let (a, bb) = (to_px([p0.pos[0], p0.pos[1]]), to_px([p1.pos[0], p1.pos[1]]));
        img.line(a, bb, c);
        if scale >= 2.0 {
            img.line((a.0 + 1, a.1), (bb.0 + 1, bb.1), c);
        }
    }
    let r = (scale * 0.8).round().max(1.0) as i32;
    for p in &tr.pts {
        let (x, y) = to_px([p.pos[0], p.pos[1]]);
        if p.wall {
            img.disc(x, y, r, [230, 30, 30]);
        } else if p.slow {
            img.disc(x, y, r, [240, 150, 20]);
        }
    }
    for p in &tr.hurt {
        let (x, y) = to_px([p[0], p[1]]);
        img.disc(x, y, r, [230, 40, 230]);
    }
    if let Some(p) = tr.pts.first() {
        let (x, y) = to_px([p.pos[0], p.pos[1]]);
        img.disc(x, y, r + 2, [20, 220, 60]);
    }
    if let Some(d) = tr.death {
        let (x, y) = to_px([d[0], d[1]]);
        img.cross(x, y, 5 + r, [255, 0, 0]);
    }
    let bytes = if path.extension().map_or(false, |e| e == "ppm") {
        let mut v = format!("P6\n{} {}\n255\n", w, h).into_bytes();
        v.extend_from_slice(&img.px);
        v
    } else {
        png(w, h, &img.px)
    };
    std::fs::write(path, bytes)
}
