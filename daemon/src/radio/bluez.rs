//! BlueZ transport.

use std::future::{self, Future};

use bluer::gatt::WriteOp;
use bluer::gatt::remote::{Characteristic, CharacteristicWriteRequest};
use bluer::{
    Adapter, AdapterEvent, AdapterProperty, Address, Device, DeviceEvent, DeviceProperty,
    DiscoveryFilter, DiscoveryTransport, ErrorKind, InternalErrorKind, Session, SessionEvent,
};
use futures_util::future::{BoxFuture, FutureExt};
use futures_util::stream::{BoxStream, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant, MissedTickBehavior, interval, timeout};

use super::{
    AdapterState, Call, ConnectFailure, LinkId, RadioEvent, classify, is_candidate, rfkill,
    sleep_until_some,
};

/// bluer waits up to 120 s for any D-Bus reply, so every call gets its own limit.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const GATT_TIMEOUT: Duration = Duration::from_secs(20);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// rfkill and bluetoothd going away raise no event, so the adapter is polled too.
const CHECK_EVERY: Duration = Duration::from_secs(2);
/// A busy bluetoothd can miss a probe's deadline while the link is fine, so
/// it takes this many unanswered probes in a row to call it unavailable.
const UNANSWERED_PROBES: u8 = 2;

pub(super) fn spawn(
    calls: mpsc::UnboundedReceiver<Call>,
    events: mpsc::UnboundedSender<RadioEvent>,
) {
    tokio::spawn(Bluez::new(events).run(calls));
}

#[derive(Debug, thiserror::Error)]
enum Fault {
    #[error(transparent)]
    Bluez(#[from] bluer::Error),
    #[error("BlueZ did not answer in time")]
    TimedOut,
}

impl Fault {
    fn is(&self, kind: &ErrorKind) -> bool {
        matches!(self, Fault::Bluez(error) if error.kind == *kind)
    }
}

/// What one look at the adapter found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    Answered(AdapterState),
    /// BlueZ did not answer in time, or failed without saying anything
    /// about the adapter.
    Unanswered,
    /// The system bus connection is gone.
    Lost,
}

impl Probe {
    fn failed(fault: &Fault) -> Probe {
        if fault.is(&ErrorKind::Internal(InternalErrorKind::DBusConnectionLost)) {
            Probe::Lost
        } else {
            Probe::Unanswered
        }
    }
}

/// The adapter state as the probes tell it.
#[derive(Debug, Default)]
struct Health {
    state: Option<AdapterState>,
    unanswered: u8,
}

impl Health {
    /// Takes in a probe, returning the adapter state when it changes. An
    /// unanswered probe keeps a known state until [`UNANSWERED_PROBES`] in
    /// a row.
    fn update(&mut self, probe: Probe) -> Option<AdapterState> {
        let state = match probe {
            Probe::Answered(state) => {
                self.unanswered = 0;
                state
            }
            Probe::Lost => {
                self.unanswered = 0;
                AdapterState::Unavailable
            }
            Probe::Unanswered => {
                self.unanswered = self.unanswered.saturating_add(1);
                if self.state.is_some() && self.unanswered < UNANSWERED_PROBES {
                    return None;
                }
                AdapterState::Unavailable
            }
        };
        (self.state != Some(state)).then(|| {
            self.state = Some(state);
            state
        })
    }
}

async fn bounded<T>(
    limit: Duration,
    call: impl Future<Output = bluer::Result<T>>,
) -> Result<T, Fault> {
    timeout(limit, call)
        .await
        .map_err(|_| Fault::TimedOut)?
        .map_err(Fault::from)
}

/// The task that owns every bluer object; nothing bluer leaves it.
struct Bluez {
    events: mpsc::UnboundedSender<RadioEvent>,
    session: Option<Session>,
    session_events: Option<BoxStream<'static, SessionEvent>>,
    adapter: Option<Adapter>,
    adapter_events: Option<BoxStream<'static, AdapterEvent>>,
    health: Health,
    /// One discovery session shared by user scans and connect searches.
    discovery: Option<BoxStream<'static, AdapterEvent>>,
    scan_until: Option<Instant>,
    search: Option<Search>,
    opening: Option<Opening>,
    link: Option<Link>,
    links: u64,
}

struct Search {
    address: Address,
    until: Instant,
}

struct Opening {
    device: Device,
    /// The pad was already connected, so a failure must not disconnect it.
    adopted: bool,
    future: BoxFuture<'static, Result<Pad, ConnectFailure>>,
}

struct Pad {
    device: Device,
    name: Option<String>,
    write: Characteristic,
    frames: BoxStream<'static, Vec<u8>>,
    changes: BoxStream<'static, DeviceEvent>,
}

struct Link {
    id: LinkId,
    pad: Pad,
}

enum LinkEvent {
    Frame(Vec<u8>),
    Lost(&'static str),
}

impl Bluez {
    fn new(events: mpsc::UnboundedSender<RadioEvent>) -> Bluez {
        Bluez {
            events,
            session: None,
            session_events: None,
            adapter: None,
            adapter_events: None,
            health: Health::default(),
            discovery: None,
            scan_until: None,
            search: None,
            opening: None,
            link: None,
            links: 0,
        }
    }

    /// Ends when the [`super::Radio`] is dropped. The link is left up then, so
    /// a daemon restarted by a shell reload adopts it.
    async fn run(mut self, mut calls: mpsc::UnboundedReceiver<Call>) {
        let mut check = interval(CHECK_EVERY);
        check.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            let search_until = self.search.as_ref().map(|search| search.until);
            tokio::select! {
                call = calls.recv() => match call {
                    Some(call) => self.call(call).await,
                    None => return,
                },
                _ = check.tick() => self.refresh().await,
                event = next(&mut self.session_events) => self.on_session_event(event).await,
                event = next(&mut self.adapter_events) => self.on_adapter_event(event).await,
                event = next(&mut self.discovery) => self.on_discovery_event(event).await,
                () = sleep_until_some(self.scan_until) => self.end_scan(),
                () = sleep_until_some(search_until) => self.end_search(ConnectFailure::NotFound),
                result = opened(&mut self.opening) => self.on_opened(result),
                event = link_event(&mut self.link) => self.on_link_event(event).await,
            }
        }
    }

    async fn call(&mut self, call: Call) {
        match call {
            Call::Scan(duration) => self.scan(duration).await,
            Call::StopScan => {
                if self.scan_until.is_some() {
                    self.end_scan();
                }
            }
            Call::Connect { address, search } => self.connect(&address, search).await,
            Call::Disconnect => self.disconnect("disconnect requested").await,
            Call::Write { link, frame } => self.write(link, &frame).await,
        }
    }

    async fn scan(&mut self, duration: Duration) {
        let joining = self.discovery.is_some();
        self.scan_until = Some(Instant::now() + duration);
        if !self.discover().await {
            self.end_scan();
        } else if joining {
            self.report_known_devices().await;
        }
    }

    fn end_scan(&mut self) {
        self.scan_until = None;
        self.release_discovery();
        self.emit(RadioEvent::ScanFinished);
    }

    async fn connect(&mut self, address: &str, search: Duration) {
        self.disconnect("replaced by a new connection").await;
        let failed = |reason| RadioEvent::ConnectFailed {
            address: address.to_owned(),
            reason,
        };
        let Ok(parsed) = address.parse::<Address>() else {
            log::warn!("not a Bluetooth address: {address:?}");
            self.emit(failed(ConnectFailure::Failed));
            return;
        };
        let device = match &self.adapter {
            Some(adapter) if self.health.state == Some(AdapterState::On) => adapter.device(parsed),
            _ => {
                self.emit(failed(ConnectFailure::Failed));
                return;
            }
        };
        let Ok(device) = device else {
            self.emit(failed(ConnectFailure::Failed));
            return;
        };
        // A connected pad does not advertise, so a search would never find it.
        if matches!(bounded(CALL_TIMEOUT, device.is_connected()).await, Ok(true)) {
            log::info!("adopting the already connected pad {parsed}");
            self.open(device, true);
            return;
        }
        self.search = Some(Search {
            address: parsed,
            until: Instant::now() + search,
        });
        if !self.discover().await {
            self.end_search(ConnectFailure::Failed);
        } else if present(&device).await {
            self.found(device);
        }
    }

    fn end_search(&mut self, reason: ConnectFailure) {
        if let Some(search) = self.search.take() {
            log::info!("search for {} ended: {reason:?}", search.address);
            self.release_discovery();
            self.emit(RadioEvent::ConnectFailed {
                address: search.address.to_string(),
                reason,
            });
        }
    }

    /// Drops the link or the attempt. A pending `Connect` keeps running in
    /// BlueZ after its future is dropped, so it is cancelled with `Disconnect`.
    async fn disconnect(&mut self, reason: &str) {
        let searching = self.search.take().is_some();
        let (link, opening) = (self.link.take(), self.opening.take());
        let active = searching || link.is_some() || opening.is_some();
        let ours = match (link, opening) {
            (Some(link), _) => Some(link.pad.device),
            (None, Some(opening)) if !opening.adopted => Some(opening.device),
            _ => None,
        };
        if let Some(device) = ours {
            release(&device).await;
        }
        if searching {
            self.release_discovery();
        }
        if active {
            self.emit(RadioEvent::Disconnected {
                reason: reason.into(),
            });
        }
    }

    async fn write(&mut self, link: LinkId, frame: &[u8]) {
        let Some(current) = self.link.as_ref().filter(|current| current.id == link) else {
            log::debug!("dropping a write for a link that is gone");
            return;
        };
        let request = CharacteristicWriteRequest {
            op_type: WriteOp::Command,
            ..Default::default()
        };
        if let Err(fault) =
            bounded(WRITE_TIMEOUT, current.pad.write.write_ext(frame, &request)).await
        {
            log::warn!("writing to the pad failed: {fault}");
        }
    }

    /// Starts discovery unless it already runs: LE only and without a UUID
    /// filter, which can crash BlueZ 5.87 when a matching device advertises.
    async fn discover(&mut self) -> bool {
        if self.discovery.is_some() {
            return true;
        }
        let Some(adapter) = self
            .adapter
            .as_ref()
            .filter(|_| self.health.state == Some(AdapterState::On))
        else {
            return false;
        };
        let filter = DiscoveryFilter {
            transport: DiscoveryTransport::Le,
            ..Default::default()
        };
        if let Err(error) = adapter.set_discovery_filter(filter).await {
            log::debug!("keeping the current discovery filter: {error}");
        }
        match bounded(CALL_TIMEOUT, adapter.discover_devices_with_changes()).await {
            Ok(stream) => {
                self.discovery = Some(stream.boxed());
                true
            }
            Err(fault) => {
                log::warn!("cannot start discovery: {fault}");
                false
            }
        }
    }

    /// Stops discovery once nobody needs it: a scan left running makes
    /// Bluetooth audio stutter.
    fn release_discovery(&mut self) {
        if self.scan_until.is_none() && self.search.is_none() {
            self.discovery = None;
        }
    }

    async fn on_discovery_event(&mut self, event: Option<AdapterEvent>) {
        match event {
            Some(AdapterEvent::DeviceAdded(address)) => self.seen(address).await,
            Some(_) => {}
            None => {
                log::warn!("discovery stopped early");
                self.discovery = None;
                if self.scan_until.is_some() {
                    self.end_scan();
                }
                self.end_search(ConnectFailure::NotFound);
            }
        }
    }

    /// A newly started discovery lists the devices BlueZ already knows; a
    /// joined one does not, so a scan joining a search asks for them.
    async fn report_known_devices(&mut self) {
        let Some(adapter) = &self.adapter else {
            return;
        };
        match bounded(CALL_TIMEOUT, adapter.device_addresses()).await {
            Ok(addresses) => {
                for address in addresses {
                    self.seen(address).await;
                }
            }
            Err(fault) => log::debug!("cannot list known devices: {fault}"),
        }
    }

    async fn seen(&mut self, address: Address) {
        let Some(device) = self
            .adapter
            .as_ref()
            .and_then(|adapter| adapter.device(address).ok())
        else {
            return;
        };
        // BlueZ also lists cached devices that are out of range; those have no RSSI.
        let Ok(Some(rssi)) = bounded(CALL_TIMEOUT, device.rssi()).await else {
            return;
        };
        if self
            .search
            .as_ref()
            .is_some_and(|search| search.address == address)
        {
            self.found(device.clone());
        }
        if self.scan_until.is_some() {
            let name = bounded(CALL_TIMEOUT, device.name()).await.ok().flatten();
            let services = bounded(CALL_TIMEOUT, device.uuids())
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            let candidate =
                is_candidate(name.as_deref(), services.iter().map(|uuid| uuid.as_u128()));
            self.emit(RadioEvent::Discovered {
                address: address.to_string(),
                name,
                rssi: Some(rssi),
                candidate,
            });
        }
    }

    fn found(&mut self, device: Device) {
        self.search = None;
        self.release_discovery();
        self.open(device, false);
    }

    fn open(&mut self, device: Device, adopted: bool) {
        self.emit(RadioEvent::Connecting {
            address: device.address().to_string(),
        });
        self.opening = Some(Opening {
            future: open(device.clone(), adopted).boxed(),
            device,
            adopted,
        });
    }

    fn on_opened(&mut self, result: Result<Pad, ConnectFailure>) {
        let Some(opening) = self.opening.take() else {
            return;
        };
        let address = opening.device.address().to_string();
        match result {
            Ok(pad) => {
                self.links += 1;
                let id = LinkId(self.links);
                log::info!("connected to {address}");
                self.emit(RadioEvent::Connected {
                    link: id,
                    address,
                    name: pad.name.clone(),
                });
                self.link = Some(Link { id, pad });
            }
            Err(reason) => {
                log::warn!("connecting to {address} failed: {reason:?}");
                self.emit(RadioEvent::ConnectFailed { address, reason });
            }
        }
    }

    async fn on_link_event(&mut self, event: LinkEvent) {
        match event {
            LinkEvent::Frame(frame) => self.emit(RadioEvent::Frame(frame)),
            LinkEvent::Lost(reason) => {
                log::warn!("lost the pad: {reason}");
                if let Some(link) = self.link.take() {
                    release(&link.pad.device).await;
                }
                self.emit(RadioEvent::Disconnected {
                    reason: reason.into(),
                });
            }
        }
    }

    async fn on_session_event(&mut self, event: Option<SessionEvent>) {
        match event {
            Some(SessionEvent::AdapterAdded(name)) if self.is_current(&name) => {
                // bluetoothd restarted: every object it handed out is gone.
                log::warn!("{name} was registered again");
                self.reset("Bluetooth restarted");
                self.health = Health::default();
            }
            Some(SessionEvent::AdapterRemoved(name)) if self.is_current(&name) => {
                self.adapter = None;
                self.adapter_events = None;
            }
            Some(_) => {}
            None => self.forget_session(),
        }
        self.refresh().await;
    }

    async fn on_adapter_event(&mut self, event: Option<AdapterEvent>) {
        match event {
            Some(AdapterEvent::PropertyChanged(AdapterProperty::Powered(_))) => {
                self.refresh().await
            }
            Some(_) => {}
            None => {
                self.adapter = None;
                self.adapter_events = None;
                self.refresh().await;
            }
        }
    }

    fn is_current(&self, name: &str) -> bool {
        self.adapter
            .as_ref()
            .is_some_and(|adapter| adapter.name() == name)
    }

    async fn refresh(&mut self) {
        let probe = self.probe().await;
        let Some(state) = self.health.update(probe) else {
            return;
        };
        log::info!("Bluetooth is {state:?}");
        if state != AdapterState::On {
            self.reset("Bluetooth is not on");
        }
        self.emit(RadioEvent::Adapter(state));
    }

    async fn probe(&mut self) -> Probe {
        if self.session.is_none() {
            match bounded(CALL_TIMEOUT, Session::new()).await {
                Ok(session) => {
                    self.session_events = bounded(CALL_TIMEOUT, session.events())
                        .await
                        .ok()
                        .map(StreamExt::boxed);
                    self.session = Some(session);
                }
                Err(fault) => {
                    log::debug!("no system bus: {fault}");
                    return Probe::Lost;
                }
            }
        }
        let Some(session) = &self.session else {
            return Probe::Lost;
        };
        if self.adapter.is_none() {
            match bounded(CALL_TIMEOUT, session.default_adapter()).await {
                Ok(adapter) => {
                    self.adapter_events = bounded(CALL_TIMEOUT, adapter.events())
                        .await
                        .ok()
                        .map(StreamExt::boxed);
                    self.adapter = Some(adapter);
                }
                Err(fault) if fault.is(&ErrorKind::NotFound) => {
                    return Probe::Answered(missing());
                }
                Err(fault) => return self.unanswered(&fault),
            }
        }
        let Some(adapter) = &self.adapter else {
            return Probe::Unanswered;
        };
        match bounded(CALL_TIMEOUT, adapter.is_powered()).await {
            Ok(true) => Probe::Answered(AdapterState::On),
            Ok(false) if rfkill::adapter_blocked(adapter.name()) => {
                Probe::Answered(AdapterState::Blocked)
            }
            Ok(false) => Probe::Answered(AdapterState::Off),
            Err(fault) if fault.is(&ErrorKind::NotFound) => {
                self.adapter = None;
                self.adapter_events = None;
                Probe::Answered(missing())
            }
            Err(fault) => self.unanswered(&fault),
        }
    }

    fn unanswered(&mut self, fault: &Fault) -> Probe {
        log::debug!("bluetoothd did not answer: {fault}");
        let probe = Probe::failed(fault);
        if probe == Probe::Lost {
            self.forget_session();
        }
        probe
    }

    fn forget_session(&mut self) {
        self.session = None;
        self.session_events = None;
        self.adapter = None;
        self.adapter_events = None;
    }

    /// Ends everything that depended on the adapter, without calling into BlueZ.
    fn reset(&mut self, reason: &str) {
        self.discovery = None;
        if self.scan_until.take().is_some() {
            self.emit(RadioEvent::ScanFinished);
        }
        if let Some(search) = self.search.take() {
            self.emit(RadioEvent::ConnectFailed {
                address: search.address.to_string(),
                reason: ConnectFailure::Failed,
            });
        }
        if let Some(opening) = self.opening.take() {
            self.emit(RadioEvent::ConnectFailed {
                address: opening.device.address().to_string(),
                reason: ConnectFailure::Failed,
            });
        }
        if self.link.take().is_some() {
            self.emit(RadioEvent::Disconnected {
                reason: reason.into(),
            });
        }
    }

    fn emit(&self, event: RadioEvent) {
        if self.events.send(event).is_err() {
            log::debug!("nobody listens to the radio");
        }
    }
}

fn missing() -> AdapterState {
    // A platform rfkill switch can make the controller vanish altogether.
    if rfkill::any_bluetooth_blocked() {
        AdapterState::Blocked
    } else {
        AdapterState::Missing
    }
}

/// Whether BlueZ saw the device advertise since discovery last stopped.
async fn present(device: &Device) -> bool {
    matches!(bounded(CALL_TIMEOUT, device.rssi()).await, Ok(Some(_)))
}

/// Connects unless adopted, classifies the GATT table and subscribes to FE01.
async fn open(device: Device, adopted: bool) -> Result<Pad, ConnectFailure> {
    let pad = subscribe(device.clone(), adopted).await;
    if pad.is_err() && !adopted {
        release(&device).await;
    }
    pad
}

async fn subscribe(device: Device, adopted: bool) -> Result<Pad, ConnectFailure> {
    let failed = |step: &'static str| {
        move |fault: Fault| {
            log::warn!("{step} failed: {fault}");
            ConnectFailure::Failed
        }
    };
    let changes = bounded(CALL_TIMEOUT, device.events())
        .await
        .map_err(failed("watching the device"))?
        .boxed();
    if !adopted {
        bounded(CONNECT_TIMEOUT, device.connect())
            .await
            .map_err(failed("connecting"))?;
    }
    let services = bounded(GATT_TIMEOUT, gatt(&device))
        .await
        .map_err(failed("resolving services"))?;
    let (notify, write) = classify(services)?;
    // StartNotify, never a read: reading FE01 re-injects its value as a notification.
    let frames = bounded(CALL_TIMEOUT, notify.notify())
        .await
        .map_err(failed("subscribing to notifications"))?
        .boxed();
    let name = bounded(CALL_TIMEOUT, device.name()).await.ok().flatten();
    Ok(Pad {
        device,
        name,
        write,
        frames,
        changes,
    })
}

type Gatt = Vec<(u128, Vec<(u128, Characteristic)>)>;

/// Services and characteristics by UUID; `services()` waits for ServicesResolved.
async fn gatt(device: &Device) -> bluer::Result<Gatt> {
    let mut table = Vec::new();
    for service in device.services().await? {
        let mut characteristics = Vec::new();
        for characteristic in service.characteristics().await? {
            characteristics.push((characteristic.uuid().await?.as_u128(), characteristic));
        }
        table.push((service.uuid().await?.as_u128(), characteristics));
    }
    Ok(table)
}

async fn release(device: &Device) {
    if let Err(fault) = bounded(CALL_TIMEOUT, device.disconnect()).await {
        log::debug!("disconnecting {} failed: {fault}", device.address());
    }
}

async fn next<T>(stream: &mut Option<BoxStream<'static, T>>) -> Option<T> {
    match stream {
        Some(stream) => stream.next().await,
        None => future::pending().await,
    }
}

async fn opened(opening: &mut Option<Opening>) -> Result<Pad, ConnectFailure> {
    match opening {
        Some(opening) => (&mut opening.future).await,
        None => future::pending().await,
    }
}

/// Frames until the link goes. A notify stream only goes quiet on disconnect,
/// so `Connected(false)` is what ends a link.
async fn link_event(link: &mut Option<Link>) -> LinkEvent {
    let Some(link) = link else {
        return future::pending().await;
    };
    loop {
        tokio::select! {
            frame = link.pad.frames.next() => match frame {
                Some(frame) => return LinkEvent::Frame(frame),
                None => return LinkEvent::Lost("notifications stopped"),
            },
            change = link.pad.changes.next() => match change {
                Some(DeviceEvent::PropertyChanged(DeviceProperty::Connected(false))) => {
                    return LinkEvent::Lost("the pad disconnected");
                }
                Some(_) => {}
                None => return LinkEvent::Lost("the pad went away"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AdapterState::{Off, On, Unavailable};
    use Probe::{Answered, Lost, Unanswered};

    fn changes(probes: &[Probe]) -> Vec<Option<AdapterState>> {
        let mut health = Health::default();
        probes.iter().map(|probe| health.update(*probe)).collect()
    }

    #[test]
    fn one_unanswered_probe_keeps_the_adapter_on() {
        assert_eq!(
            changes(&[Answered(On), Unanswered, Answered(On)]),
            [Some(On), None, None]
        );
    }

    #[test]
    fn two_unanswered_probes_in_a_row_make_bluetooth_unavailable() {
        assert_eq!(
            changes(&[Answered(On), Unanswered, Unanswered, Unanswered]),
            [Some(On), None, Some(Unavailable), None]
        );
    }

    #[test]
    fn an_answer_between_unanswered_probes_starts_the_count_over() {
        assert_eq!(
            changes(&[Answered(On), Unanswered, Answered(On), Unanswered]),
            [Some(On), None, None, None]
        );
    }

    #[test]
    fn a_lost_bus_makes_bluetooth_unavailable_at_once() {
        assert_eq!(
            changes(&[Answered(On), Lost]),
            [Some(On), Some(Unavailable)]
        );
    }

    #[test]
    fn an_unanswered_first_probe_has_no_state_to_keep() {
        assert_eq!(changes(&[Unanswered]), [Some(Unavailable)]);
    }

    #[test]
    fn an_answer_changes_the_state_at_once() {
        assert_eq!(
            changes(&[Answered(On), Answered(Off), Answered(On)]),
            [Some(On), Some(Off), Some(On)]
        );
    }

    #[test]
    fn a_timeout_leaves_the_adapter_state_unknown() {
        assert_eq!(Probe::failed(&Fault::TimedOut), Unanswered);
    }

    #[test]
    fn a_lost_dbus_connection_is_lost_and_other_errors_unanswered() {
        let fault = |kind| {
            Fault::Bluez(bluer::Error {
                kind,
                message: String::new(),
            })
        };
        assert_eq!(
            Probe::failed(&fault(ErrorKind::Internal(
                InternalErrorKind::DBusConnectionLost
            ))),
            Lost
        );
        assert_eq!(Probe::failed(&fault(ErrorKind::Failed)), Unanswered);
    }
}
