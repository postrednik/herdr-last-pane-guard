use last_pane_guard::{log, runtime, signals, Error, Result};

fn main() -> std::process::ExitCode {
    let result = run();
    let signal = signals::received();
    if signal != 0 {
        return std::process::ExitCode::from(u8::try_from(128 + signal).unwrap_or(1));
    }
    match result {
        Ok(()) | Err(Error::Stopped | Error::Stale) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            log(&error.to_string());
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let _handlers = signals::install()?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--version" {
        println!("last-pane-guard {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mode = match args.as_slice() {
        [] => "event",
        [arg] => arg
            .to_str()
            .ok_or(Error::Invalid("invalid command encoding"))?,
        _ => {
            return Err(Error::Invalid(
                "usage: last-pane-guard refresh|event|confirm",
            ))
        }
    };
    if !matches!(mode, "refresh" | "event" | "confirm") {
        return Err(Error::Invalid(
            "usage: last-pane-guard refresh|event|confirm",
        ));
    }
    runtime::run(mode)
}
