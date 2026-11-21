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
    let mut procs: Vec<String> = Vec::new();
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
            "--bot" => procs.push(next(&mut i, a)),
            "--bots" => {
                let n: usize = num(&next(&mut i, a), a);
                let cmd = next(&mut i, a);
                for _ in 0..n {
                    procs.push(cmd.clone());
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
    let mut bots: Vec<BotSpec> = procs.iter().map(|c| BotSpec::Process(c.clone())).collect();
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
    for &s in &seeds {
        let mut o = opts.clone();
        o.replay = replay.as_ref().map(|p| with_seed(p, s, many));
        o.trace = trace.as_ref().map(|p| with_seed(p, s, many));
        let res = match run_match(&env, &spec, &o, s) {
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
