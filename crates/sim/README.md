# `sim` — rough local match simulator

**This is an approximation, not the game.** There is no official simulator; this one
was written from `protocol.md` and the recorded `hello` in the kit. Everything the
rulebook does not spell out is a guess (listed below), and the built-in opponents are
invented. **Scores from this simulator do not predict the real leaderboard.** Use it to
find crashes, protocol mistakes, slow decisions, navigation bugs and obviously bad
behaviour, and to compare two versions of a bot against the same seeds. The only true
feedback is the hackathon's practice run.

It is not part of the submitted image (the Dockerfile builds `crates/bot` only).

## Quick start

```sh
cargo build --release -p sim -p bot

# our bot against 15 scripted bots playing as one squad (the practice setup), 20 matches
target/release/sim --bot target/release/bot --fill squad --seed 1 --matches 20

# 16 copies of the kit's example bot (quote paths with spaces inside the command)
target/release/sim --bots 16 "python3 -u '/path/to/mechfight-kit/python/example_bot.py'" --seed 7 --all

# no process at all: a scripted "hard" bot as the outsider against the squad
target/release/sim --fill squad --focus-level hard --matches 10

# debugging one match: replay + a zoomed picture of our path
target/release/sim --bot target/release/bot --seed 3 --replay /tmp/r.jsonl --trace /tmp/t.png --trace-crop
```

`sim --help` lists every option. Output, one line per match for the focus bot, then the
mean of `16 - place`, then a protocol report for every process bot:

```
seed=3 bot=tin-buckeye slot=0 place=14 kills=2 pts=2 died t=47.0s cause=gun by 5 (easy-5) cubes=0 dealt=4005 taken=1414 [match 47.1s in 0.73s wall]
summary: 24 match(es), focus bot [target/release/bot] vs fill=squad: mean(16-place) = 2.00, mean kills 2.00, mean survival 52s; causes: gun=16 laser=5 ...
protocol report (process bots, all matches):
  bot  0 tin-buckeye  obs 12580 ok 12580 late 0 stale 0 wrong-tick 0 bad-json 0 oversize 0 bad-fields 0 idle-decisions 0 no-ready false crashed no no-exit false | answer ms mean 0.06 max 5.7 >10ms 0 >20ms 0 | ready 926 ms
```

Speed on this laptop: a scripted-only match takes 0.05–0.4 s, one Rust bot against the
squad about 1.5 s, sixteen Python example bots about 2 s (a full match is up to 480 s
of match time). The same seed gives the same match as long as the bots are
deterministic and never late.

### Bots and slots

* `--bot CMD` (repeatable) and `--bots N CMD` add process bots; the command line is
  split like a shell would (quotes, backslashes; no variable expansion).
* `--fill` decides what pilots the remaining slots: `squad` (default), `mix` (the same
  bots, everyone for themselves), `easy` / `medium` / `hard` / `expert`, or `idle`.
* Bot `i` in the list gets slot `i` unless `--shuffle`. `--focus N` picks the bot that
  is reported (and that the squad hunts); default 0.
* `hello.map.dir` is the absolute `--arena` path (default `$MECH_ARENA` or the repo's
  `arena/`); override with `--map-dir`.
* `--config FILE` replaces the rules (same shape as `hello.config`). The default is the
  real config recorded in the kit (`config/default.json`).

### Protocol checks

The simulator speaks `mechfight/1` as section 2, 3 and 6 describe: `hello` with the full
config, lockstep `obs` / action at 10 decisions per match-second with 2 ticks each,
`end` with place and kills, then stdin closes. Per process bot it counts:

| Counter | Meaning |
|---|---|
| `late` | no valid answer inside `bot.tick_budget_ms` (50 ms; `--budget-ms` overrides). The mech keeps `move` / `boost` / aim for `coast_s`, then idles (`idle-decisions`). |
| `stale` | an answer carrying an earlier observation's tick (what a late answer becomes) |
| `wrong-tick` | a tick that was never sent, or no tick |
| `bad-json`, `oversize` | lines that are not a JSON object / longer than `bot.max_line_bytes` |
| `bad-fields` | an action field of the wrong type (that field is ignored, the rest is used) |
| `no-ready` | no `ready` inside `hello_budget_ms` |
| `crashed` | stdout closed before the mech died (the mech idles from then on) |
| `no-exit` | still running 3 s after `end` and a closed stdin (it is then killed) |
| `answer ms` | mean / max time from sending `obs` to reading the answer, and how many were over 10 and 20 ms |

By default observations go to **one process bot at a time** (send, wait, next), so that
a busy laptop does not make sixteen bots look late; `--parallel` sends them all first,
as the server does. `--no-enforce` still counts late answers but uses them (waiting up
to `--wait-ms`), which keeps a match reproducible while you look for slow decisions.
The `debug` field of an action is ignored and not checked against the caps
(`check/check.py` in the kit does that).

### Replay and trace

* `--replay FILE.jsonl`: a `header` line (seed, bots, zone circles), one `frame` every
  `--replay-every` decisions (default 5 = 0.5 s, about 2 MB per match; 1 = every
  decision, about 10 MB) with every mech's position, aim, HP, energy, cubes, weapons,
  the projectiles and beams, `event` lines (deaths, supply drops) and a final `result`.
* `--trace FILE.png` (or `.ppm`): top-down picture, north up. Grey = ground (lighter is
  higher), dark = buildings, purple = bridges, blue rings = the zone circles. The focus
  bot's path runs from green (start) to dark blue (end); cyan = in the air, white =
  outside the zone. **Red dots = rubbing a wall or the map edge**, orange dots =
  commanded to move but almost standing still, magenta dots = took weapon damage, red
  X = where it died, small black x = where others died. `--trace-crop` zooms in on the
  path; `--trace-scale` sets pixels per metre.

With several matches the seed is appended to both file names.

## Built-in opponents

Scripted bots run inside the simulator (no process) and read the true match state,
filtered by the same visibility rule bots get (plus 5 s memory). They path-find on the
`world` nav grid, walk to a random spot inside the next zone circle before it closes,
pick up weapons, cubes, crates and (hard and expert) supply drops, pick upgrades, lead
with the gun, charge and release the laser, lob the mortar on a ballistic solution,
fire lightning inside its cone, strafe, and run for cover when low.

| Level | aim error σ | reaction | lead | strafes | retreats below | notes |
|---|---|---|---|---|---|---|
| easy | 6° | 1.0 s | none | no | never | walks at you and shoots |
| medium | 3° | 0.6 s | 60 % | half the time | 20 % HP | faces whoever hit it |
| hard | 1.5° | 0.35 s | 90 % | yes | 30 % HP | boosts, takes supply drops |
| expert | 0.7° | 0.2 s | full | yes | 35 % HP | same, sharper |

Every number is tunable per level: `--tune easy.aim_sigma_deg=8,expert.react_s=0.15`
(`all.` sets all four). Keys: `aim_sigma_deg react_s lead fire_tol_deg strafe
retreat_hp loot boost drops face_threat defend_range gun_range`.

**Squad mode** (`--fill squad`) imitates what we were told about the practice run: the
15 opponents (4 easy, 4 medium, 4 hard, 3 expert) form one team against the focus bot.

* They never target each other (stray splash still hurts) and hold fire when a team
  mate stands in the line.
* Shared vision: while any member sees the focus bot, the hunters know where it is;
  otherwise they go to where it was last seen.
* Hunters: one at the start, one more for every dead member
  (`--squad-deaths-per-hunter`), everyone from 240 s (`--squad-late-t`) or once 6
  mechs are left (`--squad-late-alive`). Hunters are drafted weakest level first, then
  nearest. `--squad-omniscient` lets the squad always know where the focus bot is.
* A member that is not hunting ignores the focus bot unless it comes within the
  level's `defend_range` (25–55 m) or has shot that member in the last 4 s.
* Once the focus bot is dead the squad has nobody to fight and dies in the closing
  zone; `--stop-when-focus-dies` skips that part.

How the real house bots move, aim and decide is unknown. In this simulator the kit's
example bot scores about 0.3 against the squad (the real practice run gives it 1.15),
so the default squad is probably harsher early than the real one. Treat the difference
between two versions of our bot as a hint, never as a forecast.

## What is simulated, and the guesses

Everything below follows the numbers in `hello.config`. A "guess" is where the rulebook
is silent or ambiguous.

**Movement** (`src/physics.rs`, tested on a synthetic map)
* The velocity vector moves toward the commanded velocity at `ground_accel` /
  `air_accel`. Guess: in the air the commanded speed is `walk_speed`, and no input
  means braking at the same rate.
* Boost on the ground needs a move input and energy; it drains `boost_drain`/s.
* Gravity acts on every airborne tick; the jet adds `jet_accel` against it up to
  `max_rise_speed` and drains `jet_drain`/s. This climbs about 66 m on 100 energy (the
  protocol says "about 60").
* Walls: a prism is a wall if it overlaps the body vertically and its top is more than
  `step_up` above the feet (in the air: any top above the feet). The body is pushed out
  along the nearest edge and loses the velocity component into the wall, so it slides.
  The map edge is a wall at `bounds`.
* Ledges up to `step_up` are walked up unless that would put the head into an
  overhang. Guess: walking down, the mech stays grounded for drops up to `step_up`,
  otherwise it falls. No fall damage.
* Slopes: on terrain a step that rises more than `tan(max_slope_deg)` times its length
  is refused (checked every 0.5 m at most); the mech then keeps only one axis of the
  move. Guess: prism tops are exempt, and a mech in the air lands on a hillside that
  rises up to 1.5 m into its path.
* Undersides over any part of the body stop a rise; roof edges catch a body that still
  overhangs them. In both cases the body is slid away 0.6 m per tick unless a wall
  holds it or (guess) it is walking toward the roof.
* Overlapping mechs are pushed apart by half the overlap each.

**Weapons and damage** (`src/sim.rs`)
* Shots leave from the muzzle (mortar: `height + 0.5`), `radius + 0.5` out along the
  actual aim, shortened by a wall. Heights and radius scale with cubes.
* Gun: swept bullets against cylinders padded by `projectile.gun_radius`; exact fire
  rate while the trigger is held; automatic reload on an empty magazine. Guess: the
  spread is Gaussian with σ = `spread_deg` on yaw and on pitch; bullets do not inherit
  the shooter's velocity.
* Mortar: ballistic shell, explodes on the first solid, mech, drone or crate, or when
  `ttl` runs out. Guesses: a directly hit target takes `damage` plus the splash;
  splash falls linearly from 100 % to `splash_edge` at `splash_radius`, measured from
  the blast to the nearest point of the mech's vertical axis; it needs a clear line
  from 0.3 m above the blast to the mech's centre or top; knock-back and energy drain
  use the same falloff; drones and crates take splash but are not thrown.
* Laser: charge / release / `min_charge` / auto-fire after `hold_s` / half speed while
  charging / cook-off as written. Guess: the beam has no thickness and damage is
  linear in the charge.
* Lightning: nearest mech, drone or crate within `range` of the muzzle pivot and within
  ±`cone_deg` of the aim yaw (guess: the half-angle) with a clear line to its centre
  or top, then chains to the nearest not-yet-hit target within `chain_range` of the
  last one. Short cancels a charge and blocks charging for `short_s`.
* Rear armor, armor upgrades, cube damage scaling, own splash at `self_damage_mult` as
  written. Guess: your own blast is never a rear hit.
* Regeneration waits `regen_delay` after dealing or taking damage (guess: hitting a
  crate does not count) and never runs outside the zone.
* A kill goes to whoever dealt the final blow; the zone credits nobody. Guess: mechs
  dying on the same tick share the better place (`alive - deaths + 1`).

**World**
* Zone: phases and damage from the config. Guess: each next centre is uniform in the
  disc that keeps the next circle inside the current one, and stays on the map.
* Spawns: random ground cells of the main walkable area within `spawn_zone_fraction`
  of the start circle, `spawn_min_separation` apart, facing the zone centre (as in the
  recorded match). Crates and floor weapons are uniform over ground cells and reachable
  roofs (guess: roofs up to a 45 m climb); drones start at least 70 m from any spawn.
* Cubes, upgrade offers (three random distinct eligible upgrades, default choice 0
  after `offer_timeout_s`, queued one at a time), crates, weapon pickups with all the
  `take` / `drop` failure reasons, death drops and supply drops as written. Guesses: a
  supply drop lands on a random ground cell within `drops.spread` of the next circle's
  radius, and the ring reaches 6 m up and down.
* Drones are simple: they wander near home, pick the nearest visible mech inside
  `aggro_range` (or whoever hits them), walk straight at it without path-finding, stop
  at 28 m, shoot without lead every `cooldown`, and give up at `deaggro_range` or
  `chase_leash`. Guess: their bullets fly `deaggro_range + 30` m.
* Vision uses `world::can_see` (see that crate's README for the canopy assumptions)
  and is refreshed once per decision; mechs and drones stay listed for `memory_s`.
  Guess: cubes and pickups count as 1 m tall, projectiles and beam points as points; a
  beam is seen when any of its corners or segment middles is.
* Sounds: every kind in `sound.range`, with distance class, age and Gaussian bearing
  noise scaled by `hearing`. Guesses: `jet` repeats every `step_every` like steps,
  `laser_charge` sounds once when charging starts, `reload` when a reload starts.

**Protocol details that differ from the real server**
* JSON keys come out in alphabetical order.
* `hello.seed` is a fixed number per slot.
* `end` is sent the moment a mech dies (its place is known then), not at match end.
* A bot that never sends `ready` still gets observations.
* `debug` drawings are ignored.

## Code map

| File | What |
|---|---|
| `src/physics.rs` | one-tick movement of a body |
| `src/sim.rs` | match state, actions, weapons, damage, zone, loot, drones, drops, deaths |
| `src/obs.rs` | the `obs` message and replay frames |
| `src/proc.rs` | process bots: spawn, timing, protocol counters |
| `src/scripted.rs` | scripted levels and the squad coordinator |
| `src/runner.rs` | one match from `hello` to `end`; `Env`, `MatchSpec`, `RunOpts` (usable as a library) |
| `src/render.rs` | trace image and a tiny PNG encoder |
| `src/testworld.rs` | synthetic map for unit tests |
| `config/default.json` | the recorded real `hello.config` |

## Tests

`cargo test -p sim` (about 20 s, most of it waiting on deliberately broken shell bots):

* `tests/physics.rs` — acceleration, boost, walls and sliding, map edge, step-up,
  slopes, jump apex, jet climb, bridges and overhangs, roof landing and edges.
* `tests/rules.rs` — gun travel time, damage and rear armor, walls stopping shots,
  fire rate and reload, mortar arc, splash and concussion, laser charge, cook-off and
  auto-fire, lightning chain and short, zone schedule and damage, regeneration, cubes
  and offers, pickups, vision wedge and memory, sounds, supply drops, deaths and
  placement, drones, the clock.
* `tests/matches.rs` — whole matches on the real map: a scripted squad match completes,
  is reproducible and has no friendly kills; a free-for-all has fights and one winner;
  shell-script bots that answer correctly, with a wrong tick, with garbage, crash, stay
  silent or answer late are each reported as such.
