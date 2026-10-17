//! Turns pad counters into recorded activity.

use crate::calories;
use crate::store::{Activity, Baseline, Counters, Session, Store};
use anyhow::Result;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use std::cmp::Reverse;
use std::ops::Range;

pub const DEFAULT_WEIGHT_KG: f64 = 70.0;
const MIN_WEIGHT_KG: f64 = 20.0;
const MAX_WEIGHT_KG: f64 = 300.0;
/// Deltas of more pad seconds than this are catch-ups, spread over the
/// minutes they cover.
const LIVE_S: u32 = 5;
const IDLE_CLOSE: SignedDuration = SignedDuration::from_secs(10);
const MERGE_WITHIN: SignedDuration = SignedDuration::from_secs(120);
/// Bounds on a plausible advance of the counters, against frames that are
/// corrupt yet pass the 8-bit checksum.
const MAX_KMH: u64 = 8;
const MAX_STEPS_PER_S: u64 = 4;
const CLOCK_SLACK_S: i64 = 5;

/// The pad state carried by one status frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub time_s: u32,
    pub dist_10m: u32,
    pub steps: u32,
    /// Belt speed in 0.1 km/h.
    pub speed_dkmh: u8,
    pub running: bool,
}

impl Sample {
    fn counters(&self) -> Counters {
        Counters {
            time_s: self.time_s,
            dist_10m: self.dist_10m,
            steps: self.steps,
        }
    }

    fn speed_kmh(&self) -> f64 {
        f64::from(self.speed_dkmh) / 10.0
    }
}

/// Totals for the local calendar day.
#[derive(Clone, Debug, PartialEq)]
pub struct Today {
    pub date: Date,
    pub activity: Activity,
    /// The sessions that started on `date`.
    pub sessions: u32,
}

/// Records pad activity as deltas of the pad's cumulative counters, writing
/// each frame through to the store before reporting it.
pub struct Tracker {
    store: Store,
    tz: TimeZone,
    weight_kg: f64,
    running: bool,
    session: Option<Session>,
    last: Option<Session>,
    today: Today,
    day: Range<Timestamp>,
}

enum Origin {
    Open,
    Reopened,
    New,
}

impl Tracker {
    /// Closes the sessions a crash left open and loads today's totals.
    pub fn open(store: Store, tz: TimeZone, now: Timestamp) -> Result<Tracker> {
        let closed = store.close_open_sessions()?;
        if closed > 0 {
            log::info!("closed {closed} session(s) left open");
        }
        let weight_kg = store.weight_kg()?.map_or(DEFAULT_WEIGHT_KG, clamp_weight);
        let last = store.latest_closed_session()?;
        let (today, day) = load_today(&store, &tz, now)?;
        Ok(Tracker {
            store,
            tz,
            weight_kg,
            running: false,
            session: None,
            last,
            today,
            day,
        })
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn weight_kg(&self) -> f64 {
        self.weight_kg
    }

    /// Persists the body mass, clamped to 20–300 kg, that energy is
    /// estimated with from now on.
    pub fn set_weight_kg(&mut self, kg: f64) -> Result<()> {
        let kg = clamp_weight(kg);
        self.store.set_weight_kg(kg)?;
        self.weight_kg = kg;
        Ok(())
    }

    /// The open session.
    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    /// The most recently closed session, if it ended today.
    pub fn last_session(&self) -> Option<&Session> {
        self.last
            .as_ref()
            .filter(|session| session.ended_at.is_some_and(|at| self.day.contains(&at)))
    }

    pub fn today(&self) -> &Today {
        &self.today
    }

    /// Records what the pad at `device` did since its previous frame, in
    /// one transaction.
    pub fn on_sample(&mut self, device: &str, sample: Sample, now: Timestamp) -> Result<()> {
        let now = Timestamp::from_second(now.as_second())?;
        self.roll_over(now)?;
        self.running = sample.running;
        let reading = sample.counters();
        let delta = credit(device, self.store.baseline(device)?, reading, now);
        let kcal = energy(delta, sample.speed_kmh(), self.weight_kg);
        let shares = shares(delta, kcal, now)?;
        let mut next = self.advance(delta, &shares, sample, now)?;

        let tx = self.store.transaction()?;
        self.store.set_baseline(device, reading, now)?;
        for (at, share) in &shares {
            self.store.add_minute(*at, share)?;
        }
        let mut previous = None;
        if let Some((session, origin)) = &mut next {
            match origin {
                Origin::New => session.id = self.store.insert_session(session, device)?,
                Origin::Reopened => {
                    self.store.update_session(session)?;
                    previous = Some(self.store.latest_closed_session()?);
                }
                Origin::Open if self.session.as_ref() != Some(&*session) => {
                    self.store.update_session(session)?;
                }
                Origin::Open => {}
            }
        }
        tx.commit()?;

        if let Some(previous) = previous {
            self.last = previous;
        }
        if let Some((session, origin)) = next {
            if matches!(origin, Origin::New) && self.day.contains(&session.started_at) {
                self.today.sessions += 1;
            }
            self.session = Some(session);
        }
        self.today.activity += shares
            .into_iter()
            .filter(|(at, _)| self.day.contains(at))
            .map(|(_, share)| share)
            .sum();
        Ok(())
    }

    /// Closes the session once the belt has stood for a while, and moves
    /// `today` to a new local date.
    pub fn on_tick(&mut self, now: Timestamp) -> Result<()> {
        self.roll_over(now)?;
        let idle = self
            .session
            .as_ref()
            .is_some_and(|session| now.duration_since(session.updated_at) >= IDLE_CLOSE);
        if idle && !self.running {
            self.close()?;
        }
        Ok(())
    }

    pub fn on_link_lost(&mut self) -> Result<()> {
        self.running = false;
        self.close()
    }

    pub fn shutdown(&mut self) -> Result<()> {
        self.close()
    }

    /// The session as it stands after a frame that moved the counters by
    /// `delta`, and where it came from.
    fn advance(
        &self,
        delta: Counters,
        shares: &[(Timestamp, Activity)],
        sample: Sample,
        now: Timestamp,
    ) -> Result<Option<(Session, Origin)>> {
        let moved = !shares.is_empty();
        let mut next = match &self.session {
            Some(open) => (open.clone(), Origin::Open),
            None if moved => {
                let started_at = now.checked_sub(SignedDuration::from_secs(delta.time_s.into()))?;
                self.resume_or_start(started_at, now)
            }
            None => return Ok(None),
        };
        let session = &mut next.0;
        if moved {
            session.activity += shares.iter().map(|&(_, share)| share).sum();
            session.updated_at = now;
        }
        if sample.running {
            session.max_kmh = session.max_kmh.max(sample.speed_kmh());
        }
        Ok(Some(next))
    }

    fn resume_or_start(&self, started_at: Timestamp, now: Timestamp) -> (Session, Origin) {
        match &self.last {
            Some(last) if started_at.duration_since(last.updated_at) < MERGE_WITHIN => (
                Session {
                    ended_at: None,
                    ..last.clone()
                },
                Origin::Reopened,
            ),
            _ => (
                Session {
                    id: 0,
                    started_at,
                    updated_at: now,
                    ended_at: None,
                    activity: Activity::default(),
                    max_kmh: 0.0,
                },
                Origin::New,
            ),
        }
    }

    fn close(&mut self) -> Result<()> {
        if let Some(open) = &self.session {
            let closed = Session {
                ended_at: Some(open.updated_at),
                ..open.clone()
            };
            self.store.update_session(&closed)?;
            self.session = None;
            self.last = Some(closed);
        }
        Ok(())
    }

    fn roll_over(&mut self, now: Timestamp) -> Result<()> {
        if !self.day.contains(&now) {
            (self.today, self.day) = load_today(&self.store, &self.tz, now)?;
        }
        Ok(())
    }
}

fn load_today(store: &Store, tz: &TimeZone, now: Timestamp) -> Result<(Today, Range<Timestamp>)> {
    let date = tz.to_datetime(now).date();
    let day = tz.to_timestamp(date.into())?..tz.to_timestamp(date.tomorrow()?.into())?;
    let today = Today {
        date,
        activity: store
            .minutes(day.clone())?
            .into_iter()
            .map(|(_, a)| a)
            .sum(),
        sessions: store.sessions_started(day.clone())?,
    };
    Ok((today, day))
}

fn clamp_weight(kg: f64) -> f64 {
    if kg.is_nan() {
        DEFAULT_WEIGHT_KG
    } else {
        kg.clamp(MIN_WEIGHT_KG, MAX_WEIGHT_KG)
    }
}

/// What to credit for `reading`: nothing on a device's first frame or when
/// no pad could have advanced that far since `baseline`.
fn credit(device: &str, baseline: Option<Baseline>, reading: Counters, now: Timestamp) -> Counters {
    let Some(baseline) = baseline else {
        return Counters::default();
    };
    let delta = delta(baseline.counters, reading);
    let elapsed = now.duration_since(baseline.seen_at);
    if plausible(delta, elapsed) {
        delta
    } else {
        log::warn!("{device}: ignoring a counter advance of {delta:?} within {elapsed:#}");
        Counters::default()
    }
}

/// What the counters advanced by since `base`, everything since zero when
/// the pad has reset.
fn delta(base: Counters, reading: Counters) -> Counters {
    if reading.time_s < base.time_s
        || reading.dist_10m < base.dist_10m
        || reading.steps < base.steps
    {
        return reading;
    }
    Counters {
        time_s: reading.time_s - base.time_s,
        dist_10m: reading.dist_10m - base.dist_10m,
        steps: reading.steps - base.steps,
    }
}

/// Whether the belt could have moved by `delta` within `elapsed`. Distance
/// and steps may run a second and a 10 m tick ahead of the time counter.
fn plausible(delta: Counters, elapsed: SignedDuration) -> bool {
    let time_s = u64::from(delta.time_s);
    let metres = u64::from(delta.dist_10m) * 10;
    i64::from(delta.time_s) <= elapsed.as_secs() + CLOCK_SLACK_S
        && metres <= (time_s + 1) * MAX_KMH * 1_000 / 3_600 + 10
        && u64::from(delta.steps) <= (time_s + 1) * MAX_STEPS_PER_S
}

/// Energy for `delta`, at the belt speed for a live delta and at the
/// delta's own average speed for a catch-up.
fn energy(delta: Counters, speed_kmh: f64, weight_kg: f64) -> f64 {
    let kmh = if delta.time_s <= LIVE_S {
        speed_kmh
    } else {
        f64::from(delta.dist_10m) * 36.0 / f64::from(delta.time_s)
    };
    calories::kcal(kmh, weight_kg, delta.time_s)
}

/// Splits `delta` over UTC minutes: a live delta lands in the minute
/// containing `now`, a catch-up is spread over `[now − time, now]` by
/// seconds, so its integer fields still sum exactly.
fn shares(delta: Counters, kcal: f64, now: Timestamp) -> Result<Vec<(Timestamp, Activity)>> {
    if delta == Counters::default() {
        return Ok(Vec::new());
    }
    let whole = Activity {
        active_s: delta.time_s,
        steps: delta.steps,
        distance_m: delta.dist_10m.saturating_mul(10),
        kcal,
    };
    if delta.time_s <= LIVE_S {
        return Ok(vec![(now, whole)]);
    }
    let end = now.as_second();
    let start = end - i64::from(delta.time_s);
    let mut minutes = Vec::new();
    for minute in start.div_euclid(60)..=(end - 1).div_euclid(60) {
        let from = minute * 60;
        let seconds = u32::try_from((from + 60).min(end) - from.max(start))?;
        minutes.push((Timestamp::from_second(from)?, seconds));
    }
    let seconds: Vec<u32> = minutes.iter().map(|&(_, seconds)| seconds).collect();
    let steps = apportion(whole.steps, &seconds);
    let distance_m = apportion(whole.distance_m, &seconds);
    Ok(minutes
        .into_iter()
        .zip(steps.into_iter().zip(distance_m))
        .map(|((at, active_s), (steps, distance_m))| {
            let kcal = kcal * f64::from(active_s) / f64::from(delta.time_s);
            let share = Activity {
                active_s,
                steps,
                distance_m,
                kcal,
            };
            (at, share)
        })
        .collect())
}

/// Splits `total` in proportion to non-empty `weights` by largest
/// remainder, earlier parts winning ties.
fn apportion(total: u32, weights: &[u32]) -> Vec<u32> {
    let sum: u64 = weights.iter().copied().map(u64::from).sum();
    let exact: Vec<u64> = weights
        .iter()
        .map(|&weight| u64::from(total) * u64::from(weight))
        .collect();
    let mut parts: Vec<u32> = exact.iter().map(|&part| (part / sum) as u32).collect();
    let short = total - parts.iter().sum::<u32>();
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by_key(|&i| Reverse(exact[i] % sum));
    for &i in order.iter().take(short as usize) {
        parts[i] += 1;
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::TempDir;
    use jiff::civil::DateTime;

    const PAD: &str = "AA:BB:CC:DD:EE:FF";

    fn sarajevo() -> TimeZone {
        TimeZone::get("Europe/Sarajevo").unwrap()
    }

    fn local(datetime: &str) -> Timestamp {
        sarajevo()
            .to_timestamp(datetime.parse::<DateTime>().unwrap())
            .unwrap()
    }

    fn at(clock: &str) -> Timestamp {
        local(&format!("2026-10-04T{clock}"))
    }

    fn walking(time_s: u32, dist_10m: u32, steps: u32) -> Sample {
        Sample {
            time_s,
            dist_10m,
            steps,
            speed_dkmh: 40,
            running: true,
        }
    }

    fn stopped(time_s: u32, dist_10m: u32, steps: u32) -> Sample {
        Sample {
            speed_dkmh: 0,
            running: false,
            ..walking(time_s, dist_10m, steps)
        }
    }

    fn tracker_at(now: Timestamp) -> Tracker {
        Tracker::open(Store::open_in_memory().unwrap(), sarajevo(), now).unwrap()
    }

    fn feed(tracker: &mut Tracker, frames: &[(Timestamp, Sample)]) {
        for &(now, sample) in frames {
            tracker.on_sample(PAD, sample, now).unwrap();
        }
    }

    fn walk_from(tracker: &mut Tracker, start: Timestamp, seconds: u32) {
        let frames: Vec<_> = (0..=seconds)
            .map(|s| {
                let now = start + SignedDuration::from_secs(s.into());
                (now, walking(s, s / 10, 2 * s))
            })
            .collect();
        feed(tracker, &frames);
    }

    fn minutes(tracker: &Tracker, range: Range<Timestamp>) -> Vec<(Timestamp, Activity)> {
        tracker.store().minutes(range).unwrap()
    }

    fn integers(activity: Activity) -> (u32, u32, u32) {
        (activity.active_s, activity.steps, activity.distance_m)
    }

    fn assert_kcal(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    #[test]
    fn the_first_frame_sets_the_baseline_and_records_nothing() {
        let mut tracker = tracker_at(at("10:00:00"));
        feed(&mut tracker, &[(at("10:00:00"), walking(500, 40, 800))]);
        assert_eq!(tracker.session(), None);
        assert_eq!(tracker.today().activity, Activity::default());
        assert_eq!(minutes(&tracker, at("00:00")..at("23:59")), []);
        assert_eq!(
            tracker.store().baseline(PAD).unwrap(),
            Some(Baseline {
                counters: walking(500, 40, 800).counters(),
                seen_at: at("10:00:00")
            })
        );
    }

    #[test]
    fn live_deltas_add_to_the_session_the_minute_and_today() {
        let mut tracker = tracker_at(at("10:00:00"));
        feed(
            &mut tracker,
            &[
                (at("10:00:00"), walking(100, 10, 200)),
                (at("10:00:01"), walking(101, 10, 202)),
                (at("10:00:02"), walking(102, 11, 204)),
            ],
        );
        let session = tracker.session().unwrap();
        assert_eq!(session.started_at, at("10:00:00"));
        assert_eq!(session.updated_at, at("10:00:02"));
        assert_eq!(integers(session.activity), (2, 4, 10));
        assert_kcal(session.activity.kcal, 2.0 * calories::kcal(4.0, 70.0, 1));
        assert_eq!(tracker.today().activity, session.activity);
        assert_eq!(tracker.today().sessions, 1);
        let rows = minutes(&tracker, at("10:00")..at("10:01"));
        assert_eq!(rows, [(at("10:00"), session.activity)]);
    }

    #[test]
    fn a_counter_below_its_baseline_means_the_pad_reset() {
        let mut tracker = tracker_at(at("10:00:00"));
        feed(
            &mut tracker,
            &[
                (at("10:00:00"), stopped(1_000, 80, 1_500)),
                (at("10:00:05"), walking(4, 1, 8)),
            ],
        );
        assert_eq!(integers(tracker.session().unwrap().activity), (4, 8, 10));
    }

    #[test]
    fn an_advance_beyond_the_wall_clock_only_moves_the_baseline() {
        let mut tracker = tracker_at(at("10:00:00"));
        feed(
            &mut tracker,
            &[
                (at("10:00:00"), stopped(0, 0, 0)),
                (at("10:00:10"), walking(16, 1, 30)),
            ],
        );
        assert_eq!(tracker.session(), None);
        assert_eq!(tracker.today().activity, Activity::default());
        feed(&mut tracker, &[(at("10:00:11"), walking(17, 1, 32))]);
        assert_eq!(integers(tracker.today().activity), (1, 2, 0));
    }

    #[test]
    fn distance_beyond_eight_kmh_credits_nothing() {
        let mut tracker = tracker_at(at("10:00:00"));
        feed(
            &mut tracker,
            &[
                (at("10:00:00"), stopped(0, 0, 0)),
                (at("10:01:00"), walking(60, 15, 100)),
            ],
        );
        assert_eq!(tracker.session(), None);
    }

    #[test]
    fn distance_up_to_eight_kmh_and_a_tick_is_credited() {
        let mut tracker = tracker_at(at("10:00:00"));
        feed(
            &mut tracker,
            &[
                (at("10:00:00"), stopped(0, 0, 0)),
                (at("10:01:00"), walking(60, 14, 100)),
            ],
        );
        assert_eq!(integers(tracker.today().activity), (60, 100, 140));
    }

    #[test]
    fn more_than_four_steps_a_second_credits_nothing() {
        let mut tracker = tracker_at(at("10:00:00"));
        feed(
            &mut tracker,
            &[
                (at("10:00:00"), stopped(0, 0, 0)),
                (at("10:01:00"), walking(60, 5, 245)),
            ],
        );
        assert_eq!(tracker.session(), None);
        feed(&mut tracker, &[(at("10:02:00"), walking(120, 10, 489))]);
        assert_eq!(integers(tracker.today().activity), (60, 244, 50));
    }

    #[test]
    fn steps_and_a_distance_tick_may_lead_the_time_counter_by_a_second() {
        let mut tracker = tracker_at(at("10:00:00"));
        feed(
            &mut tracker,
            &[
                (at("10:00:00"), stopped(10, 1, 20)),
                (at("10:00:01"), walking(10, 2, 24)),
            ],
        );
        assert_eq!(integers(tracker.today().activity), (0, 4, 10));
    }

    #[test]
    fn a_catch_up_is_spread_over_its_minutes_and_sums_exactly() {
        let mut tracker = tracker_at(at("09:59:30"));
        feed(
            &mut tracker,
            &[
                (at("09:59:30"), stopped(0, 0, 0)),
                (at("10:02:20"), walking(150, 13, 203)),
            ],
        );
        let rows = minutes(&tracker, at("09:00")..at("11:00"));
        let starts: Vec<_> = rows.iter().map(|&(start, _)| start).collect();
        assert_eq!(starts, [at("09:59"), at("10:00"), at("10:01"), at("10:02")]);
        let parts: Vec<_> = rows.iter().map(|&(_, share)| integers(share)).collect();
        assert_eq!(
            parts,
            [(10, 14, 9), (60, 81, 52), (60, 81, 52), (20, 27, 17)]
        );
        let total: Activity = rows.iter().map(|&(_, share)| share).sum();
        assert_eq!(integers(total), (150, 203, 130));
        assert_kcal(total.kcal, calories::kcal(3.12, 70.0, 150));
        assert_kcal(rows[1].1.kcal, total.kcal * 60.0 / 150.0);
        assert_eq!(tracker.session().unwrap().started_at, at("09:59:50"));
    }

    #[test]
    fn a_catch_up_across_local_midnight_counts_only_today_s_part() {
        let mut tracker = tracker_at(local("2026-10-04T23:58:00"));
        feed(
            &mut tracker,
            &[
                (local("2026-10-04T23:58:00"), stopped(0, 0, 0)),
                (local("2026-10-05T00:01:00"), walking(150, 12, 300)),
            ],
        );
        let today = tracker.today();
        assert_eq!(today.date, jiff::civil::date(2026, 10, 5));
        assert_eq!(integers(today.activity), (60, 120, 48));
        assert_eq!(today.sessions, 0);
    }

    #[test]
    fn a_standing_belt_closes_the_session_ten_seconds_after_its_last_activity() {
        let mut tracker = tracker_at(at("10:00:00"));
        walk_from(&mut tracker, at("10:00:00"), 60);
        feed(&mut tracker, &[(at("10:01:01"), stopped(60, 6, 120))]);
        tracker.on_tick(at("10:01:09")).unwrap();
        assert!(tracker.session().is_some());
        tracker.on_tick(at("10:01:10")).unwrap();
        assert_eq!(tracker.session(), None);
        let last = tracker.last_session().unwrap();
        assert_eq!(last.ended_at, Some(at("10:01:00")));
        assert_eq!(integers(last.activity), (60, 120, 60));
    }

    #[test]
    fn a_running_belt_keeps_the_session_open_without_deltas() {
        let mut tracker = tracker_at(at("10:00:00"));
        walk_from(&mut tracker, at("10:00:00"), 60);
        feed(&mut tracker, &[(at("10:01:20"), walking(60, 6, 120))]);
        tracker.on_tick(at("10:01:30")).unwrap();
        assert!(tracker.session().is_some());
    }

    #[test]
    fn walking_again_within_two_minutes_reopens_the_session() {
        let mut tracker = tracker_at(at("10:00:00"));
        walk_from(&mut tracker, at("10:00:00"), 60);
        tracker.on_link_lost().unwrap();
        let id = tracker.last_session().unwrap().id;
        feed(
            &mut tracker,
            &[
                (at("10:02:58"), stopped(60, 6, 120)),
                (at("10:02:59"), walking(61, 6, 122)),
            ],
        );
        let session = tracker.session().unwrap();
        assert_eq!((session.id, session.ended_at), (id, None));
        assert_eq!(integers(session.activity), (61, 122, 60));
        assert_eq!(tracker.last_session(), None);
        assert_eq!(tracker.today().sessions, 1);
    }

    #[test]
    fn walking_again_after_two_minutes_starts_a_new_session() {
        let mut tracker = tracker_at(at("10:00:00"));
        walk_from(&mut tracker, at("10:00:00"), 60);
        tracker.on_link_lost().unwrap();
        let first = tracker.last_session().unwrap().clone();
        feed(
            &mut tracker,
            &[
                (at("10:02:59"), stopped(60, 6, 120)),
                (at("10:03:01"), walking(61, 6, 122)),
            ],
        );
        let second = tracker.session().unwrap();
        assert_ne!(second.id, first.id);
        assert_eq!(second.started_at, at("10:03:00"));
        assert_eq!(tracker.last_session(), Some(&first));
        assert_eq!(tracker.today().sessions, 2);
    }

    #[test]
    fn losing_the_link_closes_the_session_and_a_catch_up_on_return_reopens_it() {
        let mut tracker = tracker_at(at("10:00:00"));
        walk_from(&mut tracker, at("10:00:00"), 60);
        tracker.on_link_lost().unwrap();
        assert_eq!(tracker.session(), None);
        let closed = tracker.last_session().unwrap().clone();
        assert_eq!(closed.ended_at, Some(at("10:01:00")));
        feed(&mut tracker, &[(at("10:01:40"), walking(100, 10, 200))]);
        let session = tracker.session().unwrap();
        assert_eq!(session.id, closed.id);
        assert_eq!(integers(session.activity), (100, 200, 100));
    }

    #[test]
    fn a_crash_leaves_the_session_to_be_closed_on_the_next_open() {
        let dir = TempDir::new("tracker-crash");
        let path = dir.join("ks-fit.db");
        let open = || Tracker::open(Store::open(&path).unwrap(), sarajevo(), at("10:05:00"));
        let mut tracker = open().unwrap();
        walk_from(&mut tracker, at("10:00:00"), 60);
        let crashed = tracker.session().unwrap().clone();
        drop(tracker);
        let tracker = open().unwrap();
        assert_eq!(tracker.session(), None);
        assert_eq!(
            tracker.last_session(),
            Some(&Session {
                ended_at: Some(at("10:01:00")),
                ..crashed
            })
        );
    }

    #[test]
    fn restarting_mid_walk_reopens_the_session_from_the_stored_baseline() {
        let dir = TempDir::new("tracker-restart");
        let path = dir.join("ks-fit.db");
        let open = |now| Tracker::open(Store::open(&path).unwrap(), sarajevo(), now).unwrap();
        let mut tracker = open(at("09:00:00"));
        walk_from(&mut tracker, at("09:00:00"), 60);
        tracker.on_link_lost().unwrap();
        let earlier = tracker.last_session().unwrap().clone();
        feed(
            &mut tracker,
            &[
                (at("10:00:00"), stopped(60, 6, 120)),
                (at("10:00:01"), walking(61, 6, 122)),
                (at("10:00:30"), walking(90, 9, 180)),
            ],
        );
        let walking_id = tracker.session().unwrap().id;
        drop(tracker);

        let mut tracker = open(at("10:01:00"));
        assert_eq!(tracker.last_session().unwrap().id, walking_id);
        feed(&mut tracker, &[(at("10:01:00"), walking(120, 12, 240))]);
        let session = tracker.session().unwrap();
        assert_eq!(session.id, walking_id);
        assert_eq!(integers(session.activity), (60, 120, 60));
        assert_eq!(tracker.last_session(), Some(&earlier));
    }

    #[test]
    fn today_reloads_from_the_store_as_it_was_kept() {
        let dir = TempDir::new("tracker-today");
        let path = dir.join("ks-fit.db");
        let open = |now| Tracker::open(Store::open(&path).unwrap(), sarajevo(), now).unwrap();
        let mut tracker = open(at("10:00:00"));
        walk_from(&mut tracker, at("10:00:00"), 75);
        tracker.on_link_lost().unwrap();
        feed(&mut tracker, &[(at("10:04:00"), walking(240, 22, 470))]);
        tracker.on_link_lost().unwrap();
        let kept = tracker.today().clone();
        drop(tracker);

        let reloaded = open(at("10:10:00")).today().clone();
        assert_eq!(
            (reloaded.date, reloaded.sessions),
            (kept.date, kept.sessions)
        );
        assert_eq!(integers(reloaded.activity), integers(kept.activity));
        assert_kcal(reloaded.activity.kcal, kept.activity.kcal);
    }

    #[test]
    fn today_rolls_over_at_local_midnight() {
        let mut tracker = tracker_at(local("2026-10-04T23:59:00"));
        walk_from(&mut tracker, local("2026-10-04T23:59:50"), 9);
        assert_eq!(integers(tracker.today().activity), (9, 18, 0));
        tracker.on_tick(local("2026-10-04T23:59:59")).unwrap();
        assert_eq!(tracker.today().date, jiff::civil::date(2026, 10, 4));
        tracker.on_tick(local("2026-10-05T00:00:00")).unwrap();
        let today = tracker.today();
        assert_eq!(today.date, jiff::civil::date(2026, 10, 5));
        assert_eq!((today.activity, today.sessions), (Activity::default(), 0));
        feed(
            &mut tracker,
            &[(local("2026-10-05T00:00:01"), walking(11, 1, 22))],
        );
        assert_eq!(integers(tracker.today().activity), (2, 4, 10));
        assert_eq!(tracker.today().sessions, 0);
    }

    #[test]
    fn the_last_session_is_shown_only_on_the_day_it_ended() {
        let mut tracker = tracker_at(local("2026-10-04T23:00:00"));
        walk_from(&mut tracker, local("2026-10-04T23:00:00"), 60);
        tracker.on_link_lost().unwrap();
        assert!(tracker.last_session().is_some());
        tracker.on_tick(local("2026-10-05T00:00:30")).unwrap();
        assert_eq!(tracker.last_session(), None);
    }

    #[test]
    fn the_session_keeps_its_top_speed_while_running() {
        let mut tracker = tracker_at(at("10:00:00"));
        let at_speed = |time_s, speed_dkmh, running| Sample {
            speed_dkmh,
            running,
            ..walking(time_s, 0, 2 * time_s)
        };
        feed(
            &mut tracker,
            &[
                (at("10:00:00"), at_speed(0, 0, false)),
                (at("10:00:01"), at_speed(1, 30, true)),
                (at("10:00:02"), at_speed(2, 45, true)),
                (at("10:00:03"), at_speed(2, 52, true)),
                (at("10:00:04"), at_speed(3, 60, false)),
            ],
        );
        assert_eq!(tracker.session().unwrap().max_kmh, 5.2);
        tracker.on_link_lost().unwrap();
        assert_eq!(
            tracker
                .store()
                .latest_closed_session()
                .unwrap()
                .unwrap()
                .max_kmh,
            5.2
        );
    }

    #[test]
    fn a_new_tracker_estimates_energy_for_the_default_weight() {
        let tracker = tracker_at(at("10:00:00"));
        assert_eq!(tracker.weight_kg(), DEFAULT_WEIGHT_KG);
    }

    #[test]
    fn the_weight_is_clamped_persisted_and_used_from_then_on() {
        let dir = TempDir::new("tracker-weight");
        let path = dir.join("ks-fit.db");
        let open = || Tracker::open(Store::open(&path).unwrap(), sarajevo(), at("10:00:00"));
        let mut tracker = open().unwrap();
        tracker.set_weight_kg(500.0).unwrap();
        assert_eq!(tracker.weight_kg(), 300.0);
        tracker.set_weight_kg(10.0).unwrap();
        assert_eq!(open().unwrap().weight_kg(), 20.0);
        tracker.set_weight_kg(90.0).unwrap();
        feed(
            &mut tracker,
            &[
                (at("10:00:00"), walking(0, 0, 0)),
                (at("10:00:01"), walking(1, 0, 2)),
            ],
        );
        assert_kcal(tracker.today().activity.kcal, calories::kcal(4.0, 90.0, 1));
    }

    #[test]
    fn largest_remainders_round_up_and_ties_go_to_the_earlier_part() {
        assert_eq!(apportion(10, &[1, 1, 1]), [4, 3, 3]);
        assert_eq!(apportion(2, &[30, 60, 30]), [1, 1, 0]);
        assert_eq!(apportion(0, &[10, 60]), [0, 0]);
        assert_eq!(
            apportion(u32::MAX, &[60, 60]),
            [u32::MAX / 2 + 1, u32::MAX / 2]
        );
    }
}
