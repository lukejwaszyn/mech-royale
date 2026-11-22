//! Tests on the real map (repo `arena/`, or `$MECH_ARENA`).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use world::*;

fn arena() -> PathBuf {
    std::env::var_os("MECH_ARENA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../arena")))
}

fn world() -> Arc<World> {
    static W: OnceLock<Arc<World>> = OnceLock::new();
    W.get_or_init(|| Arc::new(World::load(&arena()).expect("load"))).clone()
}

// Nav holds a RefCell, so each test thread builds its own.
fn nav() -> Nav {
    Nav::build(world(), NavParams::default())
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
}

/// Timing limits are generous: this machine swaps and tests run in parallel.
const SLACK: f64 = if cfg!(debug_assertions) { 6.0 } else { 3.0 };

#[test]
fn loads_with_expected_content() {
    let t = Instant::now();
    let w = World::load(&arena()).unwrap();
    let dt = t.elapsed().as_secs_f64();
    assert!(dt < 2.0, "load took {dt}s");
    assert_eq!(w.prisms.len(), 259);
    assert_eq!(w.trees.len(), 1560);
    assert_eq!(w.prisms.iter().filter(|p| p.kind == PrismKind::Bridge).count(), 9);
    assert_eq!(w.prisms.iter().filter(|p| p.kind == PrismKind::Stadium).count(), 27);
    assert_eq!((w.terrain.nx, w.terrain.ny), (1070, 724));
    assert!((w.bounds.min_x + 512.38).abs() < 1e-3 && (w.bounds.max_y - 388.64).abs() < 1e-3);
    assert!((w.canopy_density - 0.35).abs() < 1e-6 && (w.canopy_block_t - 0.25).abs() < 1e-6);
    assert!(w.prisms.iter().any(|p| !p.holes.is_empty()));
    // also accepts the v2 directory itself
    assert!(World::load(&arena().join("v2")).is_ok());
}

#[test]
fn terrain_is_bilinear_on_vertices() {
    let w = world();
    let t = &w.terrain;
    // vertex values are exact
    for &(i, j) in &[(0usize, 0usize), (10, 20), (1069, 723), (500, 300)] {
        let x = t.min_x + i as f32 * t.step;
        let y = t.min_y + j as f32 * t.step;
        assert!((w.terrain_height(x, y) - t.h[j * t.nx + i]).abs() < 1e-4);
    }
    // midpoint of a cell = mean of the 4 corners
    let (i, j) = (400usize, 250usize);
    let m = (t.h[j * t.nx + i] + t.h[j * t.nx + i + 1] + t.h[(j + 1) * t.nx + i] + t.h[(j + 1) * t.nx + i + 1]) / 4.0;
    assert!((w.terrain_height(t.min_x + i as f32 + 0.5, t.min_y + j as f32 + 0.5) - m).abs() < 1e-4);
}

/// Positions recorded from a real match (kit check/stream.jsonl.gz): [x, y, z, grounded(1/0/-1 unknown), is_npc].
fn observed() -> Vec<[f32; 5]> {
    let s = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/observed.json")).unwrap();
    let v: Vec<Vec<f32>> = s.trim().trim_start_matches("[[").trim_end_matches("]]").split("],[").map(|r| r.split(',').map(|x| x.parse().unwrap()).collect()).collect();
    v.into_iter().map(|r| [r[0], r[1], r[2], r[3], r[4]]).collect()
}

#[test]
fn recorded_match_positions_agree_with_the_model() {
    let w = world();
    let obs = observed();
    assert!(obs.len() > 400);
    let body = Body { radius: 2.45, ..Body::default() };
    let mut grounded = 0;
    for o in &obs {
        let p = [o[0], o[1], o[2]];
        // nobody is ever inside solid
        assert!(!w.point_solid([p[0], p[1], p[2] + 0.05]), "observed position inside solid: {p:?}");
        if o[3] == 1.0 {
            grounded += 1;
            // a grounded mech stands exactly on the floor we compute
            let f = w.floor_at(p[0], p[1], p[2] + 0.02, 0.0).expect("floor");
            assert!((f.z - p[2]).abs() < 0.03, "floor {} vs observed {:?}", f.z, p);
            assert!(!w.mech_blocked(p, &body), "observed grounded mech is blocked: {p:?}");
        } else if o[4] == 0.0 {
            assert!(!w.mech_blocked(p, &body), "observed mech is blocked: {p:?}");
        }
    }
    assert!(grounded > 200, "grounded samples: {grounded}");
}

#[test]
fn building_blocks_movement_and_shots() {
    let w = world();
    // (-110, -194) is inside the first building prism (base -5.64, top 5.82).
    let (x, y) = (-110.0f32, -194.0f32);
    let mut tops = vec![];
    w.prisms_at(x, y, |_, p| tops.push((p.kind, p.base, p.top)));
    assert!(tops.iter().any(|t| t.0 == PrismKind::Building && (t.2 - 5.82).abs() < 0.01), "{tops:?}");
    let g = w.terrain_height(x, y);
    assert!(w.point_solid([x, y, g + 1.0]));
    assert!(w.mech_blocked([x, y, g], &Body::default()));
    assert!(w.floor_at(x, y, g, 1.2).is_none(), "ground level inside a building is solid");
    // standing on the roof works
    let roof = w.top_surface(x, y);
    assert!((roof.z - 5.82).abs() < 0.01 && roof.layer != LAYER_TERRAIN);
    assert_eq!(w.floor_at(x, y, roof.z, 1.2).unwrap().z, roof.z);
    assert!(!w.mech_blocked([x, y, roof.z], &Body::default()));
    assert_eq!(w.clearance(x, y, g, 9.0, 1.2, 5.0), 0.0);

    // a shot through the building is stopped at its north wall; over the roof it passes
    let a = [-110.0, -170.0, w.terrain_height(-110.0, -170.0) + 7.0];
    let b = [-110.0, -235.0, w.terrain_height(-110.0, -235.0) + 4.5];
    assert!(!w.los(a, b));
    let hit = w.raycast(a, b).expect("hit");
    assert!(matches!(hit.kind, HitKind::Prism(_)));
    assert!(hit.pos[1] < -185.0 && hit.pos[1] > -192.0, "hit at {:?}", hit.pos);
    assert!(w.los([a[0], a[1], 40.0], [b[0], b[1], 40.0]));
    // a level shot at chest height is stopped by the north wall (at y = -189.2 for x = -110)
    let z = w.terrain_height(-110.0, -180.0) + 3.0;
    let hit2 = w.raycast([-110.0, -175.0, z], [-110.0, -200.0, z]).expect("wall hit");
    assert!((hit2.pos[1] + 189.2).abs() < 0.3, "wall hit at {:?}", hit2.pos);
    let wall_y = hit2.pos[1];
    // just outside the wall a mech is blocked, a few metres away it is not
    assert!(w.mech_blocked([-110.0, wall_y + 2.0, w.terrain_height(-110.0, wall_y + 2.0)], &Body::default()));
    let free = [-110.0, wall_y + 3.2, w.terrain_height(-110.0, wall_y + 3.2)];
    assert!(!w.mech_blocked(free, &Body::default()));
    let c = w.clearance(free[0], free[1], free[2], 9.0, 1.2, 10.0);
    assert!(c > 2.5 && c <= 3.2, "clearance {c}");
    // walking into the wall is stopped, walking along it is fine
    assert!(w.walk_clear([-110.0, -175.0, w.terrain_height(-110.0, -175.0)], [-110.0, -200.0], &Body::default(), 1.2).is_none());
}

#[test]
fn bridge_walk_under_and_over() {
    let w = world();
    let nav = nav();
    let body = Body::default();
    let mut high = 0;
    for (pi, p) in w.prisms.iter().enumerate() {
        if p.kind != PrismKind::Bridge {
            continue;
        }
        // a point inside the footprint: centroid of the ring (bridges are convex enough), else skip
        let n = p.ring.len() as f32;
        let cx = p.ring.iter().map(|q| q[0]).sum::<f32>() / n;
        let cy = p.ring.iter().map(|q| q[1]).sum::<f32>() / n;
        if !p.contains_xy(cx, cy) {
            continue;
        }
        let g = w.terrain_height(cx, cy);
        // on the deck: the floor is the deck
        let f = w.floor_at(cx, cy, p.top, body.step_up).unwrap();
        assert_eq!(f.layer, pi as u32 + 1);
        assert!((f.z - p.top).abs() < 1e-4);
        if p.base - g >= body.height + 0.05 {
            high += 1;
            // under the deck: the floor is the terrain and (if no building is near) a mech fits
            let f = w.floor_at(cx, cy, g, body.step_up).unwrap();
            assert_eq!(f.layer, LAYER_TERRAIN);
            assert_eq!(w.ceiling_at(cx, cy, g), Some(p.base));
            // shots pass under the bridge along its axis-free direction and are stopped by the deck from above
            assert!(w.los([cx, cy, g + 1.0], [cx, cy, p.base - 0.5]));
            assert!(!w.los([cx, cy, g + 1.0], [cx, cy, p.top + 5.0]));
            let hit = w.raycast([cx, cy, p.top + 5.0], [cx, cy, g + 1.0]).unwrap();
            assert!((hit.pos[2] - p.top).abs() < 0.05);
            if w.clearance(cx, cy, g, body.height, body.step_up, 6.0) > 3.2 {
                assert!(!w.mech_blocked([cx, cy, g], &body));
                // the nav grid has a ground node here, in the main component, and a deck node above it
                let (i, j) = nav.cell_of(cx, cy);
                let mut found_ground = false;
                for di in -1..=1 {
                    for dj in -1..=1 {
                        for u in nav.cell_nodes(i + di, j + dj) {
                            if nav.node_layer(u) == LAYER_TERRAIN && nav.is_main(u) {
                                found_ground = true;
                            }
                        }
                    }
                }
                assert!(found_ground, "no walkable ground under bridge {pi}");
            }
        } else {
            // a low bridge is a wall from below
            assert!(w.mech_blocked([cx, cy, g], &body));
        }
    }
    assert!(high >= 3, "expected several bridges high enough to walk under, got {high}");

    // The bridge at (-116, -33) spans a street between two buildings: a path across the
    // street axis goes straight under it.
    let from = [-114.0, -5.0, w.terrain_height(-114.0, -5.0)];
    let to = [-114.0, -80.0, w.terrain_height(-114.0, -80.0)];
    let p = nav.find_path(from, to).unwrap();
    let direct = (from[0] - to[0]).hypot(from[1] - to[1]);
    assert!(p.goal_error < 0.1 && p.length < direct * 1.15, "path {} vs direct {} err {}", p.length, direct, p.goal_error);
}

#[test]
fn grid_raycast_matches_brute_force() {
    let w = world();
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let b = w.bounds;
    let mut hits = 0;
    for k in 0..3000 {
        let ax = rng.range(b.min_x, b.max_x);
        let ay = rng.range(b.min_y, b.max_y);
        let len = if k % 3 == 0 { 30.0 } else { 250.0 };
        let bx = (ax + rng.range(-len, len)).clamp(b.min_x, b.max_x);
        let by = (ay + rng.range(-len, len)).clamp(b.min_y, b.max_y);
        let a = [ax, ay, w.top_surface(ax, ay).z + rng.range(0.5, 12.0)];
        let c = [bx, by, w.top_surface(bx, by).z + rng.range(0.5, 12.0)];
        let fast = w.raycast(a, c);
        let brute = w.raycast_brute(a, c);
        match (fast, brute) {
            (None, None) => {}
            (Some(h), Some(t)) => {
                hits += 1;
                let l = ((c[0] - a[0]).powi(2) + (c[1] - a[1]).powi(2) + (c[2] - a[2]).powi(2)).sqrt();
                assert!((h.t - t).abs() * l < 1.2, "ray {a:?}->{c:?}: fast {} brute {}", h.t, t);
                assert_eq!(w.los(a, c), false);
            }
            (f, br) => {
                // terrain grazing can differ between 1 m marching and the 0.1 m reference; prisms must agree
                let prism_fast = matches!(f, Some(Hit { kind: HitKind::Prism(_), .. }));
                assert!(!prism_fast && br.map_or(true, |_| true), "ray {a:?}->{c:?}: fast {f:?} brute {br:?}");
                if let (None, Some(t)) = (f, br) {
                    // the reference hit must be a shallow terrain graze
                    let p = [a[0] + (c[0] - a[0]) * t, a[1] + (c[1] - a[1]) * t, a[2] + (c[2] - a[2]) * t];
                    assert!(w.terrain_height(p[0], p[1]) - p[2] < 0.5, "missed solid hit {a:?}->{c:?} at {p:?}");
                    let mut inside = false;
                    w.prisms_at(p[0], p[1], |_, pr| inside |= p[2] > pr.base && p[2] < pr.top);
                    assert!(!inside, "missed prism hit {a:?}->{c:?}");
                }
            }
        }
    }
    assert!(hits > 500, "too few hits to be a meaningful test: {hits}");
}

#[test]
fn trees_block_sight_not_shots() {
    let w = world();
    let mut trunk_checked = 0;
    let mut crown_checked = 0;
    for t in &w.trees {
        // horizontal line through the trunk, 1 m above the ground
        let z = t.z + 1.0;
        let a = [t.x - 12.0, t.y, z];
        let b = [t.x + 12.0, t.y, z];
        if w.los(a, b) && trunk_checked < 200 && t.crown_base > 1.5 {
            trunk_checked += 1;
            assert!(!w.los_vision(a, b, false), "trunk should block sight");
            assert!(!w.los_vision(a, b, true));
            let (trunk, _) = w.foliage(a, b);
            assert!(trunk);
        }
        // line through the crown centre: a wide crown blocks, shots still pass
        let cz = t.z + (t.height + t.crown_base) * 0.5;
        let a = [t.crown_x - t.crown_r - 3.0, t.crown_y, cz];
        let b = [t.crown_x + t.crown_r + 3.0, t.crown_y, cz];
        if t.crown_r > 4.0 && w.los(a, b) && crown_checked < 200 {
            let (trunk, crown) = w.foliage(a, b);
            if !trunk {
                crown_checked += 1;
                assert!(crown >= 2.0 * t.crown_r - 0.01, "crown chord {crown} for r {}", t.crown_r);
                assert!(!w.los_vision(a, b, true), "dense crown should block sight");
                assert!(w.los_vision(a, b, false), "near-range sight ignores canopy");
            }
        }
    }
    assert!(trunk_checked > 50 && crown_checked > 20, "{trunk_checked} {crown_checked}");

    // can_see: range, wedge and near range
    let vp = VisionParams::default();
    // find an open spot: nothing within 60 m east
    let nav = nav();
    let mut rng = Rng(77);
    let mut done = false;
    for _ in 0..5000 {
        let u = (rng.next() % nav.node_count() as u64) as u32;
        let p = nav.node_pos(u);
        let eye = [p[0], p[1], p[2] + 8.0];
        let tgt = [p[0] + 100.0, p[1], w.terrain_height(p[0] + 100.0, p[1])];
        let near = [p[0] - 30.0, p[1], w.terrain_height(p[0] - 30.0, p[1])];
        if !w.bounds.contains(tgt[0], tgt[1], 5.0) || !w.bounds.contains(near[0], near[1], 5.0) {
            continue;
        }
        if w.los_vision(eye, [tgt[0], tgt[1], tgt[2] + 4.5], true) && w.los_vision(eye, [near[0], near[1], near[2] + 4.5], false) {
            assert!(w.can_see(eye, 0.0, tgt, 9.0, &vp), "facing east sees a target 100 m east");
            assert!(!w.can_see(eye, std::f32::consts::PI, tgt, 9.0, &vp), "facing west does not");
            assert!(w.can_see(eye, 0.0, near, 9.0, &vp), "near range sees behind");
            let far = [p[0] + 100.0, p[1], tgt[2]];
            let vp2 = VisionParams { range: 80.0, ..vp };
            assert!(!w.can_see(eye, 0.0, far, 9.0, &vp2), "beyond range");
            done = true;
            break;
        }
    }
    assert!(done);
}

#[test]
fn nav_grid_structure() {
    let w = world();
    let t = Instant::now();
    let nav = nav();
    let dt = t.elapsed().as_secs_f64();
    assert!(dt < 1.0 * SLACK, "nav build took {dt}s");
    let n = nav.node_count();
    let main = nav.comp_size(nav.main_comp()) as usize;
    assert!(n > 150_000 && main > 110_000, "{n} nodes, main {main}");
    // main component covers most of the open ground
    let terrain_nodes = (0..n as u32).filter(|&u| nav.node_layer(u) == LAYER_TERRAIN).count();
    assert!(main as f32 > 0.9 * terrain_nodes as f32, "main {main} of {terrain_nodes} terrain nodes");
    // links are symmetric and every node respects the clearance
    let body = Body::default();
    let mut rng = Rng(5);
    for _ in 0..20000 {
        let u = (rng.next() % n as u64) as u32;
        let p = nav.node_pos(u);
        assert!(!w.mech_blocked(p, &Body { radius: body.radius + 0.55, ..body }), "node {u} at {p:?} is blocked");
        assert!(nav.node_clearance(u) >= 3.1 - 1e-3);
        let f = w.floor_at(p[0], p[1], p[2] + 0.01, 0.0).expect("node floor");
        // (the layer may differ when two surfaces coincide within a centimetre)
        assert!((f.z - p[2]).abs() < 0.02, "node {u} at {p:?} layer {} vs floor {f:?}", nav.node_layer(u));
        for (d, &v) in nav.neighbors(u).iter().enumerate() {
            if v != NO_NODE {
                assert_eq!(nav.neighbors(v)[(d + 4) % 8], u);
                assert_eq!(nav.comp_of(u), nav.comp_of(v));
                let (i, j) = nav.node_ij(u);
                assert_eq!(nav.node_ij(v), (i + DIRS[d].0, j + DIRS[d].1));
            }
        }
    }
    // inside the building there is no ground node, but a roof node (not in main)
    let (i, j) = nav.cell_of(-110.0, -194.0);
    let nodes: Vec<u32> = nav.cell_nodes(i, j).collect();
    assert!(nodes.iter().all(|&u| nav.node_layer(u) != LAYER_TERRAIN && !nav.is_main(u)));
    assert!(!nodes.is_empty());
    let r = nav.region_of(nodes[0]).expect("roof region");
    assert!(r.is_roof && !r.access.is_empty());
    for a in &r.access {
        assert!(nav.is_main(a.ground_node) && nav.comp_of(a.top_node) == r.comp);
        assert!(a.crest >= a.top[2] - 1e-3 && (a.climb - (a.crest - a.ground[2])).abs() < 1e-3);
        assert!((a.ground[0] - a.top[0]).hypot(a.ground[1] - a.top[1]) < 16.0);
    }
    // snapping
    let s = nav.nearest_walkable(-110.0, -194.0).unwrap();
    assert!((s[0] + 110.0).hypot(s[1] + 194.0) < 20.0);
    let roof = nav.nearest_node([-110.0, -194.0, 5.82]).unwrap();
    assert!(!nav.is_main(roof));
    let c = max_climb(100.0, &JetParams::default());
    assert!(c > 55.0 && c < 75.0, "max climb {c}");
}

fn check_leg(w: &World, a: [f32; 3], b: [f32; 3], body: &Body) -> Result<(), String> {
    let f = w.walk_clear(a, [b[0], b[1]], body, body.step_up).ok_or_else(|| format!("leg {a:?} -> {b:?} not walk-clear"))?;
    if (f - b[2]).abs() > 0.3 {
        return Err(format!("leg {a:?} -> {b:?} ends at z {f}"));
    }
    let r = w.walk_clear(b, [a[0], a[1]], body, body.step_up).ok_or_else(|| format!("leg {b:?} -> {a:?} (reverse) not walk-clear"))?;
    if (r - a[2]).abs() > 0.3 {
        return Err(format!("reverse leg {b:?} -> {a:?} ends at z {r}"));
    }
    Ok(())
}

#[test]
fn random_paths_are_valid_and_fast() {
    let w = world();
    let nav = nav();
    let body = Body::default(); // the real mech: radius 2.5, slope 50
    let mut rng = Rng(0xfeed_beef_1234);
    let n = nav.node_count() as u64;
    let pick = |rng: &mut Rng| loop {
        let u = (rng.next() % n) as u32;
        if nav.is_main(u) {
            return u;
        }
    };
    let pairs = 400;
    let mut worst = 0.0f64;
    let mut total = 0.0f64;
    let mut legs = 0usize;
    let mut ratio_sum = 0.0f64;
    for k in 0..pairs {
        let a = pick(&mut rng);
        let b = pick(&mut rng);
        let mut from = nav.node_pos(a);
        let to = nav.node_pos(b);
        if k % 2 == 1 {
            // off-centre start, as a real mech would be
            let cand = [from[0] + rng.range(-0.9, 0.9), from[1] + rng.range(-0.9, 0.9)];
            if let Some(f) = w.floor_at(cand[0], cand[1], from[2], body.step_up) {
                if !w.mech_blocked([cand[0], cand[1], f.z], &body) {
                    from = [cand[0], cand[1], f.z];
                }
            }
        }
        let t = Instant::now();
        let p = nav.find_path(from, to).expect("path between main nodes");
        let mut dt = t.elapsed().as_secs_f64();
        if dt > 0.004 {
            // re-measure slow ones: the machine may have hiccuped
            for _ in 0..2 {
                let t = Instant::now();
                let _ = nav.find_path(from, to);
                dt = dt.min(t.elapsed().as_secs_f64());
            }
            let t = Instant::now();
            let chain = nav.astar_nodes(nav.nearest_node(from).unwrap(), nav.nearest_node(to).unwrap(), |_| 0.0).unwrap();
            eprintln!("slow pair {k}: {from:?} -> {to:?}: {:.2} ms total, A* alone {:.2} ms, {} nodes", dt * 1e3, t.elapsed().as_secs_f64() * 1e3, chain.len());
        }
        worst = worst.max(dt);
        total += dt;
        assert!(p.goal_error < 0.01, "goal error {}", p.goal_error);
        assert!(!p.points.is_empty());
        let mut prev = from;
        let mut len = 0.0;
        for q in &p.points {
            if let Err(e) = check_leg(&w, prev, *q, &body) {
                panic!("pair {k} ({from:?} -> {to:?}): {e}");
            }
            len += (q[0] - prev[0]).hypot(q[1] - prev[1]);
            prev = *q;
            legs += 1;
        }
        assert!((len - p.length).abs() < 0.5);
        let direct = (from[0] - to[0]).hypot(from[1] - to[1]) as f64;
        assert!(p.length as f64 >= direct - 0.01);
        if direct > 50.0 {
            ratio_sum += p.length as f64 / direct;
        }
    }
    eprintln!(
        "paths: {pairs} pairs, {legs} legs, mean {:.3} ms, worst {:.3} ms, mean detour ratio {:.3}",
        total / pairs as f64 * 1e3,
        worst * 1e3,
        ratio_sum / pairs as f64
    );
    assert!(worst < 0.010 * SLACK, "worst path query {worst}s");
    assert!(total / (pairs as f64) < 0.003 * SLACK);

    // map-corner to map-corner
    let from = [-480.0, -300.0, w.terrain_height(-480.0, -300.0)];
    let to = [520.0, 360.0, w.terrain_height(520.0, 360.0)];
    let t = Instant::now();
    let p = nav.find_path(from, to).unwrap();
    let dt = t.elapsed().as_secs_f64();
    eprintln!("cross-map path: {:.0} m, {} waypoints, {:.3} ms", p.length, p.points.len(), dt * 1e3);
    assert!(dt < 0.010 * SLACK);
    assert!(p.length > 1190.0 && p.length < 1500.0);

    // a goal inside a building is snapped to the nearest reachable ground
    let p = nav.find_path(from, [-110.0, -194.0, 0.0]).unwrap();
    assert!(p.goal_error > 2.0 && p.goal_error < 25.0, "goal error {}", p.goal_error);

    // penalised search avoids a region: forbid a band and compare
    let plain = nav.find_path(from, to).unwrap();
    let pen = nav.find_path_with(from, to, |u| if nav.node_pos(u)[0].abs() < 30.0 && nav.node_pos(u)[1] < 150.0 { 5.0 } else { 0.0 }).unwrap();
    assert!(pen.length >= plain.length - 0.5);
}

#[test]
fn distance_field_matches_paths() {
    let w = world();
    let nav = nav();
    let goal = [41.3, -72.8, w.terrain_height(41.3, -72.8)];
    let t = Instant::now();
    let f = nav.dist_field(&[goal], f32::INFINITY);
    let dt = t.elapsed().as_secs_f64();
    eprintln!("full dist field: {:.2} ms", dt * 1e3);
    assert!(dt < 0.050 * SLACK, "dist field took {dt}s");
    let mut rng = Rng(99);
    let n = nav.node_count() as u64;
    let mut checked = 0;
    while checked < 60 {
        let u = (rng.next() % n) as u32;
        if !nav.is_main(u) {
            continue;
        }
        checked += 1;
        let p = nav.node_pos(u);
        let d = nav.field_dist(&f, p);
        assert!(d.is_finite());
        let path = nav.find_path(p, goal).unwrap();
        // the smoothed path is never longer than the grid distance, and not much shorter
        assert!(path.length <= d + 4.5, "path {} vs field {}", path.length, d);
        assert!(path.length >= d * 0.9 - 6.0, "path {} vs field {}", path.length, d);
        // walking down the field makes progress and every step is walk-clear
        let mut pos = p;
        let mut last = d;
        for _ in 0..400 {
            match nav.field_next(&f, pos, 30.0) {
                Some(q) => {
                    check_leg(&w, pos, q, &Body::default()).unwrap();
                    pos = q;
                    let dn = nav.field_dist(&f, pos);
                    assert!(dn < last, "no progress: {dn} >= {last}");
                    last = dn;
                }
                None => break,
            }
        }
        assert!(last < 3.0, "field descent stopped {last} m from the goal");
    }
    // disc field: zero inside the disc, bounded field leaves far nodes unreachable
    let f = nav.dist_field_disc(0.0, 0.0, 60.0, 200.0);
    let inside = nav.nearest_main(10.0, 10.0).unwrap();
    assert_eq!(f.dist[inside as usize], 0.0);
    let far = nav.nearest_main(500.0, 300.0).unwrap();
    assert!(f.dist[far as usize].is_infinite());
}

#[test]
fn cover_points_are_hidden() {
    let w = world();
    let nav = nav();
    // stand north of the building at (-110, -194); threat further north looking at us
    let me = [-110.0, -180.0, w.terrain_height(-110.0, -180.0)];
    let foe = [-100.0, -120.0, w.terrain_height(-100.0, -120.0)];
    let threat = [foe[0], foe[1], foe[2] + 7.0];
    let t = Instant::now();
    let cover = nav.cover_points(me, threat, 120.0, 9.0, 8);
    let dt = t.elapsed().as_secs_f64();
    eprintln!("cover query: {} points, {:.3} ms", cover.len(), dt * 1e3);
    assert!(!cover.is_empty(), "there is cover behind the building");
    assert!(dt < 0.010 * SLACK);
    let mut last = 0.0;
    for c in &cover {
        assert!(!w.los(threat, [c.pos[0], c.pos[1], c.pos[2] + 4.5]));
        assert!(!w.los(threat, [c.pos[0], c.pos[1], c.pos[2] + 9.0]));
        assert!(!nav.exposed(c.node, threat, 9.0));
        assert!(c.path_dist >= last && c.path_dist <= 120.0);
        last = c.path_dist;
        // and it is reachable on foot
        let p = nav.find_path(me, c.pos).unwrap();
        assert!(p.goal_error < 0.01);
    }
    // where we stand we are exposed
    assert!(nav.exposed(nav.nearest_node(me).unwrap(), threat, 9.0));
}

#[test]
fn query_speed() {
    let w = world();
    let nav = nav();
    let mut rng = Rng(4242);
    let n = nav.node_count() as u64;
    let pts: Vec<[f32; 3]> = (0..4000).map(|_| nav.node_pos((rng.next() % n) as u32)).collect();
    let t = Instant::now();
    let mut acc = 0u32;
    for k in 0..pts.len() - 1 {
        let a = [pts[k][0], pts[k][1], pts[k][2] + 7.0];
        let mut b = pts[k + 1];
        // limit to weapon range
        let d = (b[0] - a[0]).hypot(b[1] - a[1]);
        if d > 250.0 {
            b = [a[0] + (b[0] - a[0]) * 250.0 / d, a[1] + (b[1] - a[1]) * 250.0 / d, b[2]];
        }
        acc += w.los(a, [b[0], b[1], b[2] + 4.5]) as u32;
    }
    let los_us = t.elapsed().as_secs_f64() / (pts.len() - 1) as f64 * 1e6;
    let t = Instant::now();
    for p in &pts {
        acc += w.mech_blocked(*p, &Body::default()) as u32;
        acc += w.floor_at(p[0] + 0.3, p[1] - 0.2, p[2], 1.2).is_some() as u32;
    }
    let geo_us = t.elapsed().as_secs_f64() / (pts.len() * 2) as f64 * 1e6;
    let t = Instant::now();
    for k in 0..pts.len() - 1 {
        let a = [pts[k][0], pts[k][1], pts[k][2] + 8.0];
        acc += w.can_see(a, 0.3, pts[k + 1], 9.0, &VisionParams::default()) as u32;
    }
    let see_us = t.elapsed().as_secs_f64() / (pts.len() - 1) as f64 * 1e6;
    eprintln!("los (<=250 m): {los_us:.2} us, mech_blocked/floor_at: {geo_us:.3} us, can_see: {see_us:.2} us (acc {acc})");
    assert!(los_us < 20.0 * SLACK, "los too slow: {los_us} us");
    assert!(geo_us < 2.0 * SLACK, "collision queries too slow: {geo_us} us");
    assert!(see_us < 30.0 * SLACK);
}

/// Rays and sight tests with absurd numbers come back at once. (A ray of 1e9 m, or an
/// infinite one, used to march for ever: `t + dt == t` in f32. A yaw of 1e9 never left
/// the subtract-until-it-fits loop. One such observation hung the bot for the match.)
#[test]
fn absurd_rays_and_yaws_terminate() {
    let w = world();
    let a = [0.0f32, 0.0, 20.0];
    let t0 = Instant::now();
    for far in [1.0e5f32, 1.0e6, 1.6e7, 1.0e9, 1.0e30, f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
        for b in [[far, 0.0, 20.0], [0.0, far, 5.0], [far, far, far], [3.0, 4.0, far]] {
            let _ = w.los(a, b);
            let _ = w.los(b, a);
            let _ = w.raycast(a, b);
            let _ = w.raycast(b, a);
            let _ = w.walk_clear(a, [b[0], b[1]], &Body::default(), 3.0);
        }
        assert!(!w.los(a, [far, far, far]) || far.abs() <= 1.0e6, "a ray to nowhere ({far}) is not a line of fire");
    }
    let vp = VisionParams::default();
    for yaw in [1.0e9f32, -1.0e9, 1.0e30, f32::INFINITY, f32::NAN] {
        let _ = w.can_see([0.0, 0.0, 8.0], yaw, [100.0, 0.0, 0.0], 9.0, &vp);
    }
    // a sane yaw beyond one turn still sees what it faces
    let open = (0..40).map(|i| [-250.0 + 12.0 * i as f32, -40.0, 0.0f32]).find_map(|p| {
        let z = w.terrain_height(p[0], p[1]);
        let eye = [p[0], p[1], z + 8.0];
        let tgt = [p[0] + 60.0, p[1], w.terrain_height(p[0] + 60.0, p[1])];
        w.can_see(eye, 0.0, tgt, 9.0, &vp).then_some((eye, tgt))
    });
    if let Some((eye, tgt)) = open {
        let turn = 2.0 * std::f32::consts::PI;
        assert!(w.can_see(eye, 3.0 * turn, tgt, 9.0, &vp), "yaw + 3 turns faces the same way");
        assert!(w.can_see(eye, -2.0 * turn, tgt, 9.0, &vp));
        assert!(!w.can_see(eye, std::f32::consts::PI + turn, tgt, 9.0, &vp), "facing away, beyond the near range");
    }
    assert!(t0.elapsed().as_secs_f64() < 2.0 * SLACK, "absurd rays took {:?}", t0.elapsed());
}
