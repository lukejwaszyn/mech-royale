//! Process bots: spawn a command and speak mechfight/1 to it over stdin / stdout.
//! Every protocol problem is counted in `ProtoStats`.
use crate::sim::Cmd;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// Split a command line like a shell would (quotes and backslashes, no expansion).
pub fn split_cmd(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut quote: Option<char> = None;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' && matches!(it.peek(), Some('"') | Some('\\')) {
                    cur.push(it.next().unwrap());
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    has = true;
                }
                '\\' => {
                    if let Some(n) = it.next() {
                        cur.push(n);
                        has = true;
                    }
                }
                c if c.is_whitespace() => {
                    if has {
                        out.push(std::mem::take(&mut cur));
                        has = false;
                    }
                }
                c => {
                    cur.push(c);
                    has = true;
                }
            },
        }
    }
    if has {
        out.push(cur);
    }
    out
}

#[derive(Clone, Debug, Default)]
pub struct ProtoStats {
    /// Observations sent.
    pub sent: u64,
    /// Valid answers that arrived inside the budget.
    pub answered: u64,
    /// Decisions with no valid answer inside the budget (the mech coasted).
    pub late: u64,
    /// Answers whose tick was an earlier observation's (dropped).
    pub stale: u64,
    /// Answers with a tick that was never sent, or no tick at all (dropped).
    pub wrong_tick: u64,
    /// Lines that are not valid JSON objects.
    pub bad_json: u64,
    /// Lines over `bot.max_line_bytes`.
    pub oversize: u64,
    /// Valid answers containing a field of the wrong type (the field was ignored).
    pub bad_fields: u64,
    /// Decisions spent idle because the coast time ran out.
    pub idle_decisions: u64,
    pub no_ready: bool,
    pub ready_ms: f64,
    /// The process exited (or closed stdout) before its mech died / the match ended.
    pub crashed_at: Option<f32>,
    /// Still running 3 s after `end` and a closed stdin (killed).
    pub no_exit: bool,
    pub sum_ms: f64,
    pub max_ms: f64,
    pub over_10ms: u64,
    pub over_20ms: u64,
}

impl ProtoStats {
    pub fn problems(&self) -> u64 {
        self.late + self.stale + self.wrong_tick + self.bad_json + self.oversize + self.bad_fields + self.no_ready as u64 + self.crashed_at.is_some() as u64 + self.no_exit as u64
    }
    pub fn merge(&mut self, o: &ProtoStats) {
        self.sent += o.sent;
        self.answered += o.answered;
        self.late += o.late;
        self.stale += o.stale;
        self.wrong_tick += o.wrong_tick;
        self.bad_json += o.bad_json;
        self.oversize += o.oversize;
        self.bad_fields += o.bad_fields;
        self.idle_decisions += o.idle_decisions;
        self.no_ready |= o.no_ready;
        self.ready_ms = self.ready_ms.max(o.ready_ms);
        if self.crashed_at.is_none() {
            self.crashed_at = o.crashed_at;
        }
        self.no_exit |= o.no_exit;
        self.sum_ms += o.sum_ms;
        self.max_ms = self.max_ms.max(o.max_ms);
        self.over_10ms += o.over_10ms;
        self.over_20ms += o.over_20ms;
    }
}

enum Line {
    Text(String, Instant),
    Oversize,
    Eof,
}

pub struct ProcBot {
    pub name: String,
    pub cmdline: String,
    child: Child,
    tx: Option<Sender<String>>,
    rx: Receiver<Line>,
    pub stats: ProtoStats,
    pub eof: bool,
    sent_at: Instant,
    pub last_cmd: Cmd,
    pub missed_s: f32,
    pub ended: bool,
}

/// Parse an action line into a `Cmd`. Returns the command and whether any field had the wrong type.
pub fn parse_action(v: &Value) -> (Cmd, bool) {
    let mut c = Cmd::default();
    let mut bad = false;
    let num = |x: &Value| x.as_f64().filter(|f| f.is_finite()).map(|f| f as f32);
    let vec2 = |key: &str, bad: &mut bool| -> Option<[f32; 2]> {
        match v.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::Array(a)) if a.len() == 2 => match (num(&a[0]), num(&a[1])) {
                (Some(x), Some(y)) => Some([x, y]),
                _ => {
                    *bad = true;
                    None
                }
            },
            Some(_) => {
                *bad = true;
                None
            }
        }
    };
    if let Some(m) = vec2("move", &mut bad) {
        c.mv = m;
    }
    c.aim = vec2("aim", &mut bad);
    let flag = |key: &str, bad: &mut bool| -> bool {
        match v.get(key) {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => {
                *bad = true;
                false
            }
        }
    };
    c.boost = flag("boost", &mut bad);
    c.jump = flag("jump", &mut bad);
    c.reload = flag("reload", &mut bad);
    match v.get("fire") {
        None | Some(Value::Null) => {}
        Some(Value::Array(a)) => {
            for (s, x) in a.iter().take(2).enumerate() {
                match x {
                    Value::Bool(b) => c.fire[s] = *b,
                    _ => bad = true,
                }
            }
        }
        Some(_) => bad = true,
    }
    let int = |key: &str, bad: &mut bool| -> Option<u64> {
        match v.get(key) {
            None | Some(Value::Null) => None,
            Some(x) => match x.as_f64() {
                Some(f) if f >= 0.0 && f.fract() == 0.0 => Some(f as u64),
                _ => {
                    *bad = true;
                    None
                }
            },
        }
    };
    c.pick = int("pick", &mut bad).map(|x| x as usize);
    c.take = int("take", &mut bad).map(|x| x as u32);
    c.drop = int("drop", &mut bad).map(|x| x as usize);
    (c, bad)
}

impl ProcBot {
    pub fn spawn(cmdline: &str, max_line: usize, show_stderr: bool) -> Result<ProcBot, String> {
        let parts = split_cmd(cmdline);
        if parts.is_empty() {
            return Err("empty bot command".into());
        }
        let mut child = Command::new(&parts[0])
            .args(&parts[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if show_stderr { Stdio::inherit() } else { Stdio::null() })
            .spawn()
            .map_err(|e| format!("cannot start `{}`: {e}", parts[0]))?;
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, wrx) = channel::<String>();
        std::thread::spawn(move || {
            for line in wrx {
                if stdin.write_all(line.as_bytes()).is_err() || stdin.flush().is_err() {
                    break;
                }
            }
            // dropping stdin closes the pipe
        });
        let (ltx, rx) = channel::<Line>();
        std::thread::spawn(move || {
            let mut r = BufReader::with_capacity(1 << 16, stdout);
            let mut buf: Vec<u8> = Vec::new();
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = ltx.send(Line::Eof);
                        break;
                    }
                    Ok(_) => {
                        let now = Instant::now();
                        let raw_len = if buf.last() == Some(&b'\n') { buf.len() - 1 } else { buf.len() };
                        let msg = if raw_len > max_line { Line::Oversize } else { Line::Text(String::from_utf8_lossy(&buf).trim().to_string(), now) };
                        if ltx.send(msg).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(ProcBot { name: String::new(), cmdline: cmdline.to_string(), child, tx: Some(tx), rx, stats: ProtoStats::default(), eof: false, sent_at: Instant::now(), last_cmd: Cmd::default(), missed_s: 0.0, ended: false })
    }

    pub fn send(&mut self, v: &Value) {
        let mut s = v.to_string();
        s.push('\n');
        self.sent_at = Instant::now();
        if let Some(tx) = &self.tx {
            let _ = tx.send(s);
        }
    }

    /// Wait for `ready` (up to `budget_ms`). Sets the name.
    pub fn wait_ready(&mut self, budget_ms: f64) {
        let deadline = self.sent_at + Duration::from_secs_f64(budget_ms / 1000.0);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(Line::Text(s, at)) => {
                    if s.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Value>(&s) {
                        Ok(v) if v.get("type").and_then(|t| t.as_str()) == Some("ready") => {
                            let name: String = v.get("name").and_then(|n| n.as_str()).unwrap_or("").chars().filter(|c| !c.is_control()).take(32).collect();
                            self.name = name;
                            self.stats.ready_ms = at.duration_since(self.sent_at).as_secs_f64() * 1000.0;
                            return;
                        }
                        Ok(_) => self.stats.wrong_tick += 1,
                        Err(_) => self.stats.bad_json += 1,
                    }
                }
                Ok(Line::Oversize) => self.stats.oversize += 1,
                Ok(Line::Eof) | Err(RecvTimeoutError::Disconnected) => {
                    self.eof = true;
                    self.stats.no_ready = true;
                    self.stats.crashed_at = Some(0.0);
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.stats.no_ready = true;
                    return;
                }
            }
        }
    }

    /// Lines that arrived before the next observation goes out are stale by definition.
    pub fn drain_stale(&mut self, t: f32) {
        while let Ok(l) = self.rx.try_recv() {
            match l {
                Line::Text(s, _) => {
                    if s.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Value>(&s) {
                        Ok(v) if v.is_object() => {
                            if v.get("tick").and_then(|x| x.as_f64()).is_some() {
                                self.stats.stale += 1
                            } else {
                                self.stats.wrong_tick += 1
                            }
                        }
                        _ => self.stats.bad_json += 1,
                    }
                }
                Line::Oversize => self.stats.oversize += 1,
                Line::Eof => self.mark_eof(t),
            }
        }
    }

    fn mark_eof(&mut self, t: f32) {
        if !self.eof {
            self.eof = true;
            if !self.ended {
                self.stats.crashed_at = Some(t);
            }
        }
    }

    /// Collect the answer to the observation with `tick`. `budget_ms` is the protocol
    /// budget; `wait_ms >= budget_ms` is how long to really wait (a longer wait still
    /// counts the answer as late, but uses it when `enforce` is false).
    pub fn collect(&mut self, tick: u32, t: f32, budget_ms: f64, wait_ms: f64, enforce: bool) -> Option<Cmd> {
        self.stats.sent += 1;
        if self.eof {
            return None;
        }
        let deadline = self.sent_at + Duration::from_secs_f64(wait_ms.max(budget_ms) / 1000.0);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(Line::Text(s, at)) => {
                    if s.is_empty() {
                        continue;
                    }
                    let v: Value = match serde_json::from_str(&s) {
                        Ok(v @ Value::Object(_)) => v,
                        _ => {
                            self.stats.bad_json += 1;
                            continue;
                        }
                    };
                    match v.get("tick").and_then(|x| x.as_f64()) {
                        Some(k) if k == tick as f64 => {}
                        Some(k) if k < tick as f64 => {
                            self.stats.stale += 1;
                            continue;
                        }
                        _ => {
                            self.stats.wrong_tick += 1;
                            continue;
                        }
                    }
                    let ms = at.duration_since(self.sent_at).as_secs_f64() * 1000.0;
                    self.stats.sum_ms += ms;
                    self.stats.max_ms = self.stats.max_ms.max(ms);
                    self.stats.over_10ms += (ms > 10.0) as u64;
                    self.stats.over_20ms += (ms > 20.0) as u64;
                    let (cmd, bad) = parse_action(&v);
                    self.stats.bad_fields += bad as u64;
                    if ms > budget_ms {
                        self.stats.late += 1;
                        if enforce {
                            return None;
                        }
                    } else {
                        self.stats.answered += 1;
                    }
                    return Some(cmd);
                }
                Ok(Line::Oversize) => self.stats.oversize += 1,
                Ok(Line::Eof) | Err(RecvTimeoutError::Disconnected) => {
                    self.mark_eof(t);
                    return None;
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.stats.late += 1;
                    return None;
                }
            }
        }
    }

    /// Send `end` and close stdin.
    pub fn end(&mut self, place: u32, kills: u32) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.send(&serde_json::json!({"type":"end","place":place,"kills":kills}));
        self.tx = None;
    }

    /// Wait for the process to exit after `end` (kills it after `grace`).
    pub fn reap(&mut self, grace: Duration) {
        self.tx = None;
        let start = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => {
                    if start.elapsed() >= grace {
                        self.stats.no_exit = true;
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return,
            }
        }
    }
}

impl Drop for ProcBot {
    fn drop(&mut self) {
        self.tx = None;
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
