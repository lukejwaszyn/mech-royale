//! Gunnery math: intercept (lead) for straight projectiles, mortar ballistics, laser
//! damage. Pure functions, unit-tested at the bottom.
use crate::mathx::*;
use crate::proto::{V2, V3};

/// Time `t >= 0` at which a projectile of speed `speed`, fired now from the origin, meets
/// a target at relative position `rel` moving with constant velocity `vel`
/// (solves |rel + vel t| = speed t). `None` if it can never catch it.
pub fn intercept_time(rel: V3, vel: V3, speed: f32) -> Option<f32> {
    let a = dot(vel, vel) - speed * speed;
    let b = 2.0 * dot(rel, vel);
    let c = dot(rel, rel);
    if c < 1e-9 {
        return Some(0.0);
    }
    if a.abs() < 1e-6 {
        if b.abs() < 1e-9 {
            return None;
        }
        let t = -c / b;
        return if t > 0.0 { Some(t) } else { None };
    }
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return None;
    }
    let s = disc.sqrt();
    let t1 = (-b - s) / (2.0 * a);
    let t2 = (-b + s) / (2.0 * a);
    let (lo, hi) = if t1 < t2 { (t1, t2) } else { (t2, t1) };
    if lo > 0.0 {
        Some(lo)
    } else if hi > 0.0 {
        Some(hi)
    } else {
        None
    }
}

/// Lead solution for a straight projectile.
///
/// * `muzzle`: the point on the mech axis at muzzle height (the shot leaves `offset`
///   metres further along the aim, which is on the same line, so only flight time changes);
/// * `delay`: seconds until the shot actually leaves (decision latency);
/// * returns the point to aim at and the flight time.
pub fn lead_point(muzzle: V3, target: V3, vel: V3, speed: f32, delay: f32, offset: f32) -> Option<(V3, f32)> {
    let at_fire = add(target, mul(vel, delay));
    let mut t = intercept_time(sub(at_fire, muzzle), vel, speed)?;
    // The projectile starts `offset` metres out: a couple of fixed-point steps fix the time.
    for _ in 0..3 {
        let p = add(at_fire, mul(vel, t));
        t = ((dist(p, muzzle) - offset).max(0.0)) / speed;
    }
    Some((add(at_fire, mul(vel, t)), t))
}

/// Launch pitch that lands a shell `d` metres away horizontally and `h` metres higher
/// (negative = lower) with muzzle speed `v` and gravity `g`. `high` picks the lobbed arc.
pub fn mortar_pitch(d: f32, h: f32, v: f32, g: f32, high: bool) -> Option<f32> {
    if d < 1e-3 || v <= 0.0 || g <= 0.0 {
        return None;
    }
    let v2 = v * v;
    let disc = v2 * v2 - g * (g * d * d + 2.0 * h * v2);
    if disc < 0.0 {
        return None;
    }
    let s = disc.sqrt();
    let tan = if high { (v2 + s) / (g * d) } else { (v2 - s) / (g * d) };
    Some(tan.atan())
}

/// Flight time of that shell.
pub fn mortar_time(d: f32, v: f32, pitch: f32) -> f32 {
    d / (v * pitch.cos()).max(1e-3)
}

#[derive(Clone, Copy, Debug)]
pub struct MortarShot {
    pub aim: V2,
    pub time: f32,
    /// Where the shell leaves the tube.
    pub origin: V3,
}

/// Full mortar solution from the tube point on the mech axis. The shell leaves `offset`
/// metres out along the aim, so the launch point depends on the pitch: iterate.
pub fn mortar_solve(tube: V3, target: V3, v: f32, g: f32, high: bool, offset: f32, min_pitch: f32, max_pitch: f32) -> Option<MortarShot> {
    let yaw = (target[1] - tube[1]).atan2(target[0] - tube[0]);
    let d0 = dist2d(tube, target);
    let mut pitch = mortar_pitch(d0, target[2] - tube[2], v, g, high)?;
    for _ in 0..4 {
        let dir = dir_of(yaw, pitch);
        let origin = add(tube, mul(dir, offset));
        let d = dist2d(origin, target);
        if dot2([target[0] - origin[0], target[1] - origin[1]], [yaw.cos(), yaw.sin()]) <= 0.5 {
            return None; // target is inside the barrel offset
        }
        pitch = mortar_pitch(d, target[2] - origin[2], v, g, high)?;
    }
    if pitch < min_pitch || pitch > max_pitch {
        return None;
    }
    let dir = dir_of(yaw, pitch);
    let origin = add(tube, mul(dir, offset));
    Some(MortarShot { aim: [yaw, pitch], time: mortar_time(dist2d(origin, target), v, pitch), origin })
}

/// Point on a shell's arc `t` seconds after launch.
pub fn mortar_point(origin: V3, aim: V2, v: f32, g: f32, t: f32) -> V3 {
    let d = dir_of(aim[0], aim[1]);
    [origin[0] + d[0] * v * t, origin[1] + d[1] * v * t, origin[2] + d[2] * v * t - 0.5 * g * t * t]
}

/// Laser damage for a charge of `charge_s` seconds.
pub fn laser_damage(damage: f32, min_frac: f32, full_charge: f32, charge_s: f32) -> f32 {
    let f = if full_charge > 1e-3 { (charge_s / full_charge).clamp(0.0, 1.0) } else { 1.0 };
    damage * (min_frac + (1.0 - min_frac) * f)
}

/// Closest approach of a point moving with constant relative velocity to a vertical
/// cylinder at the origin (radius `r`, from z=0 to z=`h`) within `[0, t_max]`.
/// `rel` is projectile position minus cylinder feet, `vel` the relative velocity.
/// Returns the time of the hit, if any.
pub fn hits_cylinder(rel: V3, vel: V3, r: f32, h: f32, t_max: f32) -> Option<f32> {
    let a = vel[0] * vel[0] + vel[1] * vel[1];
    let b = 2.0 * (rel[0] * vel[0] + rel[1] * vel[1]);
    let c = rel[0] * rel[0] + rel[1] * rel[1] - r * r;
    let (t_in, t_out) = if a < 1e-9 {
        if c > 0.0 {
            return None;
        }
        (0.0, t_max)
    } else {
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 {
            return None;
        }
        let s = disc.sqrt();
        ((-b - s) / (2.0 * a), (-b + s) / (2.0 * a))
    };
    let t0 = t_in.max(0.0);
    let t1 = t_out.min(t_max);
    if t0 > t1 {
        return None;
    }
    // z range while inside the infinite cylinder
    let z0 = rel[2] + vel[2] * t0;
    let z1 = rel[2] + vel[2] * t1;
    let (zl, zh) = if z0 < z1 { (z0, z1) } else { (z1, z0) };
    if zh < 0.0 || zl > h {
        return None;
    }
    Some(t0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intercept_stationary() {
        let t = intercept_time([100.0, 0.0, 0.0], [0.0; 3], 250.0).unwrap();
        assert!((t - 0.4).abs() < 1e-5);
    }

    #[test]
    fn intercept_crossing_target_meets() {
        let rel = [120.0, -30.0, 2.0];
        let vel = [-4.0, 11.0, 0.0];
        let speed = 252.0;
        let t = intercept_time(rel, vel, speed).unwrap();
        let p = add(rel, mul(vel, t));
        assert!((len(p) - speed * t).abs() < 1e-2, "bullet and target meet");
        assert!(t > 0.0 && t < 1.0);
    }

    #[test]
    fn intercept_impossible_when_target_faster() {
        assert!(intercept_time([100.0, 0.0, 0.0], [300.0, 0.0, 0.0], 250.0).is_none());
        // faster target coming towards us can still be met
        assert!(intercept_time([100.0, 0.0, 0.0], [-300.0, 0.0, 0.0], 250.0).is_some());
    }

    #[test]
    fn lead_point_hits_with_delay_and_offset() {
        let muzzle = [0.0, 0.0, 7.0];
        let target = [90.0, 40.0, 4.5];
        let vel = [-8.0, 9.0, 0.0];
        let (speed, delay, offset) = (252.0, 0.075, 3.0);
        let (p, t) = lead_point(muzzle, target, vel, speed, delay, offset).unwrap();
        // simulate: the bullet leaves `offset` out at time `delay`, flies `t`
        let dir = { let d = sub(p, muzzle); mul(d, 1.0 / len(d)) };
        let bullet = add(muzzle, mul(dir, offset + speed * t));
        let tgt = add(target, mul(vel, delay + t));
        assert!(dist(bullet, tgt) < 0.05, "miss by {}", dist(bullet, tgt));
    }

    #[test]
    fn mortar_low_and_high_arcs_land_on_target() {
        let (v, g) = (72.0, 22.0);
        for &(d, h) in &[(60.0f32, 0.0f32), (150.0, -5.0), (200.0, 12.0), (30.0, 20.0)] {
            for &high in &[false, true] {
                let p = mortar_pitch(d, h, v, g, high).unwrap();
                let t = mortar_time(d, v, p);
                let z = v * p.sin() * t - 0.5 * g * t * t;
                assert!((z - h).abs() < 0.05, "d={d} h={h} high={high}: z={z}");
            }
            let lo = mortar_pitch(d, h, v, g, false).unwrap();
            let hi = mortar_pitch(d, h, v, g, true).unwrap();
            assert!(hi > lo);
        }
        // beyond v^2/g = 235.6 m on flat ground: no solution
        assert!(mortar_pitch(240.0, 0.0, v, g, false).is_none());
        assert!(mortar_pitch(230.0, 0.0, v, g, false).is_some());
    }

    #[test]
    fn mortar_solve_accounts_for_barrel_offset() {
        let tube = [10.0, -5.0, 9.5];
        let target = [130.0, 60.0, 3.0];
        let (v, g) = (72.0, 22.0);
        for &high in &[false, true] {
            let s = mortar_solve(tube, target, v, g, high, 3.0, -0.7, 1.3).unwrap();
            let end = mortar_point(s.origin, s.aim, v, g, s.time);
            assert!(dist(end, target) < 0.2, "high={high}: lands {} m off", dist(end, target));
            assert!(dist(s.origin, tube) > 2.99 && dist(s.origin, tube) < 3.01);
        }
        // pitch limits respected: a very close high lob needs > 1.3 rad
        assert!(mortar_solve(tube, [20.0, -5.0, 9.5], v, g, true, 3.0, -0.7, 1.3).is_none());
    }

    #[test]
    fn laser_damage_scales_with_charge() {
        assert!((laser_damage(315.0, 0.35, 0.75, 0.0) - 110.25).abs() < 1e-3);
        assert!((laser_damage(315.0, 0.35, 0.75, 0.75) - 315.0).abs() < 1e-3);
        assert!((laser_damage(315.0, 0.35, 0.75, 2.0) - 315.0).abs() < 1e-3);
    }

    #[test]
    fn cylinder_hit_test() {
        // straight at the mech's middle
        assert!(hits_cylinder([100.0, 0.0, 4.0], [-250.0, 0.0, 0.0], 2.8, 9.0, 1.0).is_some());
        // passes 5 m to the side
        assert!(hits_cylinder([100.0, 5.0, 4.0], [-250.0, 0.0, 0.0], 2.8, 9.0, 1.0).is_none());
        // passes overhead
        assert!(hits_cylinder([100.0, 0.0, 12.0], [-250.0, 0.0, 0.0], 2.8, 9.0, 1.0).is_none());
        // too far to arrive in time
        assert!(hits_cylinder([100.0, 0.0, 4.0], [-250.0, 0.0, 0.0], 2.8, 9.0, 0.2).is_none());
    }
}
