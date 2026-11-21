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
cargo test  --release -p bot                 # intercept and ballistics math, protocol loop
python3 <kit>/check/check.py -- "$PWD/target/release/bot"

docker build -t mech-bot .                   # about 30 s on linux/amd64
python3 <kit>/check/check.py --map-dir /arena -- docker run --rm -i --network none \
    --cpus 1 --memory 512m --read-only --tmpfs /tmp:rw,size=64m --user 65534:65534 \
    -v "$PWD/arena:/arena:ro" mech-bot

# a local match against the simulator's guess of the house squad
cargo build --release -p sim -p bot
target/release/sim --bot target/release/bot --fill squad --seed 1 --matches 20
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
(aggression, ranges, retreat thresholds, loot values, timing margins), next to the
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
Kills are the points, survival is what buys more kills, and the squad's weakest come first.

**Fight only what attacks us, one at a time.** The first version shot every mech it saw;
in the simulator that turned bystanders into attackers and it died in a crossfire within
30 seconds. Now a mech is an enemy once it has actually hit us (or charges a laser at us).
We start a fight ourselves only with a mech that stands alone, that we can see, and that
is either already weak or clearly weaker than us (cubes, second weapon, or it has shown
poor aim). Since the squad sends its weakest first, a mech that is *not* hunting us is
probably a better one. Mechs we are not fighting are kept at a distance (`avoid_range`),
and loot next to them is ignored.

**The duel estimate decides fight or flight.** For every enemy we keep the damage it dealt
to us per second while it was shooting at us, and ours on it. From that:
`margin = (our HP / incoming DPS) / (target HP / our DPS)`. Below `flee_margin` we break
off even at high HP (a laser carrier out-trading us at 120 m); above `press_margin` we
keep fighting even at low HP (a hunter that cannot hit). In between, the HP thresholds
apply. Two or more attackers mean retreat unless the margin is overwhelming.

**Retreat means breaking line of sight, at a distance.** Cover points come from the nav
grid: hidden from the main threat, preferably also from where it would be after a
sidestep and from the other enemies, inside the zone, and not where the enemy can walk
up to us — 45 m away, more from a lightning carrier. Out of sight for `regen_delay`
seconds we regenerate 45 HP/s, and we do not fire at what cannot see us (dealing damage
restarts the clock). If we are still being shot after 2.5 s the cover is not working: we
turn and take the weakest one with us, because a kill is a point.

**Range.** Lightning never misses inside 45 m, whoever pilots it, so we stay about 60 m or
more from any lightning carrier unless we carry one too and have the HP to win. Otherwise
we fight at 70–120 m, strafing: at that range visible bullets can be side-stepped and
weak bots without lead miss a jinking target, while our lead still hits. An enemy that
closes in is kited: the legs back away at full speed while the torso keeps facing and
firing.

**Finish.** A target below 30% HP is chased with boost to close range before it can break
away and regenerate, and it outranks a fresh attacker when choosing whom to shoot. Its
cubes (all it had plus two) are picked up during the fight when they lie within reach.

**Get stronger while it is quiet.** Weapon pickups first (laser > lightning > mortar,
shifted by the weapon triangle against what the enemies we have seen carry; a mortar is
worth little because carrying one makes enemy lasers hit 60% harder), then cubes, crates
and drones (shooting them restarts regeneration, so only when healthy and nobody is near),
supply drops when uncontested (three cubes, full HP and energy). With nothing known, the
bot walks around inside the next circle looking for loot until it has a second weapon and
a few cubes. Upgrades: max HP first (it also counts in the final zone race, where armor
does not), then armor, regeneration, damage. Cubes stop adding HP and damage at 10 but
keep growing the mech, so collecting stops there.

**Zone.** A walking-distance field to the inside of the next circle is rebuilt each phase.
We leave when the closing edge would reach our position within a margin plus the detour
the buildings force, or when the walk would not finish before the shrink ends; outside the
zone we boost. In the last phase the circle closes to nothing and its damage ignores
armor: every metre from the centre is a second more of 260 HP/s, so the bot goes to the
centre and stays there.

**Free-for-all (`MECH_MODE=ffa`).** For the final against other teams' bots: never starts
a fight with a healthy mech, only finishes weak ones; retreats earlier, re-engages later,
keeps a longer range. Other bots kill each other there, so outlasting them is worth more
than kills.

## What is verified and what is not

Verified here: the unit tests (intercept, mortar, laser damage, aim settling, tuning
overrides), the protocol tests (recorded stream through the in-process session, garbage
input, missing map, the real binary over pipes including queued observations and exit),
`check.py` on the native binary and on the amd64 image in the match sandbox.

The only closed-loop feedback was `crates/sim`, which is a guess at the real server and
at the house bots. There the bot scored about 2.1–2.6 points per match against the
simulated squad, where the kit's example bot scored 0.33 and the simulator's own "expert"
script 0.70; against idle opponents it won every full-length match, and six copies in
ffa mode played to the final circle with no late answers. The simulator is harsher than
the real practice run (the example bot scores 1.15 there), so these numbers say that the
bot plays coherently, not what it will score. Parameter sweeps on the simulator moved
the mean by less than its noise, so the defaults are judgement, not fits.

Not exercised against real opponents: mortar and laser use by us in volume, roof hops for
loot, the endgame centre race under fire.

Things to look at after the first practice score:

* If non-hunting house bots really ignore us until shot at, raise `aggression` and lower
  `hunt_power_ratio`; if they shoot on sight from far away, raise `avoid_range`.
* `range_gun_min` / `range_gun_max` against how well the house bots lead.
* The final centre is taken as the nearest walkable ground; a roof over the centre is not
  used yet.

## License

MIT, see `LICENSE`.
