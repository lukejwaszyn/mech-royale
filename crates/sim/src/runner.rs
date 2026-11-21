//! Running one match: process bots, scripted bots, lockstep decisions, results.
use crate::config::Config;
use crate::proc::{ProcBot, ProtoStats};
use crate::render::Trace;
use crate::scripted::{default_levels, LevelParams, Scripted, Squad, LEVEL_NAMES};
use crate::sim::{Cmd, Death, Sim, SpawnTable};
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use world::{Nav, NavParams, World};

#[derive(Clone, Debug)]
pub enum BotSpec {
    /// A command line speaking mechfight/1 on stdin / stdout.
    Process(String),
    /// Built-in scripted bot of a level (0 easy .. 3 expert).
    Scripted(usize),
    /// Stands still.
    Idle,
}

#[derive(Clone, Debug)]
pub struct MatchSpec {
    /// One entry per mech; entry `i` pilots slot `i` (unless `shuffle`).
    pub bots: Vec<BotSpec>,
    /// Index into `bots` of the bot we report on.
    pub focus: usize,
    /// All scripted bots except the focus form one squad hunting the focus bot.
    pub squad: bool,
}

#[derive(Clone, Debug)]
pub struct RunOpts {
    /// `hello.map.dir`.
    pub map_dir: String,
    /// Override `bot.tick_budget_ms` for the lateness check (None = config).
    pub budget_ms: Option<f64>,
    /// Drop late answers (the mech coasts), like the real server. If false, a late
    /// answer is still counted as late but used, waiting up to `wait_ms`.
    pub enforce: bool,
    pub wait_ms: f64,
    /// Send all observations first, then collect (like the server). Default is one
    /// bot at a time so a loaded machine does not make bots look late.
    pub parallel: bool,
    pub show_stderr: bool,
    pub shuffle: bool,
    pub stop_when_focus_dies: bool,
    pub max_t: Option<f32>,
    pub replay: Option<PathBuf>,
    pub trace: Option<PathBuf>,
    pub trace_scale: f32,
    pub levels: [LevelParams; 4],
    pub squad_late_t: f32,
    pub squad_late_alive: usize,
    pub squad_deaths_per_hunter: f32,
    pub squad_omniscient: bool,
}

impl Default for RunOpts {
    fn default() -> Self {
        RunOpts {
            map_dir: String::new(),
            budget_ms: None,
            enforce: true,
            wait_ms: 1000.0,
            parallel: false,
            show_stderr: false,
            shuffle: false,
            stop_when_focus_dies: false,
            max_t: None,
            replay: None,
            trace: None,
            trace_scale: 1.0,
            levels: default_levels(),
            squad_late_t: 240.0,
            squad_late_alive: 6,
            squad_deaths_per_hunter: 1.0,
            squad_omniscient: false,
        }
    }
}

/// Things shared by every match: map, nav grid, spawn table, config.
pub struct Env {
    pub world: Arc<World>,
    pub nav: Nav,
    pub table: Arc<SpawnTable>,
    pub cfg: Arc<Config>,
}

impl Env {
    pub fn new(world: Arc<World>, cfg: Arc<Config>) -> Env {
        let m = &cfg.mech;
        let nav = Nav::build(world.clone(), NavParams { radius: m.radius, height: m.height, step_up: m.step_up, max_slope_deg: m.max_slope_deg, ..NavParams::default() });
        let table = Arc::new(SpawnTable::build(&nav));
        Env { world, nav, table, cfg }
    }
}

#[derive(Clone, Debug)]
pub struct BotResult {
    pub slot: usize,
    pub name: String,
    pub kind: String,
    pub place: u32,
    pub kills: u32,
    pub death: Option<Death>,
    pub cubes: u32,
    pub dmg_dealt: f32,
    pub dmg_taken: f32,
    pub hp: f32,
    pub proto: Option<ProtoStats>,
}

#[derive(Clone, Debug)]
pub struct MatchResult {
    pub seed: u64,
    /// Indexed like `MatchSpec::bots`.
    pub bots: Vec<BotResult>,
    pub t_end: f32,
    pub wall_s: f64,
}

enum Ctl {
    Proc(Box<ProcBot>),
    Script(Box<Scripted>),
    Idle,
}

fn slot_seed(slot: usize) -> u32 {
    let mut x = (slot as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 29;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    (x >> 32) as u32
}

pub fn run_match(env: &Env, spec: &MatchSpec, opts: &RunOpts, seed: u64) -> Result<MatchResult, String> {
    let started = Instant::now();
    let cfg = env.cfg.clone();
    let mut sim = Sim::new(env.world.clone(), cfg.clone(), seed);
    sim.setup_random(env.table.clone());
    let n = sim.mechs.len();
    if spec.bots.len() != n {
        return Err(format!("need {} bots, got {}", n, spec.bots.len()));
    }
    // slot_of[bot index] and bot_of[slot]
    let mut bot_of: Vec<usize> = (0..n).collect();
    if opts.shuffle {
        crate::util::Rng::new(seed ^ 0x5106).shuffle(&mut bot_of);
    }
    let mut slot_of = vec![0usize; n];
    for (slot, &b) in bot_of.iter().enumerate() {
        slot_of[b] = slot;
    }
    let focus_slot = slot_of[spec.focus.min(n - 1)];

    let mut ctl: Vec<Ctl> = Vec::with_capacity(n);
    let mut names: Vec<String> = Vec::with_capacity(n);
    let mut kinds: Vec<String> = Vec::with_capacity(n);
    let mut levels = vec![0usize; n];
    for slot in 0..n {
        match &spec.bots[bot_of[slot]] {
            BotSpec::Process(cmd) => {
                let mut pb = ProcBot::spawn(cmd, cfg.bot.max_line_bytes, opts.show_stderr)?;
                pb.send(&json!({"type":"hello","protocol":"mechfight/1","slot":slot,"seed":slot_seed(slot),
                    "map":{"dir":opts.map_dir,"files":["v2/world.json","v2/terrain.i16"]},"config":cfg.raw}));
                sim.mechs[slot].rec = true;
                ctl.push(Ctl::Proc(Box::new(pb)));
                names.push(String::new());
                kinds.push("process".into());
            }
            BotSpec::Scripted(l) => {
                let l = (*l).min(3);
                levels[slot] = l;
                ctl.push(Ctl::Script(Box::new(Scripted::new(l, opts.levels[l].clone(), seed.wrapping_mul(1000).wrapping_add(slot as u64)))));
                names.push(format!("{}-{}", LEVEL_NAMES[l], slot));
                kinds.push(LEVEL_NAMES[l].into());
            }
            BotSpec::Idle => {
                ctl.push(Ctl::Idle);
                names.push(format!("idle-{slot}"));
                kinds.push("idle".into());
            }
        }
    }
    for slot in 0..n {
        if let Ctl::Proc(pb) = &mut ctl[slot] {
            pb.wait_ready(cfg.bot.hello_budget_ms);
            names[slot] = if pb.name.is_empty() { format!("proc-{slot}") } else { pb.name.clone() };
        }
    }
    let mut squad: Option<Squad> = None;
    if spec.squad {
        let mut any = false;
        for slot in 0..n {
            if slot != focus_slot && matches!(ctl[slot], Ctl::Script(_)) {
                sim.mechs[slot].team = 1;
                any = true;
            }
        }
        if any {
            let mut s = Squad::new(1, focus_slot, levels.clone());
            s.late_t = opts.squad_late_t;
            s.late_alive = opts.squad_late_alive;
            s.deaths_per_hunter = opts.squad_deaths_per_hunter;
            s.omniscient = opts.squad_omniscient;
            squad = Some(s);
        }
    }

    let mut trace = opts.trace.as_ref().map(|_| Trace::new(focus_slot, &sim));
    let mut replay = match &opts.replay {
        Some(p) => Some(std::io::BufWriter::new(std::fs::File::create(p).map_err(|e| format!("{}: {e}", p.display()))?)),
        None => None,
    };
    if let Some(w) = replay.as_mut() {
        let bots: Vec<_> = (0..n).map(|s| json!({"slot": s, "name": names[s], "kind": kinds[s], "team": sim.mechs[s].team})).collect();
        let _ = writeln!(w, "{}", json!({"type":"header","seed":seed,"focus":focus_slot,"bots":bots,
            "zone_circles": sim.zone_circles.iter().map(|(c, r)| json!([c[0], c[1], r])).collect::<Vec<_>>(),
            "note":"approximate local simulation, not the real server"}));
    }

    let budget = opts.budget_ms.unwrap_or(cfg.bot.tick_budget_ms);
    let wait = if opts.enforce { budget } else { opts.wait_ms.max(budget) };
    let decide_dt = cfg.match_.decide_every as f32 / cfg.match_.tick_hz;
    let mut log_pos = 0usize;

    loop {
        if sim.over() || opts.max_t.map_or(false, |m| sim.t >= m) || (opts.stop_when_focus_dies && !sim.mechs[focus_slot].alive) {
            break;
        }
        sim.update_vision();
        if let Some(s) = squad.as_mut() {
            s.update(&sim);
        }
        let tick = sim.tick;
        let t = sim.t;
        let mut cmds: Vec<(usize, Cmd)> = Vec::with_capacity(n);
        let mut answers: Vec<(usize, Option<Cmd>)> = Vec::new();
        if opts.parallel {
            for slot in 0..n {
                if let Ctl::Proc(pb) = &mut ctl[slot] {
                    if sim.mechs[slot].alive {
                        pb.drain_stale(t);
                        let obs = sim.observe(slot);
                        pb.send(&obs);
                    }
                }
            }
            for slot in 0..n {
                if let Ctl::Proc(pb) = &mut ctl[slot] {
                    if sim.mechs[slot].alive {
                        answers.push((slot, pb.collect(tick, t, budget, wait, opts.enforce)));
                    }
                }
            }
        } else {
            for slot in 0..n {
                if let Ctl::Proc(pb) = &mut ctl[slot] {
                    if sim.mechs[slot].alive {
                        pb.drain_stale(t);
                        let obs = sim.observe(slot);
                        pb.send(&obs);
                        answers.push((slot, pb.collect(tick, t, budget, wait, opts.enforce)));
                    }
                }
            }
        }
        for (slot, ans) in answers {
            let Ctl::Proc(pb) = &mut ctl[slot] else { continue };
            let cmd = match ans {
                Some(c) => {
                    pb.missed_s = 0.0;
                    pb.last_cmd = c.clone();
                    c
                }
                None => {
                    pb.missed_s += decide_dt;
                    if !pb.eof && pb.missed_s <= cfg.bot.coast_s + 1e-4 {
                        pb.last_cmd.coast()
                    } else {
                        pb.stats.idle_decisions += 1;
                        Cmd::default()
                    }
                }
            };
            cmds.push((slot, cmd));
        }
        for slot in 0..n {
            if !sim.mechs[slot].alive {
                continue;
            }
            if let Ctl::Script(b) = &mut ctl[slot] {
                let sq = squad.as_ref().filter(|_| sim.mechs[slot].team == 1);
                cmds.push((slot, b.decide(&sim, slot, &env.nav, sq)));
            }
        }
        if let Some(w) = replay.as_mut() {
            if tick % cfg.replay_frame_every.max(1) == 0 {
                let _ = writeln!(w, "{}", sim.frame());
            }
            while log_pos < sim.log.len() {
                let _ = writeln!(w, "{}", json!({"type":"event","e": sim.log[log_pos]}));
                log_pos += 1;
            }
        }
        sim.end_decision();
        sim.apply_cmds(cmds);
        for _ in 0..cfg.match_.decide_every {
            sim.step();
            if let Some(tr) = trace.as_mut() {
                tr.sample(&sim);
            }
        }
        for slot in 0..n {
            if !sim.mechs[slot].alive {
                if let Ctl::Proc(pb) = &mut ctl[slot] {
                    if !pb.ended {
                        pb.end(sim.mechs[slot].place, sim.mechs[slot].kills);
                    }
                }
            }
        }
    }
    let complete = sim.over();
    if complete {
        sim.finalize();
    }
    for slot in 0..n {
        if let Ctl::Proc(pb) = &mut ctl[slot] {
            pb.end(sim.mechs[slot].place, sim.mechs[slot].kills);
        }
    }
    for slot in 0..n {
        if let Ctl::Proc(pb) = &mut ctl[slot] {
            pb.reap(Duration::from_secs(3));
        }
    }
    if let (Some(tr), Some(path)) = (trace.as_mut(), opts.trace.as_ref()) {
        tr.finish(&sim);
        crate::render::render(&env.world, tr, opts.trace_scale, path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let mut bots: Vec<BotResult> = Vec::with_capacity(n);
    for b in 0..n {
        let slot = slot_of[b];
        let m = &sim.mechs[slot];
        bots.push(BotResult {
            slot,
            name: names[slot].clone(),
            kind: kinds[slot].clone(),
            place: m.place,
            kills: m.kills,
            death: m.death.clone(),
            cubes: m.cubes,
            dmg_dealt: m.dmg_dealt,
            dmg_taken: m.dmg_taken,
            hp: m.hp,
            proto: match &ctl[slot] {
                Ctl::Proc(pb) => Some(pb.stats.clone()),
                _ => None,
            },
        });
    }
    if let Some(w) = replay.as_mut() {
        while log_pos < sim.log.len() {
            let _ = writeln!(w, "{}", json!({"type":"event","e": sim.log[log_pos]}));
            log_pos += 1;
        }
        let res: Vec<_> = bots.iter().map(|b| json!({"slot": b.slot, "name": b.name, "place": b.place, "kills": b.kills,
            "died_t": b.death.as_ref().map(|d| d.t), "cause": b.death.as_ref().map(|d| d.cause), "by": b.death.as_ref().and_then(|d| d.by)})).collect();
        let _ = writeln!(w, "{}", json!({"type":"result","t": sim.t, "complete": complete, "bots": res}));
        let _ = w.flush();
    }
    Ok(MatchResult { seed, bots, t_end: sim.t, wall_s: started.elapsed().as_secs_f64() })
}
