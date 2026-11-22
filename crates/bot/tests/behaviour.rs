//! Behaviour regression tests: each one replays a small synthetic situation through the
//! real `Session` (hello with the recorded rule set, hand-written observations) and
//! checks what the bot does or remembers. They run on the flat stand-in world the bot
//! uses when the map cannot be read, so every sight line is clear and nothing depends
//! on campus geometry.
//!
//! Every test here pins down a bug that the local simulator exposed.
use bot::runner::Session;
use bot::Tuning;
use serde_json::{json, Value};

const ZONE_C: [f32; 2] = [41.3, -72.8];

fn session(tune: Tuning) -> Session {
    let text = include_str!("fixtures/stream.jsonl");
    let mut hello: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    hello["map"]["dir"] = json!("/nonexistent/arena");
    let mut s = Session::new(tune);
    assert!(s.hello(&hello.to_string()).contains("ready"));
    assert!(!s.bot.as_ref().unwrap().map_ok, "these tests run on the flat world");
    s
}

fn gun() -> Value {
    json!({"kind": "gun", "cooldown": 0.0, "ammo": 12, "reload": 0.0})
}
fn laser() -> Value {
    json!({"kind": "laser", "cooldown": 0.0, "charge": 0.0})
}

/// One observation. `k` counts decisions (0.1 s each); `extra` overrides top-level fields.
fn obs(k: u32, pos: [f32; 3], aim: [f32; 2], weapons: Value, extra: Value) -> String {
    let mut o = json!({
        "type": "obs", "tick": 2 * k, "t": k as f32 * 0.1, "alive": 16,
        "self": {"id": 3, "pos": pos, "vel": [0.0, 0.0, 0.0], "aim": aim, "hp": 1400, "max_hp": 1400, "energy": 100, "max_energy": 100,
                 "grounded": true, "cubes": 0, "upgrades": [], "in_zone": true, "weapons": weapons},
        "mechs": [], "projectiles": [], "crates": [], "cubes": [], "pickups": [], "sounds": [], "beams": [], "events": [], "drops": [],
        "zone": {"center": ZONE_C, "radius": 560.0, "next_center": ZONE_C, "next_radius": 380.0, "shrink_start": 60.0, "shrink_end": 110.0, "dps": 20.0},
        "offer": null
    });
    for (key, v) in extra.as_object().unwrap() {
        o[key] = v.clone();
    }
    o.to_string()
}

fn step(s: &mut Session, line: &str) -> Value {
    let a = s.step(&[line]).expect("an action");
    serde_json::from_str(&a.to_line()).unwrap()
}

fn mech(id: i64, pos: [f32; 3], vel: [f32; 3], aim: [f32; 2], weapons: Value) -> Value {
    json!({"id": id, "kind": "mech", "pos": pos, "vel": vel, "aim": aim, "hp": 1400, "max_hp": 1400, "cubes": 0, "weapons": weapons, "charging": false})
}

const HERE: [f32; 3] = [41.3, -72.8, 0.0];

/// A remembered cube that is no longer listed although it lies in plain view close by is
/// forgotten after a moment - and then not believed again from far away, or the bot
/// would walk to it and away from it for ever.
#[test]
fn forgets_loot_it_should_see_and_distrusts_its_ghost() {
    let mut s = session(Tuning::default());
    let cube = json!([{"id": 5001, "pos": [HERE[0] + 20.0, HERE[1], 0.0]}]);
    step(&mut s, &obs(0, HERE, [0.0, 0.0], json!([gun(), null]), json!({"cubes": cube})));
    assert_eq!(s.bot.as_ref().unwrap().model.cubes.len(), 1);
    // gone from the list: still remembered for a moment, then forgotten
    step(&mut s, &obs(1, HERE, [0.0, 0.0], json!([gun(), null]), json!({})));
    assert_eq!(s.bot.as_ref().unwrap().model.cubes.len(), 1, "one missing observation is not proof");
    for k in 2..12 {
        step(&mut s, &obs(k, HERE, [0.0, 0.0], json!([gun(), null]), json!({})));
    }
    assert!(s.bot.as_ref().unwrap().model.cubes.is_empty(), "a cube missing from plain view is forgotten");
    // listed again while we are far away: a ghost, not believed
    let far = [HERE[0] - 60.0, HERE[1], 0.0];
    step(&mut s, &obs(12, far, [0.0, 0.0], json!([gun(), null]), json!({"cubes": cube})));
    assert!(s.bot.as_ref().unwrap().model.cubes.is_empty(), "the ghost is not believed from afar");
    // listed while we stand next to it: believed
    let near = [HERE[0] + 8.0, HERE[1], 0.0];
    step(&mut s, &obs(13, near, [0.0, 0.0], json!([gun(), null]), json!({"cubes": cube})));
    assert_eq!(s.bot.as_ref().unwrap().model.cubes.len(), 1);
}

/// A cube that is merely out of sight (far away, behind us) stays remembered.
#[test]
fn keeps_loot_it_cannot_see() {
    let mut s = session(Tuning::default());
    let cube = json!([{"id": 5002, "pos": [HERE[0] + 120.0, HERE[1], 0.0]}]);
    step(&mut s, &obs(0, HERE, [0.0, 0.0], json!([gun(), null]), json!({"cubes": cube})));
    for k in 1..30 {
        step(&mut s, &obs(k, HERE, [3.0, 0.0], json!([gun(), null]), json!({})));
    }
    assert_eq!(s.bot.as_ref().unwrap().model.cubes.len(), 1);
}

/// A drone that drops out of the observations while the server would still track it
/// has been destroyed: no farming task may wait for it.
#[test]
fn forgets_a_destroyed_drone() {
    let mut s = session(Tuning::default());
    let drone = json!([{"id": 104, "kind": "npc", "pos": [HERE[0] + 30.0, HERE[1], 0.0], "vel": [0, 0, 0], "aim": [3.1, 0.0], "hp": 350, "max_hp": 350}]);
    step(&mut s, &obs(0, HERE, [0.0, 0.0], json!([gun(), null]), json!({"mechs": drone})));
    assert!(s.bot.as_ref().unwrap().model.track(104).is_some());
    step(&mut s, &obs(1, HERE, [0.0, 0.0], json!([gun(), null]), json!({})));
    assert!(s.bot.as_ref().unwrap().model.track(104).is_none());
}

/// A drone that shoots at us is shot back at (it used to be ignored, and drones killed
/// a bot that was hiding from mechs).
#[test]
fn shoots_back_at_a_drone() {
    let mut s = session(Tuning::default());
    let drone = json!([{"id": 104, "kind": "npc", "pos": [HERE[0] + 30.0, HERE[1], 0.0], "vel": [0, 0, 0], "aim": [3.1, 0.0], "hp": 350, "max_hp": 350}]);
    let hit = json!([{"type": "damaged", "amount": 25, "weapon": "npc", "bearing": 0.0, "by": 104}]);
    let mut fired = false;
    for k in 0..8 {
        let a = step(&mut s, &obs(k, HERE, [0.0, -0.1], json!([gun(), null]), json!({"mechs": drone, "events": hit})));
        fired |= a["fire"][0] == json!(true);
    }
    assert!(fired, "the gun answers a drone");
}

/// Whether a cube outside the next circle is worth fetching is judged at the cube, not
/// at where we stand: the answer must be the same on both sides of the next circle's
/// edge (it used to flip there, and the bot paced up and down across the line).
#[test]
fn loot_choice_is_the_same_on_both_sides_of_the_next_circle() {
    let cube = json!([{"id": 5003, "pos": [ZONE_C[0] + 400.0, ZONE_C[1], 0.0]}]);
    for x in [365.0f32, 372.0, 379.0, 386.0, 393.0] {
        let mut s = session(Tuning::default());
        let pos = [ZONE_C[0] + x, ZONE_C[1], 0.0];
        let mut a = Value::Null;
        for k in 50..54 {
            a = step(&mut s, &obs(k, pos, [0.0, 0.0], json!([gun(), null]), json!({"cubes": cube})));
        }
        let mx = a["move"][0].as_f64().unwrap();
        assert!(mx > 0.5, "at {x} m from the centre the bot walks to the cube (move x = {mx})");
    }
}

/// ... and the same cube is left alone once the closing circle is about to pass it.
#[test]
fn leaves_loot_the_zone_is_about_to_take() {
    let mut s = session(Tuning::default());
    let cube = json!([{"id": 5004, "pos": [ZONE_C[0] + 400.0, ZONE_C[1], 0.0]}]);
    let pos = [ZONE_C[0] + 372.0, ZONE_C[1], 0.0];
    // t = 100 s: the circle (560 -> 380 between 60 s and 110 s) passes 400 m at about 104 s
    let mut a = Value::Null;
    for k in 1000..1004 {
        a = step(&mut s, &obs(k, pos, [0.0, 0.0], json!([gun(), null]), json!({"cubes": cube})));
    }
    assert!(a["move"][0].as_f64().unwrap() < 0.3, "no trip outwards with the zone four seconds away");
}

/// The laser keeps charging while we back away from an enemy inside our preferred
/// range (it used to be switched off there, which cost half our damage).
#[test]
fn charges_the_laser_while_kiting() {
    let mut s = session(Tuning::default());
    let foe = [HERE[0] + 100.0, HERE[1], 0.0];
    let hit = json!([{"type": "damaged", "amount": 45, "weapon": "gun", "bearing": 0.0, "by": 5}]);
    let mut charging = false;
    let mut backing = false;
    for k in 0..6 {
        let a = step(&mut s, &obs(k, HERE, [0.0, -0.03], json!([gun(), laser()]), json!({"mechs": [mech(5, foe, [0.0, 0.0, 0.0], [3.14, 0.0], json!(["gun"]))], "events": hit})));
        charging |= a["fire"][1] == json!(true);
        backing |= a["move"][0].as_f64().unwrap() < -0.2;
    }
    assert!(charging, "the laser trigger is held");
    assert!(backing, "and we back away to our range");
}

/// Next to a lightning carrier the laser waits: there we must run, not stand and charge.
#[test]
fn runs_from_a_lightning_arc() {
    let mut s = session(Tuning::default());
    let foe = [HERE[0] + 38.0, HERE[1], 0.0];
    let mut away = false;
    let mut boost = false;
    for k in 0..5 {
        let a = step(&mut s, &obs(k, HERE, [0.0, 0.0], json!([gun(), null]), json!({"mechs": [mech(6, foe, [0.0, 0.0, 0.0], [3.14, 0.0], json!(["gun", "lightning"]))]})));
        away |= a["move"][0].as_f64().unwrap() < -0.5;
        boost |= a["boost"] == json!(true);
    }
    assert!(away && boost, "boosts out of the arc (away {away}, boost {boost})");
}

/// Practice mode: a mech walking up to us is shot before it opens fire. The
/// free-for-all preset does not start that fight.
#[test]
fn first_strike_only_in_practice_mode() {
    let foe = [HERE[0] + 100.0, HERE[1], 0.0];
    let run = |tune: Tuning| {
        let mut s = session(tune);
        let mut fired = false;
        for k in 0..10 {
            let a = step(&mut s, &obs(k, HERE, [0.0, -0.03], json!([gun(), laser()]), json!({"mechs": [mech(7, foe, [-9.0, 0.0, 0.0], [3.14, 0.0], json!(["gun"]))]})));
            fired |= a["fire"][0] == json!(true) || a["fire"][1] == json!(true);
        }
        fired
    };
    assert!(run(Tuning::default()), "practice: opens fire on the approaching mech");
    let mut ffa = Tuning::default();
    ffa.apply_ffa();
    assert!(!run(ffa), "free-for-all: leaves a healthy mech that is not attacking alone");
}

/// A mech in plain sight that one more burst will finish is always shot at.
#[test]
fn takes_the_kill_shot() {
    let mut s = session(Tuning::default());
    let foe = [HERE[0] + 90.0, HERE[1], 0.0];
    let mut weak = mech(8, foe, [0.0, 0.0, 0.0], [0.0, 0.0], json!(["gun"]));
    weak["hp"] = json!(120);
    // a second mech next to it: the weak one is not "isolated", and we are badly hurt
    let other = mech(9, [foe[0], foe[1] + 30.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0], json!(["gun"]));
    let mut fired = false;
    for k in 0..8 {
        let mut o: Value = serde_json::from_str(&obs(k, HERE, [0.0, -0.03], json!([gun(), null]), json!({"mechs": [weak.clone(), other.clone()]}))).unwrap();
        o["self"]["hp"] = json!(300);
        let a = step(&mut s, &o.to_string());
        fired |= a["fire"][0] == json!(true);
    }
    assert!(fired);
}

/// Standing in practice mode with low HP and one attacker in the open: the bot fights
/// (no cover to reach on the flat world, and running only gives free shots).
#[test]
fn does_not_run_from_a_single_attacker_when_hurt() {
    let mut s = session(Tuning::default());
    let foe = [HERE[0] + 150.0, HERE[1], 0.0];
    let hit = json!([{"type": "damaged", "amount": 45, "weapon": "gun", "bearing": 0.0, "by": 5}]);
    let mut fired = false;
    for k in 0..8 {
        let mut o: Value = serde_json::from_str(&obs(k, HERE, [0.0, -0.02], json!([gun(), null]), json!({"mechs": [mech(5, foe, [0.0, 0.0, 0.0], [3.14, 0.0], json!(["gun"]))], "events": hit}))).unwrap();
        o["self"]["hp"] = json!(400);
        let a = step(&mut s, &o.to_string());
        fired |= a["fire"][0] == json!(true);
    }
    assert!(fired);
    assert!(!s.bot.as_ref().unwrap().retreating);
}

/// Shot in the back by a mech we have never seen while shooting at another one: the
/// bot turns to the bearing of the hits (it used to keep its back to the attacker until
/// it died, every hit a rear hit), and when the attacker comes into view it counts as
/// an attacker from the first moment.
#[test]
fn turns_to_an_attacker_it_cannot_see() {
    let mut s = session(Tuning::default());
    // a bystander ahead (east) that we are shooting at: it walks at us
    let ahead = |k: u32| json!([mech(7, [HERE[0] + 100.0 - k as f32 * 0.5, HERE[1], 0.0], [-5.0, 0.0, 0.0], [3.14, 0.0], json!(["gun"]))]);
    let mut aim = [0.0f32, 0.0];
    for k in 0..10 {
        let a = step(&mut s, &obs(k, HERE, aim, json!([gun(), null]), json!({"mechs": ahead(k)})));
        aim = [a["aim"][0].as_f64().unwrap() as f32, a["aim"][1].as_f64().unwrap() as f32];
    }
    assert!(aim[0].abs() < 0.3, "aiming at the mech ahead, got yaw {}", aim[0]);
    // hits from due west (bearing pi) by mech 9, which is not listed
    let hit = json!([{"type": "damaged", "amount": 68, "weapon": "gun", "bearing": 3.1, "by": 9, "rear": true}]);
    let mut yaw = aim[0];
    for k in 10..22 {
        let a = step(&mut s, &obs(k, HERE, [yaw, 0.0], json!([gun(), null]), json!({"mechs": ahead(k), "events": hit})));
        let want = a["aim"][0].as_f64().unwrap() as f32;
        // the torso slews at 4 rad/s
        let d = (want - yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        yaw += d.clamp(-0.4, 0.4);
    }
    let off = (yaw - 3.1 + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    assert!(off.abs() < 0.4, "the torso turned to the unseen attacker, yaw {yaw}");
    // now it is in view: an attacker at once, and the target
    let both = json!([
        mech(7, [HERE[0] + 89.0, HERE[1], 0.0], [-5.0, 0.0, 0.0], [3.14, 0.0], json!(["gun"])),
        mech(9, [HERE[0] - 70.0, HERE[1], 0.0], [0.0, 0.0, 0.0], [0.0, 0.0], json!(["gun"]))
    ]);
    step(&mut s, &obs(22, HERE, [yaw, 0.0], json!([gun(), null]), json!({"mechs": both})));
    let bot = s.bot.as_ref().unwrap();
    assert!(bot.model.track(9).unwrap().hit_us_t > 2.0, "the new track carries the hits it dealt unseen");
    assert_eq!(bot.target, Some(9), "the mech that is hitting us is the target");
}

/// Idling just inside the rim of the next circle: the same decision at 3 m and at 8 m
/// inside it. (A ring 5-11 m inside the rim used to call for a walk to the holding
/// spot while the last 5 m did not; where the way to the holding spot led outwards the
/// bot paced across that line for the rest of the phase.)
#[test]
fn idling_at_the_rim_of_the_next_circle_is_steady() {
    let mode_at = |inside: f32| {
        let mut s = session(Tuning::default());
        let pos = [ZONE_C[0] + 380.0 - inside, ZONE_C[1], 0.0];
        for k in 0..3 {
            step(&mut s, &obs(k, pos, [0.0, 0.0], json!([gun(), laser()]), json!({})));
        }
        format!("{:?}", s.bot.as_ref().unwrap().mode)
    };
    let (near, far) = (mode_at(3.0), mode_at(8.0));
    assert_eq!(near, far, "3 m and 8 m inside the rim");
    assert_ne!(far, "Zone", "inside the next circle with a minute to go is not a zone errand");
}

/// A session on a small flat map with one building (x 20..60, y -30..-1), for the
/// tests that need a wall.
fn session_with_building(tune: Tuning) -> Session {
    use std::sync::Arc;
    let text = include_str!("fixtures/stream.jsonl");
    let hello_v: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    let hello: bot::proto::Hello = serde_json::from_value(hello_v).unwrap();
    let map = json!({
        "bounds": {"minX": -300.0, "maxX": 300.0, "minY": -300.0, "maxY": 300.0},
        "terrain": {"minX": -310.0, "minY": -310.0, "step": 620.0, "nx": 2, "ny": 2},
        "prisms": [{"kind": "building", "base": -5.0, "top": 30.0, "ring": [20.0, -30.0, 60.0, -30.0, 60.0, -1.0, 20.0, -1.0]}]
    });
    let world = Arc::new(world::World::from_bytes(map.to_string().as_bytes(), &[0u8; 8]).unwrap());
    let c = &hello.config.mech;
    let nav = world::Nav::build(world.clone(), world::NavParams { radius: c.radius, height: c.height, step_up: c.step_up, max_slope_deg: c.max_slope_deg, ..world::NavParams::default() });
    let mut s = Session::new(tune.clone());
    s.bot = Some(bot::brain::Bot::with_parts(&hello, tune, world, vec![(0.0, nav)], true));
    s
}

/// Jinking in a fight never steps to where the target is out of our line of fire: with
/// a building to one side the bot strafes the other way. (It used to step behind the
/// corner, walk out again for the line of fire, and so on - on one roof edge for more
/// than a minute without landing a shot.)
#[test]
fn strafing_keeps_the_line_of_fire() {
    let run = |tune: Tuning| -> f32 {
        let mut s = session_with_building(tune);
        // the target far to the east shoots at us; south of us the building's corner
        // hides it, north is open
        let here = [0.0, 0.0, 0.0];
        let mut lowest = 0.0f32;
        for k in 0..80 {
            let foe = json!([mech(7, [160.0, 0.0, 0.0], [0.0, 0.0, 0.0], [3.14, 0.0], json!(["gun"]))]);
            let ev = if k % 3 == 0 { json!([{"type": "damaged", "amount": 45, "weapon": "gun", "bearing": 0.0, "by": 7}]) } else { json!([]) };
            let o = obs(k, here, [0.0, 0.0], json!([gun(), laser()]), json!({"mechs": foe, "events": ev, "zone": {"center": [0.0, 0.0], "radius": 560.0, "next_center": [0.0, 0.0], "next_radius": 380.0, "shrink_start": 60.0, "shrink_end": 110.0, "dps": 20.0}}));
            let a = step(&mut s, &o);
            lowest = lowest.min(a["move"][1].as_f64().unwrap() as f32);
        }
        lowest
    };
    let mut off = Tuning::default();
    off.los_keep = 0.0;
    assert!(run(off) < -0.5, "without the rule the jink goes behind the corner too (the test bites)");
    assert!(run(Tuning::default()) > -0.3, "with it, no step south into the building's shadow");
}

/// With the circle about to pass us, a lightning carrier standing inside it does not
/// keep the bot from walking in. (Every step inwards used to cost more than standing
/// still, and the bot stood at the rim until the zone killed it.)
#[test]
fn walks_into_the_zone_past_a_lightning_carrier() {
    let east = |tune: Tuning| -> f64 {
        let mut s = session(tune);
        // a small circle to the east; we stand 20 m outside its rim, the current circle is
        // closing on it and reaches us in a few seconds; a lightning carrier stands between
        let zone = json!({"center": [100.0, 0.0], "radius": 135.0, "next_center": [100.0, 0.0], "next_radius": 70.0, "shrink_start": 0.0, "shrink_end": 30.0, "dps": 100.0});
        let pos = [-20.0, 0.0, 0.0];
        let foe = json!([mech(7, [25.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.5, 0.0], json!(["gun", "lightning"]))]);
        let mut a = json!(null);
        for k in 0..4 {
            a = step(&mut s, &obs(k, pos, [0.0, 0.0], json!([gun(), laser()]), json!({"mechs": foe, "zone": zone})));
        }
        assert_eq!(format!("{:?}", s.bot.as_ref().unwrap().mode), "Zone");
        a["move"][0].as_f64().unwrap()
    };
    let mut never = Tuning::default();
    never.zone_force_s = -1.0;
    assert!(east(never) < 0.5, "without the rule the arc holds the bot back (the test bites)");
    assert!(east(Tuning::default()) > 0.5, "walking east into the circle");
}
