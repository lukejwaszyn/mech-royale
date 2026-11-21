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
