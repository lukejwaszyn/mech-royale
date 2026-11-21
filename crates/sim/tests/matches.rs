//! Whole matches on the real map: scripted bots, and process bots over stdin / stdout.
use serde_json::Value;
use sim::config::{Config, DEFAULT_CONFIG_JSON};
use sim::runner::{run_match, BotSpec, Env, MatchResult, MatchSpec, RunOpts};
use std::path::Path;
use std::sync::Arc;
use world::World;

fn env_with(f: impl Fn(&mut Value)) -> Env {
    let arena = std::env::var("MECH_ARENA").map(std::path::PathBuf::from).unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../arena"));
    let world = Arc::new(World::load(&arena).expect("map (set MECH_ARENA)"));
    let mut v: Value = serde_json::from_str(DEFAULT_CONFIG_JSON).unwrap();
    f(&mut v);
    Env::new(world, Arc::new(Config::from_value(v).unwrap()))
}
fn scripted(n_first: usize) -> Vec<BotSpec> {
    let mut v = vec![BotSpec::Scripted(n_first)];
    for k in 0..15 {
        v.push(BotSpec::Scripted(k % 4));
    }
    v
}
fn check_places(r: &MatchResult) {
    assert_eq!(r.bots.iter().filter(|b| b.place == 1).count(), 1, "exactly one winner");
    for b in &r.bots {
        assert!((1..=16).contains(&b.place), "{} has place {}", b.name, b.place);
    }
    // death order = placement order
    let mut dead: Vec<_> = r.bots.iter().filter_map(|b| b.death.as_ref().map(|d| (d.t, b.place))).collect();
    dead.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    for w in dead.windows(2) {
        assert!(w[0].1 >= w[1].1, "earlier death must not place better: {:?}", w);
    }
    assert!(r.t_end <= 480.0 + 0.11);
}

#[test]
fn scripted_squad_match_completes_and_is_deterministic() {
    let env = env_with(|_| {});
    let spec = MatchSpec { bots: scripted(1), focus: 0, squad: true };
    let opts = RunOpts::default();
    let a = run_match(&env, &spec, &opts, 3).unwrap();
    check_places(&a);
    let b = run_match(&env, &spec, &opts, 3).unwrap();
    let key = |r: &MatchResult| r.bots.iter().map(|b| (b.place, b.kills, b.death.as_ref().map(|d| (d.t * 100.0) as i64))).collect::<Vec<_>>();
    assert_eq!(key(&a), key(&b), "same seed, same match");
    let c = run_match(&env, &spec, &opts, 4).unwrap();
    assert_ne!(key(&a), key(&c), "another seed, another match");
    // squad members never kill each other
    for (i, bot) in a.bots.iter().enumerate().skip(1) {
        if let Some(d) = &bot.death {
            assert!(d.by.map_or(true, |k| k == 0 || k >= 100), "squad bot {i} was killed by squad member {:?}", d.by);
        }
    }
}

#[test]
fn free_for_all_has_fights_and_one_winner() {
    let env = env_with(|_| {});
    let spec = MatchSpec { bots: scripted(2), focus: 0, squad: false };
    let r = run_match(&env, &spec, &RunOpts::default(), 11).unwrap();
    check_places(&r);
    assert!(r.bots.iter().map(|b| b.kills).sum::<u32>() >= 5, "scripted bots do kill each other in a free-for-all");
}

const GOOD: &str = r#"/bin/sh -c 'while IFS= read -r l; do case "$l" in *"\"type\":\"hello\""*) echo "{\"type\":\"ready\",\"name\":\"sh-bot\"}";; *"\"type\":\"obs\""*) t=$(printf "%s" "$l" | sed -E "s/.*\"tick\":([0-9]+).*/\1/"); echo "{\"tick\":$t,\"move\":[1,0],\"fire\":[true,false]}";; *"\"type\":\"end\""*) exit 0;; esac; done'"#;
const WRONG_TICK: &str = r#"/bin/sh -c 'while IFS= read -r l; do case "$l" in *"\"type\":\"hello\""*) echo "{\"type\":\"ready\",\"name\":\"wrong\"}";; *"\"type\":\"obs\""*) echo "{\"tick\":999999,\"move\":[1,0]}";; *"\"type\":\"end\""*) exit 0;; esac; done'"#;
const GARBAGE: &str = r#"/bin/sh -c 'while IFS= read -r l; do case "$l" in *"\"type\":\"hello\""*) echo "{\"type\":\"ready\",\"name\":\"garbage\"}";; *"\"type\":\"obs\""*) echo "thinking...";; *"\"type\":\"end\""*) exit 0;; esac; done'"#;
const CRASH: &str = r#"/bin/sh -c 'IFS= read -r l; echo "{\"type\":\"ready\",\"name\":\"crash\"}"; IFS= read -r l; exit 3'"#;
const SILENT: &str = r#"/bin/sh -c 'while IFS= read -r l; do :; done'"#;

#[test]
fn process_bots_protocol_is_enforced_and_reported() {
    if !Path::new("/bin/sh").exists() {
        return;
    }
    let env = env_with(|v| v["bot"]["hello_budget_ms"] = 1500.into());
    let mut bots = vec![BotSpec::Process(GOOD.into()), BotSpec::Process(WRONG_TICK.into()), BotSpec::Process(GARBAGE.into()), BotSpec::Process(CRASH.into()), BotSpec::Process(SILENT.into())];
    while bots.len() < 16 {
        bots.push(BotSpec::Idle);
    }
    let spec = MatchSpec { bots, focus: 0, squad: false };
    // a generous budget for the good bot (it forks `sed` for every line), but small enough
    // that the broken ones do not take forever
    let opts = RunOpts { budget_ms: Some(400.0), max_t: Some(1.5), map_dir: "/arena".into(), ..RunOpts::default() };
    let r = run_match(&env, &spec, &opts, 5).unwrap();
    let p = |i: usize| r.bots[i].proto.clone().unwrap();

    let good = p(0);
    assert_eq!(r.bots[0].name, "sh-bot");
    assert_eq!(good.sent, 15);
    assert_eq!(good.answered, good.sent, "{good:?}");
    assert_eq!(good.problems(), 0, "{good:?}");

    let wrong = p(1);
    assert_eq!((wrong.answered, wrong.late), (0, wrong.sent));
    assert!(wrong.wrong_tick >= 1 && wrong.bad_json == 0, "{wrong:?}");
    assert!(wrong.idle_decisions >= 1, "idles after coast_s without answers: {wrong:?}");

    let garbage = p(2);
    assert!(garbage.bad_json >= 1 && garbage.answered == 0, "{garbage:?}");

    let crash = p(3);
    assert!(crash.crashed_at.is_some() && !crash.no_ready, "{crash:?}");

    let silent = p(4);
    assert!(silent.no_ready && silent.answered == 0, "{silent:?}");
    assert!(!silent.no_exit, "exits when stdin closes");
}

#[test]
fn late_answers_are_dropped_and_the_mech_coasts() {
    if !Path::new("/bin/sh").exists() {
        return;
    }
    let env = env_with(|_| {});
    // answers correctly, but only after 80 ms
    let slow = GOOD.replace("echo \"{\\\"tick\\\":$t", "sleep 0.08; echo \"{\\\"tick\\\":$t");
    assert_ne!(slow, GOOD);
    let mut bots = vec![BotSpec::Process(slow)];
    while bots.len() < 16 {
        bots.push(BotSpec::Idle);
    }
    let spec = MatchSpec { bots, focus: 0, squad: false };
    let opts = RunOpts { budget_ms: Some(30.0), max_t: Some(1.0), ..RunOpts::default() };
    let r = run_match(&env, &spec, &opts, 5).unwrap();
    let s = r.bots[0].proto.clone().unwrap();
    assert_eq!(s.answered, 0, "{s:?}");
    assert_eq!(s.late, s.sent, "{s:?}");
    assert!(s.stale >= 1, "the late lines show up as stale answers: {s:?}");
    // with --no-enforce the same answers are used, but still counted as late
    let opts = RunOpts { budget_ms: Some(30.0), enforce: false, wait_ms: 2000.0, max_t: Some(1.0), ..RunOpts::default() };
    let r = run_match(&env, &spec, &opts, 5).unwrap();
    let s = r.bots[0].proto.clone().unwrap();
    assert_eq!((s.late, s.stale), (s.sent, 0), "{s:?}");
    assert!(r.bots[0].dmg_dealt >= 0.0);
}

/// Our own bot driven in-process (`--inproc`): it plays a squad match to its death with
/// no protocol problems, the same seed gives the same match, and the statistics that
/// `--detail` reports add up.
#[test]
fn inproc_bot_plays_a_squad_match_and_statistics_add_up() {
    let env = env_with(|_| {});
    let mut bots = vec![BotSpec::InProc(String::new())];
    for k in 0..15 {
        bots.push(BotSpec::Scripted(k % 4));
    }
    let spec = MatchSpec { bots, focus: 0, squad: true };
    let arena = std::env::var("MECH_ARENA").map(std::path::PathBuf::from).unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../arena"));
    let opts = RunOpts { map_dir: arena.to_string_lossy().into_owned(), stop_when_focus_dies: true, ..RunOpts::default() };
    let a = run_match(&env, &spec, &opts, 11).unwrap();
    let f = &a.bots[0];
    let p = f.proto.as_ref().expect("protocol statistics for the in-process bot");
    assert!(p.sent > 50 && p.answered == p.sent, "every observation answered ({} of {})", p.answered, p.sent);
    assert_eq!(p.problems(), 0);
    assert!(!f.diag.is_empty(), "the bot's own counters are reported");
    // damage taken = sum over attackers + zone + drones; and per weapon
    let by_weapon: f32 = f.dmg_by_weapon.iter().map(|x| x.1).sum();
    assert!((by_weapon - f.dmg_taken).abs() < 1.0, "per-weapon damage {} vs total {}", by_weapon, f.dmg_taken);
    let from_mechs: f32 = f.dmg_from.iter().sum();
    assert!(from_mechs <= f.dmg_taken + 1.0);
    assert!(f.gun_hits <= f.gun_shots);
    // the second match reuses the nav grids of the first and is identical
    let b = run_match(&env, &spec, &opts, 11).unwrap();
    assert_eq!((f.place, f.kills, f.death.as_ref().map(|d| (d.t * 10.0) as i64)), (b.bots[0].place, b.bots[0].kills, b.bots[0].death.as_ref().map(|d| (d.t * 10.0) as i64)));
}
