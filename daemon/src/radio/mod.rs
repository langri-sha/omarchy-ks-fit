//! The seam between the daemon and Bluetooth.

pub mod bluez;
pub mod sim;

use tokio::sync::mpsc;
use tokio::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterState {
    On,
    Off,
    /// Blocked by rfkill.
    Blocked,
    /// No Bluetooth adapter.
    Missing,
    /// bluetoothd or the system bus cannot be reached.
    Unavailable,
}

/// One connection to a pad. Writes addressed to an earlier link are dropped,
/// so nothing meant for a lost link reaches the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkId(u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RadioEvent {
    Adapter(AdapterState),
    /// A device seen during a user scan; repeated as its details change.
    Discovered {
        address: String,
        name: Option<String>,
        rssi: Option<i16>,
        candidate: bool,
    },
    ScanFinished,
    /// The pad was found and is being connected.
    Connecting {
        address: String,
    },
    /// The pad speaks the supported protocol and its notifications are on.
    Connected {
        link: LinkId,
        address: String,
        name: Option<String>,
    },
    ConnectFailed {
        address: String,
        reason: ConnectFailure,
    },
    /// A raw notification payload from the pad; frames may be split or joined.
    Frame(Vec<u8>),
    /// The link or the connection attempt is gone.
    Disconnected {
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectFailure {
    /// The search window ended without seeing the device.
    NotFound,
    Failed,
    UnsupportedFtms,
    UnsupportedKsText,
    NotAWalkingPad,
}

impl ConnectFailure {
    /// The `linkDetail` code on the wire.
    pub fn code(self) -> &'static str {
        match self {
            ConnectFailure::NotFound => "not-found",
            ConnectFailure::Failed => "connect-failed",
            ConnectFailure::UnsupportedFtms => "unsupported-ftms",
            ConnectFailure::UnsupportedKsText => "unsupported-ks-text",
            ConnectFailure::NotAWalkingPad => "not-a-walkingpad",
        }
    }
}

enum Call {
    Scan(Duration),
    StopScan,
    Connect { address: String, search: Duration },
    Disconnect,
    Write { link: LinkId, frame: Vec<u8> },
}

/// A handle on a Bluetooth backend running in its own task. Every call
/// returns at once; outcomes arrive as [`RadioEvent`]s.
#[derive(Debug)]
pub struct Radio {
    calls: mpsc::UnboundedSender<Call>,
    events: mpsc::UnboundedReceiver<RadioEvent>,
}

impl Radio {
    /// The simulated adapter and pad, and the hooks that drive it.
    pub fn simulated() -> (Radio, sim::Control) {
        let (radio, calls, events) = Radio::channel();
        (radio, sim::spawn(calls, events))
    }

    fn channel() -> (
        Radio,
        mpsc::UnboundedReceiver<Call>,
        mpsc::UnboundedSender<RadioEvent>,
    ) {
        let (calls, call_rx) = mpsc::unbounded_channel();
        let (event_tx, events) = mpsc::unbounded_channel();
        (Radio { calls, events }, call_rx, event_tx)
    }

    pub fn events(&mut self) -> &mut mpsc::UnboundedReceiver<RadioEvent> {
        &mut self.events
    }

    /// Discovers devices for `duration`, then reports [`RadioEvent::ScanFinished`].
    pub fn scan(&self, duration: Duration) {
        self.call(Call::Scan(duration));
    }

    pub fn stop_scan(&self) {
        self.call(Call::StopScan);
    }

    /// Adopts the device if it is already connected, otherwise searches for it
    /// for at most `search` and connects. Replaces any current link or attempt.
    pub fn connect(&self, address: &str, search: Duration) {
        self.call(Call::Connect {
            address: address.to_owned(),
            search,
        });
    }

    /// Drops the current link or attempt, reported as [`RadioEvent::Disconnected`].
    pub fn disconnect(&self) {
        self.call(Call::Disconnect);
    }

    /// Writes one frame to the pad on `link`, without response.
    pub fn write(&self, link: LinkId, frame: Vec<u8>) {
        self.call(Call::Write { link, frame });
    }

    fn call(&self, call: Call) {
        if self.calls.send(call).is_err() {
            log::error!("the Bluetooth task has stopped");
        }
    }
}

const fn bluetooth_uuid(short: u32) -> u128 {
    ((short as u128) << 96) | 0x0000_1000_8000_0080_5F9B_34FB
}

const WALKINGPAD_SERVICE: u128 = bluetooth_uuid(0xFE00);
const NOTIFY_CHARACTERISTIC: u128 = bluetooth_uuid(0xFE01);
const WRITE_CHARACTERISTIC: u128 = bluetooth_uuid(0xFE02);
const FTMS_SERVICE: u128 = bluetooth_uuid(0x1826);
const KS_TEXT_SERVICES: [u128; 3] = [
    bluetooth_uuid(0x1234),
    bluetooth_uuid(0x0001_1234),
    bluetooth_uuid(0x0002_1234),
];

/// Whether a scan result may be a WalkingPad. Deliberately broad: a false
/// positive costs one probe connection, a false negative hides the pad.
fn is_candidate(name: Option<&str>, services: impl IntoIterator<Item = u128>) -> bool {
    let named = name.is_some_and(|name| {
        let name = name.trim().to_uppercase();
        name.contains("WALKINGPAD")
            || ["KS-", "KINGSMITH", "R1 PRO"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
            || name == "RE"
    });
    named || services.into_iter().any(|uuid| uuid == WALKINGPAD_SERVICE)
}

/// Classifies a connected device by its GATT table: services with their
/// characteristics, each paired with whatever handle the backend needs.
/// A supported pad yields its notify and write characteristics.
fn classify<C>(
    services: impl IntoIterator<Item = (u128, Vec<(u128, C)>)>,
) -> Result<(C, C), ConnectFailure> {
    let mut failure = ConnectFailure::NotAWalkingPad;
    for (service, characteristics) in services {
        if service == WALKINGPAD_SERVICE {
            let (mut notify, mut write) = (None, None);
            for (uuid, handle) in characteristics {
                match uuid {
                    NOTIFY_CHARACTERISTIC => notify = Some(handle),
                    WRITE_CHARACTERISTIC => write = Some(handle),
                    _ => {}
                }
            }
            if let (Some(notify), Some(write)) = (notify, write) {
                return Ok((notify, write));
            }
        } else if service == FTMS_SERVICE {
            failure = ConnectFailure::UnsupportedFtms;
        } else if KS_TEXT_SERVICES.contains(&service) && failure != ConnectFailure::UnsupportedFtms
        {
            failure = ConnectFailure::UnsupportedKsText;
        }
    }
    Err(failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BATTERY_SERVICE: u128 = bluetooth_uuid(0x180F);

    fn verdict(services: Vec<(u128, Vec<u128>)>) -> Result<(u128, u128), ConnectFailure> {
        classify(services.into_iter().map(|(service, characteristics)| {
            (
                service,
                characteristics
                    .into_iter()
                    .map(|uuid| (uuid, uuid))
                    .collect(),
            )
        }))
    }

    #[test]
    fn expands_short_uuids_onto_the_bluetooth_base() {
        assert_eq!(
            WALKINGPAD_SERVICE,
            0x0000_FE00_0000_1000_8000_0080_5F9B_34FB
        );
        assert_eq!(
            KS_TEXT_SERVICES[1],
            0x0001_1234_0000_1000_8000_0080_5F9B_34FB
        );
    }

    #[test]
    fn treats_walkingpad_names_as_candidates() {
        let names = [
            "KS-ST-A1P",
            "ks-blc2",
            "WalkingPad",
            "My walkingpad A1",
            "KINGSMITH R2",
            "R1 PRO",
            "RE",
            " KS-HD-Z1D ",
        ];
        for name in names {
            assert!(is_candidate(Some(name), []), "{name}");
        }
    }

    #[test]
    fn treats_other_names_as_non_candidates() {
        for name in ["Some Headphones", "REX", "Keyboard KS-1", ""] {
            assert!(!is_candidate(Some(name), []), "{name}");
        }
        assert!(!is_candidate(None, [BATTERY_SERVICE]));
    }

    #[test]
    fn treats_an_advertised_fe00_service_as_a_candidate_whatever_the_name() {
        assert!(is_candidate(None, [BATTERY_SERVICE, WALKINGPAD_SERVICE]));
        assert!(is_candidate(Some("Unnamed"), [WALKINGPAD_SERVICE]));
    }

    #[test]
    fn accepts_fe00_with_its_notify_and_write_characteristics() {
        assert_eq!(
            verdict(vec![
                (BATTERY_SERVICE, vec![bluetooth_uuid(0x2A19)]),
                (
                    WALKINGPAD_SERVICE,
                    vec![WRITE_CHARACTERISTIC, NOTIFY_CHARACTERISTIC]
                ),
            ]),
            Ok((NOTIFY_CHARACTERISTIC, WRITE_CHARACTERISTIC))
        );
    }

    #[test]
    fn prefers_fe00_over_ftms_when_a_pad_has_both() {
        assert!(
            verdict(vec![
                (FTMS_SERVICE, vec![]),
                (
                    WALKINGPAD_SERVICE,
                    vec![NOTIFY_CHARACTERISTIC, WRITE_CHARACTERISTIC]
                ),
            ])
            .is_ok()
        );
    }

    #[test]
    fn reports_ftms_pads_as_unsupported() {
        assert_eq!(
            verdict(vec![(FTMS_SERVICE, vec![bluetooth_uuid(0x2ACD)])]),
            Err(ConnectFailure::UnsupportedFtms)
        );
        assert_eq!(
            verdict(vec![
                (WALKINGPAD_SERVICE, vec![NOTIFY_CHARACTERISTIC]),
                (FTMS_SERVICE, vec![]),
            ]),
            Err(ConnectFailure::UnsupportedFtms)
        );
    }

    #[test]
    fn reports_ks_text_pads_as_unsupported() {
        for service in KS_TEXT_SERVICES {
            assert_eq!(
                verdict(vec![(service, vec![bluetooth_uuid(0xFED7)])]),
                Err(ConnectFailure::UnsupportedKsText)
            );
        }
    }

    #[test]
    fn reports_anything_else_as_not_a_walkingpad() {
        assert_eq!(verdict(vec![]), Err(ConnectFailure::NotAWalkingPad));
        assert_eq!(
            verdict(vec![(BATTERY_SERVICE, vec![bluetooth_uuid(0x2A19)])]),
            Err(ConnectFailure::NotAWalkingPad)
        );
        assert_eq!(
            verdict(vec![(WALKINGPAD_SERVICE, vec![WRITE_CHARACTERISTIC])]),
            Err(ConnectFailure::NotAWalkingPad)
        );
    }

    #[test]
    fn connect_failure_codes_match_the_wire_contract() {
        let cases = [
            (ConnectFailure::NotFound, "not-found"),
            (ConnectFailure::Failed, "connect-failed"),
            (ConnectFailure::UnsupportedFtms, "unsupported-ftms"),
            (ConnectFailure::UnsupportedKsText, "unsupported-ks-text"),
            (ConnectFailure::NotAWalkingPad, "not-a-walkingpad"),
        ];
        for (failure, code) in cases {
            assert_eq!(failure.code(), code);
        }
    }
}
