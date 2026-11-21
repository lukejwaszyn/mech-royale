//! `sim` - run local approximate mechfight/1 matches. See README.md (`sim --help`).
use sim::config::Config;
use sim::proc::ProtoStats;
use sim::runner::{run_match, BotSpec, Env, MatchSpec, RunOpts};
use sim::scripted::LEVEL_NAMES;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use world::World;

const HELP: &str = r#"sim - rough local simulator for mechfight/1 (an approximation; scores do NOT predict the leaderboard)

USAGE
  sim [--bot CMD]... [--bots N CMD] [--fill KIND] [--seed S --matches N | --seeds A,B,C] [options]

BOTS (16 mechs; process bots first, the rest is filled)
  --bot CMD            add one process bot (a command line speaking mechfight/1; quote it)
  --bots N CMD         add N copies of a process bot
  --inproc TUNE        add our own bot (crates/bot) in-process: no pipes, no timing checks, nav grids
                       shared between matches - about ten times faster, for tuning runs. TUNE is a
                       MECH_TUNE list (key=value,...; "" = defaults; ffa=1 = the free-for-all preset)
  --inprocs N TUNE     add N of them
  --fill KIND          what pilots the remaining slots (default: squad)
                         squad   4 easy, 4 medium, 4 hard, 3 expert scripted bots acting as ONE team
                                 against the focus bot (shared vision, no friendly targeting,
                                 one hunter early, one more per dead member, everyone late)
                         mix     the same 15 scripted bots, every mech for itself
                         easy | medium | hard | expert | idle   all the same
  --focus N            which bot to report on: index in the bot list (default 0)
  --focus-level L      with no --bot at all, slot 0 is a scripted bot of this level (default medium)
  --shuffle            random slot order per match (default: bot i gets slot i)

MATCHES
  --seed S             first match seed (default 1)
  --matches N          number of matches, seeds S, S+1, ... (default 1)
  --seeds A,B,C        explicit seeds
  --max-t SEC          stop every match at this match time
  --jobs N             play N matches at a time (default 1; use it with --inproc or scripted bots only,
                       process bots would compete for the CPU and look late)
  --stop-when-focus-dies   end a match as soon as the focus bot is dead (faster)

PROTOCOL / TIMING
  --budget-ms MS       per-decision budget (default: config bot.tick_budget_ms = 50)
  --no-enforce         count late answers but still use them (wait up to --wait-ms, default 1000)
  --wait-ms MS
  --parallel           send all observations, then collect (as the server does); default is one
                       process bot at a time, so a busy machine does not make bots look late
  --bot-stderr         show the bots' stderr

OUTPUT
  --replay FILE.jsonl  dump a replay; with several matches the seed is appended to the file name
  --replay-every N     one replay frame every N decisions (default 5 = 0.5 s; 1 = every decision, ~10 MB/match)
  --trace FILE.png     top-down image of the focus bot's path (.png or .ppm); seed appended likewise
  --trace-scale PX     pixels per metre (default 1)
  --trace-crop         crop the image to the path and zoom in
  --all                print every bot's result per match, not only the focus bot
  --detail             per match: who damaged the focus bot (H = a hunter at the end), by which weapon,
                       and gun accuracy both ways; the summary adds the totals per opponent level
  --quiet              only the summary

WORLD / RULES
  --arena DIR          map directory holding v2/world.json (default: $MECH_ARENA or the repo's arena/)
  --map-dir DIR        what to send bots as hello.map.dir (default: the absolute --arena path)
  --config FILE        JSON with the same shape as hello.config (default: the recorded real config)

SCRIPTED BOTS
  --tune K=V,...       e.g. easy.aim_sigma_deg=8,expert.react_s=0.15  (keys: aim_sigma_deg react_s lead
                       fire_tol_deg strafe retreat_hp loot boost drops face_threat defend_range gun_range)
  --squad-late-t SEC   from this time every squad member hunts (default 240)
  --squad-late-alive N ... or once this few mechs are left (default 6)
  --squad-deaths-per-hunter X   one more hunter per X dead members (default 1)
  --squad-omniscient   the squad always knows where the focus bot is
  --focus-weapon KIND  experiment: the focus bot starts with a mortar, laser or lightning in slot 1

EXAMPLES
  sim --bot "target/release/bot" --fill squad --seed 1 --matches 20
  sim --bots 16 "python3 -u '/path with spaces/example_bot.py'" --seed 7
  sim --fill squad --focus-level hard --matches 10 --trace /tmp/t.png
"#;

fn die(msg: &str) -> ! {
    eprintln!("sim: {msg}");
    std::process::exit(2);
}

fn default_arena() -> PathBuf {
    if let Ok(p) = std::env::var("MECH_ARENA") {
        return PathBuf::from(p);
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../arena");
    if repo.join("v2/world.json").exists() {
        return repo;
    }
    PathBuf::from("arena")
}

fn with_seed(p: &Path, seed: u64, many: bool) -> PathBuf {
    if !many {
        return p.to_path_buf();
    }
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
    p.with_file_name(format!("{stem}-{seed}{ext}"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut procs: Vec<BotSpec> = Vec::new();
    let mut jobs = 1usize;
    let mut fill = "squad".to_string();
    let mut focus = 0usize;
    let mut focus_level = 1usize;
    let mut seed = 1u64;
    let mut matches = 1usize;
    let mut seeds: Option<Vec<u64>> = None;
    let mut opts = RunOpts::default();
    let mut arena = default_arena();
    let mut map_dir: Option<String> = None;
    let mut config: Option<PathBuf> = None;
    let mut replay: Option<PathBuf> = None;
    let mut trace: Option<PathBuf> = None;
    let mut all = false;
    let mut quiet = false;
    let mut detail = false;

    let mut i = 0;
    let next = |i: &mut usize, what: &str| -> String {
        *i += 1;
        args.get(*i).cloned().unwrap_or_else(|| die(&format!("{what} needs a value")))
    };
    fn num<T: std::str::FromStr>(s: &str, what: &str) -> T {
        s.parse().unwrap_or_else(|_| die(&format!("bad value `{s}` for {what}")))
    }
    let level = |s: &str| -> usize { LEVEL_NAMES.iter().position(|l| *l == s).unwrap_or_else(|| die(&format!("unknown level `{s}` (easy, medium, hard, expert)"))) };
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" | "--help" => {
                print!("{HELP}");
                return;
            }
            "--bot" => procs.push(BotSpec::Process(next(&mut i, a))),
            "--inproc" => procs.push(BotSpec::InProc(next(&mut i, a))),
            "--inprocs" => {
                let n: usize = num(&next(&mut i, a), a);
                let tune = next(&mut i, a);
                for _ in 0..n {
                    procs.push(BotSpec::InProc(tune.clone()));
                }
            }
            "--jobs" | "-j" => jobs = num::<usize>(&next(&mut i, a), a).max(1),
            "--bots" => {
                let n: usize = num(&next(&mut i, a), a);
                let cmd = next(&mut i, a);
                for _ in 0..n {
                    procs.push(BotSpec::Process(cmd.clone()));
                }
            }
            "--fill" => fill = next(&mut i, a),
            "--focus" => focus = num(&next(&mut i, a), a),
            "--focus-level" => focus_level = level(&next(&mut i, a)),
            "--shuffle" => opts.shuffle = true,
            "--seed" => seed = num(&next(&mut i, a), a),
            "--matches" | "-n" => matches = num(&next(&mut i, a), a),
            "--seeds" => seeds = Some(next(&mut i, a).split(',').filter(|s| !s.is_empty()).map(|s| num(s, a)).collect()),
            "--max-t" => opts.max_t = Some(num(&next(&mut i, a), a)),
            "--stop-when-focus-dies" => opts.stop_when_focus_dies = true,
            "--budget-ms" => opts.budget_ms = Some(num(&next(&mut i, a), a)),
            "--no-enforce" => opts.enforce = false,
            "--wait-ms" => opts.wait_ms = num(&next(&mut i, a), a),
            "--parallel" => opts.parallel = true,
            "--bot-stderr" => opts.show_stderr = true,
            "--replay" => replay = Some(PathBuf::from(next(&mut i, a))),
            "--trace" => trace = Some(PathBuf::from(next(&mut i, a))),
            "--trace-scale" => opts.trace_scale = num(&next(&mut i, a), a),
            "--trace-crop" => opts.trace_crop = true,
            "--replay-every" => opts.replay_every = num(&next(&mut i, a), a),
            "--all" => all = true,
            "--detail" => detail = true,
            "--quiet" | "-q" => quiet = true,
            "--arena" => arena = PathBuf::from(next(&mut i, a)),
            "--map-dir" => map_dir = Some(next(&mut i, a)),
            "--config" => config = Some(PathBuf::from(next(&mut i, a))),
            "--tune" => {
                for kv in next(&mut i, a).split(',').filter(|s| !s.is_empty()) {
                    let (k, v) = kv.split_once('=').unwrap_or_else(|| die(&format!("--tune wants level.key=value, got `{kv}`")));
                    let (l, key) = k.split_once('.').unwrap_or_else(|| die(&format!("--tune wants level.key=value, got `{kv}`")));
                    let v: f32 = num(v, "--tune");
                    let targets: Vec<usize> = if l == "all" { vec![0, 1, 2, 3] } else { vec![level(l)] };
                    for t in targets {
                        if !opts.levels[t].set(key, v) {
                            die(&format!("unknown tuning key `{key}`"));
                        }
                    }
                }
            }
            "--squad-late-t" => opts.squad_late_t = num(&next(&mut i, a), a),
            "--squad-late-alive" => opts.squad_late_alive = num(&next(&mut i, a), a),
            "--squad-deaths-per-hunter" => opts.squad_deaths_per_hunter = num(&next(&mut i, a), a),
            "--squad-omniscient" => opts.squad_omniscient = true,
            "--focus-weapon" => {
                let k = next(&mut i, a);
                opts.focus_weapon = Some(sim::config::WKind::from_name(&k).unwrap_or_else(|| die(&format!("unknown weapon `{k}`"))));
            }
            _ => die(&format!("unknown option `{a}` (try --help)")),
        }
        i += 1;
    }

    let cfg = match &config {
        Some(p) => {
            let text = std::fs::read_to_string(p).unwrap_or_else(|e| die(&format!("{}: {e}", p.display())));
            Config::from_json(&text).unwrap_or_else(|e| die(&format!("{}: {e}", p.display())))
        }
        None => Config::default_config(),
    };
    let n = cfg.match_.mechs;
    if procs.len() > n {
        die(&format!("{} process bots but a match has {} mechs", procs.len(), n));
    }
    let mut bots: Vec<BotSpec> = procs.clone();
    if bots.is_empty() && matches!(fill.as_str(), "squad" | "mix") {
        bots.push(BotSpec::Scripted(focus_level));
    }
    const PATTERN: [usize; 4] = [0, 1, 2, 3];
    let mut k = 0;
    while bots.len() < n {
        bots.push(match fill.as_str() {
            "squad" | "mix" => BotSpec::Scripted(PATTERN[k % 4]),
            "idle" => BotSpec::Idle,
            l => BotSpec::Scripted(level(l)),
        });
        k += 1;
    }
    if focus >= n {
        die("--focus out of range");
    }
    let spec = MatchSpec { bots, focus, squad: fill == "squad" };

    let t0 = Instant::now();
    let world = Arc::new(World::load(&arena).unwrap_or_else(|e| die(&format!("cannot load the map from {}: {e} (use --arena)", arena.display()))));
    opts.map_dir = map_dir.unwrap_or_else(|| std::fs::canonicalize(&arena).unwrap_or(arena.clone()).to_string_lossy().to_string());
    let env = Env::new(world, Arc::new(cfg));
    if !quiet {
        eprintln!("sim: map and nav ready in {:.2}s ({} ground spawn cells, {} roof cells) - approximate rules, not the real server", t0.elapsed().as_secs_f64(), env.table.main.len(), env.table.roof.len());
    }

    let seeds: Vec<u64> = seeds.unwrap_or_else(|| (0..matches as u64).map(|k| seed + k).collect());
    let many = seeds.len() > 1;
    let mut pts_sum = 0.0f64;
    let mut kills_sum = 0.0f64;
    let mut surv_sum = 0.0f64;
    let mut done = 0usize;
    let mut proto: Vec<Option<(String, ProtoStats)>> = vec![None; n];
    let mut causes: std::collections::BTreeMap<String, usize> = Default::default();
    // statistics over all matches: damage the focus bot took per opponent kind and per weapon,
    // gun bullets fired / hit by the focus bot, and per opponent kind (all their targets)
    let mut from_kind: std::collections::BTreeMap<String, f64> = Default::default();
    let mut from_weapon: std::collections::BTreeMap<String, f64> = Default::default();
    let mut killer_kind: std::collections::BTreeMap<String, usize> = Default::default();
    let mut dealt_weapon: std::collections::BTreeMap<String, f64> = Default::default();
    let mut diag_sum: Vec<(&'static str, f64)> = Vec::new();
    let mut hunters_sum = 0usize;
    let (mut f_shots, mut f_hits, mut dealt_sum, mut taken_sum) = (0u64, 0u64, 0.0f64, 0.0f64);
    let mut wall_sum = 0.0f64;
    // With --jobs, worker threads (each with its own nav grid) play the matches and the
    // results are reported here in seed order.
    let opts_for = |s: u64| {
        let mut o = opts.clone();
        o.replay = replay.as_ref().map(|p| with_seed(p, s, many));
        o.trace = trace.as_ref().map(|p| with_seed(p, s, many));
        o
    };
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Result<sim::runner::MatchResult, String>)>();
    let next_idx = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let workers = if jobs > 1 { jobs.min(seeds.len()) } else { 0 };
    for _ in 0..workers {
        let (tx, next_idx, seeds, spec) = (tx.clone(), next_idx.clone(), seeds.clone(), spec.clone());
        let (world, cfg) = (env.world.clone(), env.cfg.clone());
        let jobs_opts: Vec<RunOpts> = seeds.iter().map(|&s| opts_for(s)).collect();
        std::thread::spawn(move || {
            let env = Env::new(world, cfg);
            loop {
                let k = next_idx.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if k >= seeds.len() {
                    break;
                }
                if tx.send((k, run_match(&env, &spec, &jobs_opts[k], seeds[k]))).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let mut pending: std::collections::BTreeMap<usize, Result<sim::runner::MatchResult, String>> = Default::default();
    for (k, &s) in seeds.iter().enumerate() {
        let res = if workers == 0 {
            run_match(&env, &spec, &opts_for(s), s)
        } else {
            loop {
                if let Some(r) = pending.remove(&k) {
                    break r;
                }
                match rx.recv() {
                    Ok((i, r)) => {
                        pending.insert(i, r);
                    }
                    Err(_) => break Err("a worker thread stopped".into()),
                }
            }
        };
        let res = match res {
            Ok(r) => r,
            Err(e) => die(&e),
        };
        let f = &res.bots[spec.focus];
        let place = if f.place == 0 { 1 } else { f.place };
        let pts = n as f64 - place as f64;
        pts_sum += pts;
        kills_sum += f.kills as f64;
        surv_sum += f.death.as_ref().map(|d| d.t).unwrap_or(res.t_end) as f64;
        done += 1;
        let cause = match &f.death {
            Some(d) => {
                let by = match d.by {
                    Some(b) if b >= 100 => format!(" by drone {b}"),
                    Some(b) => format!(" by {} ({})", b, res.bots.iter().find(|x| x.slot == b as usize).map(|x| x.name.as_str()).unwrap_or("?")),
                    None => String::new(),
                };
                *causes.entry(d.cause.to_string()).or_default() += 1;
                format!("died t={:.1}s cause={}{}", d.t, d.cause, by)
            }
            None => {
                *causes.entry("survived".into()).or_default() += 1;
                format!("alive at t={:.1}s hp={:.0}", res.t_end, f.hp)
            }
        };
        if !quiet {
            println!(
                "seed={} bot={} slot={} place={} kills={} pts={} {} cubes={} dealt={:.0} taken={:.0} [match {:.1}s in {:.2}s wall]",
                s, f.name, f.slot, f.place, f.kills, pts, cause, f.cubes, f.dmg_dealt, f.dmg_taken, res.t_end, res.wall_s
            );
            if all {
                let mut order: Vec<&sim::runner::BotResult> = res.bots.iter().collect();
                order.sort_by_key(|b| (if b.place == 0 { 99 } else { b.place }, b.slot));
                for b in order {
                    let d = match &b.death {
                        Some(d) => format!("died {:6.1}s {:9} by {}", d.t, d.cause, d.by.map(|x| x.to_string()).unwrap_or_else(|| "-".into())),
                        None => format!("alive hp {:.0}", b.hp),
                    };
                    println!("    place {:2}  slot {:2}  {:14} kills {}  cubes {:2}  dealt {:5.0}  {}", b.place, b.slot, b.name, b.kills, b.cubes, b.dmg_dealt, d);
                }
            }
        }
        wall_sum += f.wall_s as f64;
        f_shots += f.gun_shots as u64;
        f_hits += f.gun_hits as u64;
        dealt_sum += f.dmg_dealt as f64;
        taken_sum += f.dmg_taken as f64;
        hunters_sum += res.bots.iter().filter(|b| b.hunter).count();
        for (w, x) in &f.dmg_by_weapon {
            *from_weapon.entry(w.to_string()).or_default() += *x as f64;
        }
        for (k, v) in &f.diag {
            match diag_sum.iter_mut().find(|(n, _)| n == k) {
                Some((_, x)) => *x += *v as f64,
                None => diag_sum.push((k, *v as f64)),
            }
        }
        if detail && !quiet && !f.diag.is_empty() {
            println!("    bot: {}", f.diag.iter().filter(|(_, v)| *v != 0.0).map(|(k, v)| format!("{k} {v:.0}")).collect::<Vec<_>>().join(" "));
        }
        for (w, x) in &f.dealt_by_weapon {
            *dealt_weapon.entry(w.to_string()).or_default() += *x as f64;
        }
        let mut parts: Vec<(f32, String)> = Vec::new();
        for (slot, &x) in f.dmg_from.iter().enumerate() {
            if x <= 0.0 {
                continue;
            }
            if let Some(b) = res.bots.iter().find(|b| b.slot == slot) {
                *from_kind.entry(if slot == f.slot { "self".to_string() } else { b.kind.clone() }).or_default() += x as f64;
                parts.push((x, format!("{}{} {:.0}", b.name, if b.hunter { "(H)" } else { "" }, x)));
            }
        }
        if let Some(by) = f.death.as_ref().and_then(|d| d.by) {
            let k = res.bots.iter().find(|b| b.slot == by as usize).map(|b| b.kind.clone()).unwrap_or_else(|| "drone".into());
            *killer_kind.entry(k).or_default() += 1;
        }
        if detail && !quiet {
            parts.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            println!(
                "    hurt by: {} | weapons: {} | own gun {}/{} hit | hunters at the end: {}",
                parts.iter().map(|p| p.1.as_str()).collect::<Vec<_>>().join(", "),
                f.dmg_by_weapon.iter().map(|(w, x)| format!("{w} {x:.0}")).collect::<Vec<_>>().join(" "),
                f.gun_hits,
                f.gun_shots,
                res.bots.iter().filter(|b| b.hunter).count()
            );
        }
        for (bi, b) in res.bots.iter().enumerate() {
            if let Some(ps) = &b.proto {
                match &mut proto[bi] {
                    Some((_, acc)) => acc.merge(ps),
                    slot => *slot = Some((b.name.clone(), ps.clone())),
                }
            }
        }
    }
    let fname = match &spec.bots[spec.focus] {
        BotSpec::Process(c) => c.clone(),
        BotSpec::Scripted(l) => format!("scripted {}", LEVEL_NAMES[*l]),
        BotSpec::Idle => "idle".into(),
        BotSpec::InProc(t) => format!("inproc {t}"),
    };
    println!(
        "summary: {} match(es), focus bot [{}] vs {}: mean(16-place) = {:.2}, mean kills {:.2}, mean survival {:.0}s; causes: {}",
        done,
        fname,
        if procs.len() >= n { "process bots only".to_string() } else { format!("fill={fill}") },
        pts_sum / done.max(1) as f64,
        kills_sum / done.max(1) as f64,
        surv_sum / done.max(1) as f64,
        causes.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
    );
    if detail {
        let d = done.max(1) as f64;
        println!(
            "detail (per match): dealt {:.0} ({}), taken {:.0}; taken from {}; by weapon {}; killed by {}; own gun accuracy {:.0}% ({:.0} shots/match); hunters at the end {:.1}; rubbing walls {:.1}s",
            dealt_sum / d,
            dealt_weapon.iter().map(|(k, v)| format!("{k} {:.0}", v / d)).collect::<Vec<_>>().join(" "),
            taken_sum / d,
            from_kind.iter().map(|(k, v)| format!("{k} {:.0}", v / d)).collect::<Vec<_>>().join(" "),
            from_weapon.iter().map(|(k, v)| format!("{k} {:.0}", v / d)).collect::<Vec<_>>().join(" "),
            killer_kind.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "),
            100.0 * f_hits as f64 / f_shots.max(1) as f64,
            f_shots as f64 / d,
            hunters_sum as f64 / d,
            wall_sum / d
        );
        if !diag_sum.is_empty() {
            println!("bot counters (per match): {}", diag_sum.iter().map(|(k, v)| format!("{k} {:.1}", v / d)).collect::<Vec<_>>().join(" "));
        }
    }
    println!("note: this simulator is an approximation of the rules; these numbers do not predict the real leaderboard.");
    let mut any = false;
    for (bi, p) in proto.iter().enumerate() {
        let Some((name, s)) = p else { continue };
        if !any {
            println!("protocol report (process bots, all matches):");
            any = true;
        }
        let mean = if s.answered + s.late > 0 { s.sum_ms / (s.answered + s.late).max(1) as f64 } else { 0.0 };
        println!(
            "  bot {:2} {:16} obs {:6} ok {:6} late {} stale {} wrong-tick {} bad-json {} oversize {} bad-fields {} idle-decisions {} no-ready {} crashed {} no-exit {} | answer ms mean {:.2} max {:.1} >10ms {} >20ms {} | ready {:.0} ms{}",
            bi,
            name,
            s.sent,
            s.answered,
            s.late,
            s.stale,
            s.wrong_tick,
            s.bad_json,
            s.oversize,
            s.bad_fields,
            s.idle_decisions,
            s.no_ready,
            s.crashed_at.map(|t| format!("at t={t:.1}s")).unwrap_or_else(|| "no".into()),
            s.no_exit,
            mean,
            s.max_ms,
            s.over_10ms,
            s.over_20ms,
            s.ready_ms,
            if s.problems() > 0 { "  <-- PROBLEMS" } else { "" }
        );
    }
}
