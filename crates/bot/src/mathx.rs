//! Small vector and angle helpers (f32, protocol frame: x east, y north, z up).
use crate::proto::{V2, V3};
use std::f32::consts::PI;

#[inline]
pub fn wrap(a: f32) -> f32 {
    let mut a = a % (2.0 * PI);
    if a > PI {
        a -= 2.0 * PI;
    } else if a < -PI {
        a += 2.0 * PI;
    }
    a
}
#[inline]
pub fn ang_diff(a: f32, b: f32) -> f32 {
    wrap(a - b)
}
#[inline]
pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
#[inline]
pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
#[inline]
pub fn mul(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
#[inline]
pub fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
#[inline]
pub fn len(a: V3) -> f32 {
    dot(a, a).sqrt()
}
#[inline]
pub fn dist(a: V3, b: V3) -> f32 {
    len(sub(a, b))
}
#[inline]
pub fn dist2d(a: V3, b: V3) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}
#[inline]
pub fn xy(a: V3) -> V2 {
    [a[0], a[1]]
}
#[inline]
pub fn len2(a: V2) -> f32 {
    a[0].hypot(a[1])
}
#[inline]
pub fn norm2(a: V2) -> V2 {
    let l = len2(a);
    if l > 1e-6 { [a[0] / l, a[1] / l] } else { [0.0, 0.0] }
}
#[inline]
pub fn dot2(a: V2, b: V2) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}
#[inline]
pub fn perp(a: V2) -> V2 {
    [-a[1], a[0]]
}
/// Unit direction of a `[yaw, pitch]` aim.
#[inline]
pub fn dir_of(yaw: f32, pitch: f32) -> V3 {
    [pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin()]
}
/// `[yaw, pitch]` that points from `from` at `to`.
#[inline]
pub fn aim_at(from: V3, to: V3) -> V2 {
    let d = sub(to, from);
    [d[1].atan2(d[0]), d[2].atan2(d[0].hypot(d[1]))]
}
#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Deterministic xorshift RNG seeded from `hello.seed`.
#[derive(Clone, Debug)]
pub struct Rng(pub u64);
impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// Uniform in [0, 1).
    pub fn f(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    pub fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
    pub fn sign(&mut self) -> f32 {
        if self.next_u64() & 1 == 0 { 1.0 } else { -1.0 }
    }
}
