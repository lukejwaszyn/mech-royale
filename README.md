# tin-buckeye — a Mech Royale bot

A bot for the `mechfight/1` protocol (16-mech battle royale on the Ohio State campus),
written in Rust. The submitted image is a single static binary of about 0.6 MB.

```
crates/world   map loading, collision, line of sight, navigation      (library)
crates/bot     the bot: protocol loop, world model, movement, gunnery (the image's binary)
crates/sim     rough local simulator, for testing only                (not in the image)
arena/         a copy of the static map, so tests need no setup
Dockerfile     multi-stage build: rust alpine/musl -> empty image with /bot
```

## Build, test, run

```sh
cargo build --release -p bot                 # target/release/bot
cargo test  --release -p bot                 # math, protocol loop, behaviour regressions
python3 <kit>/check/check.py -- "$PWD/target/release/bot"

docker build -t mech-bot .                   # about 30 s on linux/amd64
python3 <kit>/check/check.py --map-dir /arena -- docker run --rm -i --network none \
    --cpus 1 --memory 512m --read-only --tmpfs /tmp:rw,size=64m --user 65534:65534 \
    -v "$PWD/arena:/arena:ro" mech-bot

# local matches against the simulator's guess of the house squad
cargo build --release -p sim -p bot
target/release/sim --bot target/release/bot --fill squad --seed 1 --matches 20      # the real binary over pipes
target/release/sim --inproc "" --fill squad --seed 1000 --matches 600 --jobs 3 \
    --stop-when-focus-dies --quiet --detail                                         # tuning runs, about 30 s
target/release/sim --inproc "range_gun_min=100" --fill squad --matches 600 --jobs 3 --quiet   # try a setting
```

Measured on an Apple Silicon laptop: ready in about 1.1 s (map plus five nav grids),
a typical decision takes 0.05–0.2 ms, the worst about 12 ms (the tick on which a new zone
distance field is built), 85–95 MB of memory. The amd64 image under emulation with the
match sandbox flags passes `check.py` with 300 of 300 answers in time.

## Switches

| | |
|---|---|
| `MECH_MODE=ffa` or `--ffa` | free-for-all preset for the final (default: `practice`) |
| `MECH_TUNE="key=value,..."` or `--tune=...` | override any field of `Tuning` (`crates/bot/src/config.rs`) |
| `MECH_VERBOSE=1` | two state lines per match-second on stderr |
| `MECH_DEBUG=1` | attach debug drawings (path, target line, holding spot) to actions |

Every behaviour number is a field of the one `Tuning` struct in `crates/bot/src/config.rs`
(ranges, first strike, laser errands, retreat thresholds, loot values, timing margins), next to the
upgrade preference table. Rule numbers are never hard-coded: they are read from
`hello.config`, `self.stats` and `self.weapons[i].stats` on every tick.

## How the bot is put together

`runner.rs` — the protocol loop, three threads:

* a **reader** turns stdin into batches of lines (everything already buffered is one
  batch, so queued observations arrive together);
* the **brain** owns the bot. Of a batch it answers only the newest observation; the
  older ones only contribute their events (damage, kills, pickups);
* the **coordinator** writes stdout. It gives the brain 60% of `tick_budget_ms`; if no
  answer has come by then it sends a *coasting* action for that tick (last move, boost,
  aim and triggers; no one-shots), so an answer is never late even if a decision is slow.

A decision that panics is caught and answered with the same coasting action. A hello that
does not parse, or a map that cannot be read, still gives a playing bot (on a flat world
without walls). stdout carries protocol lines only; logs go to stderr, including decision
times (a summary every 10 match-seconds and at exit).

`model.rs` — what we believe: enemy tracks (position, raw and smoothed velocity, HP,
weapons, last seen, clear line of fire, a duel record per enemy), remembered pickups,
crates and cubes, heard sounds with a rough position, damage history, the regeneration
clock. A mech that vanishes from the list just as the alive counter drops is dead; any
other track goes stale and expires.

`brain.rs` — one decision:

1. **Assess.** Who is attacking us (a mech counts only once it has hit us or is charging a
   laser down our line), whom to shoot, a duel estimate, fight or flight.
2. **Legs**, in priority order: endgame centre, zone, retreat, fight, stay away from mechs
   we are not fighting, loot, explore or hold.
3. **Steer.** Seventeen candidate directions are scored: progress along the nav path,
   strafing across the threat's line with random direction changes, backing off, walls
   and ledges, the zone edge, and predicted hits from every visible projectile (our motion
   is integrated with the acceleration limit for one second; mortar shells are flown to
   their impact point).
4. **Guns** (`gunnery.rs`), 5. loot actions (`tasks.rs`), 6. upgrade pick.

`mover.rs` — nav paths from `crates/world` (re-planned twice a second), hops on and off
roofs and out of enclosed regions through the map's access points (walk to the edge, jump,
jet until the feet clear the crest, move over), and stuck recovery that escalates: jump
and re-plan, sidestep, jump and jet over the obstacle, back off. Five nav grids are built
at hello for a mech at 0, 2, 5, 9 and 14 cubes, because a mech grows 5% per cube and stops
fitting under the low bridges.

### Gunnery

* **Gun:** iterative intercept on the target's centre of mass (or near its top when only
  the top is visible) with the bullet speed from the weapon's own stats, the 3 m barrel
  offset, and the latency between observation and shot. On long shots the lead blends
  towards the target's smoothed velocity, which is the better predictor for a jinking
  target. The trigger is pulled only when the *actual* torso aim, plus the slew it gets
  before the shot, is inside the target's angular size, the line of fire is clear in the
  world raycast, and the lead point is in range.
* **Laser:** hold to charge, aim directly with a one-tick lead, release when fully charged
  and settled; release early if the partial charge already kills. During the charge the
  gun keeps the aim until the last 0.3 s.
* **Lightning:** fired when a target is inside the range and a conservative part of the
  cone with a clear line, keeping an energy reserve unless the target is nearly dead.
* **Mortar:** closed-form low and high arcs from the tube, corrected for the barrel
  offset, led with the smoothed velocity, aimed at the ground under the target; the arc is
  sampled against the world, so the high arc lobs over a building at a mech the server
  still tracks behind it. Not closer than our own splash, and only at slow or close
  targets when the gun could shoot instead.
* **Reload** at once when empty; early only when no enemy is about, or, with one about but
  out of sight, when the magazine is nearly empty.
* **Facing:** with nothing to shoot the torso faces whoever last hurt us (rear hits take
  ×1.5), then the last known enemy, then recent weapon sounds, and otherwise sweeps so
  the 120° view wedge covers the full circle every few seconds.

## Strategy, and why

**What scores.** Placement is death order. In the practice run the 15 house bots are one
squad that never fights itself, so before the final circle nobody dies unless we kill it:
our place is roughly 16 minus our kills at the moment we die, unless we live to the end.
Kills are the points; survival is what buys more kills.

Everything below was shaped on the local simulator (`crates/sim`), which is a guess at the
server and at the house bots. Each rule is there because switching it off cost points over
600–1500 simulated matches; the numbers in brackets are that difference in mean points.

**The laser decides the match** [about +1.5]. Hitscan, 315 damage every 2.4 s at any range,
on top of the gun: with a laser a kill takes half as long. Matches in which the bot gets a
laser average 5.4 points in the simulator, the others 2.0. So:

* weapon values are laser ≫ mortar > lightning (`w_laser`, `w_mortar`, `w_lightning`), and
  a laser replaces whichever special weapon we carry;
* for a laser the bot walks further (`laser_range`), closer past mechs it is not fighting
  (`laser_keep`), and up onto roofs;
* it keeps walking to a known laser **during** a fight and past bystanders, as long as the
  straight way there stays `errand_clear` metres from every enemy (`laser_run`);
* while it knows of a laser it holds none of, it starts no fight beyond
  `unarmed_strike_range`;
* the laser keeps charging while we back away (charging halves our speed, which only
  matters next to a lightning carrier).

**Fight at long range** [+0.5]: `range_gun_min`–`range_gun_max` = 140–190 m. Inaccurate
opponents hit in proportion to our angular size, our own gun and laser still hit, and
visible bullets take 0.6 s to arrive. An enemy that closes in is kited: the legs back away
while the torso keeps firing. Kiting never walks into a wall [`wall_penalty`], out of the
zone, or towards other mechs [`crowd_avoid`, +0.2].

**Stay out of lightning arcs** [+0.25]. Lightning never misses inside 45 m and does 240
damage per second. Every known carrier is a repulsor in the steering, in every mode, and
inside arc range + 8 m the bot boosts away.

**Shoot first, at what is coming** [+0.2]. A mech that walks towards us or lines us up
inside `first_strike_range` is one of the squad on its way to shoot us; it is attacked
before it opens fire (practice mode only). A mech in plain sight that one more burst will
finish is always shot (`kill_shot_range`). Having opened fire we stay on the target for
`hunt_commit_s`: a hunt that stops when a condition flickers has only made an enemy.
Bystanders that are neither attacking nor coming are still left alone and kept at
`avoid_range` (with hysteresis), walking away from them even when there is no cover.

**No running in practice mode** [+0.25]. The squad shares vision and keeps coming, cover is
seldom close enough to regenerate behind, and a retreat in the open gives away free hits.
`retreat_hp`, `flee_margin` and `max_enemies` are therefore off by default; the retreat
code (cover search, last stand) is still there and is used by the free-for-all preset.

**Drones shoot back at.** A drone that hits us is shot when no mech can be: it stops the
damage, which also blocks regeneration, and drops two cubes. Ignored drones used to kill
a bot that was hiding from mechs.

**Loot.** Cubes, crates, drones and supply drops as before, valued per second of walking;
the current values are a local optimum in the simulator. A task is judged leniently once
chosen, so that a mech drifting near the goal does not send the bot back and forth. Loot
outside the next circle is judged at the loot (will the circle have passed it when we get
there?), never at where we stand — that rule used to flip as the bot crossed the line and
had it pacing up and down. Loot the bot should see and does not (close by, clear line
past buildings and tree trunks) is forgotten after 0.6 s and not believed again from afar.

**Zone.** A walking-distance field to the inside of the next circle is rebuilt each phase.
We leave when the closing edge would reach our position within a margin plus the detour
the buildings force, and once on the way in we keep going until properly inside, fight or
no fight. In the last phase the circle closes to nothing: the bot goes to the centre and
stays there.

**Free-for-all (`MECH_MODE=ffa`).** For the final against other teams' bots: never starts
a fight with a healthy mech, only finishes weak ones; retreats to cover when hurt or
outnumbered, re-engages late. In a small circle the distance kept from bystanders shrinks
with the circle, and the bot hides only where the zone lets it stay.

## What is verified and what is not

Verified here:

* unit tests (intercept, mortar, laser damage, aim settling, tuning overrides);
* protocol tests (recorded stream through the in-process session, garbage input, missing
  map, the real binary over pipes including queued observations and exit);
* behaviour tests (`crates/bot/tests/behaviour.rs`): one small synthetic situation per bug
  the simulator exposed — loot memory and ghosts, destroyed drones, shooting back at
  drones, the loot rule at the next circle's edge, the laser while kiting, lightning arcs,
  first strike only in practice mode, kill shots, no running from a single attacker;
* `check.py` on the native binary and on the amd64 image in the match sandbox.

Closed-loop numbers, all from `crates/sim` (see its README; they do not predict the
leaderboard — the kit's example bot scores about 0.3 there and 1.15 in the real practice
run):

| | mean of 16 − place |
|---|---|
| bot as first written, against the simulated squad | 2.4 |
| this version, against the simulated squad (1500 matches) | 4.4 |
| this version, starting with a laser (the ceiling of "get a laser sooner") | 5.4 |
| this version, 15 scripted bots each for itself | 13.1 |

Open problems, largest first:

* **No laser in 28% of matches** (15% never see one, 13% see one and do not reach it);
  those matches average 2.0 points. Looking for one deliberately, and killing laser
  carriers for theirs, is the obvious next step.
* **Three or more attackers at once** is still simply lost. A rule that moves to a spot
  hidden from all but the current target exists (`split_cover`) but made no difference
  in the simulator and is off.
* **Lightning in our own hands** is worth nothing as played (rushing costs as much as it
  gains), so it is valued low. A corner ambush would be the way to use it.
* The free-for-all preset cannot be judged here: the scripted free-for-all is far too
  bloody, and sixteen copies of this bot mostly stand apart until the last circle.
* The final centre is taken as the nearest walkable ground; a roof over the centre is not
  used.

Things to look at after the first real practice score:

* If the real squad's non-hunters ignore us until shot at, `first_strike_range` and the
  hunt rules are worth more; if they shoot on sight from far away, raise `avoid_range`.
* `range_gun_min` / `range_gun_max` against how well the house bots aim at 150 m.
* `retreat_hp` and `flee_margin` if the real hunters lose track of a hidden mech.

## License

MIT, see `LICENSE`.
