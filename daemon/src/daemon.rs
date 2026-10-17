//! The actor loop between the shell, the pad and the store.

use std::fs::{File, OpenOptions, TryLockError};
use std::future::Future;
use std::io;
use std::mem;
use std::path::{Path, PathBuf};
use std::pin::pin;

use anyhow::Context;
use jiff::Timestamp;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::time::{self, Duration, Instant, MissedTickBehavior};

use crate::controller::{Action, Controller, Failure, Request};
use crate::ipc::{self, Address, BadCommand, Command, Commands, Message};
use crate::protocol::{Belt, Notification, Reassembler, Status};
use crate::radio::{AdapterState, ConnectFailure, LinkId, Radio, RadioEvent};
use crate::stats;
use crate::store::{self, DataDirError, Device};
use crate::tracker::{Sample, Tracker};

const TICK: Duration = Duration::from_secs(1);
/// At most one snapshot per window; the latest one goes out when it closes.
const COALESCE: Duration = Duration::from_millis(250);
/// How long to wait for the adapter state before greeting the shell.
const FIRST_EVENT: Duration = Duration::from_secs(2);
const SCAN: Duration = Duration::from_secs(10);
/// Discovery makes Bluetooth audio stutter, so a search stops after this.
const SEARCH: Duration = Duration::from_secs(8);
const RETRY_WINDOW: Duration = Duration::from_secs(10 * 60);
const RETRIES: [Duration; 3] = [
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
];
const RETRY_EVERY: Duration = Duration::from_secs(120);
const WAKE_EVERY: Duration = Duration::from_secs(20);
/// The target until the walker picks one: the usual start-speed
/// preference, so that a first start sends no speed command.
const DEFAULT_TARGET_KMH: f64 = 2.0;
/// A previous instance may still be shutting down during a shell reload.
const LOCK_WAIT: Duration = Duration::from_secs(5);
const LOCK_RETRY: Duration = Duration::from_millis(250);

/// Why the daemon cannot run.
#[derive(Debug, thiserror::Error)]
pub enum Fatal {
    #[error("another ks-fit daemon is running")]
    AlreadyRunning,
    #[error("no data directory: pass --data-dir or set XDG_DATA_HOME or HOME")]
    NoDataDir,
    #[error(transparent)]
    DataDir(#[from] DataDirError),
    #[error("cannot lock {}: {source}", path.display())]
    Lock { path: PathBuf, source: io::Error },
    #[error("cannot watch for signals: {0}")]
    Signals(io::Error),
    #[error("{0:#}")]
    Database(#[from] anyhow::Error),
    #[error("the Bluetooth task stopped")]
    RadioStopped,
}

impl Fatal {
    /// The `fatal` code on the wire.
    pub fn code(&self) -> &'static str {
        match self {
            Fatal::AlreadyRunning => "already-running",
            Fatal::NoDataDir | Fatal::DataDir(_) => "data-dir",
            Fatal::Lock { .. } => "lock",
            Fatal::Signals(_) => "signals",
            Fatal::Database(_) => "database",
            Fatal::RadioStopped => "radio",
        }
    }

    pub fn exit_code(&self) -> u8 {
        match self {
            Fatal::AlreadyRunning => 3,
            _ => 1,
        }
    }
}

/// Takes the single-instance lock in `dir`, created 0700 like the data
/// directory, waiting out a previous instance for up to [`LOCK_WAIT`]. The
/// simulator has a lock of its own, so a demo never blocks the real pad.
pub async fn lock(dir: &Path, simulated: bool) -> Result<File, Fatal> {
    let dir = store::prepare_data_dir(dir)?;
    let path = dir.join(if simulated {
        "daemon-sim.lock"
    } else {
        "daemon.lock"
    });
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path);
    let file = match file {
        Ok(file) => file,
        Err(source) => return Err(Fatal::Lock { path, source }),
    };
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                time::sleep(LOCK_RETRY).await;
            }
            Err(TryLockError::WouldBlock) => return Err(Fatal::AlreadyRunning),
            Err(TryLockError::Error(source)) => return Err(Fatal::Lock { path, source }),
        }
    }
}

/// Where the loop reads the wall-clock time from.
#[derive(Clone, Copy, Debug)]
pub enum Clock {
    System,
    /// Advances with tokio's clock from `origin`, so that a paused test
    /// clock moves the recorded activity too.
    Tokio {
        origin: Timestamp,
        since: Instant,
    },
}

impl Clock {
    pub fn tokio(origin: Timestamp) -> Clock {
        Clock::Tokio {
            origin,
            since: Instant::now(),
        }
    }

    fn now(self) -> Timestamp {
        match self {
            Clock::System => Timestamp::now(),
            Clock::Tokio { origin, since } => origin + since.elapsed(),
        }
    }
}

/// Why a command could not be carried out.
#[derive(Debug, thiserror::Error)]
enum Refusal {
    #[error(transparent)]
    Pad(#[from] Failure),
    #[error("Bluetooth is not on")]
    BluetoothOff,
    #[error("no pad is remembered; scan and connect to one first")]
    NoDevice,
    #[error("the database failed: {0}")]
    Store(#[from] store::Error),
    #[error("the database failed: {0:#}")]
    Tracker(#[from] anyhow::Error),
}

impl Refusal {
    fn code(&self) -> &'static str {
        match self {
            Refusal::Pad(failure) => failure.code(),
            Refusal::BluetoothOff => "bluetooth-off",
            Refusal::NoDevice => "no-device",
            Refusal::Store(_) | Refusal::Tracker(_) => "database",
        }
    }
}

enum Link {
    Down,
    /// Looking for the pad at this address.
    Searching(String),
    Connecting(String),
    Up(Box<Up>),
}

struct Up {
    id: LinkId,
    address: String,
    controller: Controller,
    frames: Reassembler,
}

/// Why the pad is not connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Detail {
    Failed(ConnectFailure),
    LinkLost,
    Paused,
}

impl Detail {
    fn code(self) -> &'static str {
        match self {
            Detail::Failed(failure) => failure.code(),
            Detail::LinkLost => "link-lost",
            Detail::Paused => "paused",
        }
    }
}

/// The bounded stretch after startup or a link loss in which failed
/// searches are retried on their own.
struct Retry {
    until: Instant,
    failures: usize,
    at: Option<Instant>,
}

impl Retry {
    fn window(now: Instant) -> Retry {
        Retry {
            until: now + RETRY_WINDOW,
            failures: 0,
            at: None,
        }
    }
}

/// Serves the shell: commands in on `input`, messages out on `output`.
pub struct Daemon<R, W> {
    radio: Radio,
    tracker: Tracker,
    input: Commands<BufReader<R>>,
    output: W,
    clock: Clock,
    simulated: bool,
    bluetooth: AdapterState,
    device: Option<Device>,
    auto_connect: bool,
    /// No search on our own until the next `connect`.
    hold: bool,
    link: Link,
    detail: Option<Detail>,
    retry: Option<Retry>,
    last_search: Option<Instant>,
    scan: ipc::Scan,
    outbox: Vec<Message>,
    sent: Option<ipc::State>,
    quiet_until: Instant,
    /// A changed snapshot waits for `quiet_until`.
    pending: bool,
    sync: bool,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Daemon<R, W> {
    pub fn new(radio: Radio, tracker: Tracker, input: R, output: W) -> Result<Self, Fatal> {
        let store = tracker.store();
        let device = store
            .remembered_device()
            .context("reading the remembered pad")?;
        let auto_connect = store
            .auto_connect()
            .context("reading the auto-connect setting")?
            .unwrap_or(true);
        let now = Instant::now();
        Ok(Daemon {
            radio,
            tracker,
            input: Commands::new(BufReader::new(input)),
            output,
            clock: Clock::System,
            simulated: false,
            bluetooth: AdapterState::Unavailable,
            retry: (auto_connect && device.is_some()).then(|| Retry::window(now)),
            device,
            auto_connect,
            hold: false,
            link: Link::Down,
            detail: None,
            last_search: None,
            scan: ipc::Scan::default(),
            outbox: Vec::new(),
            sent: None,
            quiet_until: now,
            pending: false,
            sync: false,
        })
    }

    /// Tells the shell that the pad is simulated.
    pub fn simulated(self, simulated: bool) -> Self {
        Daemon { simulated, ..self }
    }

    pub fn with_clock(self, clock: Clock) -> Self {
        Daemon { clock, ..self }
    }

    pub fn tracker(&self) -> &Tracker {
        &self.tracker
    }

    /// Serves until the input ends, the output breaks or `shutdown`
    /// completes, then closes the open session. The belt is left as it is.
    pub async fn run(&mut self, shutdown: impl Future<Output = ()>) -> Result<(), Fatal> {
        let mut shutdown = pin!(shutdown);
        self.outbox.push(Message::hello(self.simulated));
        self.sync = true;
        tokio::select! {
            first = time::timeout(FIRST_EVENT, self.radio.events().recv()) => {
                if let Ok(Some(event)) = first {
                    self.on_radio(event);
                }
            }
            () = &mut shutdown => return Ok(()),
        }
        let mut tick = time::interval(TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let outcome = loop {
            let flushed = tokio::select! {
                flushed = self.flush() => flushed,
                () = &mut shutdown => break Ok(()),
            };
            if let Err(error) = flushed {
                log::info!("the shell stopped reading: {error}");
                break Ok(());
            }
            let deadline = self.controller().and_then(Controller::next_deadline);
            let publish = self.pending.then_some(self.quiet_until);
            tokio::select! {
                read = self.input.next() => match read {
                    Ok(Some(Ok(command))) => self.on_command(command),
                    Ok(Some(Err(bad))) => self.reject(bad),
                    Ok(None) => break Ok(()),
                    Err(error) => {
                        log::warn!("cannot read commands: {error}");
                        break Ok(());
                    }
                },
                event = self.radio.events().recv() => match event {
                    Some(event) => self.on_radio(event),
                    None => break Err(Fatal::RadioStopped),
                },
                _ = tick.tick() => self.on_tick(),
                () = time::sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                    self.on_deadline();
                }
                () = time::sleep_until(publish.unwrap_or_else(Instant::now)), if publish.is_some() => {}
                () = &mut shutdown => break Ok(()),
            }
        };
        if let Err(error) = self.tracker.shutdown() {
            log::error!("cannot close the session: {error:#}");
        }
        outcome
    }

    /// Writes the queued messages, then the snapshot if it changed and the
    /// window allows.
    async fn flush(&mut self) -> io::Result<()> {
        for message in mem::take(&mut self.outbox) {
            ipc::write(&mut self.output, &message).await?;
        }
        let state = self.state();
        let now = Instant::now();
        let changed = self.sent.as_ref() != Some(&state);
        let due = mem::take(&mut self.sync) || (changed && now >= self.quiet_until);
        self.pending = changed && !due;
        if due {
            ipc::write(&mut self.output, &Message::State(Box::new(state.clone()))).await?;
            self.sent = Some(state);
            self.quiet_until = now + COALESCE;
        }
        Ok(())
    }

    fn state(&self) -> ipc::State {
        let (link, pad) = match &self.link {
            Link::Down => (ipc::Link::Disconnected, None),
            Link::Searching(_) => (ipc::Link::Searching, None),
            Link::Connecting(_) => (ipc::Link::Connecting, None),
            Link::Up(up) => match up.controller.view() {
                Some(view) => (ipc::Link::Connected, Some(view.into())),
                None => (ipc::Link::Connecting, None),
            },
        };
        ipc::State {
            bluetooth: self.bluetooth,
            link,
            link_detail: self.detail.map(Detail::code),
            device: self.device.as_ref().map(ipc::Device::from),
            auto_connect: self.auto_connect,
            scan: self.scan.clone(),
            pad,
            session: self.tracker.session().map(ipc::Session::from),
            last_session: self.tracker.last_session().map(ipc::Session::from),
            today: self.tracker.today().into(),
            weight_kg: self.tracker.weight_kg(),
        }
    }

    fn controller(&self) -> Option<&Controller> {
        match &self.link {
            Link::Up(up) => Some(&up.controller),
            _ => None,
        }
    }

    fn on_command(&mut self, command: Command) {
        let cmd = command.name();
        log::debug!("received {cmd}");
        if let Err(refusal) = self.command(command) {
            self.refuse(cmd, &refusal);
        }
    }

    fn command(&mut self, command: Command) -> Result<(), Refusal> {
        let now = Instant::now();
        match command {
            Command::Configure {
                weight_kg,
                auto_connect,
            } => {
                if let Some(kg) = weight_kg {
                    self.tracker.set_weight_kg(kg)?;
                }
                if let Some(on) = auto_connect {
                    self.tracker.store().set_auto_connect(on)?;
                    self.auto_connect = on;
                    if !on {
                        self.retry = None;
                    }
                }
            }
            Command::Sync => self.sync = true,
            Command::Wake => {
                if self.seek(now, true)
                    && let Some(retry) = &mut self.retry
                {
                    retry.failures = 0;
                }
            }
            Command::Scan => {
                if self.bluetooth != AdapterState::On {
                    return Err(Refusal::BluetoothOff);
                }
                self.scan = ipc::Scan {
                    active: true,
                    devices: Vec::new(),
                };
                self.radio.scan(SCAN);
            }
            Command::StopScan => {
                if self.scan.active {
                    self.radio.stop_scan();
                }
            }
            Command::Connect { address, name } => self.connect(now, address, name)?,
            Command::Disconnect => {
                self.hold = true;
                self.detail = Some(Detail::Paused);
                self.hang_up();
            }
            Command::Forget => {
                self.tracker.store().forget_device()?;
                self.device = None;
                self.hold = false;
                self.detail = None;
                self.hang_up();
            }
            Command::Start => self.drive(now, Request::Start)?,
            Command::Stop => self.drive(now, Request::Stop)?,
            Command::Toggle => self.drive(now, Request::Toggle)?,
            Command::Speed { kmh } => self.drive(now, Request::SetTarget(kmh))?,
            Command::Nudge { delta_kmh } => self.drive(now, Request::Nudge(delta_kmh))?,
            Command::Stats { id, period, offset } => {
                let tracker = &self.tracker;
                let stats = stats::query(
                    tracker.store(),
                    tracker.time_zone(),
                    self.clock.now(),
                    period,
                    offset,
                )?;
                let reply = ipc::Stats::new(id, period, &stats);
                self.outbox.push(Message::Stats(reply));
            }
        }
        Ok(())
    }

    /// Remembers `address` if given, and connects to the remembered pad
    /// unless it is already linked or being looked for.
    fn connect(
        &mut self,
        now: Instant,
        address: Option<Address>,
        name: Option<String>,
    ) -> Result<(), Refusal> {
        if let Some(address) = address {
            let store = self.tracker.store();
            store.remember_device(address.as_str(), name.as_deref())?;
            self.device = store.remembered_device()?;
        }
        let address = self
            .device
            .as_ref()
            .map(|device| device.address.clone())
            .ok_or(Refusal::NoDevice)?;
        self.hold = false;
        self.detail = None;
        if self.bluetooth != AdapterState::On {
            return Err(Refusal::BluetoothOff);
        }
        let current = match &self.link {
            Link::Down => None,
            Link::Searching(current) | Link::Connecting(current) => Some(current),
            Link::Up(up) => Some(&up.address),
        };
        if !current.is_some_and(|current| current.eq_ignore_ascii_case(&address)) {
            self.leave();
            self.search(now, address);
        }
        Ok(())
    }

    fn drive(&mut self, now: Instant, request: Request) -> Result<(), Refusal> {
        let Link::Up(up) = &mut self.link else {
            if matches!(request, Request::Start | Request::Toggle) {
                self.seek(now, true);
            }
            return Err(Failure::NotConnected.into());
        };
        let actions = up.controller.request(now, request);
        self.act(actions);
        Ok(())
    }

    fn reject(&mut self, bad: BadCommand) {
        log::info!("refusing a line from the shell: {bad}");
        self.outbox.push(Message::Error {
            code: Failure::BadCommand.code(),
            message: bad.message,
            cmd: bad.cmd,
        });
    }

    fn refuse(&mut self, cmd: &str, refusal: &Refusal) {
        log::info!("{cmd} failed: {refusal}");
        self.outbox.push(Message::Error {
            code: refusal.code(),
            message: refusal.to_string(),
            cmd: Some(cmd.to_owned()),
        });
    }

    fn on_radio(&mut self, event: RadioEvent) {
        let now = Instant::now();
        match event {
            RadioEvent::Adapter(state) => {
                let was = mem::replace(&mut self.bluetooth, state);
                if state != was {
                    log::info!("Bluetooth is {state:?}");
                }
                if state == AdapterState::On && was != AdapterState::On {
                    self.seek(now, false);
                }
            }
            RadioEvent::Discovered {
                address,
                name,
                rssi,
                candidate,
            } => self.discovered(ipc::ScanDevice {
                address,
                name,
                rssi,
                candidate,
            }),
            RadioEvent::ScanFinished => self.scan.active = false,
            RadioEvent::Connecting { address } => {
                if self.wanted(&address) && !matches!(self.link, Link::Up(_)) {
                    self.link = Link::Connecting(address);
                }
            }
            RadioEvent::Connected {
                link,
                address,
                name,
            } => self.connected(now, link, address, name),
            RadioEvent::ConnectFailed { address, reason } => {
                if self.attempting(&address) {
                    self.attempt_failed(now, reason);
                }
            }
            RadioEvent::Frame(chunk) => self.frame(&chunk),
            RadioEvent::Disconnected { reason } => {
                if matches!(self.link, Link::Up(_)) {
                    log::warn!("lost the pad: {reason}");
                    self.lose_link(now);
                }
            }
        }
    }

    /// Whether the radio's news about `address` concerns the pad we want.
    /// News about any other is stale: the call that superseded it is on its
    /// way to the radio.
    fn wanted(&self, address: &str) -> bool {
        !self.hold
            && self
                .device
                .as_ref()
                .is_some_and(|device| device.address.eq_ignore_ascii_case(address))
    }

    fn attempting(&self, address: &str) -> bool {
        match &self.link {
            Link::Searching(current) | Link::Connecting(current) => {
                current.eq_ignore_ascii_case(address)
            }
            _ => false,
        }
    }

    fn connected(&mut self, now: Instant, id: LinkId, address: String, name: Option<String>) {
        if !self.wanted(&address) || matches!(self.link, Link::Up(_)) {
            log::debug!("ignoring a stale link to {address}");
            return;
        }
        log::info!("linked to {address}");
        if let Some(device) = &mut self.device
            && name.is_some()
            && device.name != name
        {
            match self
                .tracker
                .store()
                .remember_device(&device.address, name.as_deref())
            {
                Ok(()) => device.name = name,
                Err(error) => log::error!("cannot store the pad's name: {error}"),
            }
        }
        let target = self
            .tracker
            .store()
            .target_kmh()
            .unwrap_or_else(|error| {
                log::error!("cannot read the target speed: {error}");
                None
            })
            .unwrap_or(DEFAULT_TARGET_KMH);
        self.link = Link::Up(Box::new(Up {
            id,
            address,
            controller: Controller::new(now, target),
            frames: Reassembler::default(),
        }));
        self.detail = None;
        self.retry = None;
    }

    fn attempt_failed(&mut self, now: Instant, reason: ConnectFailure) {
        log::info!("connecting failed: {}", reason.code());
        self.link = Link::Down;
        self.detail = Some(Detail::Failed(reason));
        if matches!(reason, ConnectFailure::NotFound | ConnectFailure::Failed) {
            self.schedule_retry(now);
        } else {
            self.hold = true;
            self.retry = None;
        }
    }

    fn frame(&mut self, chunk: &[u8]) {
        let Link::Up(up) = &mut self.link else {
            return;
        };
        let now = Instant::now();
        let mut actions = Vec::new();
        for frame in up.frames.push(chunk) {
            match frame.decode() {
                Notification::Status(status) => {
                    actions.extend(up.controller.on_status(now, &status));
                    let sample = sample(&status);
                    if let Err(error) =
                        self.tracker
                            .on_sample(&up.address, sample, self.clock.now())
                    {
                        log::error!("cannot record the pad's activity: {error:#}");
                    }
                }
                Notification::Prefs(prefs) => actions.extend(up.controller.on_prefs(&prefs)),
                Notification::Other(_) => {}
            }
        }
        self.act(actions);
    }

    fn act(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Write(command) => {
                    if let Link::Up(up) = &self.link {
                        self.radio.write(up.id, command.encode());
                    }
                }
                Action::Fail { request, failure } => {
                    self.refuse(request_name(request), &failure.into());
                }
                Action::TargetChanged { kmh, persist } => {
                    if persist && let Err(error) = self.tracker.store().set_target_kmh(kmh) {
                        log::error!("cannot store the target speed: {error}");
                    }
                }
                Action::LinkDead => {
                    self.radio.disconnect();
                    self.lose_link(Instant::now());
                }
            }
        }
    }

    fn on_tick(&mut self) {
        let now = Instant::now();
        if let Err(error) = self.tracker.on_tick(self.clock.now()) {
            log::error!("cannot update the activity: {error:#}");
        }
        if let Some(retry) = &mut self.retry
            && retry.at.is_some_and(|at| now >= at)
        {
            retry.at = None;
            if !self.seek(now, false) {
                self.schedule_retry(now);
            }
        }
    }

    fn on_deadline(&mut self) {
        if let Link::Up(up) = &mut self.link {
            let actions = up.controller.on_tick(Instant::now());
            self.act(actions);
        }
    }

    fn discovered(&mut self, mut device: ipc::ScanDevice) {
        if !self.scan.active {
            return;
        }
        let devices = &mut self.scan.devices;
        match devices
            .iter_mut()
            .find(|known| known.address == device.address)
        {
            Some(known) => {
                if device.name.is_none() {
                    device.name = known.name.take();
                }
                *known = device;
            }
            None => devices.push(device),
        }
        devices.sort_by(|a, b| {
            b.candidate
                .cmp(&a.candidate)
                .then(b.rssi.cmp(&a.rssi))
                .then_with(|| a.address.cmp(&b.address))
        });
    }

    /// Searches for the remembered pad on our own when nothing rules it
    /// out; a `limited` search also waits out [`WAKE_EVERY`] since the last.
    fn seek(&mut self, now: Instant, limited: bool) -> bool {
        let free = self.auto_connect
            && !self.hold
            && self.bluetooth == AdapterState::On
            && matches!(self.link, Link::Down);
        let rested = !limited || self.last_search.is_none_or(|at| now >= at + WAKE_EVERY);
        match &self.device {
            Some(device) if free && rested => {
                let address = device.address.clone();
                self.search(now, address);
                true
            }
            _ => false,
        }
    }

    fn search(&mut self, now: Instant, address: String) {
        log::info!("searching for {address}");
        self.radio.connect(&address, SEARCH);
        self.link = Link::Searching(address);
        if self.detail != Some(Detail::LinkLost) {
            self.detail = None;
        }
        self.last_search = Some(now);
    }

    fn schedule_retry(&mut self, now: Instant) {
        let Some(retry) = &mut self.retry else {
            return;
        };
        let at = now + RETRIES.get(retry.failures).copied().unwrap_or(RETRY_EVERY);
        retry.failures += 1;
        if at < retry.until {
            retry.at = Some(at);
        } else {
            log::info!("no longer searching for the pad on our own");
            self.retry = None;
        }
    }

    fn lose_link(&mut self, now: Instant) {
        self.leave();
        self.detail = Some(Detail::LinkLost);
        self.retry = self.auto_connect.then(|| Retry::window(now));
        self.seek(now, false);
    }

    fn hang_up(&mut self) {
        self.retry = None;
        self.leave();
        self.radio.disconnect();
    }

    /// Forgets the link or attempt; a link that was up reports the start or
    /// stop it leaves unfinished and closes its session.
    fn leave(&mut self) {
        if let Link::Up(mut up) = mem::replace(&mut self.link, Link::Down) {
            log::info!("unlinked from {}", up.address);
            let abandoned = up.controller.abandon();
            self.act(abandoned);
            if let Err(error) = self.tracker.on_link_lost() {
                log::error!("cannot close the session: {error:#}");
            }
        }
    }
}

fn sample(status: &Status) -> Sample {
    Sample {
        time_s: status.time_s,
        dist_10m: status.dist_10m,
        steps: status.steps,
        speed_dkmh: status.speed_dkmh,
        running: status.belt == Belt::Running,
    }
}

/// The `cmd` that made the request.
fn request_name(request: Request) -> &'static str {
    match request {
        Request::Start => "start",
        Request::Stop => "stop",
        Request::Toggle => "toggle",
        Request::SetTarget(_) => "speed",
        Request::Nudge(_) => "nudge",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::TempDir;

    #[tokio::test(start_paused = true)]
    async fn a_second_instance_gives_up_after_five_seconds() {
        let dir = TempDir::new("lock-held");
        let _held = lock(&dir.0, false).await.unwrap();
        let started = Instant::now();
        assert!(matches!(
            lock(&dir.0, false).await,
            Err(Fatal::AlreadyRunning)
        ));
        assert!(started.elapsed() >= LOCK_WAIT);
        assert_eq!(Fatal::AlreadyRunning.code(), "already-running");
        assert_eq!(Fatal::AlreadyRunning.exit_code(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_instance_takes_over_once_the_first_exits() {
        let dir = TempDir::new("lock-released");
        let held = lock(&dir.0, false).await.unwrap();
        let release = tokio::spawn(async move {
            time::sleep(Duration::from_secs(2)).await;
            drop(held);
        });
        let started = Instant::now();
        assert!(lock(&dir.0, false).await.is_ok());
        assert!(started.elapsed() >= Duration::from_secs(2));
        assert!(started.elapsed() < LOCK_WAIT);
        release.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn the_simulator_locks_apart_from_the_real_pad() {
        let dir = TempDir::new("lock-simulated");
        let _real = lock(&dir.0, false).await.unwrap();
        let _simulated = lock(&dir.0, true).await.unwrap();
        assert!(dir.join("daemon.lock").is_file());
        assert!(dir.join("daemon-sim.lock").is_file());
    }

    #[tokio::test]
    async fn refuses_a_lock_directory_that_is_a_symlink() {
        let dir = TempDir::new("lock-symlink");
        let link = dir.join("runtime");
        std::os::unix::fs::symlink(&dir.0, &link).unwrap();
        let refused = lock(&link, false).await.unwrap_err();
        assert!(matches!(refused, Fatal::DataDir(DataDirError::Symlink(_))));
        assert_eq!(refused.code(), "data-dir");
    }

    #[tokio::test]
    async fn creates_a_missing_lock_directory_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("lock-created");
        let runtime = dir.join("omarchy-ks-fit");
        lock(&runtime, false).await.unwrap();
        let mode = std::fs::metadata(&runtime).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}
