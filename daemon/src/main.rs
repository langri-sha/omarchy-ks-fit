use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::future::Future;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use ks_fit::daemon::{self, Daemon, Fatal};
use ks_fit::demo;
use ks_fit::ipc::{self, Message};
use ks_fit::radio::{AdapterState, Radio, RadioEvent};
use ks_fit::stats::{self, Period};
use ks_fit::store::{self, Store};
use ks_fit::tracker::Tracker;
use log::LevelFilter;
use serde::Serialize;
use serde_json::Value;
use tokio::runtime;
use tokio::signal::unix::{SignalKind, signal};
use tokio::time::Duration;

const USAGE: &str = "\
Usage:
  ks-fit daemon [--simulate] [--data-dir <dir>]
  ks-fit scan [--seconds <n>] [--all]
  ks-fit stats [--period day|week|month|year] [--offset <n>] [--data-dir <dir>]
  ks-fit --version | --help
";
const APP: &str = "omarchy-ks-fit";

#[derive(Debug, PartialEq)]
enum Cli {
    Daemon {
        simulate: bool,
        data_dir: Option<PathBuf>,
    },
    Scan {
        seconds: u64,
        all: bool,
    },
    Stats {
        period: Period,
        offset: i32,
        data_dir: Option<PathBuf>,
    },
    Version,
    Help,
}

fn main() -> ExitCode {
    let cli = match parse(env::args_os().skip(1)) {
        Ok(cli) => cli,
        Err(problem) => {
            let _ = write!(io::stderr(), "ks-fit: {problem}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    log_to_stderr();
    match cli {
        Cli::Help => exit_after(print(USAGE)),
        Cli::Version => exit_after(print(&format!("ks-fit {}\n", env!("CARGO_PKG_VERSION")))),
        Cli::Daemon { simulate, data_dir } => block_on(run_daemon(simulate, data_dir)),
        Cli::Scan { seconds, all } => block_on(scan(seconds, all)),
        Cli::Stats {
            period,
            offset,
            data_dir,
        } => match query_stats(period, offset, data_dir) {
            Ok(stats) => exit_after(emit(&Message::Stats(stats))),
            Err(error) => {
                log::error!("{error}");
                ExitCode::FAILURE
            }
        },
    }
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Cli, String> {
    let mut args = args.into_iter();
    let command = args.next().ok_or("missing a command")?;
    let mut cli = match command.to_str() {
        Some("daemon") => Cli::Daemon {
            simulate: false,
            data_dir: None,
        },
        Some("scan") => Cli::Scan {
            seconds: 10,
            all: false,
        },
        Some("stats") => Cli::Stats {
            period: Period::Week,
            offset: 0,
            data_dir: None,
        },
        Some("--version" | "-V") => Cli::Version,
        Some("--help" | "-h") => Cli::Help,
        _ => return Err(format!("unknown command {command:?}")),
    };
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{} needs a value", flag.to_string_lossy()))
        };
        match (&mut cli, flag.to_str()) {
            (_, Some("--help" | "-h")) => return Ok(Cli::Help),
            (Cli::Daemon { simulate, .. }, Some("--simulate")) => *simulate = true,
            (Cli::Daemon { data_dir, .. } | Cli::Stats { data_dir, .. }, Some("--data-dir")) => {
                *data_dir = Some(value()?.into());
            }
            (Cli::Scan { seconds, .. }, Some("--seconds")) => {
                *seconds = number(value()?)?;
                if *seconds == 0 {
                    return Err("--seconds must be at least 1".into());
                }
            }
            (Cli::Scan { all, .. }, Some("--all")) => *all = true,
            (Cli::Stats { period, .. }, Some("--period")) => {
                *period = match value()?.to_str() {
                    Some("day") => Period::Day,
                    Some("week") => Period::Week,
                    Some("month") => Period::Month,
                    Some("year") => Period::Year,
                    _ => return Err("--period must be day, week, month or year".into()),
                };
            }
            (Cli::Stats { offset, .. }, Some("--offset")) => {
                *offset = number(value()?)?;
                if *offset < ipc::MIN_STATS_OFFSET {
                    return Err(format!(
                        "--offset must be at least {}",
                        ipc::MIN_STATS_OFFSET
                    ));
                }
            }
            _ => return Err(format!("unexpected argument {flag:?}")),
        }
    }
    Ok(cli)
}

fn number<T: std::str::FromStr>(value: OsString) -> Result<T, String> {
    value
        .to_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| format!("{value:?} is not a valid number"))
}

/// Runs `task` to completion on a single thread.
fn block_on(task: impl Future<Output = ExitCode>) -> ExitCode {
    let runtime = match runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            log::error!("cannot start the runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(task);
    // Reading stdin blocks a thread that nothing can interrupt; waiting for
    // it would hold up the exit until the shell writes another line.
    runtime.shutdown_background();
    code
}

async fn run_daemon(simulate: bool, data_dir: Option<PathBuf>) -> ExitCode {
    match serve(simulate, data_dir).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(fatal) => {
            log::error!("{fatal}");
            let _ = emit(&Message::Fatal {
                code: fatal.code(),
                message: fatal.to_string(),
            });
            ExitCode::from(fatal.exit_code())
        }
    }
}

async fn serve(simulate: bool, data_dir: Option<PathBuf>) -> Result<(), Fatal> {
    let mut terminate = signal(SignalKind::terminate()).map_err(Fatal::Signals)?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(Fatal::Signals)?;
    let dir = data_dir.or_else(default_data_dir).ok_or(Fatal::NoDataDir)?;
    let dir = store::prepare_data_dir(&dir)?;
    let runtime_dir = absolute_var("XDG_RUNTIME_DIR").map(|dir| dir.join(APP));
    let _lock = daemon::lock(runtime_dir.as_ref().unwrap_or(&dir), simulate).await?;

    let now = Timestamp::now();
    let database = dir.join(if simulate { "demo.db" } else { "ks-fit.db" });
    let store =
        Store::open(&database).with_context(|| format!("opening {}", database.display()))?;
    let mut tracker = Tracker::open(store, time_zone(), now)?;
    if simulate && demo::seed(&mut tracker, now)? {
        log::info!("seeded the demo history");
    }
    let radio = if simulate {
        Radio::simulated().0
    } else {
        Radio::bluez()
    };
    let mut daemon =
        Daemon::new(radio, tracker, tokio::io::stdin(), tokio::io::stdout())?.simulated(simulate);
    daemon
        .run(async move {
            tokio::select! {
                _ = terminate.recv() => log::info!("terminated"),
                _ = interrupt.recv() => log::info!("interrupted"),
            }
        })
        .await
}

/// Prints the devices one scan finds, each once, until it ends.
async fn scan(seconds: u64, all: bool) -> ExitCode {
    let mut radio = Radio::bluez();
    let mut scanning = false;
    let mut seen = HashSet::new();
    loop {
        let event = tokio::select! {
            event = radio.events().recv() => event,
            _ = tokio::signal::ctrl_c() => {
                radio.stop_scan();
                continue;
            }
        };
        match event {
            Some(RadioEvent::Adapter(AdapterState::On)) if !scanning => {
                scanning = true;
                radio.scan(Duration::from_secs(seconds));
            }
            Some(RadioEvent::Adapter(state)) if !scanning => {
                log::error!("Bluetooth is {state:?}");
                return ExitCode::FAILURE;
            }
            Some(RadioEvent::Discovered {
                address,
                name,
                rssi,
                candidate,
            }) => {
                if (all || candidate) && seen.insert(address.clone()) {
                    let device = ipc::ScanDevice {
                        address,
                        name,
                        rssi,
                        candidate,
                    };
                    if let Err(error) = emit(&device) {
                        return exit_after(Err(error));
                    }
                }
            }
            Some(RadioEvent::ScanFinished) => return ExitCode::SUCCESS,
            Some(_) => {}
            None => return ExitCode::FAILURE,
        }
    }
}

fn query_stats(
    period: Period,
    offset: i32,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<ipc::Stats> {
    let dir = data_dir.or_else(default_data_dir).ok_or(Fatal::NoDataDir)?;
    let dir = store::prepare_data_dir(&dir)?;
    let store = Store::open(&dir.join("ks-fit.db"))?;
    let stats = stats::query(&store, &time_zone(), Timestamp::now(), period, offset)?;
    Ok(ipc::Stats::new(Value::Null, period, &stats))
}

fn default_data_dir() -> Option<PathBuf> {
    absolute_var("XDG_DATA_HOME")
        .map(|dir| dir.join(APP))
        .or_else(|| absolute_var("HOME").map(|home| home.join(".local/share").join(APP)))
}

/// The variable as a path, unless it is unset or relative, which the XDG
/// base directory specification says to ignore.
fn absolute_var(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn time_zone() -> TimeZone {
    TimeZone::try_system().unwrap_or_else(|error| {
        log::warn!("using UTC: {error}");
        TimeZone::UTC
    })
}

/// Prints `value` as one JSON line.
fn emit(value: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    print(&line)
}

fn print(text: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()
}

/// A reader that went away before the output ended is not a failure.
fn exit_after(written: io::Result<()>) -> ExitCode {
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(error) => {
            log::error!("cannot write to stdout: {error}");
            ExitCode::FAILURE
        }
    }
}

struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let line = format!("{} {}\n", record.level(), record.args());
            let _ = io::stderr().write_all(line.as_bytes());
        }
    }

    fn flush(&self) {}
}

/// Logs to stderr at the level in `KS_FIT_LOG`, `info` by default.
fn log_to_stderr() {
    static LOGGER: Stderr = Stderr;
    let setting = env::var("KS_FIT_LOG").ok();
    let level = setting.as_deref().map(str::parse::<LevelFilter>);
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(match level {
            Some(Ok(level)) => level,
            _ => LevelFilter::Info,
        });
    }
    if let (Some(setting), Some(Err(_))) = (setting, level) {
        log::warn!("KS_FIT_LOG={setting:?} is not a log level; logging at info");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Cli, String> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn parses_every_command_with_its_defaults() {
        assert_eq!(
            parsed(&["daemon"]),
            Ok(Cli::Daemon {
                simulate: false,
                data_dir: None
            })
        );
        assert_eq!(
            parsed(&["scan"]),
            Ok(Cli::Scan {
                seconds: 10,
                all: false
            })
        );
        assert_eq!(
            parsed(&["stats"]),
            Ok(Cli::Stats {
                period: Period::Week,
                offset: 0,
                data_dir: None
            })
        );
        assert_eq!(parsed(&["--version"]), Ok(Cli::Version));
        assert_eq!(parsed(&["-V"]), Ok(Cli::Version));
        assert_eq!(parsed(&["--help"]), Ok(Cli::Help));
        assert_eq!(parsed(&["stats", "--help"]), Ok(Cli::Help));
    }

    #[test]
    fn parses_every_flag() {
        assert_eq!(
            parsed(&["daemon", "--data-dir", "/srv/ks", "--simulate"]),
            Ok(Cli::Daemon {
                simulate: true,
                data_dir: Some("/srv/ks".into())
            })
        );
        assert_eq!(
            parsed(&["scan", "--all", "--seconds", "30"]),
            Ok(Cli::Scan {
                seconds: 30,
                all: true
            })
        );
        assert_eq!(
            parsed(&[
                "stats",
                "--period",
                "day",
                "--offset",
                "-3",
                "--data-dir",
                "d"
            ]),
            Ok(Cli::Stats {
                period: Period::Day,
                offset: -3,
                data_dir: Some("d".into())
            })
        );
        for (name, period) in [
            ("week", Period::Week),
            ("month", Period::Month),
            ("year", Period::Year),
        ] {
            assert!(matches!(
                parsed(&["stats", "--period", name]),
                Ok(Cli::Stats { period: p, .. }) if p == period
            ));
        }
    }

    #[test]
    fn refuses_bad_usage() {
        let cases: [&[&str]; 12] = [
            &[],
            &["serve"],
            &["daemon", "--verbose"],
            &["daemon", "--all"],
            &["daemon", "--data-dir"],
            &["scan", "--simulate"],
            &["scan", "--seconds", "soon"],
            &["scan", "--seconds", "0"],
            &["stats", "--period", "fortnight"],
            &["stats", "--offset", "-10001"],
            &["stats", "--offset"],
            &["--version", "extra"],
        ];
        for args in cases {
            assert!(parsed(args).is_err(), "{args:?}");
        }
    }
}
