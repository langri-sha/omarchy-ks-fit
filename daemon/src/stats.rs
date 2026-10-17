//! Period statistics bucketed in local time.

use crate::store::{Activity, Store};
use anyhow::Result;
use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use std::iter;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    /// A calendar day in hours.
    Day,
    /// A week from Monday to Sunday in days.
    Week,
    /// A calendar month in days.
    Month,
    /// A calendar year in months.
    Year,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bucket {
    pub key: String,
    pub activity: Activity,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    /// The offset the stats are for, at most 0.
    pub offset: i32,
    /// The first local date of the period.
    pub start: Date,
    /// The last local date of the period.
    pub end: Date,
    pub buckets: Vec<Bucket>,
    pub totals: Activity,
}

impl Stats {
    /// How many buckets have any steps.
    pub fn active_buckets(&self) -> usize {
        self.buckets
            .iter()
            .filter(|bucket| bucket.activity.steps > 0)
            .count()
    }
}

/// The activity in the `period` that lies `offset` periods before the one
/// containing `now` in `tz`, with every bucket present.
pub fn query(
    store: &Store,
    tz: &TimeZone,
    now: Timestamp,
    period: Period,
    offset: i32,
) -> Result<Stats> {
    let offset = offset.min(0);
    let (start, end) = period.dates(tz.to_datetime(now).date(), offset)?;
    let range = tz.to_timestamp(start.into())?..tz.to_timestamp(end.tomorrow()?.into())?;
    let mut buckets: Vec<Bucket> = period
        .keys(start, end)
        .into_iter()
        .map(|key| Bucket {
            key,
            activity: Activity::default(),
        })
        .collect();
    for (at, activity) in store.minutes(range)? {
        if let Some(bucket) = buckets.get_mut(period.slot(tz.to_datetime(at))) {
            bucket.activity += activity;
        }
    }
    let totals = buckets.iter().map(|bucket| bucket.activity).sum();
    Ok(Stats {
        offset,
        start,
        end,
        buckets,
        totals,
    })
}

impl Period {
    fn dates(self, today: Date, offset: i32) -> Result<(Date, Date)> {
        let span = Span::new();
        Ok(match self {
            Period::Day => {
                let day = today.checked_add(span.try_days(offset)?)?;
                (day, day)
            }
            Period::Week => {
                let monday = today
                    .checked_sub(span.try_days(today.weekday().to_monday_zero_offset())?)?
                    .checked_add(span.try_weeks(offset)?)?;
                (monday, monday.checked_add(span.try_days(6)?)?)
            }
            Period::Month => {
                let first = today
                    .first_of_month()
                    .checked_add(span.try_months(offset)?)?;
                (first, first.last_of_month())
            }
            Period::Year => {
                let first = today.first_of_year().checked_add(span.try_years(offset)?)?;
                (first, first.last_of_year())
            }
        })
    }

    fn keys(self, start: Date, end: Date) -> Vec<String> {
        match self {
            Period::Day => (0..24).map(|hour| format!("{start}T{hour:02}")).collect(),
            Period::Week | Period::Month => {
                iter::successors(Some(start), |date| date.tomorrow().ok())
                    .take_while(|date| *date <= end)
                    .map(|date| date.to_string())
                    .collect()
            }
            Period::Year => (1..=12)
                .map(|month| format!("{:04}-{month:02}", start.year()))
                .collect(),
        }
    }

    /// The bucket `local`, a time within the period, falls in. A repeated
    /// local hour merges into its label and a skipped one stays empty.
    fn slot(self, local: DateTime) -> usize {
        let slot = match self {
            Period::Day => local.hour(),
            Period::Week => local.weekday().to_monday_zero_offset(),
            Period::Month => local.day() - 1,
            Period::Year => local.month() - 1,
        };
        usize::from(slot.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn sarajevo() -> TimeZone {
        TimeZone::get("Europe/Sarajevo").unwrap()
    }

    fn local(datetime: &str) -> Timestamp {
        sarajevo()
            .to_timestamp(datetime.parse::<DateTime>().unwrap())
            .unwrap()
    }

    fn utc(instant: &str) -> Timestamp {
        instant.parse().unwrap()
    }

    fn store_with(minutes: &[(Timestamp, u32)]) -> Store {
        let store = Store::open_in_memory().unwrap();
        for &(at, steps) in minutes {
            let activity = Activity {
                active_s: 60,
                steps,
                distance_m: steps / 2,
                kcal: f64::from(steps) / 4.0,
            };
            store.add_minute(at, &activity).unwrap();
        }
        store
    }

    fn stats(store: &Store, now: &str, period: Period, offset: i32) -> Stats {
        query(store, &sarajevo(), local(now), period, offset).unwrap()
    }

    fn keys(stats: &Stats) -> Vec<&str> {
        stats
            .buckets
            .iter()
            .map(|bucket| bucket.key.as_str())
            .collect()
    }

    fn steps(stats: &Stats) -> Vec<u32> {
        stats
            .buckets
            .iter()
            .map(|bucket| bucket.activity.steps)
            .collect()
    }

    fn steps_at(stats: &Stats, key: &str) -> u32 {
        stats
            .buckets
            .iter()
            .find(|bucket| bucket.key == key)
            .map(|bucket| bucket.activity.steps)
            .unwrap()
    }

    #[test]
    fn every_bucket_is_present_and_zero_filled() {
        let stats = stats(&store_with(&[]), "2026-10-04T13:20", Period::Day, 0);
        assert_eq!(
            (stats.start, stats.end),
            (date(2026, 10, 4), date(2026, 10, 4))
        );
        let expected: Vec<String> = (0..24)
            .map(|hour| format!("2026-10-04T{hour:02}"))
            .collect();
        assert_eq!(keys(&stats), expected);
        assert_eq!(steps(&stats), [0; 24]);
        assert_eq!(stats.totals, Activity::default());
        assert_eq!(stats.active_buckets(), 0);
    }

    #[test]
    fn a_day_buckets_by_local_hour_and_totals_the_buckets() {
        let store = store_with(&[
            (local("2026-10-04T09:15"), 100),
            (local("2026-10-04T09:59"), 20),
            (local("2026-10-04T13:00"), 7),
        ]);
        let stats = stats(&store, "2026-10-04T20:00", Period::Day, 0);
        assert_eq!(steps_at(&stats, "2026-10-04T09"), 120);
        assert_eq!(steps_at(&stats, "2026-10-04T13"), 7);
        assert_eq!(
            stats.totals,
            Activity {
                active_s: 180,
                steps: 127,
                distance_m: 50 + 10 + 3,
                kcal: 31.75
            }
        );
    }

    #[test]
    fn a_week_runs_from_monday_to_sunday() {
        let store = store_with(&[
            (local("2026-09-27T23:59"), 1),
            (local("2026-09-28T00:00"), 10),
            (local("2026-10-04T23:59"), 20),
            (local("2026-10-05T00:00"), 2),
        ]);
        for now in ["2026-09-28T00:00", "2026-10-01T12:00", "2026-10-04T23:59"] {
            let stats = stats(&store, now, Period::Week, 0);
            assert_eq!(
                (stats.start, stats.end),
                (date(2026, 9, 28), date(2026, 10, 4))
            );
            assert_eq!(
                keys(&stats),
                [
                    "2026-09-28",
                    "2026-09-29",
                    "2026-09-30",
                    "2026-10-01",
                    "2026-10-02",
                    "2026-10-03",
                    "2026-10-04"
                ]
            );
            assert_eq!(steps(&stats), [10, 0, 0, 0, 0, 0, 20]);
        }
    }

    #[test]
    fn months_have_as_many_buckets_as_days() {
        let store = store_with(&[]);
        for (now, last) in [
            ("2026-02-14T12:00", "2026-02-28"),
            ("2028-02-14T12:00", "2028-02-29"),
            ("2026-04-30T23:00", "2026-04-30"),
            ("2026-10-01T00:30", "2026-10-31"),
        ] {
            let stats = stats(&store, now, Period::Month, 0);
            assert_eq!(stats.end.to_string(), last);
            assert_eq!(
                stats.buckets.len(),
                usize::from(stats.end.day().unsigned_abs())
            );
            assert_eq!(keys(&stats)[0], format!("{}-01", &last[..7]));
            assert_eq!(keys(&stats).last(), Some(&last));
        }
    }

    #[test]
    fn a_year_has_a_bucket_per_month() {
        let store = store_with(&[
            (local("2026-01-01T00:00"), 3),
            (local("2026-06-15T12:00"), 5),
            (local("2026-12-31T23:59"), 7),
            (local("2027-01-01T00:00"), 11),
        ]);
        let stats = stats(&store, "2026-10-04T12:00", Period::Year, 0);
        assert_eq!(
            (stats.start, stats.end),
            (date(2026, 1, 1), date(2026, 12, 31))
        );
        let expected: Vec<String> = (1..=12).map(|month| format!("2026-{month:02}")).collect();
        assert_eq!(keys(&stats), expected);
        assert_eq!(steps(&stats), [3, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 7]);
        assert_eq!(stats.active_buckets(), 3);
    }

    #[test]
    fn negative_offsets_reach_back_whole_periods() {
        let store = store_with(&[]);
        let range = |period, offset| {
            let stats = stats(&store, "2026-10-04T12:00", period, offset);
            (stats.start, stats.end, stats.offset)
        };
        assert_eq!(
            range(Period::Day, -1),
            (date(2026, 10, 3), date(2026, 10, 3), -1)
        );
        assert_eq!(
            range(Period::Week, -1),
            (date(2026, 9, 21), date(2026, 9, 27), -1)
        );
        assert_eq!(
            range(Period::Month, -1),
            (date(2026, 9, 1), date(2026, 9, 30), -1)
        );
        assert_eq!(
            range(Period::Month, -10),
            (date(2025, 12, 1), date(2025, 12, 31), -10)
        );
        assert_eq!(
            range(Period::Year, -1),
            (date(2025, 1, 1), date(2025, 12, 31), -1)
        );
    }

    #[test]
    fn positive_offsets_clamp_to_the_current_period() {
        let store = store_with(&[]);
        let stats = stats(&store, "2026-10-04T12:00", Period::Month, 3);
        assert_eq!((stats.start, stats.offset), (date(2026, 10, 1), 0));
    }

    #[test]
    fn an_offset_beyond_the_calendar_is_an_error() {
        let store = store_with(&[]);
        for period in [Period::Day, Period::Week, Period::Month, Period::Year] {
            let result = query(
                &store,
                &sarajevo(),
                local("2026-10-04T12:00"),
                period,
                i32::MIN,
            );
            assert!(result.is_err(), "{period:?}");
        }
    }

    #[test]
    fn spring_forward_leaves_the_skipped_hour_empty() {
        let store = store_with(&[
            (local("2026-03-29T00:00"), 1),
            (local("2026-03-29T01:59"), 2),
            (local("2026-03-29T03:00"), 3),
            (local("2026-03-29T23:59"), 4),
            (local("2026-03-30T00:00"), 50),
        ]);
        let stats = stats(&store, "2026-03-29T12:00", Period::Day, 0);
        assert_eq!(stats.buckets.len(), 24);
        assert_eq!(&steps(&stats)[..4], [1, 2, 0, 3]);
        assert_eq!(steps_at(&stats, "2026-03-29T23"), 4);
        assert_eq!((stats.totals.steps, stats.totals.active_s), (10, 240));
    }

    #[test]
    fn fall_back_merges_the_repeated_hour() {
        let store = store_with(&[
            (local("2026-10-25T00:00"), 1),
            (utc("2026-10-25T00:30:00Z"), 2),
            (utc("2026-10-25T01:30:00Z"), 3),
            (local("2026-10-25T03:00"), 4),
            (local("2026-10-25T23:59"), 5),
        ]);
        let stats = stats(&store, "2026-10-25T12:00", Period::Day, 0);
        assert_eq!(stats.buckets.len(), 24);
        assert_eq!(&steps(&stats)[..4], [1, 0, 5, 4]);
        assert_eq!(steps_at(&stats, "2026-10-25T23"), 5);
        assert_eq!(stats.totals.steps, 15);
    }

    #[test]
    fn a_minute_late_in_the_local_day_stays_in_that_day() {
        let store = store_with(&[
            (local("2026-10-04T23:59"), 7),
            (local("2026-10-05T00:30"), 9),
        ]);
        assert_eq!(local("2026-10-05T00:30"), utc("2026-10-04T22:30:00Z"));
        let month = stats(&store, "2026-10-04T12:00", Period::Month, 0);
        assert_eq!(steps_at(&month, "2026-10-04"), 7);
        assert_eq!(steps_at(&month, "2026-10-05"), 9);
        let day = stats(&store, "2026-10-04T12:00", Period::Day, 0);
        assert_eq!(steps_at(&day, "2026-10-04T23"), 7);
        assert_eq!(day.totals.steps, 7);
        let next_day = stats(&store, "2026-10-05T12:00", Period::Day, 0);
        assert_eq!(steps_at(&next_day, "2026-10-05T00"), 9);
        assert_eq!(next_day.totals.steps, 9);
    }

    #[test]
    fn buckets_with_time_but_no_steps_are_not_active() {
        let store = store_with(&[
            (local("2026-10-04T09:00"), 0),
            (local("2026-10-04T10:00"), 4),
        ]);
        let stats = stats(&store, "2026-10-04T12:00", Period::Day, 0);
        assert_eq!(stats.active_buckets(), 1);
        assert_eq!(stats.totals.active_s, 120);
    }
}
