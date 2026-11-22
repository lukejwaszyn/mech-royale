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
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
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
    /// Observations that could not be parsed at all.
    pub bad_obs: u32,
    /// Test hook (`MECH_TEST_HANG_AT=<tick>`, never set in a match): the decision for
    /// that tick never returns, as a decision stuck in a loop would not. It lets the
    /// tests exercise the coordinator's deadline and its hung-brain watchdog.
    hang_at: Option<i64>,
}

impl Session {
    pub fn new(tune: Tuning) -> Session {
        Session { bot: None, tune, last: None, panics: 0, bad_obs: 0, hang_at: std::env::var("MECH_TEST_HANG_AT").ok().and_then(|v| v.parse().ok()) }
    }

    /// Handle `hello`; returns the `ready` line. Loading the map and building the nav
    /// grids happens here.
    pub fn hello(&mut self, line: &str) -> String {
        let tune = self.tune.clone();
        let built = catch_unwind(AssertUnwindSafe(|| {
            let (hello, ok) = Hello::parse(line);
            if !ok {
                crate::logln!("[bot] hello did not parse; using defaults");
            }
            Bot::new(&hello, tune)
        }));
        match built {
            Ok(b) => {
                let name = b.name();
                self.bot = Some(b);
                ready_line(name)
            }
            Err(_) => {
                crate::logln!("[bot] init panicked; answering with safe actions only");
                ready_line(DEFAULT_NAME)
            }
        }
    }

    /// Answer a batch of queued observation lines: older ones only contribute their
    /// events, the newest one gets the action. `None` only if no line carries a tick.
    pub fn step(&mut self, batch: &[&str]) -> Option<Action> {
        let (newest, older) = batch.split_last()?;
        for line in older {
            if let (Some(bot), Some(mut obs)) = (self.bot.as_mut(), Obs::parse(line)) {
                obs.sanitize();
                let _ = catch_unwind(AssertUnwindSafe(|| bot.absorb(&obs)));
            }
        }
        let mut obs: Obs = match Obs::parse(newest) {
            Some(o) => o,
            None => {
                self.bad_obs += 1;
                if self.bad_obs <= 5 {
                    crate::logln!("[bot] observation did not parse; coasting");
                }
                let env: Envelope = serde_json::from_str(newest).ok()?;
                let a = coast(self.last.as_ref(), env.tick? as i64);
                return Some(a);
            }
        };
        obs.sanitize();
        let tick = obs.tick as i64;
        if self.hang_at == Some(tick) {
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        let action = match self.bot.as_mut() {
            Some(bot) => match catch_unwind(AssertUnwindSafe(|| bot.decide(&obs))) {
                Ok(a) => a,
                Err(_) => {
                    self.panics += 1;
                    if self.panics <= 5 {
                        crate::logln!("[bot] decision panicked at tick {tick}; coasting");
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
    /// `ready` line and answers, tagged with the generation of the brain that sent them.
    Ready(String, u32),
    Answer(Action, u32),
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

/// Start a brain thread (it owns a fresh [`Session`]) and return the channel that feeds
/// it. `gen` tags its messages, so that answers of a brain that has been replaced are
/// recognised. The thread ends when the channel is dropped.
fn spawn_brain(tune: Tuning, tx: Sender<Msg>, gen: u32) -> Sender<Work> {
    let (work_tx, work_rx) = channel::<Work>();
    let spawned = std::thread::Builder::new().name("brain".into()).stack_size(8 << 20).spawn(move || {
        let mut session = Session::new(tune);
        while let Ok(mut work) = work_rx.recv() {
            // Skip to the newest queued work: merge observation batches.
            let mut lines: Vec<String> = Vec::new();
            loop {
                match work {
                    Work::Hello(l) => {
                        let ready = session.hello(&l);
                        let _ = tx.send(Msg::Ready(ready, gen));
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
                    let _ = tx.send(Msg::Answer(a, gen));
                }
            }
        }
    });
    if spawned.is_err() {
        crate::logln!("[bot] could not start a brain thread");
    }
    work_tx
}

/// Deadline coasts in a row (with the brain initialised) after which the brain is taken
/// for hung and replaced by a fresh one, and how often that may happen in a match. A
/// hung thread cannot be stopped; it keeps its memory and competes for the one CPU, so
/// this is a last resort - but a mech that coasts to the end of the match is worse.
const HUNG_COASTS: u32 = 30;
const MAX_RESPAWNS: u32 = 2;

/// Run the bot on stdin/stdout until `end` or EOF. Returns the process exit code.
pub fn run_stdio(tune: Tuning) -> i32 {
    crate::log::install_panic_hook();
    let (tx, rx) = channel::<Msg>();

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
    let mut work_tx = spawn_brain(tune.clone(), tx.clone(), 0);

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
    // hung-brain watchdog
    let mut hello_line: Option<String> = None;
    let mut gen = 0u32;
    let mut brain_ready = false;
    let mut coasts_in_a_row = 0u32;

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
                            crate::logln!("[bot] deadline: coasted tick {tick} after {ms:.1} ms");
                        }
                        waiting = None;
                        // A brain that has finished its init and then stays silent for
                        // this many decisions is stuck in a loop: start a fresh one from
                        // the hello line. (Not while it is still loading the map.)
                        if brain_ready {
                            coasts_in_a_row += 1;
                        }
                        if coasts_in_a_row >= HUNG_COASTS && gen < MAX_RESPAWNS {
                            if let Some(h) = hello_line.clone() {
                                gen += 1;
                                crate::logln!("[bot] brain silent for {coasts_in_a_row} decisions: starting a fresh one ({gen})");
                                work_tx = spawn_brain(tune.clone(), tx.clone(), gen);
                                let _ = work_tx.send(Work::Hello(h));
                                brain_ready = false;
                                coasts_in_a_row = 0;
                            }
                        }
                    }
                }
                if !ready_sent && ready_deadline.map_or(false, |d| now >= d) {
                    crate::logln!("[bot] init is slow: sending ready early");
                    if !write_line(&mut out, &ready_line(DEFAULT_NAME)) {
                        break 0;
                    }
                    ready_sent = true;
                }
            }
            Some(Msg::Eof) => break 0,
            Some(Msg::Ready(line, g)) => {
                if g == gen {
                    brain_ready = true;
                    coasts_in_a_row = 0;
                    if g > 0 {
                        crate::logln!("[bot] fresh brain {g} is ready");
                    }
                }
                if !ready_sent {
                    if !write_line(&mut out, &line) {
                        break 0;
                    }
                    ready_sent = true;
                    crate::logln!("[bot] ready after {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
                }
            }
            Some(Msg::Answer(a, g)) => {
                if g != gen {
                    continue; // from a brain that has been replaced
                }
                coasts_in_a_row = 0;
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
                            // The early-ready fallback counts from the arrival of hello,
                            // not from process start: a server that starts every container
                            // first and says hello later would otherwise find the deadline
                            // already passed, and the mech would idle through the map load.
                            let (h, ok) = Hello::parse(&line);
                            let hb = if ok {
                                budget_ms = (h.config.bot.tick_budget_ms as f64).clamp(5.0, 1000.0);
                                (h.config.bot.hello_budget_ms as f64 * 0.6).clamp(1000.0, 12000.0)
                            } else {
                                10000.0
                            };
                            ready_deadline = Some(at + Duration::from_millis(hb as u64));
                            hello_line = Some(line.clone());
                            let _ = work_tx.send(Work::Hello(line));
                        }
                        "obs" => {
                            if let Some(t) = env.tick {
                                newest = Some(t as i64);
                                obs.push(line);
                            }
                        }
                        "end" => {
                            crate::logln!("[bot] end: {}", line.trim());
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
    crate::logln!(
        "[bot] exit: answered {} (avg {:.2} ms, max {:.1} ms from line read to answer), deadline coasts {}",
        stats.answered, stats.sum_ms / n, stats.max_ms, stats.fallbacks
    );
    let _ = out.flush();
    code
}
