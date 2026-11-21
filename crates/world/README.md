# `world` — map, collision, line of sight, navigation

Foundation crate for the Mech Royale bot (`crates/bot`) and the local simulator
(`crates/sim`). It loads the static map (`v2/world.json` + `v2/terrain.i16`,
protocol.md §11) and answers geometry and navigation queries fast enough to be
called hundreds of times per 50 ms decision.

Only dependencies: `serde`, `serde_json`. No threads, no unsafe, no I/O after loading.

```toml
[dependencies]
world = { path = "../world" }
```

```rust
use std::sync::Arc;
use world::{World, Nav, NavParams, Body};

let world = Arc::new(World::load(std::path::Path::new(&hello.map.dir))?); // ~5 ms
let nav = Nav::build(world.clone(), NavParams {                           // ~70-150 ms (1 s on a loaded box)
    radius: cfg.mech.radius, height: cfg.mech.height,
    step_up: cfg.mech.step_up, max_slope_deg: cfg.mech.max_slope_deg,
    ..NavParams::default()
});
let path = nav.find_path(self_pos, goal).unwrap();   // ~0.4 ms typical, < 3 ms worst
let next = path.points[0];                           // walk towards this
```

Everything is exported at the crate root (`use world::*` is safe: there is no item
called `world`).

## Conventions

* x east, y north, z up, metres, radians — the protocol frame (§5).
* Positions are `[f32; 3]`; horizontal points `[f32; 2]`. A mech position is its **feet**.
* The crate never reads `hello.config`: pass the numbers in (`Body`, `NavParams`,
  `VisionParams`, `JetParams`). Their `Default` impls hold the documented defaults
  (radius 2.5, height 9, step_up 1.2, slope 50°, …) for tests only — the bot must fill
  them from `hello.config.mech` / `self.stats`.
* "Layer": `LAYER_TERRAIN` (0) = terrain, `k + 1` = top of `world.prisms[k]`.
* The repo copy of the map is `arena/` (so `World::load("arena")` works locally and
  tests need no setup; override with `MECH_ARENA=/path`). In a match use `hello.map.dir`.

## `World` — loading

| | |
|---|---|
| `World::load(map_dir: &Path) -> Result<World, LoadError>` | `map_dir` = `hello.map.dir` (holds `v2/world.json`); the `v2` dir itself also works. ~5 ms release. |
| `World::from_bytes(world_json, terrain_i16)` | Same from memory. |

Public fields: `bounds: Bounds {min_x,max_x,min_y,max_y}` (`contains(x,y,margin)`,
`clamp(x,y,margin)`), `terrain: Terrain` (`height(x,y)`, `gradient(x,y)`, raw `h`,
`nx`, `ny`, `min_x`, `min_y`, `step`, `min_h`, `max_h`), `prisms: Vec<Prism>`
(`kind: PrismKind::{Building,Stadium,Bridge,Other}`, `base`, `top`, `ring`, `holes`,
`bbox`, `edges`, `block`; methods `contains_xy`, `dist_xy(x,y,cap)`, `area`),
`trees: Vec<Tree>` (`x,y,z,height,crown_r,crown_base,trunk_r,crown_x,crown_y`),
`canopy_density`, `canopy_block_t`, `rules_max_step`, `rules_max_slope_deg`.

## `World` — heights and collision

| Query | Meaning | Cost |
|---|---|---|
| `terrain_height(x, y) -> f32` | Bilinear ground height, prisms ignored. | ~5 ns |
| `floor_at(x, y, feet_z, step_up) -> Option<Floor>` | The surface a mech with feet at `feet_z` stands on: the highest surface (terrain or prism top) not above `feet_z + step_up`. `None` = that spot is inside solid (inside a building / under ground). Under a bridge with `feet_z` at ground level you get the terrain; with `feet_z` at deck level you get the deck. Use `step_up = 0.0` for "where do I land falling from z". `Floor { z, layer }`. | ~50 ns |
| `top_surface(x, y) -> Floor` | Highest surface (roof if any). | |
| `ceiling_at(x, y, z) -> Option<f32>` | Lowest prism underside above `z` (what stops a jet). | |
| `point_solid(p) -> bool` | Inside a prism or under the terrain. | |
| `clearance(x, y, z, height, step_up, cap) -> f32` | Horizontal distance to the nearest prism that is a wall for a mech with feet at `z`: it overlaps the body vertically (`base < z + height`) and cannot be stepped onto (`top > z + step_up`). Capped at `cap`; 0 = inside one. Pass a small `cap` (the radius you care about) — it bounds the work. | ~0.1 µs |
| `mech_blocked(pos, &Body) -> bool` | Would the cylinder (`radius`, `height`) with feet at `pos` intersect the world: wall/overhang within `radius`, feet more than `step_up` under the ground, or centre outside the map bounds. | ~0.1 µs |
| `walk_clear(from, to_xy, &Body, max_drop) -> Option<f32>` | Exact straight-walk test: simulates walking from `from` to `to_xy` in 0.5 m steps with the step/slope/collision rules. Returns the feet height at the end, or `None` if stopped (wall within `radius`, ledge above `step_up`, uphill slope above `max_slope_deg`, drop larger than `max_drop`; pass `f32::INFINITY` to allow stepping off roofs). `radius: 0.0` traces the centre only. | ~0.2 µs per metre |
| `prisms_at(x, y, f)`, `prisms_near(x, y, r, f)`, `no_prism_near(x, y, r)` | Spatial-grid iteration (8 m cells) for custom queries. | |

`Body { radius, height, step_up, max_slope_deg }` — remember radius and height grow
5 % per cube (`self.stats.radius/height`).

## `World` — rays and vision

| Query | Meaning | Cost |
|---|---|---|
| `raycast(a, b) -> Option<Hit>` | First solid a shot from `a` to `b` hits: prisms (buildings, stadium, bridges — you can shoot under a bridge) and terrain. **Trees never stop shots.** `Hit { t (0..1), pos, kind: HitKind::{Terrain, Prism(i)} }`. | ~0.4 µs avg (≤250 m) |
| `los(a, b) -> bool` | Line of fire: nothing solid between. Same as `raycast(..).is_none()` but with early exit. | ~0.4 µs |
| `foliage(a, b) -> (trunk_hit, crown_metres)` | Trees on a sight line. | |
| `los_vision(a, b, canopy) -> bool` | Sight line: solid blocks, tree **trunks** block, and with `canopy = true` crowns block once `exp(-canopy_density · metres of crown) < canopy_block_t` (≈ 4 m of foliage with the map's 0.35 / 0.25). | ~1 µs |
| `can_see(eye, eye_yaw, target_feet, target_height, &VisionParams) -> bool` | Protocol §8: line of sight to the target's centre or top, and either within `near_range` (any direction, canopy ignored) or within `range` inside the `fov_deg` yaw wedge (canopy applies). `eye` = viewer feet + eye height. | |

Shot origin, muzzle heights, lead, mortar arcs are the caller's business; sample a
mortar arc as short `los` segments.

## `Nav` — walking navigation

`Nav::build(world: Arc<World>, params: NavParams) -> Nav` builds, for **one mech size**:

* a grid of `cell` (2 m) cells over the map bounds; each cell has 0..n **nodes**: one on
  the terrain if a mech can stand there, plus one per prism top above it (roofs, bridge
  decks). A node keeps `radius + margin` (3.1 m) from every wall and needs `height`
  (9 m) of head room, so ground under a high bridge is walkable and ground under a low
  one is not;
* **links** between the 8 neighbours where a mech can walk both ways (slope ≤
  `max_slope_deg − slope_safety_deg`, ledges ≤ `step_up`, clearance kept in between);
* connected **components**. The largest is the *main ground component* (125k nodes ≈
  0.5 km², 99 % of the open ground). Everything else is a `Region`: roofs, decks,
  enclosed courtyards — reachable only by jump + jet;
* ALT landmarks for a strong A* heuristic.

`NavParams { radius, height, step_up, max_slope_deg, margin: 0.6, cell: 2.0,
slope_safety_deg: 2.0, landmarks: 8 }`. Build takes 70–150 ms and ~25 MB, so the bot
may build a second `Nav` for a fatter mech (e.g. radius/height × 1.25 after 5 cubes;
the three 9.1–9.4 m high bridges stop being passable from 1 cube on — a `Nav` built
with the larger `height` knows). `Nav` uses interior scratch buffers: it is `Send`
but **not `Sync`** (one thread at a time).

### Paths

| Query | Meaning |
|---|---|
| `find_path(from, to) -> Option<NavPath>` | Shortest walk. `from` snaps to the nearest node (z-aware), `to` to the nearest node **of the same component**, so a goal inside a building gives a path to the closest reachable spot — check `goal_error`. `None` only when `from` cannot be snapped. |
| `find_path_with(from, to, penalty: Fn(NodeId) -> f32)` | Same with an extra cost per metre for entering a node (≥ 0), e.g. `|n| if nav.exposed(n, enemy_muzzle, 9.0) { 3.0 } else { 0.0 }`. Keep the closure cheap or cache it. |
| `astar_nodes(start, goal, penalty) -> Option<Vec<NodeId>>` | Raw node chain. |

`NavPath { points, length, goal_error }`: `points` are smoothed waypoints **excluding
the start position**; walk straight at `points[0]`, re-plan whenever you like
(re-planning from a point on the path returns the same path, no jitter). Every leg
(start→points[0], points[i]→points[i+1]) is a straight line that keeps ≥ `radius` +
~0.1 m from walls; legs are at most 160 m long. The last point is the exact goal when
reachable, else the nearest node centre. Waypoint z is the standing height there.

Timing (release, M-series laptop): mean 0.4 ms, worst 2.6 ms over 400 random
main-component pairs; corner-to-corner (1256 m) 0.75 ms. Under heavy machine load
these roughly double.

### Distance fields (for repeated "go there" / "how far is it" queries)

| Query | Meaning |
|---|---|
| `dist_field(goals: &[[f32;3]], max_dist) -> DistField` | Walking distance from every node to the nearest goal (one Dijkstra, ~5 ms whole map; `max_dist` bounds it). |
| `dist_field_disc(cx, cy, radius, max_dist)` | Goal = every main node inside a disc (e.g. the next zone circle). |
| `dist_field_nodes(goals: &[NodeId], max_dist)` | Goal nodes given directly. |
| `field_dist(&field, pos) -> f32` | Distance at a position (`INFINITY` = unreachable / beyond `max_dist`). `field.dist[node]` is the raw per-node value. |
| `field_next(&field, pos, lookahead) -> Option<[f32;3]>` | Next waypoint walking downhill in the field (farthest straight-clear point within `lookahead` metres of descent). `None` at the goal or when unreachable. |

### Snapping, straight walks, cover

| Query | Meaning |
|---|---|
| `nearest_node(pos) -> Option<NodeId>` | Nearest node, any component, 3D distance (a mech on a roof snaps to the roof; mid-air it may snap to either). Search radius 150 m. |
| `nearest_main(x, y) -> Option<NodeId>` / `nearest_walkable(x, y) -> Option<[f32;3]>` | Nearest point of the main ground component to an arbitrary point (inside a building, off the map, …). |
| `nearest_in_comp(pos, comp)` | Nearest node of one component. |
| `node_at(pos) -> Option<NodeId>` | Node of the cell containing `pos` within 2 m of its height: "is this spot walkable". |
| `line_clear(a, b) -> bool` | Is the straight walk a→b clear (grid links + exact wall clearance)? Cheap (~1 µs per 10 m in the open). Both ends must be on walkable cells. For an exact, grid-free answer use `World::walk_clear`. |
| `line_clear_nodes(a_node, a_xy, b_node, b_xy)` | Same with known nodes. |
| `cover_points(from, threat, max_path_dist, body_height, max_results) -> Vec<CoverPoint>` | Walkable spots within `max_path_dist` metres of walking from `from` where a mech of `body_height` cannot be shot from `threat` (the enemy muzzle/eye point): no `los` to its centre nor its top. Nearest first. `CoverPoint { pos, node, path_dist }`. ~0.15 ms for 8 results nearby; worst case ~1–2 µs per node examined (π·r²/4 nodes). |
| `exposed(node, threat, body_height) -> bool` | The same test for one node. |

### Raw grid access

`nx`, `ny`, `cell`, `x0`, `y0`, `node_count()`, `cell_of(x, y) -> (i, j)`,
`cell_nodes(i, j) -> Range<NodeId>`, `node_pos(n)`, `node_ij(n)`, `node_layer(n)`,
`node_clearance(n)` (wall distance, capped at `clearance_cap()`; `OPEN_CLR` when no
prism is anywhere near), `neighbors(n) -> &[NodeId; 8]` (order `DIRS`: E, NE, N, NW,
W, SW, S, SE; `NO_NODE` = no link), `comp_of(n)`, `comp_size(c)`, `main_comp()`,
`is_main(n)`, `world()`.

### Roofs and other regions (optional data, no jump planning)

`regions() -> &[Region]`, `region_of(node) -> Option<&Region>`.

`Region { comp, is_roof, nodes, area, z_min, z_max, centroid, access, min_climb }`;
`access: Vec<Access>` (up to 12, spread ≥ 12 m apart, lowest climb first):
`Access { ground, ground_node, top, top_node, crest, climb, climb_out }` — stand at
`ground` (main component), jump + jet until the feet clear `crest`, move to `top`.
`climb = crest − ground.z`; `climb_out = crest − top.z` (0 for a plain roof edge: just
walk off; falling does no damage per the protocol). Inside a region `find_path` works
normally (it stays inside the region).

`max_climb(energy, &JetParams) -> f32` estimates the height gained by a jump plus a
jet burn (≈ 69 m for 100 energy with default numbers; the protocol says "about 60 m",
so use a safety factor ≈ 0.85). A roof is feasible when
`access.climb < 0.85 * max_climb(self.energy, &jet)`.

## Facts about this map worth knowing

* **Ohio Stadium is sealed for walkers.** Its field and stands form regions (the bowl
  is 7236 nodes) not connected to the main component: the two gate corridors end
  under stadium prisms whose base is below the ground, i.e. solid by the §11 rule.
  A mech that starts or lands inside must jet out via an `Access` (the lowest hop is
  about 10 m). If the real server lets mechs walk through the gates the bot only
  loses a shortcut, never gets stuck.
* 9 bridges: four low ramps on the west side (walkable on top, walls from below), five
  elevated decks (walk under). Three of the elevated ones have only 9.1–9.4 m of
  clearance.
* Building bases are below the terrain, so "inside a footprint" at ground level is
  always solid; roofs are 4–73 m above the ground.
* Recorded positions from a real match (`tests/fixtures/observed.json`, from the kit's
  `check/stream.jsonl.gz`) agree with the model: grounded mechs stand on `floor_at`
  to within 3 cm and no observed mech is `mech_blocked`.

## Assumptions (not stated by the protocol — verify against the practice run)

* A prism is a wall for a mech iff its footprint is within `radius` of the centre, it
  overlaps `[feet, feet + height)` vertically and its top is more than `step_up` above
  the feet. Terrain never blocks sideways; it only limits by slope / being under it.
* Slope is measured as the terrain rise over 0.5 m samples along the walk.
* The map-edge wall is at `bounds`; nav nodes keep `radius + margin` from it.
* Tree trunk = vertical cylinder `trunk_r` from the ground to `crown_base`; crown =
  ellipsoid centred at (`crown_x`, `crown_y`), horizontal radius `crown_r`, spanning
  `crown_base..height`; transmittance = `exp(-canopyDensity · chord length)`, blocked
  below `canopyBlockT`.
* `barriers`, `cars`, `water` are ignored (the protocol says they are scenery).

## Debug tool

```sh
cargo run --release -p world --bin navdump -- --out nav.png --scale 2 --stats \
    --path " -480,-300,520,360"          # leading space so the minus is not a flag
cargo run --release -p world --bin navdump -- --out stadium.png --scale 8 --crop " -390,-30,-190,90"
```

Options: `--arena DIR`, `--out FILE.png|.ppm`, `--scale N` (pixels per cell),
`--cell M`, `--margin M`, `--radius M`, `--height M`, `--path x0,y0,x1,y1`,
`--crop x0,y0,x1,y1`, `--stats` (list regions). `--path` endpoints are snapped to the
main ground component. North is up. Grey = main ground
(brighter = higher), yellow = ground not connected to main, blue = roof/deck nodes,
teal = prism tops that are part of the main component (ramps), dark red = solid
footprint, black = unwalkable margin, green dots = roof access points, magenta = path.

## Tests

`cargo test -p world` (≈ 2 s; add `--release -- --nocapture` for timings). They run
on the real map: loading, bilinear terrain, recorded match positions, building /
bridge / roof queries, grid raycast vs brute force (3000 rays), trees, nav structure,
400 random paths with every leg verified in both directions by the exact
`World::walk_clear` at the true mech radius, distance fields, cover, and query timing.
