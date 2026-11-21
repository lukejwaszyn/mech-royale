//! A small synthetic map for unit tests: flat ground at z = 0 (x, y in -100..100) with a
//! few solids and two terrain ramps.
//!
//! * building: x 20..40, y -10..10, top 20
//! * low ledge (can be stepped onto): x -20..-10, y -5..5, top 1.0
//! * tall ledge (a wall): x -20..-10, y 20..30, top 2.0
//! * bridge (walk under, stops a jet): x -5..5, y 40..60, base 10, top 12
//! * low overhang (a wall): x -5..5, y -60..-40, base 5, top 7
//! * steep terrain ramp (60 degrees, cannot be walked up): x 60..70 for y > 0, plateau behind it
//! * gentle terrain ramp (30 degrees): y -70..-80 for x < 0, plateau behind it
use world::World;

pub fn flat_world() -> World {
    let (nx, ny) = (201usize, 201usize);
    let mut terrain = Vec::with_capacity(nx * ny * 2);
    for j in 0..ny {
        for i in 0..nx {
            let x = -100.0 + i as f32;
            let y = -100.0 + j as f32;
            let mut h = 0.0f32;
            if y > 0.0 && x > 60.0 {
                h = ((x - 60.0) * 60f32.to_radians().tan()).min(17.3);
            }
            if x < 0.0 && y < -70.0 {
                h = ((-70.0 - y) * 30f32.to_radians().tan()).min(5.77);
            }
            terrain.extend_from_slice(&((h * 100.0).round() as i16).to_le_bytes());
        }
    }
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| vec![x0, y0, x1, y0, x1, y1, x0, y1];
    let json = serde_json::json!({
        "bounds": {"minX": -100.0, "maxX": 100.0, "minY": -100.0, "maxY": 100.0},
        "terrain": {"minX": -100.0, "minY": -100.0, "step": 1.0, "nx": nx, "ny": ny},
        "prisms": [
            {"kind": "building", "base": -5.0, "top": 20.0, "ring": rect(20.0, -10.0, 40.0, 10.0)},
            {"kind": "building", "base": -5.0, "top": 1.0, "ring": rect(-20.0, -5.0, -10.0, 5.0)},
            {"kind": "building", "base": -5.0, "top": 2.0, "ring": rect(-20.0, 20.0, -10.0, 30.0)},
            {"kind": "bridge", "base": 10.0, "top": 12.0, "ring": rect(-5.0, 40.0, 5.0, 60.0)},
            {"kind": "bridge", "base": 5.0, "top": 7.0, "ring": rect(-5.0, -60.0, 5.0, -40.0)}
        ]
    });
    World::from_bytes(json.to_string().as_bytes(), &terrain).expect("synthetic world")
}
