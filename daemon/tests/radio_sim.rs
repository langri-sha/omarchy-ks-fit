use std::future;

use ks_fit::controller::{Action, Busy, Controller, PadView, Request};
use ks_fit::protocol::{Belt, Command, Notification, Reassembler, Status};
use ks_fit::radio::{LinkId, Radio, RadioEvent, sim};
use tokio::time::{Duration, Instant, sleep_until};

struct Rig {
    radio: Radio,
    sim: sim::Control,
    link: Option<(LinkId, Controller)>,
    frames: Reassembler,
    events: Vec<RadioEvent>,
    actions: Vec<Action>,
    last_status: Option<Status>,
}

impl Rig {
    fn new() -> Rig {
        let (radio, sim) = Radio::simulated();
        Rig {
            radio,
            sim,
            link: None,
            frames: Reassembler::default(),
            events: Vec::new(),
            actions: Vec::new(),
            last_status: None,
        }
    }

    async fn run_until(&mut self, limit: Duration, done: impl Fn(&Rig) -> bool) {
        let finished = self.drive(Instant::now() + limit, done).await;
        assert!(finished, "gave up waiting; events: {:?}", self.events);
    }

    async fn run_for(&mut self, span: Duration) {
        self.drive(Instant::now() + span, |_| false).await;
    }

    async fn drive(&mut self, until: Instant, done: impl Fn(&Rig) -> bool) -> bool {
        while !done(self) {
            if Instant::now() >= until {
                return false;
            }
            let deadline = self.link.as_ref().and_then(|(_, c)| c.next_deadline());
            tokio::select! {
                event = self.radio.events().recv() => match event {
                    Some(event) => self.on_event(event),
                    None => panic!("the simulator stopped"),
                },
                () = sleep_until_some(deadline) => {
                    let actions = match &mut self.link {
                        Some((_, controller)) => controller.on_tick(Instant::now()),
                        None => Vec::new(),
                    };
                    self.act(actions);
                }
                () = sleep_until(until) => {}
            }
        }
        true
    }

    fn on_event(&mut self, event: RadioEvent) {
        match &event {
            RadioEvent::Connected { link, .. } => {
                self.link = Some((*link, Controller::new(Instant::now(), 4.0)));
            }
            RadioEvent::Frame(chunk) => {
                for frame in self.frames.push(chunk) {
                    let Some((_, controller)) = &mut self.link else {
                        continue;
                    };
                    let actions = match frame.decode() {
                        Notification::Status(status) => {
                            self.last_status = Some(status);
                            controller.on_status(Instant::now(), &status)
                        }
                        Notification::Prefs(prefs) => controller.on_prefs(&prefs),
                        Notification::Other(_) => Vec::new(),
                    };
                    self.act(actions);
                }
            }
            RadioEvent::Disconnected { .. } => {
                self.link = None;
                self.frames.clear();
            }
            _ => {}
        }
        self.events.push(event);
    }

    fn act(&mut self, actions: Vec<Action>) {
        for action in actions {
            match (action, &self.link) {
                (Action::Write(command), Some((link, _))) => {
                    self.radio.write(*link, command.encode());
                }
                (Action::LinkDead, _) => self.radio.disconnect(),
                _ => {}
            }
            self.actions.push(action);
        }
    }

    fn request(&mut self, request: Request) {
        let actions = match &mut self.link {
            Some((_, controller)) => controller.request(Instant::now(), request),
            None => panic!("not linked"),
        };
        self.act(actions);
    }

    fn view(&self) -> Option<PadView> {
        self.link
            .as_ref()
            .and_then(|(_, controller)| controller.view())
    }

    fn discovered(&self) -> Vec<(&str, bool)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                RadioEvent::Discovered {
                    address, candidate, ..
                } => Some((address.as_str(), *candidate)),
                _ => None,
            })
            .collect()
    }
}

async fn sleep_until_some(at: Option<Instant>) {
    match at {
        Some(at) => sleep_until(at).await,
        None => future::pending().await,
    }
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[tokio::test(start_paused = true)]
async fn a_controller_walks_the_simulated_pad_from_scan_to_stop() {
    let mut rig = Rig::new();
    rig.radio.scan(secs(10));
    rig.run_until(secs(3), |rig| rig.discovered().len() == 2)
        .await;
    assert_eq!(
        rig.discovered(),
        [(sim::PAD_ADDRESS, true), (sim::HEADPHONES_ADDRESS, false)]
    );
    rig.radio.stop_scan();
    rig.run_until(secs(1), |rig| {
        rig.events.last() == Some(&RadioEvent::ScanFinished)
    })
    .await;

    rig.radio.connect(sim::PAD_ADDRESS, secs(8));
    rig.run_until(secs(5), |rig| rig.view().is_some()).await;
    assert_eq!(rig.view().map(|view| view.belt), Some(Belt::Standby));

    rig.request(Request::Start);
    rig.run_until(secs(15), |rig| {
        rig.view().is_some_and(|view| {
            view.belt == Belt::Running && view.speed_kmh == 4.0 && view.busy.is_none()
        })
    })
    .await;
    rig.run_for(secs(60)).await;
    let walked = rig.last_status.expect("a status");
    assert!(
        walked.time_s >= 60 && walked.steps > 100 && walked.dist_10m > 0,
        "{walked:?}"
    );

    rig.request(Request::Stop);
    assert_eq!(rig.view().and_then(|view| view.busy), Some(Busy::Stopping));
    rig.run_until(secs(10), |rig| {
        rig.view()
            .is_some_and(|view| view.belt == Belt::Idle && view.busy.is_none())
    })
    .await;

    assert!(
        !rig.actions
            .iter()
            .any(|action| matches!(action, Action::Fail { .. })),
        "{:?}",
        rig.actions
    );
    let writes = rig.sim.writes().await;
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
}
