//! A simulated adapter and pad that speak real frames.

use std::cmp::Ordering;
use std::future;

use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, Instant, sleep_until};

use super::{
    AdapterState, Call, ConnectFailure, LinkId, NOTIFY_CHARACTERISTIC, RadioEvent,
    WALKINGPAD_SERVICE, WRITE_CHARACTERISTIC, bluetooth_uuid, classify, is_candidate,
};
use crate::protocol::{Belt, checksum};

pub const PAD_ADDRESS: &str = "AA:BB:CC:DD:EE:FF";
pub const PAD_NAME: &str = "KS-ST-A1P";
pub const HEADPHONES_ADDRESS: &str = "11:22:33:44:55:66";
pub const HEADPHONES_NAME: &str = "Some Headphones";

const FOUND_AFTER: Duration = Duration::from_secs(1);
const CONNECT_DELAY: Duration = Duration::from_millis(300);
/// The pad drops a command that follows the previous one this closely.
const MIN_GAP: Duration = Duration::from_millis(50);
const TICK: Duration = Duration::from_millis(100);
const COUNTDOWN: Duration = Duration::from_secs(3);
const START_DKMH: u8 = 20;
const MIN_DKMH: u8 = 5;
const MAX_DKMH: u8 = 60;

const IDLE: u8 = 0;
const RUNNING: u8 = 1;
const STANDBY: u8 = 5;
const COUNTDOWN_FROM: u8 = 9;
const MODE_STANDBY: u8 = 2;

const OTA_SERVICE: u128 = 0x0001_0203_0405_0607_0809_0A0B_0C0D_1912;
const OTA_CHARACTERISTIC: u128 = 0x0001_0203_0405_0607_0809_0A0B_0C0D_2B12;

struct Device {
    address: &'static str,
    name: &'static str,
    rssi: i16,
    advertised: &'static [u128],
    gatt: &'static [(u128, &'static [u128])],
}

const DEVICES: [Device; 2] = [
    Device {
        address: PAD_ADDRESS,
        name: PAD_NAME,
        rssi: -58,
        advertised: &[WALKINGPAD_SERVICE],
        gatt: &[
            (
                WALKINGPAD_SERVICE,
                &[NOTIFY_CHARACTERISTIC, WRITE_CHARACTERISTIC],
            ),
            (OTA_SERVICE, &[OTA_CHARACTERISTIC]),
        ],
    },
    Device {
        address: HEADPHONES_ADDRESS,
        name: HEADPHONES_NAME,
        rssi: -71,
        advertised: &[bluetooth_uuid(0x110B)],
        gatt: &[(bluetooth_uuid(0x180F), &[bluetooth_uuid(0x2A19)])],
    },
];

/// Hooks into the simulated world, for tests and the demo.
#[derive(Clone, Debug)]
pub struct Control {
    hooks: mpsc::UnboundedSender<Hook>,
}

#[derive(Debug)]
enum Hook {
    DropLink,
    Advertise(bool),
    Remote(u8),
    Writes(oneshot::Sender<Vec<Write>>),
}

/// A write as it reached the pad.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Write {
    pub at: Instant,
    pub frame: Vec<u8>,
    /// The belt state the write found.
    pub belt: Belt,
}

impl Control {
    /// Loses the link like a radio dropout; the belt carries on.
    pub fn drop_link(&self) {
        self.send(Hook::DropLink);
    }

    /// Makes the pad appear in, or vanish from, scans and searches.
    pub fn set_advertising(&self, advertising: bool) {
        self.send(Hook::Advertise(advertising));
    }

    /// The walker sets a speed on the remote; ignored unless the belt runs.
    pub fn press_remote(&self, kmh: f64) {
        self.send(Hook::Remote((kmh * 10.0).round().clamp(0.0, 255.0) as u8));
    }

    /// Every write the pad has received, oldest first.
    pub async fn writes(&self) -> Vec<Write> {
        let (reply, writes) = oneshot::channel();
        self.send(Hook::Writes(reply));
        writes.await.unwrap_or_default()
    }

    fn send(&self, hook: Hook) {
        if self.hooks.send(hook).is_err() {
            log::warn!("the simulator has stopped");
        }
    }
}

pub(super) fn spawn(
    calls: mpsc::UnboundedReceiver<Call>,
    events: mpsc::UnboundedSender<RadioEvent>,
) -> Control {
    let (hooks, hook_rx) = mpsc::unbounded_channel();
    tokio::spawn(Sim::new(events).run(calls, hook_rx));
    Control { hooks }
}

struct Sim {
    events: mpsc::UnboundedSender<RadioEvent>,
    pad: Firmware,
    advertising: bool,
    scan: Option<Scan>,
    attempt: Option<Attempt>,
    link: Option<LinkId>,
    links: u64,
    last_write: Option<Instant>,
    writes: Vec<Write>,
}

struct Scan {
    found_at: Option<Instant>,
    until: Instant,
}

struct Attempt {
    address: String,
    step: Step,
}

enum Step {
    Searching {
        look_at: Instant,
        until: Instant,
    },
    Connecting {
        device: &'static Device,
        at: Instant,
    },
}

impl Sim {
    fn new(events: mpsc::UnboundedSender<RadioEvent>) -> Sim {
        Sim {
            events,
            pad: Firmware::new(Instant::now()),
            advertising: true,
            scan: None,
            attempt: None,
            link: None,
            links: 0,
            last_write: None,
            writes: Vec::new(),
        }
    }

    async fn run(
        mut self,
        mut calls: mpsc::UnboundedReceiver<Call>,
        mut hooks: mpsc::UnboundedReceiver<Hook>,
    ) {
        self.emit(RadioEvent::Adapter(AdapterState::On));
        let mut hooked = true;
        loop {
            let wake = self.next_wake();
            tokio::select! {
                // Calls first, so a write sent just before `writes()` is in the log.
                biased;
                call = calls.recv() => match call {
                    Some(call) => self.call(Instant::now(), call),
                    None => return,
                },
                hook = hooks.recv(), if hooked => match hook {
                    Some(hook) => self.hook(Instant::now(), hook),
                    None => hooked = false,
                },
                () = sleep_until_some(wake) => self.wake(Instant::now()),
            }
        }
    }

    fn call(&mut self, now: Instant, call: Call) {
        match call {
            Call::Scan(duration) => {
                self.scan = Some(Scan {
                    found_at: Some(now + FOUND_AFTER),
                    until: now + duration,
                });
            }
            Call::StopScan => {
                if self.scan.take().is_some() {
                    self.emit(RadioEvent::ScanFinished);
                }
            }
            Call::Connect { address, search } => {
                self.end_link("replaced by a new connection");
                self.attempt = Some(Attempt {
                    address,
                    step: Step::Searching {
                        look_at: now + FOUND_AFTER,
                        until: now + search,
                    },
                });
            }
            Call::Disconnect => self.end_link("disconnect requested"),
            Call::Write { link, frame } if self.link == Some(link) => self.write(now, frame),
            Call::Write { .. } => log::debug!("dropping a write for a link that is gone"),
        }
    }

    fn hook(&mut self, now: Instant, hook: Hook) {
        match hook {
            Hook::DropLink => {
                if self.link.take().is_some() {
                    self.emit(RadioEvent::Disconnected {
                        reason: "simulated link loss".into(),
                    });
                }
            }
            Hook::Advertise(advertising) => self.advertising = advertising,
            Hook::Remote(dkmh) => self.pad.remote(now, dkmh),
            Hook::Writes(reply) => {
                if reply.send(self.writes.clone()).is_err() {
                    log::debug!("nobody waits for the write log");
                }
            }
        }
    }

    fn next_wake(&self) -> Option<Instant> {
        let scan = self
            .scan
            .as_ref()
            .map(|scan| scan.found_at.unwrap_or(scan.until).min(scan.until));
        let attempt = self.attempt.as_ref().map(|attempt| match attempt.step {
            Step::Searching { look_at, until } => look_at.min(until),
            Step::Connecting { at, .. } => at,
        });
        scan.into_iter().chain(attempt).min()
    }

    fn wake(&mut self, now: Instant) {
        if let Some(scan) = &mut self.scan
            && scan.found_at.is_some_and(|at| now >= at)
        {
            scan.found_at = None;
            for device in DEVICES.iter().filter(|device| self.present(device)) {
                self.emit(RadioEvent::Discovered {
                    address: device.address.into(),
                    name: Some(device.name.into()),
                    rssi: Some(device.rssi),
                    candidate: is_candidate(Some(device.name), device.advertised.iter().copied()),
                });
            }
        }
        if self.scan.as_ref().is_some_and(|scan| now >= scan.until) {
            self.scan = None;
            self.emit(RadioEvent::ScanFinished);
        }
        if let Some(attempt) = self.attempt.take() {
            self.attempt = self.progress(now, attempt);
        }
    }

    fn progress(&mut self, now: Instant, attempt: Attempt) -> Option<Attempt> {
        match attempt.step {
            Step::Searching { look_at, until } if now >= look_at || now >= until => {
                if now >= look_at
                    && let Some(device) = self.find(&attempt.address)
                {
                    self.emit(RadioEvent::Connecting {
                        address: device.address.into(),
                    });
                    let step = Step::Connecting {
                        device,
                        at: now + CONNECT_DELAY,
                    };
                    return Some(Attempt { step, ..attempt });
                }
                if now >= until {
                    self.emit(RadioEvent::ConnectFailed {
                        address: attempt.address,
                        reason: ConnectFailure::NotFound,
                    });
                    return None;
                }
                let step = Step::Searching {
                    look_at: now + FOUND_AFTER,
                    until,
                };
                Some(Attempt { step, ..attempt })
            }
            Step::Connecting { device, at } if now >= at => {
                let gatt = device.gatt.iter().map(|(service, characteristics)| {
                    let handles = characteristics.iter().map(|uuid| (*uuid, ())).collect();
                    (*service, handles)
                });
                let event = match classify(gatt) {
                    Ok(_) => {
                        self.links += 1;
                        let link = LinkId(self.links);
                        self.link = Some(link);
                        RadioEvent::Connected {
                            link,
                            address: device.address.into(),
                            name: Some(device.name.into()),
                        }
                    }
                    Err(reason) => RadioEvent::ConnectFailed {
                        address: device.address.into(),
                        reason,
                    },
                };
                self.emit(event);
                None
            }
            _ => Some(attempt),
        }
    }

    fn write(&mut self, now: Instant, frame: Vec<u8>) {
        let too_soon = self
            .last_write
            .is_some_and(|last| now.saturating_duration_since(last) < MIN_GAP);
        self.last_write = Some(now);
        self.pad.advance(now);
        self.writes.push(Write {
            at: now,
            frame: frame.clone(),
            belt: Belt::from(self.pad.belt),
        });
        if too_soon {
            log::debug!("the pad drops a write that came too soon");
        } else if let Some(reply) = self.pad.receive(now, &frame) {
            self.emit(RadioEvent::Frame(reply));
        }
    }

    fn end_link(&mut self, reason: &str) {
        let linked = self.link.take().is_some();
        let attempting = self.attempt.take().is_some();
        if linked || attempting {
            self.emit(RadioEvent::Disconnected {
                reason: reason.into(),
            });
        }
    }

    fn find(&self, address: &str) -> Option<&'static Device> {
        DEVICES
            .iter()
            .find(|device| device.address.eq_ignore_ascii_case(address))
            .filter(|device| self.present(device))
    }

    /// A connected pad stops advertising, so scans and searches miss it.
    fn present(&self, device: &Device) -> bool {
        device.address != PAD_ADDRESS || (self.advertising && self.link.is_none())
    }

    fn emit(&self, event: RadioEvent) {
        if self.events.send(event).is_err() {
            log::debug!("nobody listens to the simulator");
        }
    }
}

async fn sleep_until_some(at: Option<Instant>) {
    match at {
        Some(at) => sleep_until(at).await,
        None => future::pending().await,
    }
}

/// The pad's firmware, advanced in 100 ms steps whenever something looks at it.
#[derive(Debug)]
struct Firmware {
    belt: u8,
    mode: u8,
    speed: u8,
    target: u8,
    countdown: Option<Instant>,
    clock: Instant,
    time_s: f64,
    dist_m: f64,
    steps: f64,
}

impl Firmware {
    fn new(now: Instant) -> Firmware {
        Firmware {
            belt: STANDBY,
            mode: MODE_STANDBY,
            speed: 0,
            target: 0,
            countdown: None,
            clock: now,
            time_s: 0.0,
            dist_m: 0.0,
            steps: 0.0,
        }
    }

    /// Answers a frame written to FE02, or ignores it as the pad would.
    fn receive(&mut self, now: Instant, frame: &[u8]) -> Option<Vec<u8>> {
        self.advance(now);
        match *frame {
            [0xF7, 0xA2, key, value, sum, 0xFD] if sum == checksum(&frame[1..4]) => {
                self.control(now, key, value);
                Some(self.status())
            }
            [0xF7, 0xA6, 0x00, _, _, _, _, sum, 0xFD] if sum == checksum(&frame[1..7]) => {
                Some(self.prefs())
            }
            _ => None,
        }
    }

    fn control(&mut self, now: Instant, key: u8, value: u8) {
        match (key, value) {
            (0x01, 0) if self.belt == RUNNING => self.target = 0,
            (0x01, dkmh) if self.belt == RUNNING => self.target = dkmh.clamp(MIN_DKMH, MAX_DKMH),
            (0x02, MODE_STANDBY) => *self = Firmware::new(self.clock),
            (0x02, mode @ (0 | 1)) => {
                self.mode = mode;
                if self.belt == STANDBY {
                    self.belt = IDLE;
                }
            }
            (0x04, 1) => self.start_or_stop(now),
            _ => {}
        }
    }

    /// `04 01` behaves as a toggle, like the firmware that makes it dangerous.
    fn start_or_stop(&mut self, now: Instant) {
        if self.countdown.take().is_some() {
            self.belt = IDLE;
        } else if self.belt == RUNNING {
            self.target = 0;
        } else if self.belt == IDLE && self.mode != MODE_STANDBY {
            self.countdown = Some(now);
            self.belt = COUNTDOWN_FROM;
        }
    }

    fn remote(&mut self, now: Instant, dkmh: u8) {
        self.advance(now);
        if self.belt == RUNNING {
            self.target = dkmh.clamp(MIN_DKMH, MAX_DKMH);
        }
    }

    fn advance(&mut self, now: Instant) {
        while self.clock + TICK <= now {
            self.clock += TICK;
            self.step();
        }
    }

    fn step(&mut self) {
        if let Some(since) = self.countdown {
            let elapsed = self.clock.saturating_duration_since(since);
            if elapsed >= COUNTDOWN {
                self.countdown = None;
                self.belt = RUNNING;
                self.speed = START_DKMH;
                self.target = START_DKMH;
            } else {
                self.belt = COUNTDOWN_FROM - elapsed.as_secs() as u8;
            }
            return;
        }
        if self.belt != RUNNING {
            return;
        }
        match self.speed.cmp(&self.target) {
            Ordering::Less => self.speed += 1,
            Ordering::Greater => self.speed -= 1,
            Ordering::Equal => {}
        }
        if self.speed == 0 {
            self.belt = IDLE;
            return;
        }
        let kmh = f64::from(self.speed) / 10.0;
        let seconds = TICK.as_secs_f64();
        self.time_s += seconds;
        self.dist_m += kmh / 3.6 * seconds;
        self.steps += (60.0 + 12.0 * kmh) / 60.0 * seconds;
    }

    fn status(&self) -> Vec<u8> {
        let mut frame = vec![0xF8, 0xA2, self.belt, self.speed, self.mode];
        for counter in [self.time_s, self.dist_m / 10.0, self.steps] {
            let value = (counter as u32).min(0xFF_FFFF);
            frame.extend_from_slice(&value.to_be_bytes()[1..]);
        }
        frame.extend_from_slice(&[0; 4]);
        seal(frame)
    }

    fn prefs(&self) -> Vec<u8> {
        seal(vec![
            0xF8, 0xA6, 0, 0, 0, 0, 0, MAX_DKMH, START_DKMH, 0, 2, 0x1F, 0, 0, 0, 0, 0, 0,
        ])
    }
}

fn seal(mut frame: Vec<u8>) -> Vec<u8> {
    frame.push(checksum(&frame[1..]));
    frame.push(0xFD);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Command, Mode, Notification, Prefs, Reassembler, Status};
    use crate::radio::{ConnectFailure, Radio};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn decode(reply: &[u8]) -> Notification {
        let frames = Reassembler::default().push(reply);
        assert_eq!(frames.len(), 1, "{reply:02x?}");
        frames[0].decode()
    }

    fn status_of(reply: Option<Vec<u8>>) -> Status {
        match decode(&reply.expect("a reply")) {
            Notification::Status(status) => status,
            other => panic!("expected a status, got {other:?}"),
        }
    }

    struct Pad {
        firmware: Firmware,
        now: Instant,
    }

    impl Pad {
        fn new() -> Pad {
            let now = Instant::now();
            Pad {
                firmware: Firmware::new(now),
                now,
            }
        }

        fn send(&mut self, command: Command) -> Status {
            status_of(self.firmware.receive(self.now, &command.encode()))
        }

        fn wait(&mut self, span: Duration) -> Status {
            self.now += span;
            self.send(Command::AskStats)
        }

        fn running(&mut self) -> Status {
            self.send(Command::SwitchToManual);
            self.send(Command::StartBelt);
            self.wait(COUNTDOWN)
        }
    }

    #[test]
    fn starts_in_standby_with_zeroed_counters() {
        let status = Pad::new().send(Command::AskStats);
        assert_eq!(
            status,
            Status {
                belt: Belt::Standby,
                speed_dkmh: 0,
                mode: Mode::Standby,
                time_s: 0,
                dist_10m: 0,
                steps: 0
            }
        );
    }

    #[test]
    fn answers_every_control_command_with_one_status() {
        let mut pad = Pad::new();
        for command in [
            Command::AskStats,
            Command::SwitchToManual,
            Command::ChangeSpeed(30),
            Command::StartBelt,
            Command::ChangeSpeed(0),
        ] {
            let reply = pad.firmware.receive(pad.now, &command.encode());
            assert!(matches!(decode(&reply.unwrap()), Notification::Status(_)));
        }
    }

    #[test]
    fn replies_to_a_prefs_query_with_a_6_kmh_maximum() {
        let mut pad = Pad::new();
        let reply = pad.firmware.receive(pad.now, &Command::QueryPrefs.encode());
        assert_eq!(
            decode(&reply.unwrap()),
            Notification::Prefs(Prefs {
                max_dkmh: 60,
                imperial: false
            })
        );
    }

    #[test]
    fn ignores_frames_with_a_bad_checksum_or_an_unknown_type() {
        let mut pad = Pad::new();
        let mut corrupt = Command::StartBelt.encode();
        corrupt[4] ^= 1;
        assert_eq!(pad.firmware.receive(pad.now, &corrupt), None);
        assert_eq!(
            pad.firmware
                .receive(pad.now, &[0xF7, 0xA7, 0xAA, 0xFF, 0x50, 0xFD]),
            None
        );
        assert_eq!(pad.firmware.receive(pad.now, &[]), None);
        assert_eq!(pad.send(Command::AskStats).belt, Belt::Standby);
    }

    #[test]
    fn counts_down_through_9_8_7_then_runs_at_2_kmh() {
        let mut pad = Pad::new();
        pad.send(Command::SwitchToManual);
        assert_eq!(pad.send(Command::StartBelt).belt, Belt::Starting);
        let raw = |pad: &Pad| pad.firmware.belt;
        assert_eq!(raw(&pad), 9);
        pad.wait(secs(1));
        assert_eq!(raw(&pad), 8);
        pad.wait(secs(1));
        assert_eq!(raw(&pad), 7);
        let running = pad.wait(secs(1));
        assert_eq!((running.belt, running.speed_dkmh), (Belt::Running, 20));
    }

    #[test]
    fn ignores_start_in_standby_mode() {
        let mut pad = Pad::new();
        assert_eq!(pad.send(Command::StartBelt).belt, Belt::Standby);
        assert_eq!(pad.wait(secs(4)).belt, Belt::Standby);
    }

    #[test]
    fn ignores_speed_changes_unless_the_belt_runs() {
        let mut pad = Pad::new();
        pad.send(Command::SwitchToManual);
        pad.send(Command::ChangeSpeed(40));
        pad.send(Command::StartBelt);
        pad.send(Command::ChangeSpeed(40));
        pad.wait(ms(1500));
        pad.send(Command::ChangeSpeed(0));
        let running = pad.wait(ms(1500));
        assert_eq!((running.belt, running.speed_dkmh), (Belt::Running, 20));
        assert_eq!(pad.wait(secs(2)).speed_dkmh, 20);
    }

    #[test]
    fn ramps_by_a_tenth_of_a_kmh_every_100_ms() {
        let mut pad = Pad::new();
        pad.running();
        pad.send(Command::ChangeSpeed(40));
        assert_eq!(pad.wait(ms(500)).speed_dkmh, 25);
        assert_eq!(pad.wait(ms(1500)).speed_dkmh, 40);
        assert_eq!(pad.wait(secs(5)).speed_dkmh, 40);
    }

    #[test]
    fn clamps_speeds_to_its_range() {
        let mut pad = Pad::new();
        pad.running();
        pad.send(Command::ChangeSpeed(90));
        assert_eq!(pad.wait(secs(10)).speed_dkmh, 60);
        pad.send(Command::ChangeSpeed(2));
        assert_eq!(pad.wait(secs(10)).speed_dkmh, 5);
    }

    #[test]
    fn stopping_ramps_down_to_idle_and_keeps_the_counters() {
        let mut pad = Pad::new();
        pad.running();
        pad.wait(secs(30));
        pad.send(Command::ChangeSpeed(0));
        assert_eq!(pad.wait(ms(1000)).belt, Belt::Running);
        let stopped = pad.wait(ms(1100));
        assert_eq!((stopped.belt, stopped.speed_dkmh), (Belt::Idle, 0));
        assert!(stopped.time_s >= 30 && stopped.steps > 0);
        pad.send(Command::StartBelt);
        let resumed = pad.wait(COUNTDOWN);
        assert_eq!(resumed.belt, Belt::Running);
        assert_eq!(resumed.time_s, stopped.time_s);
        assert!(pad.wait(secs(5)).time_s > stopped.time_s);
    }

    #[test]
    fn start_belt_stops_a_moving_belt_like_toggling_firmware() {
        let mut pad = Pad::new();
        pad.running();
        pad.send(Command::StartBelt);
        assert_eq!(pad.wait(secs(3)).belt, Belt::Idle);
    }

    #[test]
    fn start_belt_during_the_countdown_cancels_it() {
        let mut pad = Pad::new();
        pad.send(Command::SwitchToManual);
        pad.send(Command::StartBelt);
        pad.wait(secs(1));
        assert_eq!(pad.send(Command::StartBelt).belt, Belt::Idle);
        assert_eq!(pad.wait(secs(4)).belt, Belt::Idle);
    }

    #[test]
    fn counts_time_distance_and_steps_from_the_speed() {
        let mut pad = Pad::new();
        pad.running();
        pad.send(Command::ChangeSpeed(40));
        pad.wait(secs(2));
        let before = pad.send(Command::AskStats);
        let after = pad.wait(secs(60));
        assert_eq!(after.time_s - before.time_s, 60);
        assert_eq!(after.steps - before.steps, 108);
        assert!((6..=7).contains(&(after.dist_10m - before.dist_10m)));
    }

    #[test]
    fn standby_resets_the_counters() {
        let mut pad = Pad::new();
        pad.running();
        pad.wait(secs(20));
        let reply = pad
            .firmware
            .receive(pad.now, &[0xF7, 0xA2, 0x02, 0x02, 0xA6, 0xFD]);
        let standby = status_of(reply);
        assert_eq!(
            (standby.belt, standby.time_s, standby.steps),
            (Belt::Standby, 0, 0)
        );
    }

    #[test]
    fn the_remote_changes_the_speed_only_while_the_belt_runs() {
        let mut pad = Pad::new();
        pad.firmware.remote(pad.now, 40);
        assert_eq!(pad.wait(secs(3)).speed_dkmh, 0);
        pad.running();
        pad.firmware.remote(pad.now, 35);
        assert_eq!(pad.wait(secs(3)).speed_dkmh, 35);
    }

    async fn next(radio: &mut Radio) -> RadioEvent {
        tokio::time::timeout(secs(60), radio.events().recv())
            .await
            .expect("an event within a minute")
            .expect("the simulator is running")
    }

    async fn collect(radio: &mut Radio, span: Duration) -> Vec<RadioEvent> {
        let until = Instant::now() + span;
        let mut events = Vec::new();
        while let Ok(Some(event)) = tokio::time::timeout_at(until, radio.events().recv()).await {
            events.push(event);
        }
        events
    }

    async fn simulated() -> (Radio, Control) {
        let (mut radio, control) = Radio::simulated();
        assert_eq!(
            next(&mut radio).await,
            RadioEvent::Adapter(AdapterState::On)
        );
        (radio, control)
    }

    async fn connected() -> (Radio, Control, LinkId) {
        let (mut radio, control) = simulated().await;
        radio.connect(PAD_ADDRESS, secs(8));
        let events = collect(&mut radio, secs(3)).await;
        let link = events
            .iter()
            .find_map(|event| match event {
                RadioEvent::Connected { link, .. } => Some(*link),
                _ => None,
            })
            .expect("a link");
        (radio, control, link)
    }

    fn pad_seen() -> RadioEvent {
        RadioEvent::Discovered {
            address: PAD_ADDRESS.into(),
            name: Some(PAD_NAME.into()),
            rssi: Some(-58),
            candidate: true,
        }
    }

    fn headphones_seen() -> RadioEvent {
        RadioEvent::Discovered {
            address: HEADPHONES_ADDRESS.into(),
            name: Some(HEADPHONES_NAME.into()),
            rssi: Some(-71),
            candidate: false,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_scan_finds_the_pad_and_the_headphones_after_a_second_then_finishes() {
        let (mut radio, _control) = simulated().await;
        let started = Instant::now();
        radio.scan(secs(10));
        assert_eq!(next(&mut radio).await, pad_seen());
        assert_eq!(Instant::now() - started, FOUND_AFTER);
        assert_eq!(next(&mut radio).await, headphones_seen());
        assert_eq!(next(&mut radio).await, RadioEvent::ScanFinished);
        assert_eq!(Instant::now() - started, secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn stop_scan_ends_a_scan_early() {
        let (mut radio, _control) = simulated().await;
        radio.scan(secs(10));
        collect(&mut radio, ms(1500)).await;
        radio.stop_scan();
        assert_eq!(
            collect(&mut radio, secs(20)).await,
            [RadioEvent::ScanFinished]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn connects_to_the_pad_in_about_a_second() {
        let (mut radio, _control) = simulated().await;
        let started = Instant::now();
        radio.connect(&PAD_ADDRESS.to_lowercase(), secs(8));
        assert_eq!(
            next(&mut radio).await,
            RadioEvent::Connecting {
                address: PAD_ADDRESS.into()
            }
        );
        let RadioEvent::Connected { address, name, .. } = next(&mut radio).await else {
            panic!("expected a link");
        };
        assert_eq!(
            (address.as_str(), name.as_deref()),
            (PAD_ADDRESS, Some(PAD_NAME))
        );
        assert_eq!(Instant::now() - started, FOUND_AFTER + CONNECT_DELAY);
    }

    #[tokio::test(start_paused = true)]
    async fn connecting_to_the_headphones_fails_as_not_a_walkingpad() {
        let (mut radio, _control) = simulated().await;
        radio.connect(HEADPHONES_ADDRESS, secs(8));
        let events = collect(&mut radio, secs(5)).await;
        assert_eq!(
            events.last(),
            Some(&RadioEvent::ConnectFailed {
                address: HEADPHONES_ADDRESS.into(),
                reason: ConnectFailure::NotAWalkingPad
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_pad_that_stopped_advertising_is_neither_listed_nor_found() {
        let (mut radio, control) = simulated().await;
        control.set_advertising(false);
        radio.scan(secs(3));
        assert_eq!(
            collect(&mut radio, secs(4)).await,
            [headphones_seen(), RadioEvent::ScanFinished]
        );
        let started = Instant::now();
        radio.connect(PAD_ADDRESS, secs(8));
        assert_eq!(
            next(&mut radio).await,
            RadioEvent::ConnectFailed {
                address: PAD_ADDRESS.into(),
                reason: ConnectFailure::NotFound
            }
        );
        assert_eq!(Instant::now() - started, secs(8));
    }

    #[tokio::test(start_paused = true)]
    async fn a_pad_that_starts_advertising_mid_search_is_found() {
        let (mut radio, control) = simulated().await;
        control.set_advertising(false);
        radio.connect(PAD_ADDRESS, secs(8));
        collect(&mut radio, secs(3)).await;
        control.set_advertising(true);
        let events = collect(&mut radio, secs(3)).await;
        assert!(
            matches!(events.last(), Some(RadioEvent::Connected { .. })),
            "{events:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_connected_pad_is_not_listed_in_scans() {
        let (mut radio, _control, _link) = connected().await;
        radio.scan(secs(2));
        assert_eq!(
            collect(&mut radio, secs(3)).await,
            [headphones_seen(), RadioEvent::ScanFinished]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn answers_writes_on_the_link_with_real_frames() {
        let (mut radio, _control, link) = connected().await;
        radio.write(link, Command::AskStats.encode());
        let RadioEvent::Frame(reply) = next(&mut radio).await else {
            panic!("expected a frame");
        };
        assert!(matches!(decode(&reply), Notification::Status(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn drops_writes_closer_than_50_ms() {
        let (mut radio, control, link) = connected().await;
        radio.write(link, Command::AskStats.encode());
        radio.write(link, Command::AskStats.encode());
        tokio::time::sleep(ms(40)).await;
        radio.write(link, Command::AskStats.encode());
        tokio::time::sleep(ms(60)).await;
        radio.write(link, Command::AskStats.encode());
        let frames = collect(&mut radio, secs(1)).await;
        assert_eq!(frames.len(), 2, "{frames:?}");
        assert_eq!(control.writes().await.len(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn records_every_write_with_the_belt_state_it_found() {
        let (radio, control, link) = connected().await;
        radio.write(link, Command::SwitchToManual.encode());
        tokio::time::sleep(ms(700)).await;
        radio.write(link, Command::StartBelt.encode());
        tokio::time::sleep(ms(700)).await;
        radio.write(link, Command::AskStats.encode());
        let writes = control.writes().await;
        let seen: Vec<(Vec<u8>, Belt)> = writes.into_iter().map(|w| (w.frame, w.belt)).collect();
        assert_eq!(
            seen,
            [
                (Command::SwitchToManual.encode(), Belt::Standby),
                (Command::StartBelt.encode(), Belt::Idle),
                (Command::AskStats.encode(), Belt::Starting),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_link_reports_it_and_ignores_writes_for_it() {
        let (mut radio, control, link) = connected().await;
        control.drop_link();
        assert_eq!(
            next(&mut radio).await,
            RadioEvent::Disconnected {
                reason: "simulated link loss".into()
            }
        );
        radio.write(link, Command::AskStats.encode());
        assert_eq!(collect(&mut radio, secs(1)).await, []);
        assert_eq!(control.writes().await, []);
    }

    #[tokio::test(start_paused = true)]
    async fn writes_for_an_earlier_link_never_reach_the_next_one() {
        let (mut radio, control, old) = connected().await;
        radio.connect(PAD_ADDRESS, secs(8));
        let events = collect(&mut radio, secs(3)).await;
        assert_eq!(
            events.first(),
            Some(&RadioEvent::Disconnected {
                reason: "replaced by a new connection".into()
            })
        );
        let Some(RadioEvent::Connected { link: new, .. }) = events.last() else {
            panic!("expected a new link: {events:?}");
        };
        assert_ne!(*new, old);
        radio.write(old, Command::StartBelt.encode());
        assert_eq!(collect(&mut radio, secs(1)).await, []);
        assert_eq!(control.writes().await, []);
    }

    #[tokio::test(start_paused = true)]
    async fn disconnect_cancels_a_search() {
        let (mut radio, _control) = simulated().await;
        radio.connect(PAD_ADDRESS, secs(8));
        radio.disconnect();
        assert_eq!(
            collect(&mut radio, secs(10)).await,
            [RadioEvent::Disconnected {
                reason: "disconnect requested".into()
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_remote_hook_changes_the_belt_speed() {
        let (mut radio, control, link) = connected().await;
        for command in [Command::SwitchToManual, Command::StartBelt] {
            radio.write(link, command.encode());
            tokio::time::sleep(ms(700)).await;
        }
        tokio::time::sleep(COUNTDOWN).await;
        control.press_remote(3.0);
        tokio::time::sleep(secs(2)).await;
        collect(&mut radio, Duration::ZERO).await;
        radio.write(link, Command::AskStats.encode());
        let RadioEvent::Frame(reply) = next(&mut radio).await else {
            panic!("expected a frame");
        };
        let Notification::Status(status) = decode(&reply) else {
            panic!("expected a status");
        };
        assert_eq!((status.belt, status.speed_dkmh), (Belt::Running, 30));
    }
}
