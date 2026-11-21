//! The protocol loop (protocol.md §2-§6).
//!
//! * [`Session`]: the pure line-in / line-out core (hello -> ready, obs batch -> action).
//!   It never panics outward: any failure inside a decision still yields a valid action.
//! * [`run_stdio`]: three threads around a `Session` —
//!   a reader (stdin lines, batched so queued observations arrive together),
//!   the brain (owns the `Session`), and the coordinator (this thread) which writes
//!   stdout and enforces a hard deadline: if the brain has not answered in time, a
//!   coasting action goes out for that tick instead, so an answer is never late.
use crate::brain::Bot;
use crate::config::Tuning;
use crate::proto::*;
use std::io::{BufRead, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

pub const DEFAULT_NAME: &str = "tin-buckeye";

pub fn ready_line(name: &str) -> String {
    format!("{{\"type\":\"ready\",\"name\":{}}}", serde_json::to_string(name).unwrap_or_else(|_| "\"bot\"".into()))
}

/// The action the server would apply anyway if we said nothing: keep the continuous
/// parts of the last action, drop the one-shots.
pub fn coast(last: Option<&Action>, tick: i64) -> Action {
    match last {
        Some(a) => Action { tick, mv: a.mv, boost: a.boost, jump: false, aim: a.aim, fire: a.fire, reload: false, pick: None, take: None, drop: None, debug: None },
        None => Action { tick, ..Default::default() },
    }
}

pub struct Session {
    pub bot: Option<Bot>,
    tune: Tuning,
    last: Option<Action>,
    pub panics: u32,
}

impl Session {
    pub fn new(tune: Tuning) -> Session {
        Session { bot: None, tune, last: None, panics: 0 }
    }

    /// Handle `hello`; returns the `ready` line. Loading the map and building the nav
    /// grids happens here.
    pub fn hello(&mut self, line: &str) -> String {
        let tune = self.tune.clone();
        let built = catch_unwind(AssertUnwindSafe(|| {
            let hello: Hello = match serde_json::from_str(line) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("[bot] hello did not parse ({e}); using defaults");
                    Hello::default()
                }
            };
            Bot::new(&hello, tune)
        }));
        match built {
            Ok(b) => {
                let name = b.name();
                self.bot = Some(b);
                ready_line(name)
            }
            Err(_) => {
                eprintln!("[bot] init panicked; answering with safe actions only");
                ready_line(DEFAULT_NAME)
            }
        }
    }

    /// Answer a batch of queued observation lines: older ones only contribute their
    /// events, the newest one gets the action. `None` only if no line carries a tick.
    pub fn step(&mut self, batch: &[&str]) -> Option<Action> {
        let (newest, older) = batch.split_last()?;
        for line in older {
            if let (Some(bot), Ok(obs)) = (self.bot.as_mut(), serde_json::from_str::<Obs>(line)) {
                let _ = catch_unwind(AssertUnwindSafe(|| bot.absorb(&obs)));
            }
        }
        let obs: Obs = match serde_json::from_str(newest) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("[bot] observation did not parse: {e}");
                let env: Envelope = serde_json::from_str(newest).ok()?;
                let a = coast(self.last.as_ref(), env.tick? as i64);
                return Some(a);
            }
        };
        let tick = obs.tick as i64;
        let action = match self.bot.as_mut() {
            Some(bot) => match catch_unwind(AssertUnwindSafe(|| bot.decide(&obs))) {
                Ok(a) => a,
                Err(_) => {
                    self.panics += 1;
                    if self.panics <= 5 {
                        eprintln!("[bot] decision panicked at tick {tick}; coasting");
                    }
                    coast(self.last.as_ref(), tick)
                }
            },
            None => {
                // No bot (init failed): walk to the next circle, look where we go.
                let z = &obs.zone;
                let d = [z.next_center[0] - obs.me.pos[0], z.next_center[1] - obs.me.pos[1]];
                let l = d[0].hypot(d[1]);
                let mv = if l > 5.0 { [d[0] / l, d[1] / l] } else { [0.0, 0.0] };
                let mut a = Action { tick, mv, aim: [d[1].atan2(d[0]), 0.0], pick: obs.offer.as_ref().map(|_| 0), ..Default::default() };
                a.sanitize();
                a
            }
        };
        self.last = Some(action.clone());
        Some(action)
    }
}

enum Msg {
    Lines(Vec<String>, Instant),
    Eof,
    Ready(String),
    Answer(Action),
}
enum Work {
    Hello(String),
    Obs(Vec<String>),
}

#[derive(Default)]
struct IoStats {
    answered: u32,
    fallbacks: u32,
    sum_ms: f64,
    max_ms: f64,
}

fn write_line(out: &mut impl Write, line: &str) -> bool {
    out.write_all(line.as_bytes()).and_then(|_| out.write_all(b"\n")).and_then(|_| out.flush()).is_ok()
}

/// Run the bot on stdin/stdout until `end` or EOF. Returns the process exit code.
pub fn run_stdio(tune: Tuning) -> i32 {
    let (tx, rx) = channel::<Msg>();
    let (work_tx, work_rx) = channel::<Work>();

    // ---- reader: stdin -> batches of lines
    let tx_r = tx.clone();
    std::thread::Builder::new()
        .name("reader".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut r = std::io::BufReader::with_capacity(1 << 18, stdin.lock());
            loop {
                let mut batch = Vec::new();
                let mut line = String::new();
                match r.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        let _ = tx_r.send(Msg::Eof);
                        return;
                    }
                    Ok(_) => batch.push(line),
                }
                let at = Instant::now();
                // Everything already buffered belongs to the same batch (queued observations).
                while r.buffer().contains(&b'\n') {
                    let mut more = String::new();
                    match r.read_line(&mut more) {
                        Ok(n) if n > 0 => batch.push(more),
                        _ => break,
                    }
                }
                if tx_r.send(Msg::Lines(batch, at)).is_err() {
                    return;
                }
            }
        })
        .expect("spawn reader");

    // ---- brain: owns the session
    let tx_b = tx.clone();
    std::thread::Builder::new()
        .name("brain".into())
        .stack_size(8 << 20)
        .spawn(move || {
            let mut session = Session::new(tune);
            while let Ok(mut work) = work_rx.recv() {
                // Skip to the newest queued work: merge observation batches.
                let mut lines: Vec<String> = Vec::new();
                loop {
                    match work {
                        Work::Hello(l) => {
                            let ready = session.hello(&l);
                            let _ = tx_b.send(Msg::Ready(ready));
                        }
                        Work::Obs(mut l) => lines.append(&mut l),
                    }
                    match work_rx.try_recv() {
                        Ok(w) => work = w,
                        Err(_) => break,
                    }
                }
                if !lines.is_empty() {
                    let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
                    if let Some(a) = session.step(&refs) {
                        let _ = tx_b.send(Msg::Answer(a));
                    }
                }
            }
        })
        .expect("spawn brain");
    drop(tx);

    // ---- coordinator
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut stats = IoStats::default();
    let mut budget_ms = 50.0f64;
    let mut ready_sent = false;
    let mut ready_deadline: Option<Instant> = None;
    let mut waiting: Option<(i64, Instant, Instant)> = None; // tick, received at, fallback deadline
    let mut last: Option<Action> = None;
    let started = Instant::now();

    let code = 'outer: loop {
        let deadline = match (waiting.map(|w| w.2), if ready_sent { None } else { ready_deadline }) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let msg = match deadline {
            Some(d) => match rx.recv_timeout(d.saturating_duration_since(Instant::now())) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break 0,
            },
            None => match rx.recv() {
                Ok(m) => Some(m),
                Err(_) => break 0,
            },
        };
        match msg {
            None => {
                let now = Instant::now();
                if let Some((tick, at, d)) = waiting {
                    if now >= d {
                        // The brain is late: coast for this tick so the answer is on time.
                        let a = coast(last.as_ref(), tick);
                        if !write_line(&mut out, &a.to_line()) {
                            break 0;
                        }
                        stats.fallbacks += 1;
                        let ms = at.elapsed().as_secs_f64() * 1000.0;
                        if stats.fallbacks <= 10 {
                            eprintln!("[bot] deadline: coasted tick {tick} after {ms:.1} ms");
                        }
                        waiting = None;
                    }
                }
                if !ready_sent && ready_deadline.map_or(false, |d| now >= d) {
                    eprintln!("[bot] init is slow: sending ready early");
                    if !write_line(&mut out, &ready_line(DEFAULT_NAME)) {
                        break 0;
                    }
                    ready_sent = true;
                }
            }
            Some(Msg::Eof) => break 0,
            Some(Msg::Ready(line)) => {
                if !ready_sent {
                    if !write_line(&mut out, &line) {
                        break 0;
                    }
                    ready_sent = true;
                    eprintln!("[bot] ready after {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
                }
            }
            Some(Msg::Answer(a)) => {
                if let Some((tick, at, _)) = waiting {
                    if a.tick == tick {
                        if !write_line(&mut out, &a.to_line()) {
                            break 0;
                        }
                        let ms = at.elapsed().as_secs_f64() * 1000.0;
                        stats.answered += 1;
                        stats.sum_ms += ms;
                        stats.max_ms = stats.max_ms.max(ms);
                        waiting = None;
                    }
                }
                last = Some(a);
            }
            Some(Msg::Lines(lines, at)) => {
                let mut obs: Vec<String> = Vec::new();
                let mut newest: Option<i64> = None;
                for line in lines {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let env: Envelope = match serde_json::from_str(&line) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    match env.kind.as_str() {
                        "hello" => {
                            if let Ok(h) = serde_json::from_str::<Hello>(&line) {
                                budget_ms = h.config.bot.tick_budget_ms.max(5.0) as f64;
                                let hb = (h.config.bot.hello_budget_ms as f64 * 0.6).clamp(1000.0, 12000.0);
                                ready_deadline = Some(started + Duration::from_millis(hb as u64));
                            } else {
                                ready_deadline = Some(started + Duration::from_millis(10000));
                            }
                            let _ = work_tx.send(Work::Hello(line));
                        }
                        "obs" => {
                            if let Some(t) = env.tick {
                                newest = Some(t as i64);
                                obs.push(line);
                            }
                        }
                        "end" => {
                            eprintln!("[bot] end: {}", line.trim());
                            break 'outer 0;
                        }
                        _ => {}
                    }
                }
                if let Some(tick) = newest {
                    let _ = work_tx.send(Work::Obs(obs));
                    // Answer by 60% of the budget, leaving the rest for pipes and scheduling.
                    waiting = Some((tick, at, at + Duration::from_micros((budget_ms * 600.0) as u64)));
                }
            }
        }
    };
    let n = stats.answered.max(1) as f64;
    eprintln!(
        "[bot] exit: answered {} (avg {:.2} ms, max {:.1} ms from line read to answer), deadline coasts {}",
        stats.answered, stats.sum_ms / n, stats.max_ms, stats.fallbacks
    );
    let _ = out.flush();
    code
}
