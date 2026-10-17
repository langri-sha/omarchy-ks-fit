//! Sequences belt commands and paces writes to the pad.

use std::collections::VecDeque;

use tokio::time::{Duration, Instant};

use crate::protocol::{Belt, Command, Mode, Prefs, Status};

/// The pad silently drops commands that follow each other too closely.
const SPACING: Duration = Duration::from_millis(690);
const POLL: Duration = Duration::from_secs(1);
const WATCHDOG: Duration = Duration::from_secs(10);
/// How old the newest status may be when `04 01` is written.
const FRESH: Duration = Duration::from_secs(2);
const PREFS_QUERIES: u8 = 3;
const PREFS_RETRY: Duration = Duration::from_secs(3);
const MODE_WAIT: Duration = Duration::from_secs(3);
const MODE_SENDS: u8 = 2;
const START_WAIT: Duration = Duration::from_secs(6);
const IDLE_POLLS_AFTER_FAILED_START: u8 = 2;
const STOP_WAIT: Duration = Duration::from_secs(6);
const STOP_PROGRESS: Duration = Duration::from_secs(3);
const SPEED_WAIT: Duration = Duration::from_secs(4);
const SPEED_SENDS: u8 = 3;
const REMOTE_SETTLE: Duration = Duration::from_secs(3);
const NUDGE_LEAD_DKMH: u8 = 5;
const MIN_DKMH: u8 = 5;
const FALLBACK_MAX_DKMH: u8 = 60;
const REPLY_WINDOW: Duration = Duration::from_secs(2);
const FOREIGN_WINDOW: Duration = Duration::from_secs(5);
const FOREIGN_STATUSES: usize = 3;
const SHARED_HOLD: Duration = Duration::from_secs(10);

/// What the shell asked the pad to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Request {
    Start,
    Stop,
    /// Start an idle belt, otherwise stop.
    Toggle,
    /// Set the target speed in km/h.
    SetTarget(f64),
    /// Change the target speed by this many km/h.
    Nudge(f64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    #[error("the requested speed is not a number")]
    BadCommand,
    #[error("the pad is not connected")]
    NotConnected,
    #[error("the pad is busy")]
    Busy,
    #[error("the pad is in auto mode")]
    AutoMode,
    #[error("the pad is set to imperial units")]
    ImperialUnits,
    #[error("another app is controlling the pad")]
    InUse,
    #[error("the pad did not start the belt")]
    StartNotAcknowledged,
    #[error("the pad did not switch to manual mode")]
    ModeNotAcknowledged,
    #[error("the pad did not stop the belt")]
    StopNotAcknowledged,
}

impl Failure {
    /// The error code on the wire.
    pub fn code(self) -> &'static str {
        match self {
            Failure::BadCommand => "bad-command",
            Failure::NotConnected => "not-connected",
            Failure::Busy => "busy",
            Failure::AutoMode => "auto-mode",
            Failure::ImperialUnits => "imperial-units",
            Failure::InUse => "in-use",
            Failure::StartNotAcknowledged => "start-not-acknowledged",
            Failure::ModeNotAcknowledged => "mode-not-acknowledged",
            Failure::StopNotAcknowledged => "stop-not-acknowledged",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Write(Command),
    /// `request` could not be carried out.
    Fail {
        request: Request,
        failure: Failure,
    },
    /// Only targets the shell set are persisted; adopted ones live for the link.
    TargetChanged {
        kmh: f64,
        persist: bool,
    },
    /// No status for too long: the link must be torn down.
    LinkDead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    Starting,
    Stopping,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PadView {
    pub belt: Belt,
    pub mode: Mode,
    pub speed_kmh: f64,
    pub target_kmh: f64,
    pub min_kmh: f64,
    pub max_kmh: f64,
    pub busy: Option<Busy>,
    pub imperial: bool,
    /// Another local client is polling the pad over the same link.
    pub shared: bool,
}

/// The belt state machine for one link. Drop it when the link goes away:
/// nothing queued survives into the next link.
#[derive(Debug)]
pub struct Controller {
    target: u8,
    max: u8,
    imperial: bool,
    status: Option<(Instant, Status)>,
    heard: Instant,
    writable: Instant,
    poll: Instant,
    /// When to ask for the prefs, until the pad answers or the queries run out.
    prefs_at: Option<Instant>,
    prefs_queries: u8,
    /// A status arrived after our latest status request.
    answered: bool,
    /// Our latest status request was a poll.
    polled: bool,
    /// When `04 01` went out, until the belt is seen counting down or running.
    start_sent: Option<Instant>,
    idle_polls_before_start: u8,
    op: Option<Op>,
    remote: Option<(u8, Instant)>,
    peers: Peers,
    dead: bool,
}

#[derive(Debug)]
enum Op {
    Start { origin: Request, step: Step },
    Stop(Stopping),
    Speed(Attempt),
}

#[derive(Debug)]
enum Step {
    Manual(Attempt),
    Belt(Attempt),
    Countdown(Instant),
}

#[derive(Debug)]
struct Stopping {
    origin: Request,
    /// A speed 0 write waits for the pacer.
    queued: bool,
    /// When to report the stop as failed, pushed back while the belt keeps
    /// slowing down.
    deadline: Option<Instant>,
    /// The failure went out; speed 0 still does while the belt may move.
    reported: bool,
    last_speed: u8,
}

#[derive(Clone, Copy, Debug, Default)]
struct Attempt {
    /// When to judge the latest write; `None` while a write is queued.
    due: Option<Instant>,
    sends: u8,
    /// The pad speed when the latest write went out.
    from: u8,
}

impl Attempt {
    fn sent(&mut self, now: Instant, wait: Duration, speed: u8) {
        self.due = Some(now + wait);
        self.sends = self.sends.saturating_add(1);
        self.from = speed;
    }

    fn overdue(&self, now: Instant) -> bool {
        self.due.is_some_and(|due| now >= due)
    }

    fn again(self) -> Attempt {
        Attempt { due: None, ..self }
    }
}

impl Controller {
    /// A controller for a link that has just come up; it polls straight away.
    pub fn new(now: Instant, target_kmh: f64) -> Controller {
        Controller {
            target: tenths(target_kmh)
                .unwrap_or(MIN_DKMH)
                .clamp(MIN_DKMH, FALLBACK_MAX_DKMH),
            max: FALLBACK_MAX_DKMH,
            imperial: false,
            status: None,
            heard: now,
            writable: now,
            poll: now,
            prefs_at: None,
            prefs_queries: 0,
            answered: false,
            polled: false,
            start_sent: None,
            idle_polls_before_start: 0,
            op: None,
            remote: None,
            peers: Peers::default(),
            dead: false,
        }
    }

    pub fn on_status(&mut self, now: Instant, status: &Status) -> Vec<Action> {
        let mut actions = Vec::new();
        if self.dead {
            return actions;
        }
        if self.status.is_none() {
            self.prefs_at = Some(now);
        }
        let poll_reply = self.polled && !self.answered;
        self.answered = true;
        self.heard = now;
        self.status = Some((now, *status));
        self.peers.received(now);
        if matches!(status.belt, Belt::Starting | Belt::Running) {
            self.start_sent = None;
        }
        if self.idle_polls_before_start > 0 {
            if !at_rest(status) {
                self.idle_polls_before_start = IDLE_POLLS_AFTER_FAILED_START;
            } else if poll_reply {
                self.idle_polls_before_start -= 1;
            }
        }
        self.acknowledge(now, status, &mut actions);
        self.watch_remote(now, status, &mut actions);
        self.advance(now, &mut actions);
        actions
    }

    pub fn on_prefs(&mut self, prefs: &Prefs) -> Vec<Action> {
        let mut actions = Vec::new();
        self.prefs_at = None;
        self.imperial = prefs.imperial;
        if prefs.imperial {
            log::warn!(
                "the pad is set to imperial units; refusing to start it or change its speed"
            );
        }
        if prefs.max_dkmh < MIN_DKMH {
            log::warn!(
                "ignoring the pad's maximum speed of {}",
                kmh(prefs.max_dkmh)
            );
        } else {
            self.max = prefs.max_dkmh;
            self.set_target(self.target, false, &mut actions);
        }
        actions
    }

    /// Writes, polls, timeouts and the watchdog that are due at `now`.
    pub fn on_tick(&mut self, now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();
        self.advance(now, &mut actions);
        actions
    }

    pub fn request(&mut self, now: Instant, request: Request) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Err(failure) = self.begin(now, request, &mut actions) {
            self.fail(request, failure, &mut actions);
        }
        self.advance(now, &mut actions);
        actions
    }

    /// When [`Controller::on_tick`] has something to do next.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.dead {
            return None;
        }
        let write = if self.op_write().is_some() {
            self.writable
        } else {
            let poll = self.writable.max(self.poll);
            self.prefs_at
                .map_or(poll, |at| poll.min(self.writable.max(at)))
        };
        let op = match &self.op {
            Some(Op::Start {
                step: Step::Manual(attempt) | Step::Belt(attempt),
                ..
            })
            | Some(Op::Speed(attempt)) => attempt.due,
            Some(Op::Start {
                step: Step::Countdown(until),
                ..
            }) => Some(*until),
            Some(Op::Stop(stopping)) => stopping.deadline,
            None => None,
        };
        [
            Some(self.heard + WATCHDOG),
            Some(write),
            op,
            self.peers.until,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// The pad as last reported, or `None` before the first status.
    pub fn view(&self) -> Option<PadView> {
        let (_, status) = self.status?;
        Some(PadView {
            belt: status.belt,
            mode: status.mode,
            speed_kmh: kmh(status.speed_dkmh),
            target_kmh: kmh(self.target),
            min_kmh: kmh(MIN_DKMH),
            max_kmh: kmh(self.max),
            busy: match self.op {
                Some(Op::Start { .. }) => Some(Busy::Starting),
                Some(Op::Stop(_)) => Some(Busy::Stopping),
                _ => None,
            },
            imperial: self.imperial,
            shared: self.peers.until.is_some(),
        })
    }

    fn begin(
        &mut self,
        now: Instant,
        request: Request,
        actions: &mut Vec<Action>,
    ) -> Result<(), Failure> {
        let (at, status) = match self.status {
            Some(newest) if !self.dead => newest,
            _ => return Err(Failure::NotConnected),
        };
        match request {
            Request::Start => self.start(request, &status),
            Request::Stop => {
                self.stop(request, &status);
                Ok(())
            }
            Request::Toggle => {
                let fresh = now.saturating_duration_since(at) <= FRESH;
                if fresh && at_rest(&status) && self.op.is_none() {
                    self.start(request, &status)
                } else {
                    self.stop(request, &status);
                    Ok(())
                }
            }
            Request::SetTarget(kmh) => self.steer(&status, tenths(kmh)?, false, actions),
            Request::Nudge(delta) => {
                let dkmh = tenths(kmh(self.target) + delta)?;
                self.steer(&status, dkmh, delta > 0.0, actions)
            }
        }
    }

    fn start(&mut self, origin: Request, status: &Status) -> Result<(), Failure> {
        match (&self.op, status.belt) {
            (Some(Op::Start { .. }), _) => return Ok(()),
            (Some(Op::Stop(_)), _) => return Err(Failure::Busy),
            (_, Belt::Starting | Belt::Running) => return Ok(()),
            (_, Belt::Unknown(_)) => return Err(Failure::Busy),
            _ => {}
        }
        self.may_drive()?;
        if self.idle_polls_before_start > 0 {
            return Err(Failure::Busy);
        }
        let step = if status.mode == Mode::Manual {
            Step::Belt(Attempt::default())
        } else {
            Step::Manual(Attempt::default())
        };
        log::info!("starting the belt");
        self.op = Some(Op::Start { origin, step });
        Ok(())
    }

    fn stop(&mut self, origin: Request, status: &Status) {
        log::info!("stopping the belt");
        self.op = Some(Op::Stop(Stopping {
            origin,
            queued: true,
            deadline: None,
            reported: false,
            last_speed: status.speed_dkmh,
        }));
    }

    fn steer(
        &mut self,
        status: &Status,
        dkmh: u8,
        nudging_up: bool,
        actions: &mut Vec<Action>,
    ) -> Result<(), Failure> {
        self.may_drive()?;
        if status.mode == Mode::Auto {
            return Err(Failure::AutoMode);
        }
        let moving = matches!(status.belt, Belt::Starting | Belt::Running);
        let trailing = self.target.saturating_sub(status.speed_dkmh) >= NUDGE_LEAD_DKMH;
        if nudging_up && moving && trailing {
            return Err(Failure::Busy);
        }
        self.set_target(dkmh, true, actions);
        if matches!(self.op, None | Some(Op::Speed(_))) {
            self.op = self.apply_target(status);
        }
        Ok(())
    }

    fn may_drive(&self) -> Result<(), Failure> {
        if self.imperial {
            Err(Failure::ImperialUnits)
        } else if self.peers.until.is_some() {
            Err(Failure::InUse)
        } else {
            Ok(())
        }
    }

    /// Whether the pad is running in manual mode at a speed other than the target.
    fn off_target(&self, status: &Status) -> bool {
        status.belt == Belt::Running
            && status.mode == Mode::Manual
            && status.speed_dkmh != self.target
    }

    fn apply_target(&self, status: &Status) -> Option<Op> {
        self.off_target(status)
            .then(|| Op::Speed(Attempt::default()))
    }

    fn acknowledge(&mut self, now: Instant, status: &Status, actions: &mut Vec<Action>) {
        let Some(op) = self.op.take() else {
            return;
        };
        self.op = match op {
            Op::Start {
                origin,
                step: Step::Manual(_),
            } if status.mode == Mode::Manual => {
                self.belt_step(now, origin, Attempt::default(), status)
            }
            Op::Start {
                origin,
                step: Step::Belt(attempt),
            } => self.belt_step(now, origin, attempt, status),
            Op::Start {
                step: Step::Countdown(_),
                ..
            } if status.belt == Belt::Running => self.started(status),
            Op::Start {
                step: Step::Countdown(_),
                ..
            } if at_rest(status) => {
                log::warn!("the belt stopped during the countdown");
                None
            }
            Op::Stop(stopping) => self.stopping(now, stopping, status),
            Op::Speed(attempt) => self.speeding(attempt, status, actions),
            op => Some(op),
        };
    }

    /// Waits for `04 01` to take effect, or notices the belt already moving.
    fn belt_step(
        &self,
        now: Instant,
        origin: Request,
        attempt: Attempt,
        status: &Status,
    ) -> Option<Op> {
        let step = match status.belt {
            Belt::Running => return self.started(status),
            Belt::Starting => Step::Countdown(now + START_WAIT),
            _ => Step::Belt(attempt),
        };
        Some(Op::Start { origin, step })
    }

    fn started(&self, status: &Status) -> Option<Op> {
        log::info!("the belt is running");
        self.apply_target(status)
    }

    fn stopping(&self, now: Instant, mut stopping: Stopping, status: &Status) -> Option<Op> {
        let start_pending = self.start_sent.is_some_and(|sent| now < sent + START_WAIT);
        if at_rest(status) && !start_pending {
            log::info!("the belt stopped");
            return None;
        }
        if moving(status) {
            if status.speed_dkmh < stopping.last_speed
                && let Some(deadline) = &mut stopping.deadline
            {
                *deadline = (*deadline).max(now + STOP_PROGRESS);
            }
            stopping.queued = true;
        } else if stopping.reported && matches!(status.belt, Belt::Unknown(_)) {
            log::warn!("no longer stopping a belt in state {:?}", status.belt);
            return None;
        }
        stopping.last_speed = status.speed_dkmh;
        Some(Op::Stop(stopping))
    }

    fn speeding(
        &mut self,
        attempt: Attempt,
        status: &Status,
        actions: &mut Vec<Action>,
    ) -> Option<Op> {
        if !self.off_target(status) {
            return None;
        }
        let gap = status.speed_dkmh.abs_diff(self.target);
        let gap_when_sent = attempt.from.abs_diff(self.target);
        // A single 0.1 km/h step can still be the ramp of an earlier command.
        if attempt.due.is_some() && gap > gap_when_sent.saturating_add(1) {
            log::info!("the walker overrode the target speed");
            self.set_target(status.speed_dkmh, false, actions);
            return None;
        }
        Some(Op::Speed(attempt))
    }

    fn watch_remote(&mut self, now: Instant, status: &Status, actions: &mut Vec<Action>) {
        let speed = status.speed_dkmh;
        let drifted = self.op.is_none()
            && status.belt == Belt::Running
            && status.mode == Mode::Manual
            && speed > 0
            && speed.clamp(MIN_DKMH, self.max) != self.target;
        if !drifted {
            self.remote = None;
            return;
        }
        match self.remote {
            Some((seen, since)) if seen == speed => {
                if now.saturating_duration_since(since) >= REMOTE_SETTLE {
                    log::info!("adopting {} km/h set on the pad", kmh(speed));
                    self.remote = None;
                    self.set_target(speed, false, actions);
                }
            }
            _ => self.remote = Some((speed, now)),
        }
    }

    fn advance(&mut self, now: Instant, actions: &mut Vec<Action>) {
        if self.dead {
            return;
        }
        if now >= self.heard + WATCHDOG {
            log::warn!("no status from the pad for {} s", WATCHDOG.as_secs());
            self.dead = true;
            self.op = None;
            actions.push(Action::LinkDead);
            return;
        }
        self.peers.expire(now);
        self.expire(now, actions);
        self.pump(now, actions);
    }

    fn expire(&mut self, now: Instant, actions: &mut Vec<Action>) {
        let Some(op) = self.op.take() else {
            return;
        };
        let speed = self.speed();
        self.op = match op {
            Op::Start {
                origin,
                step: Step::Manual(attempt),
            } if attempt.overdue(now) => {
                if attempt.sends < MODE_SENDS {
                    Some(Op::Start {
                        origin,
                        step: Step::Manual(attempt.again()),
                    })
                } else {
                    self.fail(origin, Failure::ModeNotAcknowledged, actions);
                    None
                }
            }
            Op::Start {
                origin,
                step: Step::Belt(attempt),
            } if attempt.overdue(now) => {
                self.fail_start(origin, actions);
                None
            }
            Op::Start {
                step: Step::Countdown(until),
                ..
            } if now >= until => {
                log::warn!("the belt kept counting down; no longer waiting for it");
                None
            }
            Op::Stop(mut stopping) if stopping.deadline.is_some_and(|deadline| now >= deadline) => {
                self.fail(stopping.origin, Failure::StopNotAcknowledged, actions);
                stopping.deadline = None;
                stopping.reported = true;
                let settled = self.status.is_some_and(|(_, status)| {
                    matches!(status.belt, Belt::Unknown(_)) && status.speed_dkmh == 0
                });
                (!settled).then_some(Op::Stop(stopping))
            }
            Op::Speed(attempt) if attempt.overdue(now) => {
                if speed != attempt.from {
                    Some(Op::Speed(Attempt {
                        due: Some(now + SPEED_WAIT),
                        from: speed,
                        ..attempt
                    }))
                } else if attempt.sends < SPEED_SENDS {
                    Some(Op::Speed(attempt.again()))
                } else {
                    log::info!("the pad ignored the target speed; adopting its own");
                    self.set_target(speed, false, actions);
                    None
                }
            }
            op => Some(op),
        };
    }

    /// The operation's next write, and whether it may jump ahead of a due poll.
    fn op_write(&self) -> Option<(Command, bool)> {
        match self.op.as_ref()? {
            Op::Start {
                step: Step::Manual(attempt),
                ..
            } if attempt.due.is_none() => Some((Command::SwitchToManual, true)),
            Op::Start {
                step: Step::Belt(attempt),
                ..
            } if attempt.due.is_none() => Some((Command::StartBelt, true)),
            Op::Stop(stopping) if stopping.queued => Some((Command::ChangeSpeed(0), true)),
            Op::Speed(attempt) if attempt.due.is_none() => {
                Some((Command::ChangeSpeed(self.target), false))
            }
            _ => None,
        }
    }

    fn pump(&mut self, now: Instant, actions: &mut Vec<Action>) {
        if now < self.writable {
            return;
        }
        if let Some(Op::Start {
            origin,
            step: Step::Belt(attempt),
        }) = &self.op
            && attempt.due.is_none()
            && !self.clear_to_start(now)
        {
            let origin = *origin;
            self.op = None;
            self.fail_start(origin, actions);
        }
        let command = match self.op_write() {
            Some((command, true)) => command,
            _ if now >= self.poll => Command::AskStats,
            _ if self.prefs_at.is_some_and(|at| now >= at) => Command::QueryPrefs,
            Some((command, false)) => command,
            None => return,
        };
        self.sent(now, command);
        actions.push(Action::Write(command));
    }

    /// `04 01` toggles a moving belt on some firmware, so it goes out only
    /// while a status newer than our latest request shows the belt at rest.
    fn clear_to_start(&self, now: Instant) -> bool {
        self.answered
            && self.status.is_some_and(|(at, status)| {
                now.saturating_duration_since(at) <= FRESH && at_rest(&status)
            })
    }

    fn sent(&mut self, now: Instant, command: Command) {
        log::debug!("writing {command:?}");
        self.writable = now + SPACING;
        if command.requests_status() {
            self.answered = false;
            self.polled = command == Command::AskStats;
            self.peers.sent(now);
        }
        match command {
            Command::AskStats => self.poll = now + POLL,
            Command::QueryPrefs => {
                self.prefs_queries += 1;
                self.prefs_at = (self.prefs_queries < PREFS_QUERIES).then(|| now + PREFS_RETRY);
            }
            Command::StartBelt => self.start_sent = Some(now),
            _ => {}
        }
        let speed = self.speed();
        match (&mut self.op, command) {
            (
                Some(Op::Start {
                    step: Step::Manual(attempt),
                    ..
                }),
                Command::SwitchToManual,
            ) => attempt.sent(now, MODE_WAIT, speed),
            (
                Some(Op::Start {
                    step: Step::Belt(attempt),
                    ..
                }),
                Command::StartBelt,
            ) => attempt.sent(now, START_WAIT, speed),
            (Some(Op::Stop(stopping)), Command::ChangeSpeed(0)) => {
                stopping.queued = false;
                if !stopping.reported {
                    stopping.deadline.get_or_insert(now + STOP_WAIT);
                }
            }
            (Some(Op::Speed(attempt)), Command::ChangeSpeed(_)) => {
                attempt.sent(now, SPEED_WAIT, speed)
            }
            _ => {}
        }
    }

    fn fail(&self, request: Request, failure: Failure, actions: &mut Vec<Action>) {
        log::warn!("{request:?} failed: {failure}");
        actions.push(Action::Fail { request, failure });
    }

    fn fail_start(&mut self, origin: Request, actions: &mut Vec<Action>) {
        self.idle_polls_before_start = IDLE_POLLS_AFTER_FAILED_START;
        self.fail(origin, Failure::StartNotAcknowledged, actions);
    }

    fn set_target(&mut self, dkmh: u8, persist: bool, actions: &mut Vec<Action>) {
        let dkmh = dkmh.clamp(MIN_DKMH, self.max);
        if dkmh != self.target {
            self.target = dkmh;
            actions.push(Action::TargetChanged {
                kmh: kmh(dkmh),
                persist,
            });
        }
    }

    fn speed(&self) -> u8 {
        self.status.map_or(0, |(_, status)| status.speed_dkmh)
    }
}

/// Notices another local client polling the pad: BlueZ shares one link
/// between clients, so the replies to its polls reach us as well.
#[derive(Debug, Default)]
struct Peers {
    /// Our status requests that may still be answered.
    requests: VecDeque<Instant>,
    /// Statuses nobody here asked for.
    foreign: VecDeque<Instant>,
    streak: Option<Instant>,
    /// Until when the pad counts as shared.
    until: Option<Instant>,
}

impl Peers {
    fn sent(&mut self, now: Instant) {
        self.requests.push_back(now);
    }

    fn received(&mut self, now: Instant) {
        let recent =
            |window: Duration| move |at: &Instant| now.saturating_duration_since(*at) <= window;
        self.requests.retain(recent(REPLY_WINDOW));
        if self.requests.pop_front().is_some() {
            return;
        }
        self.foreign.retain(recent(FOREIGN_WINDOW));
        if self.foreign.is_empty() {
            self.streak = Some(now);
        }
        self.foreign.push_back(now);
        let sustained = self
            .streak
            .is_some_and(|since| now.saturating_duration_since(since) >= FOREIGN_WINDOW);
        if sustained && self.foreign.len() >= FOREIGN_STATUSES {
            if self.until.is_none() {
                log::warn!("another client is polling the pad");
            }
            self.until = Some(now + SHARED_HOLD);
        }
    }

    fn expire(&mut self, now: Instant) {
        if self.until.is_some_and(|until| now >= until) {
            log::info!("the pad is no longer shared");
            self.until = None;
        }
    }
}

fn at_rest(status: &Status) -> bool {
    matches!(status.belt, Belt::Idle | Belt::Standby)
}

/// An unknown state with a speed counts as moving; it is never assumed safe.
fn moving(status: &Status) -> bool {
    status.belt == Belt::Running
        || (matches!(status.belt, Belt::Unknown(_)) && status.speed_dkmh > 0)
}

fn tenths(kmh: f64) -> Result<u8, Failure> {
    if !kmh.is_finite() {
        return Err(Failure::BadCommand);
    }
    Ok((kmh * 10.0).round().clamp(0.0, f64::from(u8::MAX)) as u8)
}

fn kmh(dkmh: u8) -> f64 {
    f64::from(dkmh) / 10.0
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn status(belt: Belt, mode: Mode, speed_dkmh: u8) -> Status {
        Status {
            belt,
            speed_dkmh,
            mode,
            time_s: 0,
            dist_10m: 0,
            steps: 0,
        }
    }

    fn idle() -> Status {
        status(Belt::Idle, Mode::Manual, 0)
    }

    fn standby() -> Status {
        status(Belt::Standby, Mode::Standby, 0)
    }

    fn counting_down() -> Status {
        status(Belt::Starting, Mode::Manual, 0)
    }

    fn running(speed_dkmh: u8) -> Status {
        status(Belt::Running, Mode::Manual, speed_dkmh)
    }

    fn obey(pad: &mut Status, command: Command) {
        match command {
            Command::SwitchToManual => {
                pad.mode = Mode::Manual;
                if pad.belt == Belt::Standby {
                    pad.belt = Belt::Idle;
                }
            }
            Command::StartBelt if at_rest(pad) => pad.belt = Belt::Starting,
            Command::ChangeSpeed(0) if pad.belt == Belt::Running => {
                pad.belt = Belt::Idle;
                pad.speed_dkmh = 0;
            }
            Command::ChangeSpeed(speed) if pad.belt == Belt::Running => pad.speed_dkmh = speed,
            _ => {}
        }
    }

    struct Bench {
        controller: Controller,
        start: Instant,
        now: Instant,
        pad: Option<Status>,
        obeys: bool,
        prefs: Option<Prefs>,
        log: Vec<(Duration, Action)>,
    }

    impl Bench {
        fn obeying(pad: Status) -> Bench {
            Bench::with(Some(pad), true)
        }

        fn stubborn(pad: Status) -> Bench {
            Bench::with(Some(pad), false)
        }

        fn silent() -> Bench {
            Bench::with(None, false)
        }

        fn with(pad: Option<Status>, obeys: bool) -> Bench {
            let now = Instant::now();
            let mut bench = Bench {
                controller: Controller::new(now, 4.0),
                start: now,
                now,
                pad,
                obeys,
                prefs: Some(Prefs {
                    max_dkmh: 60,
                    imperial: false,
                }),
                log: Vec::new(),
            };
            let actions = bench.controller.on_tick(now);
            bench.apply(actions);
            bench
        }

        fn apply(&mut self, actions: Vec<Action>) {
            let mut queue = VecDeque::from(actions);
            while let Some(action) = queue.pop_front() {
                self.log.push((self.elapsed(), action));
                let Action::Write(command) = action else {
                    continue;
                };
                if command == Command::QueryPrefs {
                    if let Some(prefs) = self.prefs {
                        queue.extend(self.controller.on_prefs(&prefs));
                    }
                } else if let Some(pad) = &mut self.pad {
                    if self.obeys {
                        obey(pad, command);
                    }
                    let reply = *pad;
                    queue.extend(self.controller.on_status(self.now, &reply));
                }
            }
        }

        fn run(&mut self, span: Duration) {
            let end = self.now + span;
            for _ in 0..100_000 {
                match self.controller.next_deadline() {
                    Some(at) if at <= end => {
                        assert!(at > self.now, "a deadline at {at:?} makes no progress");
                        self.now = at;
                        let actions = self.controller.on_tick(at);
                        self.apply(actions);
                    }
                    _ => {
                        self.now = end;
                        return;
                    }
                }
            }
            panic!("the controller keeps scheduling work");
        }

        fn request(&mut self, request: Request) {
            let actions = self.controller.request(self.now, request);
            self.apply(actions);
        }

        fn inject(&mut self, status: Status) {
            let actions = self.controller.on_status(self.now, &status);
            self.apply(actions);
        }

        fn set_pad(&mut self, status: Status) {
            self.pad = Some(status);
        }

        fn elapsed(&self) -> Duration {
            self.now - self.start
        }

        fn writes(&self) -> Vec<(Duration, Command)> {
            self.log
                .iter()
                .filter_map(|(at, action)| match action {
                    Action::Write(command) => Some((*at, *command)),
                    _ => None,
                })
                .collect()
        }

        fn commands_since(&self, since: Duration) -> Vec<Command> {
            self.writes()
                .into_iter()
                .filter(|(at, command)| {
                    *at >= since && !matches!(command, Command::AskStats | Command::QueryPrefs)
                })
                .map(|(_, command)| command)
                .collect()
        }

        fn commands(&self) -> Vec<Command> {
            self.commands_since(Duration::ZERO)
        }

        fn polls(&self) -> Vec<Duration> {
            self.writes()
                .into_iter()
                .filter(|(_, command)| *command == Command::AskStats)
                .map(|(at, _)| at)
                .collect()
        }

        fn failures(&self) -> Vec<(Request, Failure)> {
            self.log
                .iter()
                .filter_map(|(_, action)| match action {
                    Action::Fail { request, failure } => Some((*request, *failure)),
                    _ => None,
                })
                .collect()
        }

        fn targets(&self) -> Vec<(f64, bool)> {
            self.log
                .iter()
                .filter_map(|(_, action)| match action {
                    Action::TargetChanged { kmh, persist } => Some((*kmh, *persist)),
                    _ => None,
                })
                .collect()
        }

        fn view(&self) -> PadView {
            self.controller.view().unwrap()
        }
    }

    fn assert_spaced(bench: &Bench) {
        let times: Vec<Duration> = bench.writes().into_iter().map(|(at, _)| at).collect();
        for pair in times.windows(2) {
            assert!(pair[1] - pair[0] >= SPACING, "writes at {pair:?}");
        }
    }

    #[test]
    fn polls_about_once_a_second_while_nothing_else_happens() {
        let mut bench = Bench::obeying(idle());
        bench.run(secs(10));
        let polls = bench.polls();
        assert_eq!(polls.len(), 10);
        assert_eq!(polls[0], Duration::ZERO);
        for pair in polls.windows(2) {
            assert!(pair[1] - pair[0] >= POLL, "polls at {pair:?}");
        }
        assert_eq!(bench.commands(), []);
    }

    fn prefs_queries(bench: &Bench) -> Vec<Duration> {
        bench
            .writes()
            .into_iter()
            .filter(|(_, command)| *command == Command::QueryPrefs)
            .map(|(at, _)| at)
            .collect()
    }

    #[test]
    fn queries_prefs_once_the_first_status_arrives() {
        let mut bench = Bench::obeying(idle());
        bench.run(secs(10));
        assert_eq!(prefs_queries(&bench), [SPACING]);
    }

    #[test]
    fn queries_prefs_again_until_the_pad_answers() {
        let mut bench = Bench::obeying(idle());
        bench.prefs = None;
        bench.run(secs(2));
        bench.prefs = Some(Prefs {
            max_dkmh: 60,
            imperial: true,
        });
        bench.run(secs(10));
        assert_eq!(prefs_queries(&bench).len(), 2);
        assert!(bench.view().imperial);
    }

    #[test]
    fn stops_querying_prefs_after_three_unanswered_queries() {
        let mut bench = Bench::obeying(idle());
        bench.prefs = None;
        bench.run(secs(30));
        let queries = prefs_queries(&bench);
        assert_eq!(queries.len(), 3);
        for pair in queries.windows(2) {
            assert!(pair[1] - pair[0] >= PREFS_RETRY, "queries at {pair:?}");
        }
        assert_eq!(bench.view().max_kmh, 6.0);
    }

    #[test]
    fn refuses_requests_until_the_first_status_arrives() {
        let mut bench = Bench::silent();
        bench.request(Request::Start);
        bench.request(Request::Stop);
        bench.request(Request::SetTarget(3.0));
        assert_eq!(
            bench.failures(),
            [
                (Request::Start, Failure::NotConnected),
                (Request::Stop, Failure::NotConnected),
                (Request::SetTarget(3.0), Failure::NotConnected),
            ]
        );
        assert_eq!(bench.controller.view(), None);
    }

    #[test]
    fn start_from_standby_switches_to_manual_and_applies_the_target_once_running() {
        let mut bench = Bench::obeying(standby());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(secs(3));
        assert_eq!(
            bench.commands(),
            [Command::SwitchToManual, Command::StartBelt]
        );
        assert_eq!(bench.view().busy, Some(Busy::Starting));
        let countdown_over = bench.elapsed();
        bench.set_pad(running(20));
        bench.run(secs(3));
        assert_eq!(
            bench.commands_since(countdown_over),
            [Command::ChangeSpeed(40)]
        );
        assert_eq!(bench.view().busy, None);
        assert_eq!(bench.view().speed_kmh, 4.0);
        assert_eq!(bench.failures(), []);
        assert_spaced(&bench);
    }

    #[test]
    fn start_in_auto_mode_switches_to_manual_first() {
        let mut bench = Bench::obeying(status(Belt::Idle, Mode::Auto, 0));
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(secs(3));
        assert_eq!(
            bench.commands(),
            [Command::SwitchToManual, Command::StartBelt]
        );
        assert_eq!(bench.view().mode, Mode::Manual);
    }

    #[test]
    fn start_in_manual_mode_goes_straight_to_start_belt() {
        let mut bench = Bench::obeying(idle());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(secs(2));
        assert_eq!(bench.commands(), [Command::StartBelt]);
        assert_eq!(bench.view().belt, Belt::Starting);
    }

    #[test]
    fn start_fails_when_manual_mode_is_not_acknowledged_after_one_resend() {
        let mut bench = Bench::stubborn(standby());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(secs(10));
        assert_eq!(
            bench.commands(),
            [Command::SwitchToManual, Command::SwitchToManual]
        );
        assert_eq!(
            bench.failures(),
            [(Request::Start, Failure::ModeNotAcknowledged)]
        );
        assert_eq!(bench.view().busy, None);
    }

    #[test]
    fn start_not_acknowledged_is_reported_once_and_never_resent() {
        let mut bench = Bench::stubborn(idle());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(secs(5));
        assert_eq!(bench.failures(), []);
        bench.run(secs(15));
        assert_eq!(bench.commands(), [Command::StartBelt]);
        assert_eq!(
            bench.failures(),
            [(Request::Start, Failure::StartNotAcknowledged)]
        );
        assert_eq!(bench.view().busy, None);
    }

    #[test]
    fn start_belt_is_withheld_without_a_reply_to_the_latest_request() {
        let mut bench = Bench::obeying(idle());
        bench.run(ms(1500));
        bench.pad = None;
        bench.run(ms(1000));
        bench.request(Request::Start);
        bench.run(secs(2));
        assert_eq!(bench.commands(), []);
        assert_eq!(
            bench.failures(),
            [(Request::Start, Failure::StartNotAcknowledged)]
        );
    }

    #[test]
    fn start_belt_is_withheld_when_the_belt_moves_before_its_turn() {
        let mut bench = Bench::stubborn(standby());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(ms(100));
        assert_eq!(bench.commands(), [Command::SwitchToManual]);
        bench.set_pad(running(20));
        bench.obeys = true;
        bench.run(secs(4));
        assert!(!bench.commands().contains(&Command::StartBelt));
        assert_eq!(bench.failures(), []);
        assert_eq!(bench.view().busy, None);
        assert_eq!(bench.view().speed_kmh, 4.0);
    }

    #[test]
    fn start_refuses_a_belt_in_an_unknown_state() {
        let mut bench = Bench::obeying(status(Belt::Unknown(2), Mode::Manual, 0));
        bench.request(Request::Start);
        bench.run(secs(3));
        assert_eq!(bench.commands(), []);
        assert_eq!(bench.failures(), [(Request::Start, Failure::Busy)]);
    }

    #[test]
    fn start_belt_is_not_sent_when_the_belt_state_turns_unknown() {
        let mut bench = Bench::stubborn(standby());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(ms(100));
        bench.set_pad(status(Belt::Unknown(3), Mode::Manual, 0));
        bench.run(secs(3));
        assert_eq!(bench.commands(), [Command::SwitchToManual]);
        assert_eq!(
            bench.failures(),
            [(Request::Start, Failure::StartNotAcknowledged)]
        );
    }

    #[test]
    fn start_is_refused_after_a_failed_start_until_two_idle_polls() {
        let mut bench = Bench::stubborn(idle());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(secs(7));
        bench.request(Request::Start);
        assert_eq!(
            bench.failures(),
            [
                (Request::Start, Failure::StartNotAcknowledged),
                (Request::Start, Failure::Busy)
            ]
        );
        bench.run(ms(2500));
        bench.request(Request::Start);
        bench.run(secs(1));
        assert_eq!(bench.commands(), [Command::StartBelt, Command::StartBelt]);
        assert_eq!(bench.failures().len(), 2);
    }

    #[test]
    fn a_moving_belt_restarts_the_idle_poll_count_after_a_failed_start() {
        let mut bench = Bench::stubborn(idle());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(ms(6200));
        assert_eq!(bench.failures().len(), 1);
        bench.set_pad(running(20));
        bench.run(ms(1100));
        bench.set_pad(idle());
        bench.run(ms(500));
        bench.request(Request::Start);
        assert_eq!(
            bench.failures().last(),
            Some(&(Request::Start, Failure::Busy))
        );
    }

    #[test]
    fn start_is_a_no_op_while_starting_or_running() {
        let mut bench = Bench::obeying(running(40));
        bench.request(Request::Start);
        bench.set_pad(counting_down());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(secs(2));
        assert_eq!(bench.commands(), []);
        assert_eq!(bench.failures(), []);
    }

    #[test]
    fn start_is_refused_while_stopping() {
        let mut bench = Bench::stubborn(running(40));
        bench.request(Request::Stop);
        bench.request(Request::Start);
        assert_eq!(bench.failures(), [(Request::Start, Failure::Busy)]);
        assert_eq!(bench.view().busy, Some(Busy::Stopping));
    }

    #[test]
    fn stop_sends_speed_zero_and_finishes_when_the_belt_idles() {
        let mut bench = Bench::obeying(running(40));
        bench.run(secs(2));
        bench.request(Request::Stop);
        assert_eq!(bench.view().busy, Some(Busy::Stopping));
        bench.run(secs(2));
        assert_eq!(bench.commands(), [Command::ChangeSpeed(0)]);
        assert_eq!(bench.view().belt, Belt::Idle);
        assert_eq!(bench.view().busy, None);
        assert_eq!(bench.failures(), []);
    }

    #[test]
    fn stop_reports_a_belt_that_keeps_running_once_and_keeps_sending_speed_zero() {
        let mut bench = Bench::stubborn(running(40));
        bench.run(secs(2));
        bench.request(Request::Stop);
        bench.run(secs(10));
        assert_eq!(
            bench.failures(),
            [(Request::Stop, Failure::StopNotAcknowledged)]
        );
        assert_eq!(bench.view().busy, Some(Busy::Stopping));
        let reported = bench.elapsed();
        bench.run(secs(5));
        let after = bench.commands_since(reported);
        assert!(after.len() >= 5, "{after:?}");
        assert!(after.iter().all(|c| *c == Command::ChangeSpeed(0)));
        assert_eq!(bench.failures().len(), 1);
        bench.set_pad(idle());
        bench.run(secs(2));
        assert_eq!(bench.view().busy, None);
        assert_spaced(&bench);
    }

    #[test]
    fn a_countdown_that_outlasts_the_stop_deadline_still_gets_speed_zero() {
        let mut bench = Bench::obeying(idle());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(secs(1));
        assert_eq!(bench.view().belt, Belt::Starting);
        let stopped_at = bench.elapsed();
        bench.request(Request::Stop);
        bench.run(secs(8));
        assert_eq!(
            bench.failures(),
            [(Request::Stop, Failure::StopNotAcknowledged)]
        );
        assert_eq!(bench.view().busy, Some(Busy::Stopping));
        let running_at = bench.elapsed();
        bench.set_pad(running(20));
        bench.run(secs(2));
        assert_eq!(bench.commands_since(running_at), [Command::ChangeSpeed(0)]);
        assert!(
            bench
                .commands_since(stopped_at)
                .iter()
                .all(|c| *c == Command::ChangeSpeed(0))
        );
        assert_eq!(bench.view().belt, Belt::Idle);
        assert_eq!(bench.view().busy, None);
        assert_eq!(bench.failures().len(), 1);
    }

    #[test]
    fn stop_ends_after_reporting_a_belt_that_rests_in_an_unknown_state() {
        let mut bench = Bench::stubborn(status(Belt::Unknown(2), Mode::Manual, 0));
        bench.request(Request::Stop);
        bench.run(secs(8));
        assert_eq!(
            bench.failures(),
            [(Request::Stop, Failure::StopNotAcknowledged)]
        );
        assert_eq!(bench.view().busy, None);
    }

    #[test]
    fn stop_repeats_speed_zero_for_an_unknown_state_that_moves() {
        let mut bench = Bench::stubborn(status(Belt::Unknown(3), Mode::Manual, 30));
        bench.request(Request::Stop);
        bench.run(secs(3));
        assert!(bench.commands().len() >= 3, "{:?}", bench.commands());
    }

    #[test]
    fn stop_keeps_waiting_while_the_belt_slows_down() {
        let mut bench = Bench::stubborn(running(60));
        bench.run(secs(2));
        bench.request(Request::Stop);
        for speed in [53, 46, 39, 32, 25, 18, 11, 4] {
            bench.run(ms(800));
            bench.set_pad(running(speed));
        }
        bench.run(ms(800));
        bench.set_pad(idle());
        bench.run(secs(2));
        assert_eq!(bench.failures(), []);
        assert_eq!(bench.view().busy, None);
    }

    #[test]
    fn stop_cancels_a_queued_start_before_start_belt_goes_out() {
        let mut bench = Bench::obeying(idle());
        bench.run(ms(1200));
        bench.request(Request::Start);
        bench.request(Request::Stop);
        bench.run(secs(10));
        assert_eq!(bench.commands(), [Command::ChangeSpeed(0)]);
        assert_eq!(bench.failures(), []);
    }

    #[test]
    fn stop_during_countdown_ends_idle_and_no_write_with_speed_above_zero_follows() {
        let mut bench = Bench::obeying(idle());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(secs(1));
        assert_eq!(bench.view().belt, Belt::Starting);
        let stopped_at = bench.elapsed();
        bench.request(Request::Stop);
        bench.request(Request::SetTarget(5.0));
        bench.run(secs(2));
        assert_eq!(bench.view().busy, Some(Busy::Stopping));
        bench.set_pad(running(20));
        bench.run(secs(5));
        let after_stop = bench.commands_since(stopped_at);
        assert!(!after_stop.is_empty());
        assert!(
            after_stop.iter().all(|c| *c == Command::ChangeSpeed(0)),
            "{after_stop:?}"
        );
        assert_eq!(bench.view().belt, Belt::Idle);
        assert_eq!(bench.view().busy, None);
        assert_eq!(bench.failures(), []);
    }

    #[test]
    fn stop_outlasts_a_start_belt_the_pad_has_not_acted_on_yet() {
        let mut bench = Bench::stubborn(idle());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(secs(1));
        assert_eq!(bench.commands(), [Command::StartBelt]);
        bench.request(Request::Stop);
        bench.run(secs(2));
        assert_eq!(bench.view().busy, Some(Busy::Stopping));
        bench.set_pad(counting_down());
        bench.run(secs(1));
        bench.set_pad(running(20));
        bench.obeys = true;
        bench.run(secs(2));
        assert_eq!(bench.view().belt, Belt::Idle);
        assert_eq!(bench.view().busy, None);
        assert_eq!(bench.failures(), []);
    }

    #[test]
    fn stop_ends_once_an_unacted_start_belt_is_given_up() {
        let mut bench = Bench::stubborn(idle());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(secs(1));
        bench.request(Request::Stop);
        bench.run(secs(7));
        assert_eq!(bench.view().busy, None);
        assert_eq!(bench.failures(), []);
    }

    #[test]
    fn speed_is_resent_while_the_pad_ignores_it_then_the_target_follows_the_pad() {
        let mut bench = Bench::stubborn(running(20));
        bench.request(Request::SetTarget(4.0));
        bench.run(secs(20));
        assert_eq!(
            bench.commands(),
            [
                Command::ChangeSpeed(40),
                Command::ChangeSpeed(40),
                Command::ChangeSpeed(40)
            ]
        );
        assert_eq!(bench.targets(), [(2.0, false)]);
        assert_eq!(bench.view().target_kmh, 2.0);
    }

    #[test]
    fn speed_is_not_resent_while_the_belt_ramps_towards_the_target() {
        let mut bench = Bench::stubborn(running(20));
        bench.request(Request::SetTarget(6.0));
        for speed in [24, 28, 32, 36, 40, 44, 48, 52, 56, 60] {
            bench.run(secs(1));
            bench.set_pad(running(speed));
        }
        bench.run(secs(10));
        assert_eq!(bench.commands(), [Command::ChangeSpeed(60)]);
        assert_eq!(bench.targets(), [(6.0, true)]);
    }

    #[test]
    fn a_speed_change_away_from_the_target_cancels_it_and_follows_the_walker() {
        let mut bench = Bench::stubborn(running(30));
        bench.request(Request::SetTarget(4.0));
        bench.run(secs(3));
        assert_eq!(bench.commands(), [Command::ChangeSpeed(40)]);
        bench.set_pad(running(25));
        bench.run(secs(10));
        assert_eq!(bench.commands(), [Command::ChangeSpeed(40)]);
        assert_eq!(bench.targets(), [(2.5, false)]);
    }

    #[test]
    fn adopts_a_speed_set_on_the_remote_once_it_settles_for_3_s_without_persisting_it() {
        let mut bench = Bench::stubborn(running(40));
        bench.run(secs(2));
        bench.set_pad(running(35));
        bench.run(ms(2500));
        assert_eq!(bench.targets(), []);
        bench.run(secs(2));
        assert_eq!(bench.targets(), [(3.5, false)]);
        assert_eq!(bench.commands(), []);
    }

    #[test]
    fn never_adopts_a_remote_speed_outside_manual_mode() {
        let mut bench = Bench::stubborn(status(Belt::Running, Mode::Auto, 35));
        bench.run(secs(10));
        assert_eq!(bench.targets(), []);
    }

    #[test]
    fn speed_requests_persist_the_target_and_apply_it_while_running() {
        let mut bench = Bench::obeying(running(40));
        bench.run(secs(2));
        bench.request(Request::SetTarget(5.5));
        bench.run(secs(2));
        assert_eq!(bench.targets(), [(5.5, true)]);
        assert_eq!(bench.commands(), [Command::ChangeSpeed(55)]);
        assert_eq!(bench.view().speed_kmh, 5.5);
    }

    #[test]
    fn speed_requests_only_set_the_target_while_the_belt_rests() {
        let mut bench = Bench::obeying(idle());
        bench.request(Request::SetTarget(3.0));
        bench.request(Request::Nudge(0.5));
        bench.run(secs(3));
        assert_eq!(bench.targets(), [(3.0, true), (3.5, true)]);
        assert_eq!(bench.commands(), []);
    }

    #[test]
    fn speed_requests_are_rejected_in_auto_mode() {
        let mut bench = Bench::obeying(status(Belt::Running, Mode::Auto, 30));
        bench.request(Request::SetTarget(3.0));
        bench.request(Request::Nudge(0.5));
        assert_eq!(
            bench.failures(),
            [
                (Request::SetTarget(3.0), Failure::AutoMode),
                (Request::Nudge(0.5), Failure::AutoMode)
            ]
        );
        assert_eq!(bench.targets(), []);
    }

    #[test]
    fn targets_are_rounded_and_clamped_to_the_pad_range() {
        let mut bench = Bench::obeying(idle());
        bench.prefs = Some(Prefs {
            max_dkmh: 80,
            imperial: false,
        });
        bench.request(Request::SetTarget(9.0));
        assert_eq!(bench.view().target_kmh, 6.0);
        bench.request(Request::SetTarget(0.0));
        assert_eq!(bench.view().target_kmh, 0.5);
        bench.request(Request::SetTarget(-3.0));
        assert_eq!(bench.view().target_kmh, 0.5);
        bench.request(Request::SetTarget(3.04));
        assert_eq!(bench.view().target_kmh, 3.0);
        bench.run(secs(1));
        assert_eq!(bench.view().max_kmh, 8.0);
        bench.request(Request::SetTarget(9.0));
        assert_eq!(bench.view().target_kmh, 8.0);
    }

    #[test]
    fn non_finite_speeds_are_bad_commands() {
        let mut bench = Bench::obeying(idle());
        bench.request(Request::SetTarget(f64::NAN));
        bench.request(Request::SetTarget(f64::INFINITY));
        bench.request(Request::Nudge(f64::NAN));
        bench.request(Request::Nudge(f64::NEG_INFINITY));
        assert!(
            bench
                .failures()
                .iter()
                .all(|(_, failure)| *failure == Failure::BadCommand)
        );
        assert_eq!(bench.failures().len(), 4);
        assert_eq!(bench.view().target_kmh, 4.0);
    }

    #[test]
    fn nudging_up_is_refused_while_the_belt_trails_the_target() {
        let mut bench = Bench::stubborn(running(20));
        bench.request(Request::SetTarget(4.0));
        bench.request(Request::Nudge(0.5));
        bench.request(Request::Nudge(-0.5));
        assert_eq!(bench.failures(), [(Request::Nudge(0.5), Failure::Busy)]);
        assert_eq!(bench.view().target_kmh, 3.5);
        bench.set_pad(running(34));
        bench.run(secs(2));
        bench.request(Request::Nudge(0.5));
        assert_eq!(bench.view().target_kmh, 4.0);
        assert_eq!(bench.failures().len(), 1);
    }

    #[test]
    fn nudging_up_is_refused_during_the_countdown() {
        let mut bench = Bench::stubborn(counting_down());
        bench.request(Request::Nudge(0.5));
        assert_eq!(bench.failures(), [(Request::Nudge(0.5), Failure::Busy)]);
    }

    #[test]
    fn pending_speed_writes_coalesce_to_the_latest_target() {
        let mut bench = Bench::stubborn(running(20));
        bench.run(ms(100));
        bench.request(Request::SetTarget(3.0));
        bench.request(Request::SetTarget(3.5));
        bench.request(Request::SetTarget(4.5));
        bench.run(secs(3));
        assert_eq!(bench.commands(), [Command::ChangeSpeed(45)]);
    }

    #[test]
    fn speed_writes_never_starve_polls() {
        let mut bench = Bench::stubborn(running(20));
        for i in 0..50 {
            let kmh = if i % 2 == 0 { 3.0 } else { 3.5 };
            bench.request(Request::SetTarget(kmh));
            bench.run(ms(200));
        }
        let polls = bench.polls();
        assert!(polls.len() >= 7, "{polls:?}");
        for pair in polls.windows(2) {
            assert!(pair[1] - pair[0] <= SPACING * 2, "polls at {pair:?}");
        }
        assert!(bench.commands().len() >= 6);
        assert_spaced(&bench);
    }

    #[test]
    fn a_whole_walk_keeps_every_write_690_ms_apart() {
        let mut bench = Bench::obeying(standby());
        bench.run(secs(1));
        bench.request(Request::Start);
        bench.run(secs(2));
        bench.set_pad(running(20));
        bench.run(secs(1));
        for _ in 0..4 {
            bench.request(Request::Nudge(-0.5));
            bench.run(ms(300));
        }
        bench.request(Request::Stop);
        bench.run(secs(3));
        assert_eq!(bench.view().belt, Belt::Idle);
        assert_eq!(bench.failures(), []);
        assert_spaced(&bench);
    }

    #[test]
    fn toggle_starts_a_resting_belt_on_a_fresh_status() {
        let mut bench = Bench::obeying(idle());
        bench.run(secs(2));
        bench.request(Request::Toggle);
        bench.run(secs(1));
        assert_eq!(bench.commands(), [Command::StartBelt]);
    }

    #[test]
    fn toggle_stops_a_running_belt() {
        let mut bench = Bench::obeying(running(40));
        bench.run(secs(2));
        bench.request(Request::Toggle);
        bench.run(secs(1));
        assert_eq!(bench.commands(), [Command::ChangeSpeed(0)]);
        assert_eq!(bench.view().belt, Belt::Idle);
    }

    #[test]
    fn toggle_stops_a_belt_that_is_starting() {
        let mut bench = Bench::obeying(idle());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.request(Request::Toggle);
        bench.run(secs(3));
        assert_eq!(bench.commands(), [Command::ChangeSpeed(0)]);
    }

    #[test]
    fn toggle_stops_instead_of_starting_on_a_stale_status() {
        let mut bench = Bench::obeying(idle());
        bench.run(ms(1500));
        bench.pad = None;
        bench.run(secs(3));
        bench.request(Request::Toggle);
        bench.run(secs(1));
        assert_eq!(bench.commands(), [Command::ChangeSpeed(0)]);
    }

    #[test]
    fn toggle_never_starts_a_belt_in_an_unknown_state() {
        let mut bench = Bench::stubborn(status(Belt::Unknown(2), Mode::Manual, 0));
        bench.run(secs(1));
        bench.request(Request::Toggle);
        bench.run(secs(1));
        assert_eq!(bench.commands(), [Command::ChangeSpeed(0)]);
    }

    #[test]
    fn toggle_reports_failures_as_the_toggle() {
        let mut bench = Bench::silent();
        bench.request(Request::Toggle);
        assert_eq!(bench.failures(), [(Request::Toggle, Failure::NotConnected)]);
    }

    #[test]
    fn an_imperial_pad_refuses_start_and_speed_but_still_stops() {
        let mut bench = Bench::obeying(running(30));
        bench.prefs = Some(Prefs {
            max_dkmh: 60,
            imperial: true,
        });
        bench.run(secs(1));
        assert!(bench.view().imperial);
        bench.request(Request::SetTarget(3.0));
        bench.request(Request::Stop);
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.request(Request::Toggle);
        assert_eq!(
            bench.failures(),
            [
                (Request::SetTarget(3.0), Failure::ImperialUnits),
                (Request::Start, Failure::ImperialUnits),
                (Request::Toggle, Failure::ImperialUnits)
            ]
        );
        assert_eq!(bench.commands(), [Command::ChangeSpeed(0)]);
    }

    #[test]
    fn a_second_client_polling_the_pad_marks_it_shared_until_it_goes_quiet() {
        let mut bench = Bench::obeying(idle());
        bench.run(ms(500));
        for _ in 0..6 {
            bench.inject(idle());
            bench.run(secs(1));
        }
        assert!(bench.view().shared);
        bench.request(Request::Start);
        bench.request(Request::SetTarget(3.0));
        assert_eq!(
            bench.failures(),
            [
                (Request::Start, Failure::InUse),
                (Request::SetTarget(3.0), Failure::InUse)
            ]
        );
        bench.set_pad(running(30));
        bench.request(Request::Stop);
        bench.run(secs(1));
        assert_eq!(bench.commands(), [Command::ChangeSpeed(0)]);
        bench.run(secs(10));
        assert!(!bench.view().shared);
        bench.request(Request::Start);
        bench.run(secs(1));
        assert_eq!(bench.commands().last(), Some(&Command::StartBelt));
    }

    #[test]
    fn a_few_unrequested_statuses_do_not_mark_the_pad_shared() {
        let mut bench = Bench::obeying(idle());
        bench.run(ms(500));
        for _ in 0..3 {
            bench.inject(idle());
            bench.run(ms(300));
        }
        bench.run(secs(10));
        assert!(!bench.view().shared);
    }

    #[test]
    fn the_watchdog_declares_the_link_dead_after_10_s_without_a_status() {
        let mut bench = Bench::obeying(idle());
        bench.run(ms(500));
        bench.pad = None;
        bench.run(secs(9));
        assert!(!bench.log.iter().any(|(_, a)| *a == Action::LinkDead));
        bench.run(secs(5));
        let dead: Vec<Duration> = bench
            .log
            .iter()
            .filter(|(_, action)| *action == Action::LinkDead)
            .map(|(at, _)| *at)
            .collect();
        assert_eq!(dead, [secs(10)]);
        assert_eq!(bench.controller.next_deadline(), None);
        bench.request(Request::Stop);
        assert_eq!(bench.failures(), [(Request::Stop, Failure::NotConnected)]);
    }

    #[test]
    fn the_watchdog_also_covers_a_link_that_never_reports() {
        let mut bench = Bench::silent();
        bench.run(secs(12));
        assert!(bench.log.contains(&(secs(10), Action::LinkDead)));
    }

    #[test]
    fn a_lower_maximum_from_the_pad_clamps_the_target_in_memory() {
        let mut bench = Bench::obeying(idle());
        bench.prefs = Some(Prefs {
            max_dkmh: 35,
            imperial: false,
        });
        bench.run(secs(1));
        assert_eq!(bench.view().max_kmh, 3.5);
        assert_eq!(bench.targets(), [(3.5, false)]);
    }

    #[test]
    fn ignores_a_maximum_speed_below_the_minimum() {
        let mut bench = Bench::obeying(idle());
        bench.prefs = Some(Prefs {
            max_dkmh: 3,
            imperial: false,
        });
        bench.run(secs(1));
        assert_eq!(bench.view().max_kmh, 6.0);
        assert_eq!(bench.view().min_kmh, 0.5);
    }

    #[test]
    fn a_countdown_the_remote_cancels_clears_busy() {
        let mut bench = Bench::obeying(idle());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(secs(1));
        bench.set_pad(idle());
        bench.run(secs(2));
        assert_eq!(bench.view().busy, None);
        assert_eq!(bench.failures(), []);
    }

    #[test]
    fn a_countdown_that_never_ends_stops_blocking_after_6_s() {
        let mut bench = Bench::obeying(idle());
        bench.run(secs(2));
        bench.request(Request::Start);
        bench.run(secs(5));
        assert_eq!(bench.view().busy, Some(Busy::Starting));
        bench.run(secs(3));
        assert_eq!(bench.view().busy, None);
    }

    #[test]
    fn failure_codes_match_the_wire_contract() {
        let cases = [
            (Failure::BadCommand, "bad-command"),
            (Failure::NotConnected, "not-connected"),
            (Failure::Busy, "busy"),
            (Failure::AutoMode, "auto-mode"),
            (Failure::ImperialUnits, "imperial-units"),
            (Failure::InUse, "in-use"),
            (Failure::StartNotAcknowledged, "start-not-acknowledged"),
            (Failure::ModeNotAcknowledged, "mode-not-acknowledged"),
            (Failure::StopNotAcknowledged, "stop-not-acknowledged"),
        ];
        for (failure, code) in cases {
            assert_eq!(failure.code(), code);
        }
    }
}
