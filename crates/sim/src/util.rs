//! Small vector helpers and a deterministic RNG (no external crates).
use std::f32::consts::PI;

pub type V3 = [f32; 3];
pub type V2 = [f32; 2];

#[inline]
pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
#[inline]
pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
#[inline]
pub fn scale(a: V3, s: f32) -> V3 {
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
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}
#[inline]
pub fn norm(a: V3) -> V3 {
    let l = len(a);
    if l > 1e-9 {
        scale(a, 1.0 / l)
    } else {
        [0.0; 3]
    }
}
#[inline]
pub fn up(a: V3, dz: f32) -> V3 {
    [a[0], a[1], a[2] + dz]
}
/// Wrap an angle to (-pi, pi].
#[inline]
pub fn wrap_pi(mut a: f32) -> f32 {
    if !a.is_finite() {
        return 0.0;
    }
    a %= 2.0 * PI;
    if a > PI {
        a -= 2.0 * PI;
    } else if a <= -PI {
        a += 2.0 * PI;
    }
    a
}
/// Direction vector for a yaw/pitch pair (protocol section 5).
#[inline]
pub fn aim_dir(yaw: f32, pitch: f32) -> V3 {
    [pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin()]
}
#[inline]
pub fn yaw_to(from: V3, to: V3) -> f32 {
    (to[1] - from[1]).atan2(to[0] - from[0])
}
#[inline]
pub fn pitch_to(from: V3, to: V3) -> f32 {
    (to[2] - from[2]).atan2(dist2d(from, to).max(1e-6))
}

/// xoshiro256** seeded through splitmix64.
#[derive(Clone, Debug)]
pub struct Rng {
    s: [u64; 4],
    spare: Option<f32>,
}

impl Rng {
    pub fn new(seed: u64) -> Rng {
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut s = [0u64; 4];
        for v in s.iter_mut() {
            z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            *v = x ^ (x >> 31);
        }
        Rng { s, spare: None }
    }
    pub fn next_u64(&mut self) -> u64 {
        let r = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        r
    }
    /// Uniform in [0, 1).
    pub fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    pub fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f32()
    }
    pub fn usize(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
    pub fn chance(&mut self, p: f32) -> bool {
        self.f32() < p
    }
    /// Standard normal (Box-Muller).
    pub fn gauss(&mut self) -> f32 {
        if let Some(v) = self.spare.take() {
            return v;
        }
        let u1 = (1.0 - self.f32()).max(1e-7);
        let u2 = self.f32();
        let r = (-2.0 * u1.ln()).sqrt();
        self.spare = Some(r * (2.0 * PI * u2).sin());
        r * (2.0 * PI * u2).cos()
    }
    /// Uniform point in the unit disc.
    pub fn disc(&mut self) -> V2 {
        let a = self.range(0.0, 2.0 * PI);
        let r = self.f32().sqrt();
        [r * a.cos(), r * a.sin()]
    }
    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.usize(i + 1);
            v.swap(i, j);
        }
    }
}

/// Round to 2 decimals for JSON output.
#[inline]
pub fn r2(x: f32) -> f64 {
    let v = (x as f64 * 100.0).round() / 100.0;
    if v == 0.0 {
        0.0
    } else {
        v
    }
}
/// Round to 3 decimals for JSON output.
#[inline]
pub fn r3(x: f32) -> f64 {
    let v = (x as f64 * 1000.0).round() / 1000.0;
    if v == 0.0 {
        0.0
    } else {
        v
    }
}
pub fn j3(p: V3) -> serde_json::Value {
    serde_json::json!([r2(p[0]), r2(p[1]), r2(p[2])])
}
