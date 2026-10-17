//! Seeds plausible history for the simulated pad.

use crate::tracker::{Sample, Tracker};
use anyhow::Result;
use jiff::civil::{Date, DateTime, Weekday};
use jiff::{SignedDuration, Span, Timestamp};

const DAYS: i32 = 120;
const DEVICE: &str = "demo";
const FIRST_MINUTE: u32 = 8 * 60;
const LAST_MINUTE: u32 = 19 * 60;
const MIN_GAP_MINUTES: u32 = 5;

/// A walk on the pad, starting `start` minutes after local midnight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Walk {
    start: u32,
    minutes: u32,
    speed_dkmh: u8,
}

/// Feeds walks on the `DAYS` days before `now`'s local date through
/// `tracker`, unless its store already has activity, and reports whether it
/// did.
pub fn seed(tracker: &mut Tracker, now: Timestamp) -> Result<bool> {
    if tracker.store().has_activity()? {
        return Ok(false);
    }
    let tz = tracker.time_zone().clone();
    let today = tz.to_datetime(now).date();
    for days_back in (1..=DAYS).rev() {
        let day = today.checked_sub(Span::new().try_days(days_back)?)?;
        let mut pad = Pad::default();
        for walk in walks(day) {
            let local = DateTime::from(day).checked_add(Span::new().try_minutes(walk.start)?)?;
            take_walk(tracker, &mut pad, walk, tz.to_timestamp(local)?)?;
        }
    }
    tracker.on_tick(now)?;
    Ok(true)
}

/// Feeds `walk` from `start` as the pad reports it: idle frames, which on
/// the day's first walk also confirm the overnight reset, a frame a minute
/// while walking, then the belt standing until the session closes.
fn take_walk(tracker: &mut Tracker, pad: &mut Pad, walk: Walk, start: Timestamp) -> Result<()> {
    let at = |seconds: i64| -> Result<Timestamp> {
        Ok(start.checked_add(SignedDuration::from_secs(seconds))?)
    };
    for seconds in [-1, 0] {
        tracker.on_sample(DEVICE, pad.sample(0), at(seconds)?)?;
    }
    for minute in 1..=i64::from(walk.minutes) {
        pad.walk_a_minute(walk.speed_dkmh);
        tracker.on_sample(DEVICE, pad.sample(walk.speed_dkmh), at(minute * 60)?)?;
    }
    let end = i64::from(walk.minutes) * 60;
    tracker.on_sample(DEVICE, pad.sample(0), at(end + 1)?)?;
    tracker.on_tick(at(end + 10)?)
}

/// The walks of `day`: one to three of 15–50 minutes at 2.5–4.5 km/h
/// between 08:00 and 19:00 on most weekdays, at most one on weekends.
fn walks(day: Date) -> Vec<Walk> {
    let mut rng = Rng::new(day);
    let count = match day.weekday() {
        Weekday::Saturday | Weekday::Sunday => u32::from(rng.below(10) < 6),
        _ if rng.below(10) == 0 => 0,
        _ => 1 + rng.below(3),
    };
    let slot = (LAST_MINUTE - FIRST_MINUTE) / count.max(1);
    (0..count)
        .map(|i| {
            let minutes = 15 + rng.below(36);
            Walk {
                start: FIRST_MINUTE + i * slot + rng.below(slot - minutes - MIN_GAP_MINUTES),
                minutes,
                speed_dkmh: 25 + rng.below(21) as u8,
            }
        })
        .collect()
}

/// The pad's counters through a day, with a cadence of 60 + 12 × km/h
/// steps a minute.
#[derive(Default)]
struct Pad {
    time_s: u32,
    thirds_of_metres: u32,
    fifths_of_steps: u32,
}

impl Pad {
    fn walk_a_minute(&mut self, speed_dkmh: u8) {
        self.time_s += 60;
        self.thirds_of_metres += 5 * u32::from(speed_dkmh);
        self.fifths_of_steps += 300 + 6 * u32::from(speed_dkmh);
    }

    fn sample(&self, speed_dkmh: u8) -> Sample {
        Sample {
            time_s: self.time_s,
            dist_10m: self.thirds_of_metres / 30,
            steps: self.fifths_of_steps / 5,
            speed_dkmh,
            running: speed_dkmh > 0,
        }
    }
}

/// SplitMix64 seeded by the date, so a day's walks are the same on every
/// run.
struct Rng(u64);

impl Rng {
    fn new(day: Date) -> Rng {
        let year = u64::from(day.year().unsigned_abs());
        Rng(year * 1_000 + u64::from(day.day_of_year().unsigned_abs()))
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let z = (self.0 ^ (self.0 >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        let z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in `0..n`.
    fn below(&mut self, n: u32) -> u32 {
        (self.next() % u64::from(n)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Activity, Store};
    use jiff::civil::date;
    use jiff::tz::TimeZone;
    use std::collections::BTreeMap;
    use std::iter;

    fn sarajevo() -> TimeZone {
        TimeZone::get("Europe/Sarajevo").unwrap()
    }

    fn now() -> Timestamp {
        sarajevo()
            .to_timestamp(date(2026, 10, 4).at(12, 0, 0, 0))
            .unwrap()
    }

    fn seeded() -> Tracker {
        let mut tracker =
            Tracker::open(Store::open_in_memory().unwrap(), sarajevo(), now()).unwrap();
        assert!(seed(&mut tracker, now()).unwrap());
        tracker
    }

    fn all_minutes(tracker: &Tracker) -> Vec<(Timestamp, Activity)> {
        tracker
            .store()
            .minutes(Timestamp::UNIX_EPOCH..now())
            .unwrap()
    }

    fn seeded_days() -> impl Iterator<Item = Date> {
        iter::successors(Some(date(2026, 6, 6)), |day| day.tomorrow().ok())
            .take_while(|day| *day < date(2026, 10, 4))
    }

    fn steps_by_day(tracker: &Tracker) -> BTreeMap<Date, u32> {
        let mut days = BTreeMap::new();
        for (at, activity) in all_minutes(tracker) {
            *days.entry(sarajevo().to_datetime(at).date()).or_default() += activity.steps;
        }
        days
    }

    #[test]
    fn a_seeded_store_holds_the_walks_of_the_days_before_today() {
        let tracker = seeded();
        let days = steps_by_day(&tracker);
        assert!(*days.first_key_value().unwrap().0 >= date(2026, 6, 6));
        assert!(*days.last_key_value().unwrap().0 <= date(2026, 10, 3));
        assert!(days.len() > 60, "{} days with walks", days.len());
        let walks: Vec<Walk> = seeded_days().flat_map(walks).collect();
        let minutes = all_minutes(&tracker);
        let active_s: u32 = minutes.iter().map(|(_, activity)| activity.active_s).sum();
        assert_eq!(
            active_s,
            walks.iter().map(|walk| walk.minutes * 60).sum::<u32>()
        );
        let range = Timestamp::UNIX_EPOCH..now();
        assert_eq!(
            tracker.store().sessions_started(range).unwrap(),
            u32::try_from(walks.len()).unwrap()
        );
    }

    #[test]
    fn the_tracker_is_back_on_today_after_seeding() {
        let tracker = seeded();
        assert_eq!(tracker.today().date, date(2026, 10, 4));
        assert_eq!(tracker.today().activity, Activity::default());
        assert_eq!(tracker.session(), None);
        assert_eq!(tracker.last_session(), None);
        let latest = tracker.store().latest_closed_session().unwrap().unwrap();
        assert!(!latest.estimated);
        assert!((2.5..=4.5).contains(&latest.max_kmh));
    }

    #[test]
    fn seeding_is_deterministic() {
        assert_eq!(all_minutes(&seeded()), all_minutes(&seeded()));
    }

    #[test]
    fn a_store_with_activity_is_left_alone() {
        let mut tracker = seeded();
        let before = all_minutes(&tracker);
        assert!(!seed(&mut tracker, now()).unwrap());
        assert_eq!(all_minutes(&tracker), before);
    }

    #[test]
    fn walks_fit_between_eight_and_seven_with_gaps() {
        for day in seeded_days() {
            let walks = walks(day);
            for walk in &walks {
                assert!((15..=50).contains(&walk.minutes), "{day}: {walk:?}");
                assert!((25..=45).contains(&walk.speed_dkmh), "{day}: {walk:?}");
                assert!(walk.start >= FIRST_MINUTE, "{day}: {walk:?}");
                assert!(walk.start + walk.minutes <= LAST_MINUTE, "{day}: {walk:?}");
            }
            for pair in walks.windows(2) {
                assert!(
                    pair[0].start + pair[0].minutes + MIN_GAP_MINUTES <= pair[1].start,
                    "{day}: {pair:?}"
                );
            }
        }
    }

    #[test]
    fn weekdays_have_up_to_three_walks_and_weekends_at_most_one() {
        let weekend = |day: &Date| matches!(day.weekday(), Weekday::Saturday | Weekday::Sunday);
        let counts = |weekends: bool| -> Vec<usize> {
            seeded_days()
                .filter(|day| weekend(day) == weekends)
                .map(|day| walks(day).len())
                .collect()
        };
        let weekdays = counts(false);
        assert!(weekdays.iter().all(|&count| count <= 3));
        assert!(weekdays.contains(&0) && weekdays.contains(&3));
        let weekends = counts(true);
        assert!(weekends.iter().all(|&count| count <= 1));
        assert!(weekends.contains(&0) && weekends.contains(&1));
    }
}
