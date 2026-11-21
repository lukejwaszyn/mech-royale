//! Mech / drone movement: acceleration, walls with sliding, step-up, slopes, jump, jet,
//! ceilings, roof edges. One call = one sim tick. Approximation of protocol.md section 7.
use crate::config::MechStats;
use crate::util::{V2, V3};
use world::{Body, Prism, World, EPS, LAYER_TERRAIN};

/// Kinematic state of a body. `pos` is the feet (bottom centre).
#[derive(Clone, Copy, Debug, Default)]
pub struct Kin {
    pub pos: V3,
    pub vel: V3,
    pub grounded: bool,
}

/// Body size and movement numbers (from `MechStats`, or made up for drones).
#[derive(Clone, Copy, Debug)]
pub struct MoveParams {
    pub radius: f32,
    pub height: f32,
    pub step_up: f32,
    pub max_slope_deg: f32,
    pub walk_speed: f32,
    pub boost_speed: f32,
    pub ground_accel: f32,
    pub air_accel: f32,
    pub gravity: f32,
    pub jump_speed: f32,
    pub jet_accel: f32,
    pub max_rise_speed: f32,
    pub boost_drain: f32,
    pub jet_drain: f32,
}

impl MoveParams {
    pub fn from_stats(st: &MechStats) -> MoveParams {
        MoveParams {
            radius: st.radius,
            height: st.height,
            step_up: st.step_up,
            max_slope_deg: st.max_slope_deg,
            walk_speed: st.walk_speed,
            boost_speed: st.boost_speed,
            ground_accel: st.ground_accel,
            air_accel: st.air_accel,
            gravity: st.gravity,
            jump_speed: st.jump_speed,
            jet_accel: st.jet_accel,
            max_rise_speed: st.max_rise_speed,
            boost_drain: st.boost_drain,
            jet_drain: st.jet_drain,
        }
    }
    pub fn body(&self) -> Body {
        Body { radius: self.radius, height: self.height, step_up: self.step_up, max_slope_deg: self.max_slope_deg }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MoveIn {
    /// Desired horizontal direction, world frame; length is clamped to 1.
    pub mv: V2,
    pub boost: bool,
    pub jump: bool,
    /// Extra multiplier on the commanded speed (0.5 while charging a laser).
    pub speed_mult: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MoveOut {
    /// Touched a wall or the map edge and slid along it.
    pub wall: bool,
    /// Could not move at all this tick (wall, ledge or slope).
    pub blocked: bool,
    pub boosted: bool,
    pub jetted: bool,
    pub jumped: bool,
    pub landed: bool,
}

/// How far a body is slid off an edge / out from under an eave per tick (protocol: 0.6 m).
pub const EDGE_SLIDE: f32 = 0.6;

#[inline]
fn closest_on_edges(p: &Prism, x: f32, y: f32) -> (f32, f32, f32) {
    let mut best = (f32::INFINITY, x, y);
    for e in &p.edges {
        let dx = e[2] - e[0];
        let dy = e[3] - e[1];
        let l2 = dx * dx + dy * dy;
        let t = if l2 > 0.0 { (((x - e[0]) * dx + (y - e[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
        let qx = e[0] + dx * t;
        let qy = e[1] + dy * t;
        let d2 = (qx - x) * (qx - x) + (qy - y) * (qy - y);
        if d2 < best.0 {
            best = (d2, qx, qy);
        }
    }
    best
}

#[inline]
fn bbox_far(p: &Prism, x: f32, y: f32, r: f32) -> bool {
    x < p.bbox[0] - r || x > p.bbox[2] + r || y < p.bbox[1] - r || y > p.bbox[3] + r
}

/// Push (x, y) out of every wall prism for a body with feet at `z`. `Err` if the centre
/// is inside a wall footprint or cannot be freed. `Ok(true)` if a push happened.
fn wall_push(w: &World, x: &mut f32, y: &mut f32, z: f32, p: &MoveParams, step: f32, vel: &mut V3) -> Result<bool, ()> {
    let r = p.radius;
    let mut pushed = false;
    for _ in 0..4 {
        let (cx, cy) = (*x, *y);
        let mut inside = false;
        let mut pen = 0.0f32;
        let mut n = [0.0f32; 2];
        w.prisms_near(cx, cy, r, |_, pr| {
            if inside || !(pr.top > z + step + EPS && pr.base < z + p.height - EPS) || bbox_far(pr, cx, cy, r) {
                return;
            }
            if pr.contains_xy(cx, cy) {
                inside = true;
                return;
            }
            let (d2, qx, qy) = closest_on_edges(pr, cx, cy);
            if d2 < r * r {
                let d = d2.sqrt();
                if r - d > pen && d > 1e-5 {
                    pen = r - d;
                    n = [(cx - qx) / d, (cy - qy) / d];
                }
            }
        });
        if inside {
            return Err(());
        }
        if pen <= 0.0 {
            return Ok(pushed);
        }
        *x += n[0] * (pen + 2e-3);
        *y += n[1] * (pen + 2e-3);
        pushed = true;
        let vn = vel[0] * n[0] + vel[1] * n[1];
        if vn < 0.0 {
            vel[0] -= vn * n[0];
            vel[1] -= vn * n[1];
        }
    }
    if w.clearance(*x, *y, z, p.height, step, r) < r - 0.01 {
        return Err(());
    }
    Ok(pushed)
}

fn try_move(w: &World, k: &mut Kin, p: &MoveParams, d: V2, out: &mut MoveOut) -> bool {
    let (ox, oy, z) = (k.pos[0], k.pos[1], k.pos[2]);
    let step = if k.grounded { p.step_up } else { 0.0 };
    let mut vel = k.vel;
    let mut wall = false;
    let b = &w.bounds;
    let (mut nx, mut ny) = (ox + d[0], oy + d[1]);
    let cx = nx.clamp(b.min_x + p.radius, (b.max_x - p.radius).max(b.min_x + p.radius));
    let cy = ny.clamp(b.min_y + p.radius, (b.max_y - p.radius).max(b.min_y + p.radius));
    if cx != nx {
        vel[0] = 0.0;
        wall = true;
    }
    if cy != ny {
        vel[1] = 0.0;
        wall = true;
    }
    nx = cx;
    ny = cy;
    match wall_push(w, &mut nx, &mut ny, z, p, step, &mut vel) {
        Err(()) => return false,
        Ok(pushed) => wall |= pushed,
    }
    match w.floor_at(nx, ny, z, step) {
        Some(f) => {
            if k.grounded && f.z > z + 1e-3 {
                if f.layer == LAYER_TERRAIN {
                    let run = ((nx - ox).powi(2) + (ny - oy).powi(2)).sqrt();
                    let on_terrain = w.floor_at(ox, oy, z, step).map_or(true, |c| c.layer == LAYER_TERRAIN);
                    if on_terrain && f.z - z > p.max_slope_deg.to_radians().tan() * run + 0.01 {
                        return false;
                    }
                }
                if let Some(c) = w.ceiling_at(nx, ny, f.z + 0.01) {
                    if c < f.z + p.height - EPS {
                        return false;
                    }
                }
            }
        }
        None => {
            // Terrain higher than the feet can step, or solid. In the air a gentle rise
            // is a landing on the slope; anything else stops the move.
            if !k.grounded {
                let h = w.terrain_height(nx, ny);
                if h > z && h - z <= 1.5 && w.floor_at(nx, ny, h, 0.0).map_or(false, |f| (f.z - h).abs() < 0.02) {
                    k.pos = [nx, ny, h];
                    k.vel = [vel[0], vel[1], 0.0];
                    k.grounded = true;
                    out.landed = true;
                    out.wall |= wall;
                    return true;
                }
            }
            return false;
        }
    }
    k.pos[0] = nx;
    k.pos[1] = ny;
    k.vel[0] = vel[0];
    k.vel[1] = vel[1];
    out.wall |= wall;
    true
}

fn move_h(w: &World, k: &mut Kin, p: &MoveParams, d: V2, out: &mut MoveOut) -> bool {
    if d[0].abs() < 1e-7 && d[1].abs() < 1e-7 {
        return true;
    }
    if try_move(w, k, p, d, out) {
        return true;
    }
    // Crude slide for terrain ledges / slopes and deep corners: keep one axis.
    if d[0].abs() > 1e-4 && d[1].abs() > 1e-4 {
        let first_x = d[0].abs() >= d[1].abs();
        let a = if first_x { [d[0], 0.0] } else { [0.0, d[1]] };
        let b = if first_x { [0.0, d[1]] } else { [d[0], 0.0] };
        if try_move(w, k, p, a, out) || try_move(w, k, p, b, out) {
            out.wall = true;
            return true;
        }
    }
    out.blocked = true;
    out.wall = true;
    k.vel[0] = 0.0;
    k.vel[1] = 0.0;
    false
}

/// Height of the surface holding up a body whose feet are at `z_from`: the floor under
/// the centre, or the top of a roof edge the cylinder still overhangs (returned with the
/// direction that slides the body off it).
fn support(w: &World, x: f32, y: f32, z_from: f32, p: &MoveParams) -> (f32, Option<V2>) {
    let mut best = w.floor_at(x, y, z_from, 0.0).map(|f| f.z).unwrap_or(f32::NEG_INFINITY);
    let center = best;
    let mut dir = None;
    let r = p.radius;
    w.prisms_near(x, y, r, |_, pr| {
        if pr.top <= z_from + EPS && pr.top > best + 0.01 && pr.top > center + p.step_up && !bbox_far(pr, x, y, r) && !pr.contains_xy(x, y) {
            let (d2, qx, qy) = closest_on_edges(pr, x, y);
            if d2 < r * r * 0.998 {
                let d = d2.sqrt().max(1e-5);
                best = pr.top;
                dir = Some([(x - qx) / d, (y - qy) / d]);
            }
        }
    });
    (best, dir)
}

/// Slide the body up to `EDGE_SLIDE` along `dir` (off a roof edge / out from under an
/// eave) unless it is walking against that direction or a wall holds it there.
fn slide(w: &World, k: &mut Kin, p: &MoveParams, dir: V2, mv: V2) {
    if mv[0] * dir[0] + mv[1] * dir[1] < -0.1 {
        return;
    }
    let (nx, ny) = (k.pos[0] + dir[0] * EDGE_SLIDE, k.pos[1] + dir[1] * EDGE_SLIDE);
    if w.bounds.contains(nx, ny, p.radius) && w.clearance(nx, ny, k.pos[2], p.height, 0.0, p.radius) >= p.radius {
        k.pos[0] = nx;
        k.pos[1] = ny;
    }
}

fn vertical(w: &World, k: &mut Kin, p: &MoveParams, mv: V2, dt: f32, out: &mut MoveOut) {
    let (x, y, z) = (k.pos[0], k.pos[1], k.pos[2]);
    if k.grounded {
        match w.floor_at(x, y, z, p.step_up) {
            Some(f) if f.z >= z - p.step_up - 0.1 => {
                k.pos[2] = f.z;
                k.vel[2] = 0.0;
            }
            Some(_) => {
                let (sz, edge) = support(w, x, y, z, p);
                if let (true, Some(dir)) = (sz >= z - 0.05, edge) {
                    k.pos[2] = sz;
                    k.vel[2] = 0.0;
                    slide(w, k, p, dir, mv);
                } else {
                    k.grounded = false;
                    k.vel[2] = 0.0;
                }
            }
            None => {}
        }
        return;
    }
    let mut nz = z + k.vel[2] * dt;
    if k.vel[2] > 0.0 {
        // Undersides (bridges, eaves) over any part of the body stop a rise.
        let r = p.radius;
        let mut ceil = f32::INFINITY;
        let mut cdir: Option<V2> = None;
        w.prisms_near(x, y, r, |_, pr| {
            if pr.base >= z + p.height - 0.02 && pr.base < nz + p.height && pr.base < ceil && !bbox_far(pr, x, y, r) {
                if pr.contains_xy(x, y) {
                    ceil = pr.base;
                    cdir = None;
                } else {
                    let (d2, qx, qy) = closest_on_edges(pr, x, y);
                    if d2 < r * r {
                        let d = d2.sqrt().max(1e-5);
                        ceil = pr.base;
                        cdir = Some([(x - qx) / d, (y - qy) / d]);
                    }
                }
            }
        });
        if nz + p.height > ceil {
            nz = (ceil - p.height).max(z);
            k.vel[2] = 0.0;
            if let Some(dir) = cdir {
                slide(w, k, p, dir, mv);
            }
        }
    }
    let (sz, edge) = support(w, k.pos[0], k.pos[1], z, p);
    if nz <= sz {
        k.pos[2] = sz;
        k.vel[2] = 0.0;
        k.grounded = true;
        out.landed = true;
        if let Some(dir) = edge {
            slide(w, k, p, dir, mv);
        }
    } else {
        k.pos[2] = nz;
    }
}

/// Advance one body by one tick. Drains `energy` for boost / jet (regeneration is the
/// caller's business).
pub fn step_kin(w: &World, k: &mut Kin, p: &MoveParams, inp: &MoveIn, energy: &mut f32, dt: f32) -> MoveOut {
    let mut out = MoveOut::default();
    let mut mv = inp.mv;
    if !mv[0].is_finite() || !mv[1].is_finite() {
        mv = [0.0, 0.0];
    }
    let ml = (mv[0] * mv[0] + mv[1] * mv[1]).sqrt();
    if ml > 1.0 {
        mv = [mv[0] / ml, mv[1] / ml];
    }
    let moving = ml > 0.01;
    let boost_ground = k.grounded && inp.boost && moving && *energy > 0.0;
    let top = if boost_ground { p.boost_speed } else { p.walk_speed } * inp.speed_mult;
    let tv = [mv[0] * top, mv[1] * top];
    let acc = if k.grounded { p.ground_accel } else { p.air_accel };
    let (dvx, dvy) = (tv[0] - k.vel[0], tv[1] - k.vel[1]);
    let dl = (dvx * dvx + dvy * dvy).sqrt();
    let maxdv = acc * dt;
    if dl <= maxdv {
        k.vel[0] = tv[0];
        k.vel[1] = tv[1];
    } else {
        k.vel[0] += dvx / dl * maxdv;
        k.vel[1] += dvy / dl * maxdv;
    }
    if boost_ground {
        *energy -= p.boost_drain * dt;
        out.boosted = true;
    }
    if k.grounded && inp.jump {
        k.vel[2] = p.jump_speed;
        k.grounded = false;
        out.jumped = true;
    }
    if !k.grounded {
        k.vel[2] -= p.gravity * dt;
        if inp.boost && *energy > 0.0 {
            if k.vel[2] < p.max_rise_speed {
                k.vel[2] = (k.vel[2] + p.jet_accel * dt).min(p.max_rise_speed);
            }
            *energy -= p.jet_drain * dt;
            out.jetted = true;
        }
    }
    if *energy < 0.0 {
        *energy = 0.0;
    }
    let (dx, dy) = (k.vel[0] * dt, k.vel[1] * dt);
    let d = (dx * dx + dy * dy).sqrt();
    let n = ((d / 0.5).ceil() as usize).clamp(1, 16);
    for _ in 0..n {
        if !move_h(w, k, p, [dx / n as f32, dy / n as f32], &mut out) {
            break;
        }
    }
    vertical(w, k, p, mv, dt, &mut out);
    out
}
