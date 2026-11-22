fn main() {
    let mut tune = bot::Tuning::from_env();
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--version" => {
                bot::logln!("bot {} (mechfight/1)", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--ffa" => tune.apply_ffa(),
            "--practice" => tune = { let mut t = bot::Tuning::default(); t.ffa = false; t },
            other => {
                if let Some(s) = other.strip_prefix("--tune=") {
                    for bad in tune.apply_overrides(s) {
                        bot::logln!("[bot] --tune: ignored '{bad}'");
                    }
                }
            }
        }
    }
    std::process::exit(bot::runner::run_stdio(tune));
}
