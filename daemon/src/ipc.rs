//! Wire types exchanged with the shell over stdio.

use std::{io, mem};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::controller::{self, PadView};
use crate::protocol;
use crate::radio::AdapterState;
use crate::stats::{self, Period};
use crate::store::{self, Activity};
use crate::tracker;

/// The longest command line accepted, in bytes.
pub const MAX_LINE: usize = 64 * 1024;
/// The furthest back a stats request may reach, so every period stays a
/// representable date.
pub const MIN_STATS_OFFSET: i32 = -10_000;

/// A line to the shell.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Message {
    Hello {
        version: &'static str,
        simulated: bool,
    },
    State(Box<State>),
    Stats(Stats),
    Error {
        code: &'static str,
        message: String,
        cmd: Option<String>,
    },
    Fatal {
        code: &'static str,
        message: String,
    },
}

impl Message {
    pub fn hello(simulated: bool) -> Message {
        Message::Hello {
            version: env!("CARGO_PKG_VERSION"),
            simulated,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    #[serde(with = "Bluetooth")]
    pub bluetooth: AdapterState,
    pub link: Link,
    pub link_detail: Option<&'static str>,
    pub device: Option<Device>,
    pub auto_connect: bool,
    pub scan: Scan,
    pub pad: Option<Pad>,
    pub session: Option<Session>,
    pub last_session: Option<Session>,
    pub today: Today,
    pub weight_kg: f64,
}

#[derive(Serialize)]
#[serde(remote = "AdapterState", rename_all = "lowercase")]
enum Bluetooth {
    On,
    Off,
    Blocked,
    Missing,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Link {
    Disconnected,
    Searching,
    Connecting,
    Connected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Device {
    pub address: String,
    pub name: Option<String>,
}

impl From<&store::Device> for Device {
    fn from(device: &store::Device) -> Device {
        Device {
            address: device.address.clone(),
            name: device.name.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Scan {
    pub active: bool,
    pub devices: Vec<ScanDevice>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScanDevice {
    pub address: String,
    pub name: Option<String>,
    pub rssi: Option<i16>,
    pub candidate: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pad {
    pub belt: Belt,
    pub mode: Mode,
    pub speed_kmh: f64,
    pub target_kmh: f64,
    pub min_kmh: f64,
    pub max_kmh: f64,
    pub busy: Option<Busy>,
    pub imperial: bool,
    pub shared: bool,
}

impl From<PadView> for Pad {
    fn from(view: PadView) -> Pad {
        Pad {
            belt: view.belt.into(),
            mode: view.mode.into(),
            speed_kmh: view.speed_kmh,
            target_kmh: view.target_kmh,
            min_kmh: view.min_kmh,
            max_kmh: view.max_kmh,
            busy: view.busy.map(Busy::from),
            imperial: view.imperial,
            shared: view.shared,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Belt {
    Idle,
    Starting,
    Running,
    Standby,
    Unknown,
}

impl From<protocol::Belt> for Belt {
    fn from(belt: protocol::Belt) -> Belt {
        match belt {
            protocol::Belt::Idle => Belt::Idle,
            protocol::Belt::Starting => Belt::Starting,
            protocol::Belt::Running => Belt::Running,
            protocol::Belt::Standby => Belt::Standby,
            protocol::Belt::Unknown(_) => Belt::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Auto,
    Manual,
    Standby,
    Unknown,
}

impl From<protocol::Mode> for Mode {
    fn from(mode: protocol::Mode) -> Mode {
        match mode {
            protocol::Mode::Auto => Mode::Auto,
            protocol::Mode::Manual => Mode::Manual,
            protocol::Mode::Standby => Mode::Standby,
            protocol::Mode::Unknown(_) => Mode::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Busy {
    Starting,
    Stopping,
}

impl From<controller::Busy> for Busy {
    fn from(busy: controller::Busy) -> Busy {
        match busy {
            controller::Busy::Starting => Busy::Starting,
            controller::Busy::Stopping => Busy::Stopping,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: i64,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub active_s: u32,
    pub steps: u32,
    pub distance_m: u32,
    pub kcal: f64,
    pub avg_kmh: f64,
    pub max_kmh: f64,
    pub estimated: bool,
}

impl From<&store::Session> for Session {
    fn from(session: &store::Session) -> Session {
        Session {
            id: session.id,
            started_at: session.started_at.as_second(),
            ended_at: session.ended_at.map(|at| at.as_second()),
            active_s: session.activity.active_s,
            steps: session.activity.steps,
            distance_m: session.activity.distance_m,
            kcal: tenths(session.activity.kcal),
            avg_kmh: tenths(session.avg_kmh()),
            max_kmh: tenths(session.max_kmh),
            estimated: session.estimated,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Today {
    pub date: String,
    pub steps: u32,
    pub distance_m: u32,
    pub active_s: u32,
    pub kcal: f64,
    pub sessions: u32,
}

impl From<&tracker::Today> for Today {
    fn from(today: &tracker::Today) -> Today {
        Today {
            date: today.date.to_string(),
            steps: today.activity.steps,
            distance_m: today.activity.distance_m,
            active_s: today.activity.active_s,
            kcal: tenths(today.activity.kcal),
            sessions: today.sessions,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub id: Value,
    #[serde(with = "PeriodName")]
    pub period: Period,
    pub offset: i32,
    pub start: String,
    pub end: String,
    pub buckets: Vec<Bucket>,
    pub totals: Totals,
    pub active_buckets: usize,
}

impl Stats {
    pub fn new(id: Value, period: Period, stats: &stats::Stats) -> Stats {
        Stats {
            id,
            period,
            offset: stats.offset,
            start: stats.start.to_string(),
            end: stats.end.to_string(),
            buckets: stats
                .buckets
                .iter()
                .map(|bucket| Bucket {
                    key: bucket.key.clone(),
                    totals: bucket.activity.into(),
                })
                .collect(),
            totals: stats.totals.into(),
            active_buckets: stats.active_buckets(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Bucket {
    pub key: String,
    #[serde(flatten)]
    pub totals: Totals,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub steps: u32,
    pub kcal: f64,
    pub distance_m: u32,
    pub active_s: u32,
}

impl From<Activity> for Totals {
    fn from(activity: Activity) -> Totals {
        Totals {
            steps: activity.steps,
            kcal: tenths(activity.kcal),
            distance_m: activity.distance_m,
            active_s: activity.active_s,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Period", rename_all = "lowercase")]
enum PeriodName {
    Day,
    Week,
    Month,
    Year,
}

fn tenths(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// A line from the shell.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Command {
    Configure {
        weight_kg: Option<f64>,
        auto_connect: Option<bool>,
    },
    Sync,
    Wake,
    Scan,
    StopScan,
    Connect {
        address: Option<Address>,
        name: Option<String>,
    },
    Disconnect,
    Forget,
    Start,
    Stop,
    Toggle,
    Speed {
        kmh: f64,
    },
    Nudge {
        delta_kmh: f64,
    },
    Stats {
        #[serde(default)]
        id: Value,
        #[serde(with = "PeriodName")]
        period: Period,
        #[serde(default, deserialize_with = "stats_offset")]
        offset: i32,
    },
}

impl Command {
    /// The `cmd` the shell sent.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Configure { .. } => "configure",
            Command::Sync => "sync",
            Command::Wake => "wake",
            Command::Scan => "scan",
            Command::StopScan => "stopScan",
            Command::Connect { .. } => "connect",
            Command::Disconnect => "disconnect",
            Command::Forget => "forget",
            Command::Start => "start",
            Command::Stop => "stop",
            Command::Toggle => "toggle",
            Command::Speed { .. } => "speed",
            Command::Nudge { .. } => "nudge",
            Command::Stats { .. } => "stats",
        }
    }
}

fn stats_offset<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i32, D::Error> {
    let offset = i32::deserialize(deserializer)?;
    if offset < MIN_STATS_OFFSET {
        return Err(serde::de::Error::custom(format!(
            "offset {offset} reaches back further than {MIN_STATS_OFFSET}"
        )));
    }
    Ok(offset)
}

/// A Bluetooth address, `AA:BB:CC:DD:EE:FF` in upper case.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Address(String);

impl Address {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Address {
    type Error = String;

    fn try_from(text: String) -> Result<Address, String> {
        let pairs: Vec<&str> = text.split(':').collect();
        let valid = pairs.len() == 6
            && pairs
                .iter()
                .all(|pair| pair.len() == 2 && pair.bytes().all(|b| b.is_ascii_hexdigit()));
        if valid {
            Ok(Address(text.to_ascii_uppercase()))
        } else {
            Err(format!("{text:?} is not a Bluetooth address"))
        }
    }
}

/// A line that is not a command the daemon knows.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct BadCommand {
    /// The `cmd` the line named, if it named one.
    pub cmd: Option<String>,
    pub message: String,
}

pub fn parse(line: &[u8]) -> Result<Command, BadCommand> {
    let value: Value = serde_json::from_slice(line).map_err(|error| BadCommand {
        cmd: None,
        message: error.to_string(),
    })?;
    let cmd = value.get("cmd").and_then(Value::as_str).map(str::to_owned);
    Command::deserialize(&value).map_err(|error| BadCommand {
        cmd,
        message: error.to_string(),
    })
}

/// Reads the shell's commands, one JSON object per line.
pub struct Commands<R> {
    reader: R,
    line: Vec<u8>,
    overlong: bool,
}

impl<R: AsyncBufRead + Unpin> Commands<R> {
    pub fn new(reader: R) -> Commands<R> {
        Commands {
            reader,
            line: Vec::new(),
            overlong: false,
        }
    }

    /// The next command, or why the next line is none; `None` once the
    /// input ends. Cancel safe: a partly read line waits for the next call.
    pub async fn next(&mut self) -> io::Result<Option<Result<Command, BadCommand>>> {
        loop {
            if self.line.len() > MAX_LINE {
                self.overlong = true;
                self.line.clear();
            }
            let room = MAX_LINE + 1 - self.line.len();
            let read = (&mut self.reader)
                .take(room as u64)
                .read_until(b'\n', &mut self.line)
                .await?;
            if self.line.last() == Some(&b'\n') {
                self.line.pop();
                return Ok(Some(self.finish()));
            }
            if read == 0 {
                let pending = !self.line.is_empty() || self.overlong;
                return Ok(pending.then(|| self.finish()));
            }
        }
    }

    fn finish(&mut self) -> Result<Command, BadCommand> {
        let line = mem::take(&mut self.line);
        if mem::take(&mut self.overlong) {
            return Err(BadCommand {
                cmd: None,
                message: format!("the line is longer than {MAX_LINE} bytes"),
            });
        }
        parse(&line)
    }
}

/// Writes `message` as one line and flushes it.
pub async fn write(output: &mut (impl AsyncWrite + Unpin), message: &Message) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    output.write_all(&line).await?;
    output.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use serde_json::json;

    fn json(message: &Message) -> String {
        serde_json::to_string(message).unwrap()
    }

    fn command(line: &str) -> Command {
        parse(line.as_bytes()).unwrap()
    }

    fn refusal(line: &[u8]) -> BadCommand {
        parse(line).unwrap_err()
    }

    fn walk() -> Session {
        Session {
            id: 12,
            started_at: 1_759_570_000,
            ended_at: None,
            active_s: 754,
            steps: 1023,
            distance_m: 870,
            kcal: 41.2,
            avg_kmh: 4.2,
            max_kmh: 4.5,
            estimated: false,
        }
    }

    fn full_state() -> State {
        State {
            bluetooth: AdapterState::On,
            link: Link::Connected,
            link_detail: None,
            device: Some(Device {
                address: "AA:BB:CC:DD:EE:FF".into(),
                name: Some("KS-ST-A1P".into()),
            }),
            auto_connect: true,
            scan: Scan {
                active: false,
                devices: vec![ScanDevice {
                    address: "AA:BB:CC:DD:EE:FF".into(),
                    name: Some("KS-ST-A1P".into()),
                    rssi: Some(-58),
                    candidate: true,
                }],
            },
            pad: Some(Pad {
                belt: Belt::Running,
                mode: Mode::Manual,
                speed_kmh: 3.5,
                target_kmh: 3.5,
                min_kmh: 0.5,
                max_kmh: 6.0,
                busy: None,
                imperial: false,
                shared: false,
            }),
            session: Some(walk()),
            last_session: Some(Session {
                id: 11,
                started_at: 1_759_560_000,
                ended_at: Some(1_759_561_900),
                active_s: 1800,
                steps: 2600,
                distance_m: 2100,
                kcal: 96.0,
                estimated: true,
                ..walk()
            }),
            today: Today {
                date: "2026-10-04".into(),
                steps: 6012,
                distance_m: 4310,
                active_s: 3900,
                kcal: 251.3,
                sessions: 3,
            },
            weight_kg: 70.0,
        }
    }

    #[test]
    fn serializes_a_full_state_exactly() {
        assert_eq!(
            json(&Message::State(Box::new(full_state()))),
            concat!(
                r#"{"type":"state","bluetooth":"on","link":"connected","linkDetail":null,"#,
                r#""device":{"address":"AA:BB:CC:DD:EE:FF","name":"KS-ST-A1P"},"autoConnect":true,"#,
                r#""scan":{"active":false,"devices":[{"address":"AA:BB:CC:DD:EE:FF","name":"KS-ST-A1P","rssi":-58,"candidate":true}]},"#,
                r#""pad":{"belt":"running","mode":"manual","speedKmh":3.5,"targetKmh":3.5,"minKmh":0.5,"maxKmh":6.0,"busy":null,"imperial":false,"shared":false},"#,
                r#""session":{"id":12,"startedAt":1759570000,"endedAt":null,"activeS":754,"steps":1023,"distanceM":870,"kcal":41.2,"avgKmh":4.2,"maxKmh":4.5,"estimated":false},"#,
                r#""lastSession":{"id":11,"startedAt":1759560000,"endedAt":1759561900,"activeS":1800,"steps":2600,"distanceM":2100,"kcal":96.0,"avgKmh":4.2,"maxKmh":4.5,"estimated":true},"#,
                r#""today":{"date":"2026-10-04","steps":6012,"distanceM":4310,"activeS":3900,"kcal":251.3,"sessions":3},"#,
                r#""weightKg":70.0}"#
            )
        );
    }

    #[test]
    fn serializes_an_unlinked_state_with_every_key_present() {
        let state = State {
            bluetooth: AdapterState::Blocked,
            link: Link::Disconnected,
            link_detail: Some("not-a-walkingpad"),
            device: Some(Device {
                address: "AA:BB:CC:DD:EE:FF".into(),
                name: None,
            }),
            scan: Scan::default(),
            pad: None,
            session: None,
            last_session: None,
            ..full_state()
        };
        let value = serde_json::to_value(Message::State(Box::new(state))).unwrap();
        assert_eq!(value["bluetooth"], "blocked");
        assert_eq!(value["link"], "disconnected");
        assert_eq!(value["linkDetail"], "not-a-walkingpad");
        assert_eq!(
            value["device"],
            json!({"address": "AA:BB:CC:DD:EE:FF", "name": null})
        );
        assert_eq!(value["scan"], json!({"active": false, "devices": []}));
        for key in ["pad", "session", "lastSession"] {
            assert_eq!(value.get(key), Some(&Value::Null), "{key}");
        }
        assert_eq!(value["weightKg"], 70.0);
    }

    #[test]
    fn names_every_bluetooth_state_and_belt_state() {
        let bluetooth = [
            (AdapterState::On, "on"),
            (AdapterState::Off, "off"),
            (AdapterState::Blocked, "blocked"),
            (AdapterState::Missing, "missing"),
            (AdapterState::Unavailable, "unavailable"),
        ];
        for (adapter, name) in bluetooth {
            let state = State {
                bluetooth: adapter,
                ..full_state()
            };
            let value = serde_json::to_value(Message::State(Box::new(state))).unwrap();
            assert_eq!(value["bluetooth"], name);
        }
        let belts = [
            (protocol::Belt::Idle, "idle"),
            (protocol::Belt::Starting, "starting"),
            (protocol::Belt::Running, "running"),
            (protocol::Belt::Standby, "standby"),
            (protocol::Belt::Unknown(3), "unknown"),
        ];
        for (belt, name) in belts {
            assert_eq!(serde_json::to_value(Belt::from(belt)).unwrap(), name);
        }
        let modes = [
            (protocol::Mode::Auto, "auto"),
            (protocol::Mode::Manual, "manual"),
            (protocol::Mode::Standby, "standby"),
            (protocol::Mode::Unknown(7), "unknown"),
        ];
        for (mode, name) in modes {
            assert_eq!(serde_json::to_value(Mode::from(mode)).unwrap(), name);
        }
    }

    #[test]
    fn reports_a_pad_view_as_the_pad() {
        let view = PadView {
            belt: protocol::Belt::Starting,
            mode: protocol::Mode::Unknown(9),
            speed_kmh: 0.0,
            target_kmh: 2.5,
            min_kmh: 0.5,
            max_kmh: 6.0,
            busy: Some(controller::Busy::Starting),
            imperial: true,
            shared: true,
        };
        assert_eq!(
            serde_json::to_value(Pad::from(view)).unwrap(),
            json!({"belt": "starting", "mode": "unknown", "speedKmh": 0.0, "targetKmh": 2.5,
                   "minKmh": 0.5, "maxKmh": 6.0, "busy": "starting", "imperial": true, "shared": true})
        );
        let stopping = PadView {
            busy: Some(controller::Busy::Stopping),
            ..view
        };
        assert_eq!(Pad::from(stopping).busy, Some(Busy::Stopping));
    }

    #[test]
    fn rounds_energy_and_speeds_to_a_tenth() {
        let started_at = Timestamp::from_second(1_759_570_000).unwrap();
        let session = store::Session {
            id: 3,
            started_at,
            updated_at: started_at,
            ended_at: Some(Timestamp::from_second(1_759_571_000).unwrap()),
            activity: Activity {
                active_s: 1000,
                steps: 1400,
                distance_m: 1001,
                kcal: 41.249_999,
            },
            max_kmh: 4.000_000_1,
            estimated: false,
        };
        assert_eq!(
            serde_json::to_value(Session::from(&session)).unwrap(),
            json!({"id": 3, "startedAt": 1_759_570_000, "endedAt": 1_759_571_000, "activeS": 1000,
                   "steps": 1400, "distanceM": 1001, "kcal": 41.2, "avgKmh": 3.6, "maxKmh": 4.0,
                   "estimated": false})
        );
    }

    #[test]
    fn serializes_stats_exactly() {
        let day = |key: &str, steps, kcal, distance_m, active_s| Bucket {
            key: key.into(),
            totals: Totals {
                steps,
                kcal,
                distance_m,
                active_s,
            },
        };
        let stats = Stats {
            id: json!(7),
            period: Period::Week,
            offset: 0,
            start: "2026-09-28".into(),
            end: "2026-10-04".into(),
            buckets: vec![
                day("2026-09-28", 5321, 212.4, 3900, 3400),
                day("2026-09-29", 0, 0.0, 0, 0),
            ],
            totals: Totals {
                steps: 5321,
                kcal: 212.4,
                distance_m: 3900,
                active_s: 3400,
            },
            active_buckets: 1,
        };
        assert_eq!(
            json(&Message::Stats(stats)),
            concat!(
                r#"{"type":"stats","id":7,"period":"week","offset":0,"start":"2026-09-28","end":"2026-10-04","#,
                r#""buckets":[{"key":"2026-09-28","steps":5321,"kcal":212.4,"distanceM":3900,"activeS":3400},"#,
                r#"{"key":"2026-09-29","steps":0,"kcal":0.0,"distanceM":0,"activeS":0}],"#,
                r#""totals":{"steps":5321,"kcal":212.4,"distanceM":3900,"activeS":3400},"activeBuckets":1}"#
            )
        );
    }

    #[test]
    fn reports_stats_for_the_clamped_offset_and_every_period_name() {
        let store = store::Store::open_in_memory().unwrap();
        let now = Timestamp::from_second(1_759_570_000).unwrap();
        for (period, name, buckets) in [
            (Period::Day, "day", 24),
            (Period::Week, "week", 7),
            (Period::Month, "month", 31),
            (Period::Year, "year", 12),
        ] {
            let stats = stats::query(&store, &jiff::tz::TimeZone::UTC, now, period, 3).unwrap();
            let value =
                serde_json::to_value(Message::Stats(Stats::new(json!("a"), period, &stats)))
                    .unwrap();
            assert_eq!(value["id"], "a");
            assert_eq!(value["period"], name);
            assert_eq!(value["offset"], 0);
            assert_eq!(value["buckets"].as_array().unwrap().len(), buckets);
            assert_eq!(value["activeBuckets"], 0);
        }
    }

    #[test]
    fn serializes_hello_error_and_fatal_exactly() {
        assert_eq!(
            json(&Message::hello(false)),
            concat!(
                r#"{"type":"hello","version":""#,
                env!("CARGO_PKG_VERSION"),
                r#"","simulated":false}"#
            )
        );
        assert_eq!(
            json(&Message::Error {
                code: "not-connected",
                message: "the pad is not connected".into(),
                cmd: Some("start".into()),
            }),
            r#"{"type":"error","code":"not-connected","message":"the pad is not connected","cmd":"start"}"#
        );
        assert_eq!(
            json(&Message::Error {
                code: "bad-command",
                message: "expected value at line 1 column 1".into(),
                cmd: None,
            }),
            r#"{"type":"error","code":"bad-command","message":"expected value at line 1 column 1","cmd":null}"#
        );
        assert_eq!(
            json(&Message::Fatal {
                code: "already-running",
                message: "another ks-fit daemon is running".into(),
            }),
            r#"{"type":"fatal","code":"already-running","message":"another ks-fit daemon is running"}"#
        );
    }

    #[test]
    fn parses_every_command() {
        let simple = [
            ("sync", Command::Sync),
            ("wake", Command::Wake),
            ("scan", Command::Scan),
            ("stopScan", Command::StopScan),
            ("disconnect", Command::Disconnect),
            ("forget", Command::Forget),
            ("start", Command::Start),
            ("stop", Command::Stop),
            ("toggle", Command::Toggle),
        ];
        for (name, expected) in simple {
            let parsed = command(&format!(r#"{{"cmd":"{name}"}}"#));
            assert_eq!(parsed.name(), name);
            assert_eq!(parsed, expected);
        }
        let cases = [
            (
                r#"{"cmd":"configure","weightKg":72.5,"autoConnect":false}"#,
                Command::Configure {
                    weight_kg: Some(72.5),
                    auto_connect: Some(false),
                },
            ),
            (
                r#"{"cmd":"configure","weightKg":82}"#,
                Command::Configure {
                    weight_kg: Some(82.0),
                    auto_connect: None,
                },
            ),
            (
                r#"{"cmd":"configure"}"#,
                Command::Configure {
                    weight_kg: None,
                    auto_connect: None,
                },
            ),
            (
                r#"{"cmd":"connect","address":"AA:BB:CC:DD:EE:FF","name":"KS-ST-A1P"}"#,
                Command::Connect {
                    address: Some(Address("AA:BB:CC:DD:EE:FF".into())),
                    name: Some("KS-ST-A1P".into()),
                },
            ),
            (
                r#"{"cmd":"connect"}"#,
                Command::Connect {
                    address: None,
                    name: None,
                },
            ),
            (r#"{"cmd":"speed","kmh":3.5}"#, Command::Speed { kmh: 3.5 }),
            (
                r#"{"cmd":"nudge","deltaKmh":-0.5}"#,
                Command::Nudge { delta_kmh: -0.5 },
            ),
            (
                r#"{"cmd":"stats","id":7,"period":"week","offset":-1}"#,
                Command::Stats {
                    id: json!(7),
                    period: Period::Week,
                    offset: -1,
                },
            ),
            (
                r#"{"cmd":"stats","period":"year"}"#,
                Command::Stats {
                    id: Value::Null,
                    period: Period::Year,
                    offset: 0,
                },
            ),
        ];
        for (line, expected) in cases {
            assert_eq!(command(line), expected, "{line}");
        }
        for (period, name) in [(Period::Day, "day"), (Period::Month, "month")] {
            assert_eq!(
                command(&format!(r#"{{"cmd":"stats","id":"x","period":"{name}"}}"#)),
                Command::Stats {
                    id: json!("x"),
                    period,
                    offset: 0
                }
            );
        }
    }

    #[test]
    fn tolerates_unknown_fields() {
        assert_eq!(
            command(r#"{"cmd":"sync","seq":4,"extra":{"a":[1]}}"#),
            Command::Sync
        );
        assert_eq!(
            command(r#"{"id":1,"cmd":"speed","kmh":4,"unit":"kmh"}"#),
            Command::Speed { kmh: 4.0 }
        );
    }

    #[test]
    fn upper_cases_addresses() {
        assert_eq!(
            command(r#"{"cmd":"connect","address":"aa:bb:cc:dd:ee:0f"}"#),
            Command::Connect {
                address: Some(Address("AA:BB:CC:DD:EE:0F".into())),
                name: None,
            }
        );
    }

    #[test]
    fn rejects_malformed_lines_naming_the_command_when_there_is_one() {
        let cases: [(&[u8], Option<&str>); 17] = [
            (b"not json", None),
            (b"", None),
            (b"\xff\xfe{}", None),
            (br#"{"cmd":"st"#, None),
            (b"[1,2]", None),
            (br#""start""#, None),
            (b"{}", None),
            (br#"{"cmd":5}"#, None),
            (br#"{"cmd":"mode","mode":"manual"}"#, Some("mode")),
            (br#"{"cmd":"speed"}"#, Some("speed")),
            (br#"{"cmd":"speed","kmh":"fast"}"#, Some("speed")),
            (br#"{"cmd":"nudge","deltaKmh":null}"#, Some("nudge")),
            (
                br#"{"cmd":"configure","weightKg":"heavy"}"#,
                Some("configure"),
            ),
            (
                br#"{"cmd":"connect","address":"AA:BB:CC:DD:EE"}"#,
                Some("connect"),
            ),
            (
                br#"{"cmd":"connect","address":"GG:BB:CC:DD:EE:FF"}"#,
                Some("connect"),
            ),
            (br#"{"cmd":"stats","period":"fortnight"}"#, Some("stats")),
            (
                br#"{"cmd":"stats","period":"day","offset":-10001}"#,
                Some("stats"),
            ),
        ];
        for (line, cmd) in cases {
            let refused = refusal(line);
            assert_eq!(
                refused.cmd.as_deref(),
                cmd,
                "{}",
                String::from_utf8_lossy(line)
            );
            assert!(!refused.message.is_empty());
        }
        assert!(
            refusal(br#"{"cmd":"connect","address":"nope"}"#)
                .message
                .contains("not a Bluetooth address")
        );
        assert_eq!(
            command(r#"{"cmd":"stats","period":"day","offset":-10000}"#),
            Command::Stats {
                id: Value::Null,
                period: Period::Day,
                offset: MIN_STATS_OFFSET
            }
        );
    }

    #[tokio::test]
    async fn reads_one_command_per_line_through_bad_and_overlong_lines() {
        let mut input = b"{\"cmd\":\"sync\"}\n\xff\n".to_vec();
        input.extend(vec![b' '; MAX_LINE + 10]);
        input.extend(b"\n{\"cmd\":\"stop\"}\n");
        input.extend(vec![b' '; MAX_LINE]);
        input.extend(b"{\"cmd\":\"wake\"}\n{\"cmd\":\"start\"}");
        let mut commands = Commands::new(input.as_slice());
        assert_eq!(commands.next().await.unwrap(), Some(Ok(Command::Sync)));
        assert_eq!(
            commands.next().await.unwrap().unwrap().unwrap_err().cmd,
            None
        );
        assert!(
            commands
                .next()
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .message
                .contains("longer than")
        );
        assert_eq!(commands.next().await.unwrap(), Some(Ok(Command::Stop)));
        assert!(commands.next().await.unwrap().unwrap().is_err());
        assert_eq!(commands.next().await.unwrap(), Some(Ok(Command::Start)));
        assert_eq!(commands.next().await.unwrap(), None);
        assert_eq!(commands.next().await.unwrap(), None);
    }

    #[tokio::test]
    async fn accepts_a_line_of_exactly_the_longest_length() {
        let mut input = br#"{"cmd":"sync"}"#.to_vec();
        input.resize(MAX_LINE, b' ');
        input.push(b'\n');
        let mut commands = Commands::new(input.as_slice());
        assert_eq!(commands.next().await.unwrap(), Some(Ok(Command::Sync)));
        assert_eq!(commands.next().await.unwrap(), None);
    }

    #[tokio::test]
    async fn writes_one_message_per_line() {
        let mut output = Vec::new();
        write(&mut output, &Message::hello(true)).await.unwrap();
        write(
            &mut output,
            &Message::Fatal {
                code: "data-dir",
                message: "no".into(),
            },
        )
        .await
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(text.ends_with('\n'));
        assert!(lines[0].contains(r#""simulated":true"#));
        assert_eq!(
            lines[1],
            r#"{"type":"fatal","code":"data-dir","message":"no"}"#
        );
    }
}
