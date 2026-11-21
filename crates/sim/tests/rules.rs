//! Game rules on the synthetic map: weapons, damage, zone, cubes, loot, vision, drops, placement.
use serde_json::Value;
use sim::config::{Config, WKind, DEFAULT_CONFIG_JSON};
use sim::sim::{Cmd, Sim, SupplyDrop, Tgt};
use sim::testworld::flat_world;
use std::f32::consts::PI;
use std::sync::Arc;

fn cfg_with(f: impl Fn(&mut Value)) -> Arc<Config> {
    let mut v: Value = serde_json::from_str(DEFAULT_CONFIG_JSON).unwrap();
    v["weapons"]["gun"]["spread_deg"] = 0.0.into();
    f(&mut v);
    Arc::new(Config::from_value(v).unwrap())
}
fn new_sim() -> Sim {
    new_sim_with(|_| {})
}
fn new_sim_with(f: impl Fn(&mut Value)) -> Sim {
    let mut s = Sim::new(Arc::new(flat_world()), cfg_with(f), 7);
    s.set_zone_center([0.0, 0.0]);
    s
}
fn run(s: &mut Sim, ticks: usize) {
    for _ in 0..ticks {
        s.step();
    }
}
fn cmd(s: &mut Sim, i: usize, c: Cmd) {
    s.apply_cmds(vec![(i, c)]);
}
fn fire(slot: usize) -> Cmd {
    let mut c = Cmd::default();
    c.fire[slot] = true;
    c
}
/// Aim mech `i` exactly at the centre of mech `j` (instantly).
fn aim_at(s: &mut Sim, i: usize, j: usize, mortar: bool) {
    let (base, _, _) = s.muzzle(i, mortar);
    let c = s.mechs[j].center();
    let yaw = (c[1] - base[1]).atan2(c[0] - base[0]);
    let pitch = (c[2] - base[2]).atan2(((c[0] - base[0]).powi(2) + (c[1] - base[1]).powi(2)).sqrt());
    s.mechs[i].aim = [yaw, pitch];
    s.mechs[i].cmd_aim = [yaw, pitch];
}
fn count_sounds(s: &Sim, kind: &str) -> usize {
    s.sounds.iter().filter(|x| x.kind == kind).count()
}

#[test]
fn gun_bullet_travel_damage_and_rear_armor() {
    let mut s = new_sim();
    let a = s.add_mech([-60.0, -30.0, 0.0], 0.0);
    let b = s.add_mech([20.0, -30.0, 0.0], PI); // facing the shooter
    aim_at(&mut s, a, b, false);
    cmd(&mut s, a, fire(0));
    run(&mut s, 1);
    assert_eq!(s.projs.len(), 1);
    assert_eq!(s.mechs[a].weapons[0].unwrap().ammo, 11);
    cmd(&mut s, a, Cmd::default());
    // 80 m at 252 m/s = 0.32 s: not there after 0.25 s, there after 0.4 s
    run(&mut s, 4);
    assert_eq!(s.mechs[b].hp, 1400.0);
    run(&mut s, 3);
    assert_eq!(s.mechs[b].hp, 1400.0 - 45.0);
    assert!(s.projs.is_empty());
    // from behind: x1.5
    s.mechs[b].aim = [0.0, 0.0];
    s.mechs[b].cmd_aim = [0.0, 0.0];
    cmd(&mut s, a, fire(0));
    run(&mut s, 1);
    cmd(&mut s, a, Cmd::default());
    run(&mut s, 8);
    assert!((s.mechs[b].hp - (1400.0 - 45.0 - 67.5)).abs() < 1e-3, "hp {}", s.mechs[b].hp);
    assert!(s.mechs[a].dmg_dealt > 112.0);
}

#[test]
fn walls_stop_bullets_and_lasers() {
    let mut s = new_sim();
    let a = s.add_mech([0.0, 0.0, 0.0], 0.0);
    let b = s.add_mech([60.0, 0.0, 0.0], PI); // behind the building (x 20..40)
    s.give_weapon(a, WKind::Laser);
    aim_at(&mut s, a, b, false);
    cmd(&mut s, a, Cmd { fire: [true, true], ..Cmd::default() });
    run(&mut s, 20);
    cmd(&mut s, a, Cmd::default());
    run(&mut s, 10);
    assert_eq!(s.mechs[b].hp, 1400.0);
    assert!(count_sounds(&s, "laser") == 1 && count_sounds(&s, "gun") >= 4);
    let beam = &s.beams[0];
    assert!((beam.path[1][0] - 20.0).abs() < 0.1, "beam ends at the wall: {:?}", beam.path);
}

#[test]
fn gun_fire_rate_magazine_and_reload() {
    let mut s = new_sim();
    let a = s.add_mech([-60.0, -30.0, 0.0], 0.0);
    cmd(&mut s, a, fire(0));
    run(&mut s, 54); // 2.7 s: 12 shots need 11 * 0.24 = 2.64 s
    assert_eq!(count_sounds(&s, "gun"), 12);
    assert_eq!(count_sounds(&s, "reload"), 1);
    run(&mut s, 46); // 5.0 s: still reloading (2.4 s)
    assert_eq!(count_sounds(&s, "gun"), 12);
    run(&mut s, 56); // 7.8 s: second magazine gone
    assert_eq!(count_sounds(&s, "gun"), 24);
    // exactly one shot per cooldown on average
    let mut s = new_sim_with(|v| v["weapons"]["gun"]["mag"] = 1000.into());
    let a = s.add_mech([-60.0, -30.0, 0.0], 0.0);
    cmd(&mut s, a, fire(0));
    run(&mut s, 20 * 24);
    assert_eq!(count_sounds(&s, "gun"), 100);
}

#[test]
fn mortar_arc_splash_and_concussion() {
    let mut s = new_sim();
    let a = s.add_mech([-90.0, -30.0, 0.0], 0.0);
    s.give_weapon(a, WKind::Mortar);
    // flat shot from the tube (9.5 m up, 3 m out): lands where 9.5 = g t^2 / 2
    let ms = s.cfg.weapons.mortar;
    let t = (2.0 * 9.5 / ms.gravity).sqrt();
    let land = -90.0 + 3.0 + ms.speed * t;
    let b = s.add_mech([land + 3.0, -30.0, 0.0], PI);
    let far = s.add_mech([land + 3.0, -45.0, 0.0], PI);
    cmd(&mut s, a, fire(1));
    run(&mut s, 1);
    assert_eq!(s.projs.len(), 1);
    cmd(&mut s, a, Cmd::default());
    run(&mut s, 30);
    assert!(s.projs.is_empty());
    assert_eq!(count_sounds(&s, "explosion"), 1);
    let hp = s.mechs[b].hp;
    let d = 1400.0 - hp;
    assert!(d > 0.3 * ms.splash && d <= ms.splash + ms.damage + 1.0, "splash damage {d}");
    assert!(s.mechs[b].energy < 100.0, "concussion drains energy");
    assert!(s.mechs[b].kin.pos[0] > land + 3.5, "thrown away from the blast: {:?}", s.mechs[b].kin.pos);
    assert_eq!(s.mechs[far].hp, 1400.0);
    assert_eq!(s.mechs[a].hp, 1400.0);

    // own blast hurts at 50%: fire into a wall 5 m away
    let mut s = new_sim();
    let a = s.add_mech([15.0, 0.0, 0.0], 0.0);
    s.give_weapon(a, WKind::Mortar);
    cmd(&mut s, a, fire(1));
    run(&mut s, 3);
    let lost = 1400.0 - s.mechs[a].hp;
    let expect = ms.splash * (1.0 - (1.0 - ms.splash_edge) * 5.0 / ms.splash_radius) * 0.5;
    assert!((lost - expect).abs() < 6.0, "self splash {lost} vs {expect}");
    assert!(s.mechs[a].kin.vel[0] < -1.0 || s.mechs[a].kin.pos[0] < 15.0);
}

#[test]
fn laser_charge_scaling_cookoff_and_autofire() {
    let dmg_after = |hold_ticks: usize, target_mortar: bool| -> (f32, usize) {
        let mut s = new_sim();
        let a = s.add_mech([-60.0, -30.0, 0.0], 0.0);
        let b = s.add_mech([20.0, -30.0, 0.0], PI);
        s.mechs[a].weapons[0] = None;
        s.give_weapon(a, WKind::Laser);
        if target_mortar {
            s.give_weapon(b, WKind::Mortar);
        }
        aim_at(&mut s, a, b, false);
        cmd(&mut s, a, fire(0));
        run(&mut s, hold_ticks);
        let fired_while_held = count_sounds(&s, "laser");
        cmd(&mut s, a, Cmd::default());
        run(&mut s, 6);
        assert_eq!(count_sounds(&s, "laser"), 1);
        assert!(s.mechs[a].weapons[0].unwrap().cooldown > 1.0);
        (1400.0 - s.mechs[b].hp, fired_while_held)
    };
    let ls = Config::default_config().weapons.laser;
    // tap: fires as soon as min_charge is in
    let (d, _) = dmg_after(1, false);
    let frac = ls.min_charge / ls.charge;
    assert!((d - ls.damage * (ls.min_frac + (1.0 - ls.min_frac) * frac)).abs() < 1.0, "tap {d}");
    // full charge
    let (d, held) = dmg_after(20, false);
    assert!((d - ls.damage).abs() < 1e-2 && held == 0, "full {d}");
    // cook-off on a mortar carrier
    let (d, _) = dmg_after(20, true);
    assert!((d - ls.damage * (1.0 + ls.cookoff)).abs() < 1e-2, "cookoff {d}");
    // held too long: fires on its own after charge + hold_s
    let (d, held) = dmg_after(((ls.charge + ls.hold_s) * 20.0) as usize + 2, false);
    assert!(held == 1 && (d - ls.damage).abs() < 1e-2);
}

#[test]
fn lightning_chains_costs_energy_and_shorts_lasers() {
    let mut s = new_sim();
    let a = s.add_mech([-60.0, -30.0, 0.0], 0.0);
    let b = s.add_mech([-30.0, -30.0, 0.0], PI);
    let c = s.add_mech([-20.0, -30.0, 0.0], PI);
    let d = s.add_mech([-10.0, -30.0, 0.0], PI);
    let e = s.add_mech([0.0, -30.0, 0.0], PI);
    s.give_weapon(a, WKind::Lightning);
    s.give_weapon(b, WKind::Laser);
    // b is charging its laser
    cmd(&mut s, b, fire(1));
    run(&mut s, 2);
    assert!(s.mechs[b].charging());
    cmd(&mut s, a, fire(1));
    run(&mut s, 1);
    let ls = s.cfg.weapons.lightning;
    assert!((s.mechs[b].hp - (1400.0 - ls.damage)).abs() < 1e-3);
    assert!((s.mechs[c].hp - (1400.0 - ls.damage * ls.chain_falloff)).abs() < 1e-3);
    assert!((s.mechs[d].hp - (1400.0 - ls.damage * ls.chain_falloff * ls.chain_falloff)).abs() < 1e-3);
    assert_eq!(s.mechs[e].hp, 1400.0, "chain is 2 jumps");
    assert!((s.mechs[a].energy - (100.0 - ls.energy)).abs() < 1e-3);
    assert!(!s.mechs[b].charging() && s.mechs[b].short_until > s.t + 1.5);
    assert_eq!(s.beams.iter().find(|x| x.kind == "lightning").unwrap().path.len(), 4);
    // shorted laser cannot start charging
    cmd(&mut s, a, Cmd::default());
    cmd(&mut s, b, fire(1));
    run(&mut s, 10);
    assert!(!s.mechs[b].charging());
    // out of range: fires anyway, costs energy, hits nothing
    let mut s = new_sim();
    let a = s.add_mech([-60.0, -30.0, 0.0], 0.0);
    let b = s.add_mech([0.0, -30.0, 0.0], PI);
    s.give_weapon(a, WKind::Lightning);
    cmd(&mut s, a, fire(1));
    run(&mut s, 1);
    assert_eq!(s.mechs[b].hp, 1400.0);
    assert!(s.mechs[a].energy < 100.0);
}

#[test]
fn zone_schedule_damage_and_regen() {
    let mut s = new_sim();
    let z = s.zone_at(0.0);
    assert_eq!((z.radius, z.next_radius, z.shrink_start, z.shrink_end, z.dps), (560.0, 380.0, 60.0, 110.0, 20.0));
    assert!((s.zone_at(85.0).radius - 470.0).abs() < 1e-3);
    let z = s.zone_at(110.0);
    assert_eq!((z.radius, z.next_radius, z.shrink_start, z.shrink_end, z.dps), (380.0, 240.0, 155.0, 195.0, 35.0));
    assert!(s.zone_at(415.0).radius < 1e-3 && s.zone_at(450.0).dps == 260.0);
    for w in s.zone_circles.windows(2) {
        let d = ((w[0].0[0] - w[1].0[0]).powi(2) + (w[0].0[1] - w[1].0[1]).powi(2)).sqrt();
        assert!(d + w[1].1 <= w[0].1 + 1e-3, "next circle inside the current one");
    }
    // t = 300: phase 4 (dps 100), radius 140 -> 70 between 295 and 325
    let inside = s.add_mech([0.0, -30.0, 0.0], 0.0);
    let outside = s.add_mech([95.0, -95.0, 0.0], 0.0);
    s.tick = 300 * 20;
    s.t = 300.0;
    s.mechs[inside].hp = 1000.0;
    s.mechs[inside].last_combat_t = 300.0;
    run(&mut s, 20);
    assert!((s.mechs[outside].hp - 1300.0).abs() < 1.0, "zone dps: {}", s.mechs[outside].hp);
    assert!(!s.mechs[outside].in_zone && s.mechs[inside].in_zone);
    assert_eq!(s.mechs[inside].hp, 1000.0, "no regen for regen_delay after damage");
    run(&mut s, 80 + 20);
    assert!((s.mechs[inside].hp - 1045.0).abs() < 3.0, "regen 45/s after 5 s: {}", s.mechs[inside].hp);
}

#[test]
fn cubes_upgrades_and_offers() {
    let mut s = new_sim();
    let a = s.add_mech([-60.0, -30.0, 0.0], 0.0);
    s.mechs[a].rec = true;
    s.mechs[a].hp = 1000.0;
    s.mechs[a].last_combat_t = 1e9; // no regen in this test
    s.add_cube([-58.0, -30.0, 0.0]);
    s.add_cube([-40.0, -30.0, 0.0]); // out of reach
    run(&mut s, 1);
    assert_eq!(s.mechs[a].cubes, 1);
    assert_eq!(s.cubes.len(), 1);
    assert!((s.mechs[a].max_hp - 1456.0).abs() < 1e-2 && (s.mechs[a].hp - 1056.0).abs() < 1e-2);
    assert!((s.mechs[a].st.radius - 2.625).abs() < 1e-4 && (s.mechs[a].st.height - 9.45).abs() < 1e-4);
    assert!((s.dmg_mult(a) - 1.04).abs() < 1e-5);
    let offer = s.mechs[a].offer.clone().expect("offer at 1 cube");
    assert_eq!(offer.choices.len(), 3);
    assert!(offer.choices.iter().all(|&u| s.cfg.upgrades[u].requires.as_deref().map_or(true, |r| r == "gun")));
    let picked = s.cfg.upgrades[offer.choices[1]].id.clone();
    cmd(&mut s, a, Cmd { pick: Some(1), ..Cmd::default() });
    assert_eq!(s.mechs[a].upgrades, vec![picked.clone()]);
    assert!(s.mechs[a].offer.is_none());
    // 3 cubes: next offer; no answer -> choice 0 after offer_timeout_s
    s.add_cube([-60.0, -30.0, 0.0]);
    s.add_cube([-60.0, -30.0, 0.0]);
    run(&mut s, 2);
    let offer = s.mechs[a].offer.clone().expect("offer at 3 cubes");
    let first = s.cfg.upgrades[offer.choices[0]].id.clone();
    run(&mut s, 101);
    assert_eq!(s.mechs[a].upgrades, vec![picked, first]);
    let obs = s.observe(a);
    let kinds: Vec<&str> = obs["events"].as_array().unwrap().iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(kinds.iter().filter(|k| **k == "pickup").count(), 3);
    assert_eq!(kinds.iter().filter(|k| **k == "upgrade").count(), 2);
    // a specific upgrade: plating = +15% max hp
    let plating = s.cfg.upgrades.iter().position(|u| u.id == "plating").unwrap();
    let before = s.mechs[a].max_hp;
    s.apply_upgrade(a, plating);
    assert!((s.mechs[a].max_hp / before - 1.15).abs() < 1e-3);
}

#[test]
fn weapon_pickups_take_swap_and_contest() {
    let mut s = new_sim();
    let a = s.add_mech([-60.0, -30.0, 0.0], 0.0);
    let b = s.add_mech([-54.0, -30.0, 0.0], PI);
    s.mechs[a].rec = true;
    s.mechs[b].rec = true;
    let near = s.add_pickup(WKind::Laser, [-58.0, -30.0, 0.0]); // 2 m from a, 4 m from b
    let far = s.add_pickup(WKind::Mortar, [-40.0, -30.0, 0.0]);
    s.apply_cmds(vec![(a, Cmd { take: Some(near), ..Cmd::default() }), (b, Cmd { take: Some(near), ..Cmd::default() })]);
    let w = s.mechs[a].weapons[1].expect("closer mech got it");
    assert_eq!(w.kind, WKind::Laser);
    assert!((w.cooldown - s.cfg.weapons.laser.cooldown).abs() < 1e-4, "cannot fire for max(equip_s, cooldown)");
    assert!(s.mechs[b].weapons[1].is_none());
    assert_eq!(s.mechs[b].events[0]["reason"], "gone");
    cmd(&mut s, a, Cmd { take: Some(far), ..Cmd::default() });
    assert_eq!(s.mechs[a].events.last().unwrap()["reason"], "range");
    // both slots full: needs `drop`
    let third = s.add_pickup(WKind::Lightning, [-60.0, -29.0, 0.0]);
    cmd(&mut s, a, Cmd { take: Some(third), ..Cmd::default() });
    assert_eq!(s.mechs[a].events.last().unwrap()["reason"], "full");
    cmd(&mut s, a, Cmd { take: Some(third), drop: Some(0), ..Cmd::default() });
    assert_eq!(s.mechs[a].weapons[0].unwrap().kind, WKind::Lightning);
    let dropped = s.pickups.iter().find(|p| p.w.kind == WKind::Gun).expect("gun on the ground");
    assert_eq!(dropped.w.ammo, 12);
    // cannot hold two of a kind
    let dup = s.add_pickup(WKind::Laser, [-60.0, -30.0, 0.0]);
    cmd(&mut s, a, Cmd { take: Some(dup), drop: Some(0), ..Cmd::default() });
    assert_eq!(s.mechs[a].events.last().unwrap()["reason"], "have");
    // plain drop
    cmd(&mut s, a, Cmd { drop: Some(1), ..Cmd::default() });
    assert!(s.mechs[a].weapons[1].is_none());
    assert_eq!(s.mechs[a].events.last().unwrap()["type"], "dropped");
}

#[test]
fn vision_wedge_near_range_walls_and_memory() {
    let mut s = new_sim();
    let a = s.add_mech([-90.0, -30.0, 0.0], 0.0); // looks east
    let front = s.add_mech([60.0, -30.0, 0.0], PI); // 150 m ahead
    let behind_near = s.add_mech([-90.0, 10.0, 0.0], 0.0); // 40 m to the side/behind
    let side_far = s.add_mech([-90.0, 70.0, 0.0], 0.0); // 100 m at 90 degrees: outside the wedge
    let hidden = s.add_mech([50.0, 0.0, 0.0], 0.0); // behind the building from (0, 0)
    let viewer2 = s.add_mech([0.0, 0.0, 0.0], 0.0);
    s.update_vision();
    assert!(s.sees(a, front) && s.sees(a, behind_near) && !s.sees(a, side_far));
    assert!(!s.sees(viewer2, hidden), "buildings block sight");
    // turn the torso: the far one ahead is lost from view but remembered for memory_s
    s.mechs[a].aim = [PI / 2.0, 0.0];
    s.mechs[a].cmd_aim = [PI / 2.0, 0.0];
    run(&mut s, 20);
    s.update_vision();
    assert!(s.sees(a, side_far));
    assert!(s.sees(a, front) && !s.sees_now(a, front), "still tracked 1 s later");
    run(&mut s, 100);
    s.update_vision();
    assert!(!s.sees(a, front), "forgotten after memory_s");
    // observation lists exactly the tracked ones
    let obs = s.observe(a);
    let ids: Vec<u64> = obs["mechs"].as_array().unwrap().iter().map(|m| m["id"].as_u64().unwrap()).collect();
    assert_eq!(ids, vec![behind_near as u64, side_far as u64]);
    assert_eq!(obs["type"], "obs");
    assert_eq!(obs["self"]["weapons"][0]["kind"], "gun");
    assert!(obs["self"]["weapons"][1].is_null());
    for k in ["tick", "t", "alive", "self", "mechs", "projectiles", "crates", "cubes", "pickups", "sounds", "beams", "events", "zone", "offer", "drops"] {
        assert!(obs.get(k).is_some(), "obs.{k} missing");
    }
}

#[test]
fn sounds_are_heard_within_their_range() {
    let mut s = new_sim();
    let a = s.add_mech([-90.0, -30.0, 0.0], 0.0);
    let b = s.add_mech([90.0, -30.0, 0.0], PI); // 180 m: hears a gun (320) but not a step (40)
    cmd(&mut s, a, Cmd { mv: [0.0, 1.0], fire: [true, false], ..Cmd::default() });
    run(&mut s, 2);
    s.update_vision();
    let obs = s.observe(b);
    let sounds = obs["sounds"].as_array().unwrap();
    assert!(sounds.iter().any(|x| x["kind"] == "gun" && x["dist"] == "far"));
    assert!(!sounds.iter().any(|x| x["kind"] == "step"));
    let bearing = sounds[0]["bearing"].as_f64().unwrap().abs();
    assert!((bearing - std::f64::consts::PI).abs() < 0.6, "bearing roughly west: {bearing}");
    // own sounds are not reported
    let own = s.observe(a);
    assert!(own["sounds"].as_array().unwrap().is_empty());
}

#[test]
fn supply_drop_capture_alone() {
    let mut s = new_sim();
    let a = s.add_mech([-60.0, -30.0, 0.0], 0.0);
    let b = s.add_mech([-56.0, -30.0, 0.0], PI);
    s.mechs[a].hp = 500.0;
    s.mechs[a].last_combat_t = 1e9;
    s.drops.push(SupplyDrop { id: 20000, pos: [-60.0, -30.0, 0.0], lands_at: 0.0, landed: true, holder: None, progress: 0.0 });
    run(&mut s, 100);
    assert_eq!(s.mechs[a].cubes, 0, "two mechs in the ring: frozen");
    s.mechs[b].kin.pos = [-30.0, -30.0, 0.0];
    run(&mut s, 70);
    assert_eq!(s.mechs[a].cubes, 0, "needs capture_s alone");
    run(&mut s, 20);
    assert_eq!(s.mechs[a].cubes, s.cfg.drops.cubes);
    assert_eq!(s.mechs[a].hp, s.mechs[a].max_hp);
    assert!(s.drops.is_empty());
}

#[test]
fn deaths_kills_loot_and_placement() {
    let mut s = new_sim();
    let a = s.add_mech([-60.0, -30.0, 0.0], 0.0);
    let b = s.add_mech([-20.0, -30.0, 0.0], PI);
    let c = s.add_mech([60.0, 60.0, 0.0], 0.0);
    let d = s.add_mech([-60.0, 60.0, 0.0], 0.0);
    s.give_weapon(b, WKind::Mortar);
    s.mechs[b].hp = 40.0;
    s.gain_cubes(b, 2);
    let hp_b = s.mechs[b].hp;
    assert!(hp_b > 40.0);
    s.mechs[b].hp = 40.0;
    aim_at(&mut s, a, b, false);
    cmd(&mut s, a, fire(0));
    run(&mut s, 6);
    assert!(!s.mechs[b].alive);
    assert_eq!(s.mechs[b].place, 4);
    assert_eq!(s.mechs[a].kills, 1);
    let death = s.mechs[b].death.clone().unwrap();
    assert_eq!((death.cause, death.by), ("gun", Some(a as i32)));
    assert_eq!(s.cubes.len(), 2 + s.cfg.cubes.death_drop_extra as usize);
    assert!(s.pickups.iter().any(|p| p.w.kind == WKind::Mortar) && !s.pickups.iter().any(|p| p.w.kind == WKind::Gun));
    assert_eq!(s.alive_count, 3);
    assert!(!s.over());
    // two die on the same tick: they share a place; the last one standing wins
    s.mechs[c].hp = 0.0;
    s.mechs[d].hp = 0.0;
    run(&mut s, 1);
    assert_eq!((s.mechs[c].place, s.mechs[d].place), (2, 2));
    assert!(s.over());
    assert_eq!(s.mechs[a].place, 1);
}

#[test]
fn drones_fight_back_and_drop_cubes() {
    let mut s = new_sim();
    let a = s.add_mech([-60.0, -30.0, 0.0], 0.0);
    let k = s.add_npc([-30.0, -30.0, 0.0]);
    run(&mut s, 60);
    assert_eq!(s.npcs[k].target, Some(a), "aggro inside 45 m with line of sight");
    assert!(s.mechs[a].hp < 1400.0, "drone bullets hurt");
    let hp = s.npcs[k].hp;
    s.hurt(Tgt::Npc(k), hp + 1.0, "gun", Some(a as i32), [0.0; 3], None);
    assert!(!s.npcs[k].alive);
    assert_eq!(s.cubes.len(), s.cfg.npc.cubes as usize);
    // crates: 1 cube, sometimes a weapon
    let c = s.add_crate([-60.0, -20.0, 0.0]);
    s.hurt(Tgt::Crate(c), 301.0, "gun", Some(a as i32), [0.0; 3], None);
    assert_eq!(s.cubes.len(), s.cfg.npc.cubes as usize + 1);
}

#[test]
fn clock_ranks_survivors_by_hp() {
    let mut s = new_sim_with(|v| v["match"]["duration_s"] = 1.into());
    let a = s.add_mech([-60.0, -30.0, 0.0], 0.0);
    let b = s.add_mech([60.0, -30.0, 0.0], PI);
    let c = s.add_mech([60.0, 60.0, 0.0], PI);
    s.mechs[a].hp = 700.0;
    s.mechs[c].hp = 900.0;
    s.mechs[a].last_combat_t = 1e9;
    s.mechs[c].last_combat_t = 1e9;
    run(&mut s, 20);
    assert!(s.over());
    assert_eq!((s.mechs[b].place, s.mechs[c].place, s.mechs[a].place), (1, 2, 3));
}
