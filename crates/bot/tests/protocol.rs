//! Protocol loop tests: the in-process `Session` on a recorded stream, and the real
//! binary over pipes (ready, tick echo, newest-observation rule, clean exit).
use bot::runner::{coast, ready_line, Session};
use bot::Tuning;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn arena() -> String {
    std::env::var("MECH_ARENA").unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../arena").to_string_lossy().into_owned()
    })
}

/// (hello with map.dir pointing at the repo arena, observation lines, end line)
fn stream() -> (String, Vec<String>, String) {
    let text = include_str!("fixtures/stream.jsonl");
    let mut lines: Vec<String> = text.lines().filter(|l| !l.trim().is_empty()).map(|l| l.to_string()).collect();
    let end = lines.pop().unwrap();
    let mut hello: Value = serde_json::from_str(&lines.remove(0)).unwrap();
    hello["map"]["dir"] = json!(arena());
    (hello.to_string(), lines, end)
}

fn check_action(a: &Value, tick: i64) {
    assert_eq!(a["tick"].as_i64(), Some(tick), "tick echoed: {a}");
    for key in ["move", "aim"] {
        let v = a[key].as_array().unwrap_or_else(|| panic!("{key} missing: {a}"));
        assert_eq!(v.len(), 2);
        assert!(v.iter().all(|x| x.as_f64().map_or(false, f64::is_finite)), "{key} finite: {a}");
    }
    let m = a["move"].as_array().unwrap();
    let l = m[0].as_f64().unwrap().hypot(m[1].as_f64().unwrap());
    assert!(l <= 1.0001, "move length {l}");
    let fire = a["fire"].as_array().unwrap();
    assert!(fire.len() == 2 && fire.iter().all(Value::is_boolean));
    assert!(a["boost"].is_boolean() && a["jump"].is_boolean() && a["reload"].is_boolean());
    for key in ["pick", "take", "drop"] {
        assert!(a.get(key).map_or(true, Value::is_number), "{key} is a number when present");
    }
    let pitch = a["aim"][1].as_f64().unwrap();
    assert!((-0.71..=1.31).contains(&pitch), "pitch within limits: {pitch}");
}

#[test]
fn session_answers_every_recorded_observation() {
    let (hello, obs, _) = stream();
    let mut s = Session::new(Tuning::default());
    let ready: Value = serde_json::from_str(&s.hello(&hello)).unwrap();
    assert_eq!(ready["type"], "ready");
    assert!(ready["name"].as_str().unwrap().len() <= 32);
    assert!(s.bot.as_ref().unwrap().map_ok, "the repo arena loads");
    let mut worst = 0.0f64;
    for line in &obs {
        let tick = serde_json::from_str::<Value>(line).unwrap()["tick"].as_i64().unwrap();
        let t0 = std::time::Instant::now();
        let a = s.step(&[line.as_str()]).expect("an action");
        worst = worst.max(t0.elapsed().as_secs_f64() * 1000.0);
        let v: Value = serde_json::from_str(&a.to_line()).unwrap();
        check_action(&v, tick);
        assert!(a.to_line().len() < 65536);
    }
    assert_eq!(s.panics, 0);
    eprintln!("worst decision {worst:.2} ms");
}

#[test]
fn session_answers_only_the_newest_of_a_batch() {
    let (hello, obs, _) = stream();
    let mut s = Session::new(Tuning::default());
    s.hello(&hello);
    let batch: Vec<&str> = obs[10..13].iter().map(|l| l.as_str()).collect();
    let newest = serde_json::from_str::<Value>(batch[2]).unwrap()["tick"].as_i64().unwrap();
    let a = s.step(&batch).unwrap();
    assert_eq!(a.tick, newest);
}

#[test]
fn session_survives_garbage_and_missing_fields() {
    let (hello, obs, _) = stream();
    let mut s = Session::new(Tuning::default());
    s.hello(&hello);
    // not JSON at all: nothing to echo
    assert!(s.step(&["this is not json"]).is_none());
    // an observation with nearly everything missing still gets its tick echoed
    let a = s.step(&[r#"{"type":"obs","tick":7}"#]).unwrap();
    assert_eq!(a.tick, 7);
    check_action(&serde_json::from_str(&a.to_line()).unwrap(), 7);
    // wrong types inside: falls back to a coasting action with the right tick
    let a = s.step(&[r#"{"type":"obs","tick":9,"self":{"pos":"oops"}}"#]).unwrap();
    assert_eq!(a.tick, 9);
    // and it keeps working afterwards
    let a = s.step(&[obs[0].as_str()]).unwrap();
    check_action(&serde_json::from_str(&a.to_line()).unwrap(), a.tick);
}

#[test]
fn session_without_a_map_still_plays() {
    let (hello, obs, _) = stream();
    let mut h: Value = serde_json::from_str(&hello).unwrap();
    h["map"]["dir"] = json!("/nonexistent/arena");
    let mut s = Session::new(Tuning::default());
    let ready = s.hello(&h.to_string());
    assert!(ready.contains("\"ready\""));
    assert!(!s.bot.as_ref().unwrap().map_ok);
    for line in obs.iter().take(12) {
        let a = s.step(&[line.as_str()]).unwrap();
        check_action(&serde_json::from_str(&a.to_line()).unwrap(), a.tick);
    }
    // a hello that is not even JSON: defaults, still ready
    let mut s2 = Session::new(Tuning::default());
    assert!(s2.hello("{").contains("\"ready\""));
    assert!(s2.step(&[obs[0].as_str()]).is_some());
}

#[test]
fn coast_keeps_continuous_parts_only() {
    let last = bot::proto::Action { tick: 4, mv: [0.6, -0.8], boost: true, jump: true, aim: [1.0, 0.2], fire: [true, false], reload: true, pick: Some(1), take: Some(8001), drop: Some(1), debug: None };
    let c = coast(Some(&last), 6);
    assert_eq!(c.tick, 6);
    assert_eq!(c.mv, [0.6, -0.8]);
    assert!(c.boost && c.fire[0] && !c.jump && !c.reload);
    assert!(c.pick.is_none() && c.take.is_none() && c.drop.is_none());
    let v: Value = serde_json::from_str(&coast(None, 3).to_line()).unwrap();
    check_action(&v, 3);
    assert_eq!(serde_json::from_str::<Value>(&ready_line("a\"b")).unwrap()["name"], "a\"b");
}

/// The real binary over pipes.
#[test]
fn binary_speaks_the_protocol() {
    let (hello, obs, end) = stream();
    let mut child = Command::new(env!("CARGO_BIN_EXE_bot")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut read = || -> Value {
        let mut l = String::new();
        assert!(out.read_line(&mut l).unwrap() > 0, "bot closed stdout early");
        serde_json::from_str(&l).unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {l}"))
    };
    writeln!(stdin, "{hello}").unwrap();
    assert_eq!(read()["type"], "ready");

    let tick_of = |l: &str| serde_json::from_str::<Value>(l).unwrap()["tick"].as_i64().unwrap();
    // one at a time
    for line in obs.iter().take(20) {
        writeln!(stdin, "{line}").unwrap();
        check_action(&read(), tick_of(line));
    }
    // three queued at once: answers may include older ticks, but the newest must come
    // and nothing after it
    let batch = &obs[20..23];
    stdin.write_all(format!("{}\n{}\n{}\n", batch[0], batch[1], batch[2]).as_bytes()).unwrap();
    stdin.flush().unwrap();
    let newest = tick_of(&batch[2]);
    loop {
        let a = read();
        let t = a["tick"].as_i64().unwrap();
        assert!(batch.iter().any(|l| tick_of(l) == t), "answer to a tick that was never sent: {a}");
        if t == newest {
            break;
        }
    }
    // the next single observation is answered with its own tick: nothing stale is left over
    writeln!(stdin, "{}", obs[23]).unwrap();
    check_action(&read(), tick_of(&obs[23]));

    writeln!(stdin, "{end}").unwrap();
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "clean exit after end: {status:?}");
    let mut rest = String::new();
    out.read_line(&mut rest).unwrap();
    assert!(rest.trim().is_empty(), "nothing on stdout after end: {rest}");
}

#[test]
fn binary_exits_cleanly_on_eof() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_bot")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
}

/// One observation with absurd numbers (a shell at 1e9 m/s, a mech at 1e9 m, our own
/// position or aim at 1e9, a speed of 1e30) is answered like any other, in time, and
/// the bot goes on answering. (Such values once hung the decision for the rest of the
/// match: a ray march and an angle loop that never ended.)
#[test]
fn session_answers_absurd_numbers() {
    let (hello, obs, _) = stream();
    let mut s = Session::new(Tuning::default());
    s.hello(&hello);
    for line in obs.iter().take(5) {
        s.step(&[line.as_str()]).unwrap();
    }
    let base: Value = serde_json::from_str(&obs[5]).unwrap();
    let mut cases: Vec<Value> = Vec::new();
    for big in [1.0e9f64, -1.0e9, 1.0e30, 1.0e300] {
        let mut o = base.clone();
        o["projectiles"] = json!([{"id": 1, "kind": "mortar", "pos": o["self"]["pos"], "vel": [big, big, big]}, {"id": 2, "kind": "gun", "pos": [big, 0, 0], "vel": [big, 0, 0]}]);
        cases.push(o);
        let mut o = base.clone();
        o["mechs"] = json!([{"id": 9, "kind": "mech", "pos": [big, big, 0], "vel": [big, 0, 0], "aim": [big, big], "hp": 1400, "max_hp": 1400, "weapons": ["gun"]}]);
        o["pickups"] = json!([{"id": 8001, "kind": "laser", "pos": [big, 0, 0]}]);
        o["cubes"] = json!([{"id": 5001, "pos": [0, big, 0]}]);
        o["drops"] = json!([{"id": 20000, "pos": [big, big, big], "radius": 8, "lands_in": big, "capture_s": 4}]);
        cases.push(o);
        let mut o = base.clone();
        o["self"]["pos"] = json!([big, big, big]);
        cases.push(o);
        let mut o = base.clone();
        o["self"]["vel"] = json!([big, -big, big]);
        o["self"]["aim"] = json!([big, big]);
        cases.push(o);
        let mut o = base.clone();
        o["zone"] = json!({"center": [big, big], "radius": big, "next_center": [big, 0], "next_radius": big, "shrink_start": big, "shrink_end": big, "dps": big});
        o["self"]["stats"]["min_pitch"] = json!(1.0);
        o["self"]["stats"]["max_pitch"] = json!(-1.0);
        o["self"]["stats"]["walk_speed"] = json!(big);
        cases.push(o);
    }
    let n = cases.len();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for (i, mut o) in cases.into_iter().enumerate() {
            let tick = 1000 + 2 * i as i64;
            o["tick"] = json!(tick);
            o["t"] = json!(tick as f64 * 0.05);
            let t0 = std::time::Instant::now();
            let a = s.step(&[o.to_string().as_str()]).expect("an action");
            let _ = tx.send((i, tick, a.to_line(), t0.elapsed().as_secs_f64() * 1000.0, s.panics));
        }
    });
    for _ in 0..n {
        let (i, tick, line, ms, panics) = rx.recv_timeout(std::time::Duration::from_secs(20)).expect("a decision hung on absurd numbers");
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["tick"].as_i64(), Some(tick));
        assert!(v["move"].as_array().unwrap().iter().chain(v["aim"].as_array().unwrap()).all(|x| x.as_f64().map_or(false, f64::is_finite)), "case {i}: {line}");
        assert_eq!(panics, 0, "case {i} panicked");
        assert!(ms < 400.0, "case {i} took {ms:.0} ms");
    }
}

/// A hello with a float slot, a negative seed and a number sent as a string still
/// loads the map it names. (One such field used to discard the whole hello: no map,
/// a flat world and default rules for the match.)
#[test]
fn hello_with_odd_types_still_loads_the_map() {
    let (hello, obs, _) = stream();
    let mut h: Value = serde_json::from_str(&hello).unwrap();
    h["slot"] = json!(3.0);
    h["seed"] = json!(-12345.5);
    h["config"]["mech"]["walk_speed"] = json!("11.7");
    h["config"]["zone"] = json!("none");
    let mut s = Session::new(Tuning::default());
    assert!(s.hello(&h.to_string()).contains("\"ready\""));
    let bot = s.bot.as_ref().unwrap();
    assert!(bot.map_ok, "the map named by hello is loaded");
    assert_eq!(bot.slot, 3);
    assert_eq!(bot.cfg.mech.walk_speed, 11.7);
    assert!(s.step(&[obs[0].as_str()]).is_some());
}

/// A decision that never returns: the coordinator answers each tick in time with a
/// coasting action, and after a while replaces the brain by a fresh one that plays on.
#[test]
fn binary_replaces_a_hung_brain() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    let (hello, obs, end) = stream();
    let tick_of = |l: &str| serde_json::from_str::<Value>(l).unwrap()["tick"].as_i64().unwrap();
    let hang_at = tick_of(&obs[10]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_bot"))
        .env("MECH_TEST_HANG_AT", hang_at.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let err = BufReader::new(child.stderr.take().unwrap());
    let fresh = Arc::new(AtomicBool::new(false));
    let log = Arc::new(Mutex::new(String::new()));
    let (fresh2, log2) = (fresh.clone(), log.clone());
    let reader = std::thread::spawn(move || {
        for l in err.lines().map_while(Result::ok) {
            if l.contains("fresh brain") {
                fresh2.store(true, Ordering::SeqCst);
            }
            log2.lock().unwrap().push_str(&format!("{l}\n"));
        }
    });
    let mut read = || -> Value {
        let mut l = String::new();
        assert!(out.read_line(&mut l).unwrap() > 0, "bot closed stdout early");
        serde_json::from_str(&l).unwrap()
    };
    writeln!(stdin, "{hello}").unwrap();
    assert_eq!(read()["type"], "ready");
    // Every observation is answered with its own tick within the budget, hung brain or not.
    let mut tick = 0;
    let mut sent = 0;
    let mut after_fresh = 0;
    let t0 = std::time::Instant::now();
    while after_fresh < 15 {
        assert!(t0.elapsed().as_secs() < 120, "no fresh brain after {sent} observations:\n{}", log.lock().unwrap());
        let mut o: Value = serde_json::from_str(&obs[sent % obs.len()]).unwrap();
        tick = if sent < obs.len() { o["tick"].as_i64().unwrap() } else { tick + 2 };
        o["tick"] = json!(tick);
        let at = std::time::Instant::now();
        writeln!(stdin, "{o}").unwrap();
        let a = read();
        check_action(&a, tick);
        assert!(at.elapsed().as_millis() < 200, "answer to tick {tick} took {:?}", at.elapsed());
        sent += 1;
        if fresh.load(Ordering::SeqCst) {
            after_fresh += 1;
        }
    }
    writeln!(stdin, "{end}").unwrap();
    drop(stdin);
    assert!(child.wait().unwrap().success());
    reader.join().unwrap();
    let log = log.lock().unwrap().clone();
    let exit = log.lines().find(|l| l.contains("[bot] exit:")).unwrap_or_else(|| panic!("no exit line:\n{log}"));
    let num_after = |key: &str| exit.split(key).nth(1).and_then(|r| r.split_whitespace().next()).and_then(|x| x.trim_matches(|c: char| !c.is_ascii_digit()).parse::<i64>().ok()).unwrap();
    let (answered, coasts) = (num_after("answered "), num_after("deadline coasts "));
    assert!(answered >= 10 + 10, "the fresh brain answers again ({exit})");
    assert!(coasts >= 30 && answered + coasts == sent as i64, "every tick was answered by the brain or by a coast ({exit}, sent {sent})");
}
