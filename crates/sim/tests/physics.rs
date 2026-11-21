//! Core movement physics on a synthetic map (see `sim::testworld`).
use sim::config::Config;
use sim::physics::{step_kin, Kin, MoveIn, MoveParams};
use sim::testworld::flat_world;
use world::World;

const DT: f32 = 0.05;

fn params() -> MoveParams {
    MoveParams::from_stats(&Config::default_config().mech)
}
fn walk(w: &World, k: &mut Kin, p: &MoveParams, mv: [f32; 2], boost: bool, jump: bool, e: &mut f32, ticks: usize) -> (bool, bool) {
    let (mut wall, mut blocked) = (false, false);
    for n in 0..ticks {
        let out = step_kin(w, k, p, &MoveIn { mv, boost, jump: jump && n == 0, speed_mult: 1.0 }, e, DT);
        wall |= out.wall;
        blocked |= out.blocked;
        assert!(!w.mech_blocked(k.pos, &world::Body { step_up: if k.grounded { p.step_up } else { 0.0 }, ..p.body() }) || !k.grounded, "ended inside geometry at {:?}", k.pos);
    }
    (wall, blocked)
}
fn at(x: f32, y: f32) -> Kin {
    Kin { pos: [x, y, 0.0], vel: [0.0; 3], grounded: true }
}

#[test]
fn accelerates_to_walk_speed() {
    let (w, p) = (flat_world(), params());
    let mut k = at(0.0, 0.0);
    let mut e = 100.0;
    walk(&w, &mut k, &p, [1.0, 0.0], false, false, &mut e, 2);
    assert!((k.vel[0] - p.ground_accel * 0.1).abs() < 1e-3, "after 0.1 s: {}", k.vel[0]);
    walk(&w, &mut k, &p, [1.0, 0.0], false, false, &mut e, 10);
    assert!((k.vel[0] - p.walk_speed).abs() < 1e-3);
    assert_eq!(e, 100.0);
    // half stick = half speed
    walk(&w, &mut k, &p, [0.5, 0.0], false, false, &mut e, 10);
    assert!((k.vel[0] - p.walk_speed * 0.5).abs() < 1e-3);
    // stopping takes time too
    walk(&w, &mut k, &p, [0.0, 0.0], false, false, &mut e, 1);
    assert!(k.vel[0] > 1.0);
    walk(&w, &mut k, &p, [0.0, 0.0], false, false, &mut e, 5);
    assert_eq!(k.vel[0], 0.0);
}

#[test]
fn boost_runs_faster_and_drains() {
    let (w, p) = (flat_world(), params());
    let mut k = at(-50.0, -20.0);
    let mut e = 100.0;
    walk(&w, &mut k, &p, [0.0, 1.0], true, false, &mut e, 20);
    assert!((k.vel[1] - p.boost_speed).abs() < 1e-3);
    assert!((e - (100.0 - p.boost_drain * 1.0)).abs() < 0.01, "energy {e}");
    // no energy: back to walking speed
    e = 0.0;
    walk(&w, &mut k, &p, [0.0, 1.0], true, false, &mut e, 20);
    assert!((k.vel[1] - p.walk_speed).abs() < 1e-3);
}

#[test]
fn walls_stop_and_slide() {
    let (w, p) = (flat_world(), params());
    // straight into the building's west wall (x = 20)
    let mut k = at(10.0, 0.0);
    let mut e = 100.0;
    let (wall, _) = walk(&w, &mut k, &p, [1.0, 0.0], false, false, &mut e, 40);
    assert!(wall);
    assert!((k.pos[0] - (20.0 - p.radius)).abs() < 0.02, "x = {}", k.pos[0]);
    assert!(k.pos[1].abs() < 1e-3);
    // diagonally: stopped in x, keeps moving in y (slides), then walks around the corner
    let mut k = at(10.0, 0.0);
    walk(&w, &mut k, &p, [0.7071, 0.7071], false, false, &mut e, 20);
    assert!(k.pos[0] <= 20.0 - p.radius + 0.02);
    assert!(k.pos[1] > 5.0, "slid to y = {}", k.pos[1]);
    walk(&w, &mut k, &p, [0.7071, 0.7071], false, false, &mut e, 60);
    assert!(k.pos[0] > 25.0, "got around the corner: {:?}", k.pos);
}

#[test]
fn map_edge_is_a_wall() {
    let (w, p) = (flat_world(), params());
    let mut k = at(-90.0, 50.0);
    let mut e = 100.0;
    walk(&w, &mut k, &p, [-1.0, 0.2], false, false, &mut e, 60);
    assert!((k.pos[0] - (-100.0 + p.radius)).abs() < 0.01);
    assert!(k.pos[1] > 55.0);
}

#[test]
fn step_up_ledges() {
    let (w, p) = (flat_world(), params());
    let mut e = 100.0;
    // 1.0 m ledge: walked up
    let mut k = at(-30.0, 0.0);
    walk(&w, &mut k, &p, [1.0, 0.0], false, false, &mut e, 30);
    assert!((k.pos[2] - 1.0).abs() < 1e-3 && k.pos[0] > -20.0 && k.grounded, "{:?}", k.pos);
    // and back down
    walk(&w, &mut k, &p, [1.0, 0.0], false, false, &mut e, 40);
    assert!(k.pos[2].abs() < 1e-3 && k.grounded, "{:?}", k);
    // 2.0 m ledge: a wall
    let mut k = at(-30.0, 25.0);
    walk(&w, &mut k, &p, [1.0, 0.0], false, false, &mut e, 40);
    assert!(k.pos[0] <= -20.0 - p.radius + 0.02 && k.pos[2] == 0.0);
    // ... unless you jump and jet over the edge
    let mut k = at(-26.0, 25.0);
    walk(&w, &mut k, &p, [0.0, 0.0], false, true, &mut e, 1);
    walk(&w, &mut k, &p, [1.0, 0.0], true, false, &mut e, 8);
    walk(&w, &mut k, &p, [1.0, 0.0], false, false, &mut e, 30);
    assert!(k.grounded && (k.pos[2] - 2.0).abs() < 1e-3 || k.pos[0] > -10.0, "{:?}", k);
}

#[test]
fn slopes() {
    let (w, p) = (flat_world(), params());
    let mut e = 100.0;
    // 60 degree terrain ramp: cannot be walked up
    let mut k = at(50.0, 50.0);
    walk(&w, &mut k, &p, [1.0, 0.0], false, false, &mut e, 60);
    assert!(k.pos[0] < 61.5 && k.pos[2] < 2.0, "{:?}", k.pos);
    // 30 degree ramp: fine
    let mut k = at(-50.0, -60.0);
    walk(&w, &mut k, &p, [0.0, -1.0], false, false, &mut e, 60);
    assert!(k.pos[1] < -80.0 && (k.pos[2] - 5.77).abs() < 0.05 && k.grounded, "{:?}", k.pos);
}

#[test]
fn jump_and_jet() {
    let (w, p) = (flat_world(), params());
    let mut e = 100.0;
    let mut k = at(-50.0, 0.0);
    let mut apex = 0.0f32;
    let mut air = 0;
    walk(&w, &mut k, &p, [0.0, 0.0], false, true, &mut e, 1);
    assert!(!k.grounded);
    while !k.grounded && air < 100 {
        walk(&w, &mut k, &p, [0.0, 0.0], false, false, &mut e, 1);
        apex = apex.max(k.pos[2]);
        air += 1;
    }
    let ideal = p.jump_speed * p.jump_speed / (2.0 * p.gravity);
    assert!((apex - ideal).abs() < 0.4, "apex {apex} vs {ideal}");
    assert!(k.grounded && k.pos[2] == 0.0 && e == 100.0);

    // full-energy climb: the protocol says about 60 m
    walk(&w, &mut k, &p, [0.0, 0.0], false, true, &mut e, 1);
    let mut top = 0.0f32;
    for _ in 0..400 {
        walk(&w, &mut k, &p, [0.0, 0.0], true, false, &mut e, 1);
        top = top.max(k.pos[2]);
        assert!(k.vel[2] <= p.jump_speed + 1e-3);
        if k.grounded {
            break;
        }
    }
    assert!(top > 50.0 && top < 75.0, "climbed {top}");
    assert!(e < 1.0);
}

#[test]
fn bridges_and_overhangs() {
    let (w, p) = (flat_world(), params());
    let mut e = 100.0;
    // high bridge (underside at 10): walk under it
    let mut k = at(-20.0, 50.0);
    walk(&w, &mut k, &p, [1.0, 0.0], false, false, &mut e, 60);
    assert!(k.pos[0] > 10.0 && k.pos[2] == 0.0);
    // and its underside stops a jet
    let mut k = at(0.0, 50.0);
    walk(&w, &mut k, &p, [0.0, 0.0], false, true, &mut e, 1);
    let mut top = 0.0f32;
    for _ in 0..40 {
        walk(&w, &mut k, &p, [0.0, 0.0], true, false, &mut e, 1);
        top = top.max(k.pos[2]);
    }
    assert!((top - (10.0 - p.height)).abs() < 0.05, "head stopped at {top}");
    // low overhang (underside at 5): a wall
    let mut k = at(-20.0, -50.0);
    walk(&w, &mut k, &p, [1.0, 0.0], false, false, &mut e, 60);
    assert!(k.pos[0] <= -5.0 - p.radius + 0.02);
}

#[test]
fn roof_landing_and_walking_off() {
    let (w, p) = (flat_world(), params());
    let mut e = 100.0;
    // dropped onto the middle of the roof: lands on it
    let mut k = Kin { pos: [30.0, 0.0, 30.0], vel: [0.0; 3], grounded: false };
    walk(&w, &mut k, &p, [0.0, 0.0], false, false, &mut e, 40);
    assert!(k.grounded && (k.pos[2] - 20.0).abs() < 1e-3);
    // walk off the west edge: falls clear of the wall and lands on the ground
    walk(&w, &mut k, &p, [-1.0, 0.0], false, false, &mut e, 50);
    assert!(k.grounded && k.pos[2].abs() < 1e-3 && k.pos[0] <= 20.0 - p.radius + 0.02, "{:?}", k);
    // coming down past the edge while overhanging it: caught by the edge, then slid off
    let mut k = Kin { pos: [18.5, 0.0, 24.0], vel: [0.0; 3], grounded: false };
    walk(&w, &mut k, &p, [0.0, 0.0], false, false, &mut e, 10);
    assert!(k.pos[2] >= 20.0 - 1e-3 || k.pos[0] <= 20.0 - p.radius + 0.02, "never inside the wall: {:?}", k);
    walk(&w, &mut k, &p, [0.0, 0.0], false, false, &mut e, 60);
    assert!(k.grounded && k.pos[2].abs() < 1e-3);
    // ... but keep moving toward the roof and you step onto it
    let mut k = Kin { pos: [18.5, 0.0, 24.0], vel: [0.0; 3], grounded: false };
    walk(&w, &mut k, &p, [1.0, 0.0], false, false, &mut e, 40);
    assert!(k.grounded && (k.pos[2] - 20.0).abs() < 1e-3 && k.pos[0] > 22.0, "{:?}", k);
}
