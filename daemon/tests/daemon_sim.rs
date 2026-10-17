use std::future;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use ks_fit::daemon::{Clock, Daemon};
use ks_fit::protocol::{Belt, Command};
use ks_fit::radio::{Radio, sim};
use ks_fit::store::{Activity, Device, Store};
use ks_fit::tracker::Tracker;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, duplex};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant, sleep, timeout, timeout_at};

type Served = Daemon<DuplexStream, DuplexStream>;

const ORIGIN: &str = "2026-10-14T10:00:00Z";

struct Shell {
    input: Option<DuplexStream>,
    lines: mpsc::UnboundedReceiver<(Instant, Value)>,
    sim: sim::Control,
    daemon: JoinHandle<Served>,
    started: Instant,
}

impl Shell {
    fn start() -> Shell {
        Shell::with(Store::open_in_memory().unwrap())
    }

    fn remembering_the_pad() -> Shell {
        let store = Store::open_in_memory().unwrap();
        store
            .remember_device(sim::PAD_ADDRESS, Some(sim::PAD_NAME))
            .unwrap();
        Shell::with(store)
    }

    fn with(store: Store) -> Shell {
        let origin = origin();
        let tracker = Tracker::open(store, TimeZone::UTC, origin).unwrap();
        let (radio, sim) = Radio::simulated();
        let (input, daemon_input) = duplex(1 << 17);
        let (daemon_output, output) = duplex(1 << 16);
        let mut daemon = Daemon::new(radio, tracker, daemon_input, daemon_output)
            .unwrap()
            .simulated(true)
            .with_clock(Clock::tokio(origin));
        let daemon = tokio::spawn(async move {
            daemon.run(future::pending()).await.unwrap();
            daemon
        });
        let (sender, lines) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut output = BufReader::new(output).lines();
            while let Some(line) = output.next_line().await.unwrap() {
                let value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"));
                if sender.send((Instant::now(), value)).is_err() {
                    return;
                }
            }
        });
        Shell {
            input: Some(input),
            lines,
            sim,
            daemon,
            started: Instant::now(),
        }
    }

    async fn send(&mut self, command: Value) {
        self.send_line(command.to_string().as_bytes()).await;
    }

    async fn send_line(&mut self, line: &[u8]) {
        let input = self.input.as_mut().unwrap();
        input.write_all(line).await.unwrap();
        input.write_all(b"\n").await.unwrap();
    }

    async fn next_until(&mut self, deadline: Instant) -> Option<(Instant, Value)> {
        match timeout_at(deadline, self.lines.recv()).await {
            Ok(Some(line)) => Some(line),
            Ok(None) => panic!("the daemon closed its output"),
            Err(_) => None,
        }
    }

    async fn message(&mut self, kind: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let (_, message) = self
                .next_until(deadline)
                .await
                .unwrap_or_else(|| panic!("no {kind} message"));
            if message["type"] == kind {
                return message;
            }
        }
    }

    async fn state(&mut self, what: &str, done: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(900);
        let mut last = Value::Null;
        loop {
            let Some((_, message)) = self.next_until(deadline).await else {
                panic!("no state where {what}; the last was {last}");
            };
            if message["type"] == "state" {
                if done(&message) {
                    return message;
                }
                last = message;
            }
        }
    }

    async fn watch(&mut self, span: Duration) -> Vec<(Duration, Value)> {
        let until = Instant::now() + span;
        let mut seen = Vec::new();
        while let Some((at, message)) = self.next_until(until).await {
            seen.push((at - self.started, message));
        }
        seen
    }

    async fn quit(mut self) -> Served {
        drop(self.input.take());
        self.daemon.await.unwrap()
    }

    async fn greeted(&mut self) -> Value {
        assert_eq!(
            self.message("hello").await,
            json!({"type": "hello", "version": env!("CARGO_PKG_VERSION"), "simulated": true})
        );
        self.message("state").await
    }

    async fn link(&mut self) -> Value {
        self.send(json!({"cmd": "connect", "address": sim::PAD_ADDRESS, "name": sim::PAD_NAME}))
            .await;
        self.state("the pad is connected", |s| s["link"] == "connected")
            .await
    }

    async fn walk(&mut self) -> Value {
        self.send(json!({"cmd": "start"})).await;
        self.state("the belt runs", |s| {
            s["pad"]["belt"] == "running" && s["pad"]["busy"].is_null()
        })
        .await
    }

    async fn walked_for(&mut self, seconds: u64) -> Value {
        self.state("the session grew", |s| {
            s["session"]["activeS"]
                .as_u64()
                .is_some_and(|active| active >= seconds)
        })
        .await
    }
}

fn origin() -> Timestamp {
    ORIGIN.parse().unwrap()
}

fn errors(seen: &[(Duration, Value)]) -> Vec<(String, Value)> {
    seen.iter()
        .filter(|(_, message)| message["type"] == "error")
        .map(|(_, message)| {
            (
                message["code"].as_str().unwrap().to_owned(),
                message["cmd"].clone(),
            )
        })
        .collect()
}

fn states(seen: &[(Duration, Value)]) -> impl Iterator<Item = &(Duration, Value)> {
    seen.iter()
        .filter(|(_, message)| message["type"] == "state")
}

fn search_starts(seen: &[(Duration, Value)]) -> Vec<u64> {
    let mut starts = Vec::new();
    let mut searching = false;
    for (at, state) in states(seen) {
        let now_searching = state["link"] == "searching";
        if now_searching && !searching {
            starts.push(at.as_secs());
        }
        searching = now_searching;
    }
    starts
}

fn activity_of(session: &Value) -> Activity {
    let count = |key: &str| u32::try_from(session[key].as_u64().unwrap()).unwrap();
    Activity {
        active_s: count("activeS"),
        steps: count("steps"),
        distance_m: count("distanceM"),
        kcal: session["kcal"].as_f64().unwrap(),
    }
}

#[tokio::test(start_paused = true)]
async fn walks_from_scan_to_stop_and_records_the_walk() {
    let mut shell = Shell::start();
    let first = shell.greeted().await;
    assert_eq!(first["bluetooth"], "on");
    assert_eq!(first["link"], "disconnected");
    assert_eq!(first["linkDetail"], Value::Null);
    assert_eq!(first["device"], Value::Null);
    assert_eq!(first["autoConnect"], true);
    assert_eq!(first["scan"], json!({"active": false, "devices": []}));
    assert_eq!(first["pad"], Value::Null);
    assert_eq!(first["session"], Value::Null);
    assert_eq!(first["lastSession"], Value::Null);
    assert_eq!(
        first["today"],
        json!({"date": "2026-10-14", "steps": 0, "distanceM": 0, "activeS": 0, "kcal": 0.0, "sessions": 0})
    );
    assert_eq!(first["weightKg"], 70.0);

    shell
        .send(json!({"cmd": "configure", "weightKg": 80}))
        .await;
    shell
        .state("the weight is 80 kg", |s| s["weightKg"] == 80.0)
        .await;

    shell.send(json!({"cmd": "scan"})).await;
    let scanned = shell
        .state("both devices are listed", |s| {
            s["scan"]["devices"].as_array().unwrap().len() == 2
        })
        .await;
    assert_eq!(
        scanned["scan"],
        json!({"active": true, "devices": [
            {"address": sim::PAD_ADDRESS, "name": sim::PAD_NAME, "rssi": -58, "candidate": true},
            {"address": sim::HEADPHONES_ADDRESS, "name": sim::HEADPHONES_NAME, "rssi": -71, "candidate": false}
        ]})
    );

    shell
        .send(json!({"cmd": "connect", "address": sim::PAD_ADDRESS, "name": sim::PAD_NAME}))
        .await;
    let searching = shell
        .state("the pad is looked for", |s| s["link"] == "searching")
        .await;
    assert_eq!(
        searching["device"],
        json!({"address": sim::PAD_ADDRESS, "name": sim::PAD_NAME})
    );
    let linked = shell
        .state("the pad is connected", |s| s["link"] == "connected")
        .await;
    assert_eq!(linked["pad"]["belt"], "standby");
    assert_eq!(linked["pad"]["mode"], "standby");
    assert_eq!(linked["pad"]["targetKmh"], 2.0);
    assert_eq!(linked["pad"]["busy"], Value::Null);
    let scan_over = shell
        .state("the scan is over", |s| s["scan"]["active"] == false)
        .await;
    assert_eq!(scan_over["scan"]["devices"].as_array().unwrap().len(), 2);

    shell.send(json!({"cmd": "speed", "kmh": 3.0})).await;
    shell
        .state("the target is 3 km/h", |s| s["pad"]["targetKmh"] == 3.0)
        .await;

    shell.send(json!({"cmd": "start"})).await;
    shell
        .state("the belt is starting", |s| s["pad"]["busy"] == "starting")
        .await;
    let running = shell
        .state("the belt runs at the target", |s| {
            s["pad"]["belt"] == "running"
                && s["pad"]["speedKmh"] == 3.0
                && s["pad"]["busy"].is_null()
        })
        .await;
    assert_eq!(running["pad"]["mode"], "manual");

    let early = shell.walked_for(60).await;
    let walked = shell.walked_for(300).await;
    let session = &walked["session"];
    assert_eq!(session["id"], early["session"]["id"]);
    assert!(session["steps"].as_u64().unwrap() > early["session"]["steps"].as_u64().unwrap());
    assert!(session["distanceM"].as_u64().unwrap() > 200);
    assert!(session["kcal"].as_f64().unwrap() > 10.0);
    assert_eq!(session["endedAt"], Value::Null);
    assert_eq!(session["estimated"], false);
    assert_eq!(walked["today"]["steps"], session["steps"]);
    assert_eq!(walked["today"]["distanceM"], session["distanceM"]);
    assert_eq!(walked["today"]["sessions"], 1);
    assert_eq!(walked["lastSession"], Value::Null);

    shell.send(json!({"cmd": "nudge", "deltaKmh": 0.5})).await;
    shell
        .state("the belt runs at 3.5 km/h", |s| {
            s["pad"]["speedKmh"] == 3.5 && s["pad"]["targetKmh"] == 3.5
        })
        .await;

    shell.send(json!({"cmd": "stop"})).await;
    shell
        .state("the belt is stopping", |s| s["pad"]["busy"] == "stopping")
        .await;
    shell
        .state("the belt stands", |s| {
            s["pad"]["belt"] == "idle" && s["pad"]["busy"].is_null()
        })
        .await;
    let closed = shell
        .state("the session is closed", |s| {
            s["session"].is_null() && s["lastSession"].is_object()
        })
        .await;
    let last = &closed["lastSession"];
    assert_eq!(last["id"], session["id"]);
    assert!(last["endedAt"].as_i64().unwrap() > last["startedAt"].as_i64().unwrap());
    assert!(last["activeS"].as_u64().unwrap() >= 300);
    assert_eq!(last["maxKmh"], 3.5);
    assert_eq!(closed["today"]["steps"], last["steps"]);
    assert_eq!(closed["today"]["kcal"], last["kcal"]);

    shell
        .send(json!({"cmd": "stats", "id": 7, "period": "day", "offset": 0}))
        .await;
    let stats = shell.message("stats").await;
    assert_eq!(stats["id"], 7);
    assert_eq!(stats["period"], "day");
    assert_eq!(stats["offset"], 0);
    assert_eq!(stats["start"], "2026-10-14");
    assert_eq!(stats["buckets"].as_array().unwrap().len(), 24);
    assert_eq!(stats["buckets"][10]["key"], "2026-10-14T10");
    assert_eq!(stats["buckets"][10]["steps"], last["steps"]);
    assert_eq!(stats["totals"]["distanceM"], last["distanceM"]);
    assert_eq!(stats["activeBuckets"], 1);

    let writes = shell.sim.writes().await;
    for pair in writes.windows(2) {
        assert!(
            pair[1].at - pair[0].at >= Duration::from_millis(690),
            "writes too close: {pair:?}"
        );
    }
    let starts: Vec<_> = writes
        .iter()
        .filter(|write| write.frame == Command::StartBelt.encode())
        .collect();
    assert_eq!(starts.len(), 1);
    assert!(matches!(starts[0].belt, Belt::Idle | Belt::Standby));

    let daemon = shell.quit().await;
    let store = daemon.tracker().store();
    assert_eq!(
        store.remembered_device().unwrap(),
        Some(Device {
            address: sim::PAD_ADDRESS.into(),
            name: Some(sim::PAD_NAME.into()),
        })
    );
    assert_eq!(store.target_kmh().unwrap(), Some(3.5));
    assert_eq!(store.weight_kg().unwrap(), Some(80.0));
    let recorded = store.latest_closed_session().unwrap().unwrap();
    assert_eq!(json!(recorded.id), last["id"]);
    assert_eq!(
        json!(recorded.ended_at.unwrap().as_second()),
        last["endedAt"]
    );
    let expected = activity_of(last);
    assert_eq!(recorded.activity.steps, expected.steps);
    assert_eq!(recorded.activity.distance_m, expected.distance_m);
    assert_eq!(recorded.activity.active_s, expected.active_s);
    let minutes: Activity = store
        .minutes(origin()..origin() + SignedDuration::from_hours(1))
        .unwrap()
        .into_iter()
        .map(|(_, activity)| activity)
        .sum();
    assert_eq!(minutes.steps, recorded.activity.steps);
    assert_eq!(minutes.distance_m, recorded.activity.distance_m);
    assert_eq!(minutes.active_s, recorded.activity.active_s);
    assert!((minutes.kcal - recorded.activity.kcal).abs() < 1e-6);
}

#[tokio::test(start_paused = true)]
async fn a_dropped_link_reconnects_into_the_same_session_without_touching_the_belt() {
    let mut shell = Shell::start();
    shell.greeted().await;
    shell.link().await;
    shell.walk().await;
    let before = shell.walked_for(60).await;
    let id = before["session"]["id"].clone();
    let writes_before = shell.sim.writes().await.len();

    shell.sim.drop_link();
    let lost = shell
        .state("the link is gone", |s| s["link"] != "connected")
        .await;
    assert_eq!(lost["pad"], Value::Null);
    assert_eq!(lost["session"], Value::Null);
    assert_eq!(lost["lastSession"]["id"], id);
    let back = shell
        .state("the pad is back", |s| s["link"] == "connected")
        .await;
    assert_eq!(back["pad"]["belt"], "running");
    let resumed = shell
        .state("the walk goes on in the same session", |s| {
            s["session"]["activeS"]
                .as_u64()
                .is_some_and(|active| active >= 90)
        })
        .await;
    assert_eq!(resumed["session"]["id"], id);
    assert_eq!(resumed["session"]["estimated"], false);
    assert_eq!(resumed["today"]["sessions"], 1);

    let polls = [Command::AskStats.encode(), Command::QueryPrefs.encode()];
    let writes = shell.sim.writes().await;
    assert!(
        writes[writes_before..]
            .iter()
            .all(|write| polls.contains(&write.frame)),
        "{:?}",
        &writes[writes_before..]
    );
    assert_eq!(writes.last().unwrap().belt, Belt::Running);
}

#[tokio::test(start_paused = true)]
async fn a_link_lost_while_the_belt_stops_reports_the_stop_as_not_acknowledged() {
    let mut shell = Shell::start();
    shell.greeted().await;
    shell.link().await;
    shell.send(json!({"cmd": "speed", "kmh": 4.0})).await;
    shell.walk().await;
    shell
        .state("the belt runs at 4 km/h", |s| s["pad"]["speedKmh"] == 4.0)
        .await;

    shell.send(json!({"cmd": "stop"})).await;
    shell
        .state("the belt is stopping", |s| s["pad"]["busy"] == "stopping")
        .await;
    shell.sim.drop_link();
    let seen = shell.watch(Duration::from_secs(5)).await;
    assert_eq!(
        errors(&seen),
        [("stop-not-acknowledged".to_owned(), json!("stop"))]
    );
    let (_, after) = states(&seen).next().unwrap();
    assert_eq!(after["pad"], Value::Null);
}

#[tokio::test(start_paused = true)]
async fn forgetting_the_pad_drops_the_link_and_stops_reconnecting() {
    let mut shell = Shell::start();
    shell.greeted().await;
    shell.link().await;

    shell.send(json!({"cmd": "forget"})).await;
    let forgotten = shell
        .state("the pad is forgotten", |s| s["device"].is_null())
        .await;
    assert_eq!(forgotten["link"], "disconnected");
    assert_eq!(forgotten["linkDetail"], Value::Null);
    assert_eq!(forgotten["pad"], Value::Null);

    let mut seen = shell.watch(Duration::from_secs(60)).await;
    shell.send(json!({"cmd": "wake"})).await;
    shell.send(json!({"cmd": "connect"})).await;
    seen.extend(shell.watch(Duration::from_secs(120)).await);
    assert_eq!(search_starts(&seen), Vec::<u64>::new());
    assert_eq!(errors(&seen), [("no-device".to_owned(), json!("connect"))]);

    let daemon = shell.quit().await;
    assert_eq!(daemon.tracker().store().remembered_device().unwrap(), None);
}

#[tokio::test(start_paused = true)]
async fn toggle_starts_a_standing_belt_and_stops_a_moving_one() {
    let mut shell = Shell::start();
    shell.greeted().await;
    shell.link().await;

    shell.send(json!({"cmd": "toggle"})).await;
    shell
        .state("the belt is starting", |s| s["pad"]["busy"] == "starting")
        .await;
    shell
        .state("the belt runs", |s| {
            s["pad"]["belt"] == "running" && s["pad"]["busy"].is_null()
        })
        .await;

    shell.send(json!({"cmd": "toggle"})).await;
    shell
        .state("the belt is stopping", |s| s["pad"]["busy"] == "stopping")
        .await;
    shell
        .state("the belt stands", |s| {
            s["pad"]["belt"] == "idle" && s["pad"]["busy"].is_null()
        })
        .await;

    let writes = shell.sim.writes().await;
    let count = |command: Command| {
        writes
            .iter()
            .filter(|write| write.frame == command.encode())
            .count()
    };
    assert_eq!(count(Command::StartBelt), 1);
    assert!(count(Command::ChangeSpeed(0)) >= 1);
}

#[tokio::test(start_paused = true)]
async fn wake_searches_at_most_once_every_20_seconds_and_never_while_paused() {
    let mut shell = Shell::start();
    shell.greeted().await;
    shell.sim.set_advertising(false);
    shell
        .send(json!({"cmd": "connect", "address": sim::PAD_ADDRESS}))
        .await;
    let missing = shell
        .state("the pad is not found", |s| s["linkDetail"] == "not-found")
        .await;
    assert_eq!(missing["link"], "disconnected");
    assert_eq!(missing["device"]["name"], Value::Null);

    shell.send(json!({"cmd": "wake"})).await;
    let mut seen = shell.watch(Duration::from_secs(13)).await;
    shell.sim.set_advertising(true);
    shell.send(json!({"cmd": "wake"})).await;
    seen.extend(shell.watch(Duration::from_secs(3)).await);
    assert_eq!(search_starts(&seen), [21]);
    let (_, linked) = states(&seen).last().unwrap();
    assert_eq!(linked["link"], "connected");
    assert_eq!(linked["device"]["name"], sim::PAD_NAME);

    shell.send(json!({"cmd": "disconnect"})).await;
    let paused = shell
        .state("the link is paused", |s| s["linkDetail"] == "paused")
        .await;
    assert_eq!(paused["link"], "disconnected");
    let mut seen = shell.watch(Duration::from_secs(30)).await;
    shell.send(json!({"cmd": "wake"})).await;
    shell.send(json!({"cmd": "start"})).await;
    seen.extend(shell.watch(Duration::from_secs(30)).await);
    assert_eq!(search_starts(&seen), Vec::<u64>::new());
    assert_eq!(
        errors(&seen),
        [("not-connected".to_owned(), json!("start"))]
    );

    shell.send(json!({"cmd": "connect"})).await;
    let resumed = shell
        .state("the pad is connected again", |s| s["link"] == "connected")
        .await;
    assert_eq!(resumed["linkDetail"], Value::Null);
}

#[tokio::test(start_paused = true)]
async fn retries_a_remembered_pad_for_ten_minutes_after_startup_then_only_on_attention() {
    let mut shell = Shell::remembering_the_pad();
    shell.sim.set_advertising(false);
    let seen = shell.watch(Duration::from_secs(20 * 60)).await;
    assert_eq!(search_starts(&seen), [0, 23, 61, 129, 257, 385, 513]);
    let last = states(&seen).last().unwrap();
    assert_eq!(last.1["link"], "disconnected");
    assert_eq!(last.1["linkDetail"], "not-found");

    shell.sim.set_advertising(true);
    shell.send(json!({"cmd": "start"})).await;
    let seen = shell.watch(Duration::from_secs(5)).await;
    assert_eq!(
        errors(&seen),
        [("not-connected".to_owned(), json!("start"))]
    );
    assert_eq!(search_starts(&seen), [1200]);
    let (_, linked) = states(&seen).last().unwrap();
    assert_eq!(linked["link"], "connected");
    assert_eq!(linked["pad"]["belt"], "standby");
    assert_eq!(linked["pad"]["busy"], Value::Null);
    shell.watch(Duration::from_secs(10)).await;
    let writes = shell.sim.writes().await;
    assert!(
        writes
            .iter()
            .all(|write| write.frame != Command::StartBelt.encode())
    );
}

#[tokio::test(start_paused = true)]
async fn coalesces_bursts_into_one_snapshot_per_quarter_second_keeping_the_last() {
    let mut shell = Shell::start();
    shell.greeted().await;
    shell.watch(Duration::from_secs(1)).await;

    let burst = |weights: &[u32]| {
        weights
            .iter()
            .map(|kg| format!("{}\n", json!({"cmd": "configure", "weightKg": kg})))
            .collect::<String>()
    };
    let weights = |seen: &[(Duration, Value)]| {
        states(seen)
            .map(|(at, state)| (*at, state["weightKg"].as_f64().unwrap()))
            .collect::<Vec<_>>()
    };

    shell
        .send_line(
            burst(&[71, 72, 73, 74, 75, 76, 77, 78, 79, 80])
                .trim_end()
                .as_bytes(),
        )
        .await;
    let seen = weights(&shell.watch(Duration::from_secs(2)).await);
    assert_eq!(
        seen.iter().map(|(_, kg)| *kg).collect::<Vec<_>>(),
        [71.0, 80.0]
    );
    assert!(seen[1].0 - seen[0].0 >= Duration::from_millis(250));

    shell
        .send_line(burst(&[81, 82, 81]).trim_end().as_bytes())
        .await;
    let seen = weights(&shell.watch(Duration::from_secs(2)).await);
    assert_eq!(seen.iter().map(|(_, kg)| *kg).collect::<Vec<_>>(), [81.0]);

    shell.send(json!({"cmd": "sync"})).await;
    let seen = weights(&shell.watch(Duration::from_secs(1)).await);
    assert_eq!(seen.iter().map(|(_, kg)| *kg).collect::<Vec<_>>(), [81.0]);
}

#[tokio::test(start_paused = true)]
async fn refuses_malformed_commands_and_keeps_running() {
    let mut shell = Shell::start();
    shell.greeted().await;

    let mut overlong = vec![b' '; 70_000];
    overlong.extend(br#"{"cmd":"sync"}"#);
    let lines: [&[u8]; 9] = [
        b"not json",
        b"\xff\xfe{\"cmd\":\"sync\"}",
        &overlong,
        br#"{"cmd":"mode","mode":"manual"}"#,
        br#"{"cmd":"speed","kmh":"fast"}"#,
        br#"{"cmd":"connect","address":"not-an-address"}"#,
        br#"{"cmd":"stop"}"#,
        br#"{"cmd":"nudge","deltaKmh":0.5}"#,
        br#"{"cmd":"connect"}"#,
    ];
    for line in lines {
        shell.send_line(line).await;
    }
    let seen = shell.watch(Duration::from_secs(1)).await;
    assert_eq!(
        errors(&seen),
        [
            ("bad-command".to_owned(), Value::Null),
            ("bad-command".to_owned(), Value::Null),
            ("bad-command".to_owned(), Value::Null),
            ("bad-command".to_owned(), json!("mode")),
            ("bad-command".to_owned(), json!("speed")),
            ("bad-command".to_owned(), json!("connect")),
            ("not-connected".to_owned(), json!("stop")),
            ("not-connected".to_owned(), json!("nudge")),
            ("no-device".to_owned(), json!("connect")),
        ]
    );
    assert!(seen.iter().all(|(_, message)| message["type"] != "state"));

    shell.send(json!({"cmd": "sync"})).await;
    let state = shell.message("state").await;
    assert_eq!(state["link"], "disconnected");
    assert_eq!(state["device"], Value::Null);
}

#[tokio::test(start_paused = true)]
async fn closing_stdin_closes_the_session_and_leaves_the_belt_running() {
    let mut shell = Shell::start();
    shell.greeted().await;
    shell.link().await;
    shell.walk().await;
    let walking = shell.walked_for(30).await;
    let id = walking["session"]["id"].clone();
    let sim = shell.sim.clone();
    let closed_at = Instant::now();

    let daemon = shell.quit().await;
    let tracker = daemon.tracker();
    assert!(tracker.session().is_none());
    let session = tracker.store().latest_closed_session().unwrap().unwrap();
    assert_eq!(json!(session.id), id);
    assert!(session.ended_at.is_some());

    sleep(Duration::from_secs(5)).await;
    let writes = sim.writes().await;
    assert!(!writes.is_empty());
    assert!(writes.iter().all(|write| write.at <= closed_at));
    assert!(
        writes
            .iter()
            .all(|write| write.frame != Command::ChangeSpeed(0).encode())
    );
    assert_eq!(writes.last().unwrap().belt, Belt::Running);
}

#[tokio::test(start_paused = true)]
async fn shuts_down_on_the_signal_while_the_shell_is_not_reading() {
    let store = Store::open_in_memory().unwrap();
    let tracker = Tracker::open(store, TimeZone::UTC, origin()).unwrap();
    let (radio, _sim) = Radio::simulated();
    let (mut input, daemon_input) = duplex(1 << 16);
    let (daemon_output, _unread) = duplex(256);
    let mut daemon = Daemon::new(radio, tracker, daemon_input, daemon_output)
        .unwrap()
        .with_clock(Clock::tokio(origin()));
    let (stop, stopped) = oneshot::channel::<()>();
    let served = tokio::spawn(async move {
        daemon
            .run(async {
                let _ = stopped.await;
            })
            .await
    });
    for kg in 21..60 {
        let line = format!("{}\n", json!({"cmd": "configure", "weightKg": kg}));
        input.write_all(line.as_bytes()).await.unwrap();
        sleep(Duration::from_millis(300)).await;
    }
    stop.send(()).unwrap();
    let outcome = timeout(Duration::from_secs(2), served).await;
    assert!(matches!(outcome, Ok(Ok(Ok(())))), "{outcome:?}");
}
