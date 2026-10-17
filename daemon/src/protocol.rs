//! The WalkingPad wire protocol: frames in, commands out.

/// Length of every frame the pad sends.
pub const FRAME_LEN: usize = 20;

const TO_PAD: u8 = 0xF7;
const FROM_PAD: u8 = 0xF8;
const END: u8 = 0xFD;
const CONTROL: u8 = 0xA2;
const PROFILE: u8 = 0xA5;
const PREFS: u8 = 0xA6;
const RECORD: u8 = 0xA7;

/// A command the host writes to the pad's FE02 characteristic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Asks for one status frame.
    AskStats,
    /// Sets the belt speed in 0.1 km/h; 0 stops the belt. Ignored unless the belt moves.
    ChangeSpeed(u8),
    /// Puts the pad in manual mode.
    SwitchToManual,
    /// Starts the belt after a countdown. Some firmware treats it as a start/stop toggle.
    StartBelt,
    /// Asks for the preferences frame.
    QueryPrefs,
}

impl Command {
    pub fn encode(self) -> Vec<u8> {
        let mut frame = match self {
            Command::AskStats => vec![TO_PAD, CONTROL, 0x00, 0x00],
            Command::ChangeSpeed(dkmh) => vec![TO_PAD, CONTROL, 0x01, dkmh],
            Command::SwitchToManual => vec![TO_PAD, CONTROL, 0x02, 0x01],
            Command::StartBelt => vec![TO_PAD, CONTROL, 0x04, 0x01],
            Command::QueryPrefs => vec![TO_PAD, PREFS, 0x00, 0x00, 0x00, 0x00, 0x00],
        };
        frame.push(checksum(&frame[1..]));
        frame.push(END);
        frame
    }

    /// Whether the pad answers this command with a status frame.
    pub fn requests_status(self) -> bool {
        self != Command::QueryPrefs
    }
}

/// The sum of every byte between the start byte and the checksum byte.
fn checksum(body: &[u8]) -> u8 {
    body.iter().fold(0, |sum, byte| sum.wrapping_add(*byte))
}

fn starts_frame(first: u8, kind: u8) -> bool {
    first == FROM_PAD && matches!(kind, CONTROL | PROFILE | PREFS | RECORD)
}

/// A 20-byte frame from the pad whose start byte, end byte and checksum are valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame([u8; FRAME_LEN]);

impl Frame {
    fn parse(bytes: &[u8; FRAME_LEN]) -> Option<Frame> {
        let valid = starts_frame(bytes[0], bytes[1])
            && bytes[19] == END
            && bytes[18] == checksum(&bytes[1..18]);
        valid.then_some(Frame(*bytes))
    }

    pub fn decode(&self) -> Notification {
        let b = &self.0;
        let u24 = |at: usize| u32::from_be_bytes([0, b[at], b[at + 1], b[at + 2]]);
        match b[1] {
            CONTROL => Notification::Status(Status {
                belt: Belt::from(b[2]),
                speed_dkmh: b[3],
                mode: Mode::from(b[4]),
                time_s: u24(5),
                dist_10m: u24(8),
                steps: u24(11),
            }),
            PREFS => Notification::Prefs(Prefs {
                max_dkmh: b[7],
                imperial: b[13] != 0,
            }),
            kind => Notification::Other(kind),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notification {
    Status(Status),
    Prefs(Prefs),
    /// A valid frame of a type the daemon does not use (`A5` profile, `A7` record).
    Other(u8),
}

/// An `F8 A2` status. Counters are cumulative across belt stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub belt: Belt,
    /// Actual belt speed in 0.1 km/h.
    pub speed_dkmh: u8,
    pub mode: Mode,
    pub time_s: u32,
    pub dist_10m: u32,
    pub steps: u32,
}

/// An `F8 A6` preferences reply, reduced to what the daemon uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefs {
    /// Highest speed the pad accepts, in 0.1 km/h.
    pub max_dkmh: u8,
    /// Whether the pad displays miles; the status fields may then not be metric.
    pub imperial: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Belt {
    Idle,
    /// Counting down to `Running`.
    Starting,
    Running,
    Standby,
    /// Anything else; possibly moving.
    Unknown(u8),
}

impl From<u8> for Belt {
    fn from(state: u8) -> Belt {
        match state {
            0 => Belt::Idle,
            1 => Belt::Running,
            5 => Belt::Standby,
            6..=9 => Belt::Starting,
            other => Belt::Unknown(other),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Manual,
    Standby,
    Unknown(u8),
}

impl From<u8> for Mode {
    fn from(mode: u8) -> Mode {
        match mode {
            0 => Mode::Auto,
            1 => Mode::Manual,
            2 => Mode::Standby,
            other => Mode::Unknown(other),
        }
    }
}

/// Turns notification chunks into frames. Frames are found by position and
/// checksum, never by scanning for the end byte: the checksum can be `0xFD`.
#[derive(Debug, Default)]
pub struct Reassembler {
    pending: Vec<u8>,
}

impl Reassembler {
    /// Appends a notification payload and returns every frame it completes.
    /// Afterwards at most one partial frame stays buffered.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Frame> {
        self.pending.extend_from_slice(chunk);
        let mut frames = Vec::new();
        let mut start = 0;
        let mut skipped = 0;
        while let Some(rest) = self.pending.get(start..) {
            let Some(&[first, kind]) = rest.first_chunk::<2>() else {
                break;
            };
            if starts_frame(first, kind) {
                let Some(candidate) = rest.first_chunk::<FRAME_LEN>() else {
                    break;
                };
                if let Some(frame) = Frame::parse(candidate) {
                    frames.push(frame);
                    start += FRAME_LEN;
                    continue;
                }
            }
            start += 1;
            skipped += 1;
        }
        if skipped > 0 {
            log::debug!("skipped {skipped} bytes that do not start a valid frame");
        }
        self.pending.drain(..start);
        frames
    }

    pub fn clear(&mut self) {
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    fn frames(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut reassembler = Reassembler::default();
        chunks
            .iter()
            .flat_map(|chunk| reassembler.push(chunk))
            .map(|frame| frame.0.to_vec())
            .collect()
    }

    fn decode(text: &str) -> Notification {
        let bytes = hex(text);
        let frame = Frame::parse(bytes.as_slice().try_into().unwrap()).unwrap();
        frame.decode()
    }

    fn status(text: &str) -> Status {
        match decode(text) {
            Notification::Status(status) => status,
            other => panic!("expected a status, got {other:?}"),
        }
    }

    const N1: &str = "f8a2010f01000fd10000ab0012ae3c0000003afd";
    const N4: &str = "f8a200000100000000000000000001000000a4fd";
    const N5: &str = "f8a20000010000000000000000005a000000fdfd";

    #[test]
    fn encodes_the_commands_the_daemon_sends() {
        let cases = [
            (Command::AskStats, "f7a20000a2fd"),
            (Command::SwitchToManual, "f7a20201a5fd"),
            (Command::StartBelt, "f7a20401a7fd"),
            (Command::ChangeSpeed(0), "f7a20100a3fd"),
            (Command::ChangeSpeed(5), "f7a20105a8fd"),
            (Command::ChangeSpeed(30), "f7a2011ec1fd"),
            (Command::ChangeSpeed(60), "f7a2013cdffd"),
            (Command::QueryPrefs, "f7a60000000000a6fd"),
        ];
        for (command, expected) in cases {
            assert_eq!(command.encode(), hex(expected), "{command:?}");
        }
    }

    #[test]
    fn only_prefs_queries_go_unanswered_by_a_status() {
        assert!(Command::AskStats.requests_status());
        assert!(Command::ChangeSpeed(0).requests_status());
        assert!(Command::SwitchToManual.requests_status());
        assert!(Command::StartBelt.requests_status());
        assert!(!Command::QueryPrefs.requests_status());
    }

    #[test]
    fn every_published_command_carries_a_valid_checksum() {
        let vectors = [
            "f7a20000a2fd",
            "f7a20200a4fd",
            "f7a20201a5fd",
            "f7a20202a6fd",
            "f7a20401a7fd",
            "f7a20100a3fd",
            "f7a20105a8fd",
            "f7a2011ec1fd",
            "f7a2013cdffd",
            "f7a7aaff50fd",
            "f7a7aa0051fd",
            "f7a20307acfd",
            "f7a5604a4d937129c9fd",
            "f7a60000000000a6fd",
            "f7a6030000003ce5fd",
            "f7a60400000014befd",
            "f7a60500000001acfd",
            "f7a60500000000abfd",
            "f7a60600000001adfd",
            "f7a60600000003affd",
            "f7a6070000001fccfd",
            "f7a60800000001affd",
            "f7a60900000001b0fd",
            "f7a60103000708b9fd",
            "f7a6010100012cd5fd",
            "f7a60200000001a9fd",
            "f7a20400a6fd",
        ];
        for vector in vectors {
            let bytes = hex(vector);
            let n = bytes.len();
            assert_eq!(bytes[n - 2], checksum(&bytes[1..n - 2]), "{vector}");
        }
    }

    #[test]
    fn checksum_of_an_empty_body_is_zero() {
        assert_eq!(checksum(&[]), 0);
    }

    #[test]
    fn decodes_every_published_status() {
        let cases = [
            (N1, Belt::Running, 15, Mode::Manual, 4049, 171, 4782),
            (
                "f8a2013c0100022a00004f0003d1b4000000e3fd",
                Belt::Running,
                60,
                Mode::Manual,
                554,
                79,
                977,
            ),
            (
                "f8a205000200000000000000000002000000abfd",
                Belt::Standby,
                0,
                Mode::Standby,
                0,
                0,
                0,
            ),
            (N4, Belt::Idle, 0, Mode::Manual, 0, 0, 0),
            (N5, Belt::Idle, 0, Mode::Manual, 0, 0, 0),
            (
                "f8a200000000000000000000000000000000a2fd",
                Belt::Idle,
                0,
                Mode::Auto,
                0,
                0,
                0,
            ),
            (
                "f8a200000100000000000000000007000000aafd",
                Belt::Idle,
                0,
                Mode::Manual,
                0,
                0,
                0,
            ),
            (
                "f8a209000100000000000000000000000000acfd",
                Belt::Starting,
                0,
                Mode::Manual,
                0,
                0,
                0,
            ),
            (
                "f8a201230100070800012c000bb8690000002ffd",
                Belt::Running,
                35,
                Mode::Manual,
                1800,
                300,
                3000,
            ),
            (
                "f8a200000100070800012c000bb800000000a2fd",
                Belt::Idle,
                0,
                Mode::Manual,
                1800,
                300,
                3000,
            ),
            (
                "f8a2013c0100e1000003e8012345b4000000c9fd",
                Belt::Running,
                60,
                Mode::Manual,
                57600,
                1000,
                74565,
            ),
        ];
        for (vector, belt, speed_dkmh, mode, time_s, dist_10m, steps) in cases {
            assert_eq!(
                status(vector),
                Status {
                    belt,
                    speed_dkmh,
                    mode,
                    time_s,
                    dist_10m,
                    steps
                },
                "{vector}"
            );
        }
    }

    #[test]
    fn decodes_metric_and_imperial_prefs() {
        assert_eq!(
            decode("f8a600000000003c1400021f00000000000017fd"),
            Notification::Prefs(Prefs {
                max_dkmh: 60,
                imperial: false
            })
        );
        assert_eq!(
            decode("f8a60000000000501400021f0001000000002cfd"),
            Notification::Prefs(Prefs {
                max_dkmh: 80,
                imperial: true
            })
        );
    }

    #[test]
    fn passes_records_and_profile_replies_through_undecoded() {
        let cases = [
            ("f8a700000000000000000000000000000000a7fd", 0xA7),
            ("f8a7000de3000009000dda0001580017510048fd", 0xA7),
            ("f8a7001c200015180007080000fa000bb802defd", 0xA7),
            ("f8a54d5031120ab40a000100002173180000fafd", 0xA5),
        ];
        for (vector, kind) in cases {
            assert_eq!(frames(&[&hex(vector)]), [hex(vector)], "{vector}");
            assert_eq!(decode(vector), Notification::Other(kind), "{vector}");
        }
    }

    #[test]
    fn maps_belt_states() {
        let cases = [
            (0, Belt::Idle),
            (1, Belt::Running),
            (2, Belt::Unknown(2)),
            (4, Belt::Unknown(4)),
            (5, Belt::Standby),
            (6, Belt::Starting),
            (9, Belt::Starting),
            (10, Belt::Unknown(10)),
            (255, Belt::Unknown(255)),
        ];
        for (state, belt) in cases {
            assert_eq!(Belt::from(state), belt, "{state}");
        }
    }

    #[test]
    fn maps_modes() {
        let cases = [
            (0, Mode::Auto),
            (1, Mode::Manual),
            (2, Mode::Standby),
            (4, Mode::Unknown(4)),
        ];
        for (byte, mode) in cases {
            assert_eq!(Mode::from(byte), mode, "{byte}");
        }
    }

    #[test]
    fn buffers_a_fragment_until_the_rest_arrives() {
        let mut reassembler = Reassembler::default();
        assert_eq!(reassembler.push(&hex("f8a2000001000000000000")), []);
        let completed = reassembler.push(&hex("00000001000000a4fd"));
        assert_eq!(
            completed.iter().map(|f| f.0.to_vec()).collect::<Vec<_>>(),
            [hex(N4)]
        );
    }

    #[test]
    fn rejects_a_corrupt_checksum_and_resyncs_on_the_next_frame() {
        let mut corrupt = hex(N1);
        corrupt[3] = 0x10;
        assert_eq!(frames(&[&corrupt, &hex(N1)]), [hex(N1)]);
    }

    #[test]
    fn skips_leading_garbage() {
        let mut chunk = vec![0x00, 0xFF];
        chunk.extend(hex(N1));
        assert_eq!(frames(&[&chunk]), [hex(N1)]);
    }

    #[test]
    fn splits_two_frames_when_the_first_checksum_equals_the_end_byte() {
        let mut chunk = hex(N5);
        chunk.extend(hex(N1));
        assert_eq!(frames(&[&chunk]), [hex(N5), hex(N1)]);
    }

    #[test]
    fn reassembles_frames_fed_one_byte_at_a_time() {
        let mut stream = hex(N5);
        stream.extend(hex(N1));
        let bytes: Vec<[u8; 1]> = stream.iter().map(|b| [*b]).collect();
        let chunks: Vec<&[u8]> = bytes.iter().map(|b| b.as_slice()).collect();
        assert_eq!(frames(&chunks), [hex(N5), hex(N1)]);
    }

    #[test]
    fn rejects_unknown_frame_types_even_with_a_valid_checksum() {
        let mut bogus = hex(N1);
        bogus[1] = 0xB1;
        bogus[18] = checksum(&bogus[1..18]);
        assert_eq!(frames(&[&bogus]), Vec::<Vec<u8>>::new());
    }

    #[test]
    fn clear_drops_a_partial_frame() {
        let mut reassembler = Reassembler::default();
        reassembler.push(&hex("f8a2000001000000000000"));
        reassembler.clear();
        assert_eq!(reassembler.push(&hex("00000001000000a4fd")), []);
        assert_eq!(reassembler.push(&hex(N1)).len(), 1);
    }

    #[test]
    fn survives_arbitrary_noise_with_a_bounded_buffer() {
        let mut reassembler = Reassembler::default();
        let mut seed: u32 = 0x2545_F491;
        for round in 0..2000 {
            let len = (seed % 41) as usize;
            let chunk: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    if seed.is_multiple_of(7) {
                        FROM_PAD
                    } else {
                        seed as u8
                    }
                })
                .collect();
            for frame in reassembler.push(&chunk) {
                frame.decode();
            }
            assert!(reassembler.pending.len() < FRAME_LEN, "round {round}");
        }
        assert_eq!(
            reassembler.push(&hex(N1)).last().map(|f| f.0.to_vec()),
            Some(hex(N1))
        );
    }
}
