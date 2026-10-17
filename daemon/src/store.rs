//! SQLite persistence for sessions, activity and settings.

use jiff::Timestamp;
use rusqlite::types::Type;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params};
use std::fmt::Display;
use std::fs::{self, DirBuilder};
use std::io;
use std::iter::Sum;
use std::ops::{AddAssign, Range};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Schema migrations; the database's `user_version` counts those applied.
const MIGRATIONS: [&str; 1] = [r"
CREATE TABLE sessions (
  id INTEGER PRIMARY KEY,
  started_at INTEGER NOT NULL,
  ended_at INTEGER,
  updated_at INTEGER NOT NULL,
  active_s INTEGER NOT NULL DEFAULT 0,
  steps INTEGER NOT NULL DEFAULT 0,
  distance_m INTEGER NOT NULL DEFAULT 0,
  kcal REAL NOT NULL DEFAULT 0,
  max_kmh REAL NOT NULL DEFAULT 0,
  estimated INTEGER NOT NULL DEFAULT 0,
  device TEXT
) STRICT;
CREATE INDEX sessions_started_at ON sessions (started_at);

CREATE TABLE minutes (
  minute INTEGER PRIMARY KEY,
  active_s INTEGER NOT NULL,
  steps INTEGER NOT NULL,
  distance_m INTEGER NOT NULL,
  kcal REAL NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE devices (
  address TEXT PRIMARY KEY,
  name TEXT,
  pad_time INTEGER, pad_dist INTEGER, pad_steps INTEGER,
  seen_at INTEGER
) STRICT, WITHOUT ROWID;

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT, WITHOUT ROWID;
"];

const SESSION: &str =
    "id, started_at, ended_at, updated_at, active_s, steps, distance_m, kcal, max_kmh, estimated";

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("the database has schema version {found}; this ks-fit knows versions up to {known}")]
    Schema { found: i64, known: usize },
    #[error("the database journal mode is {0}, not wal")]
    JournalMode(String),
    #[error("setting {key} holds {value:?}, which is not a valid value")]
    Setting { key: &'static str, value: String },
}

#[derive(Debug, thiserror::Error)]
pub enum DataDirError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{} is a symlink", .0.display())]
    Symlink(PathBuf),
    #[error("{} is not a directory", .0.display())]
    NotADirectory(PathBuf),
    #[error("{} belongs to uid {owner}, not to uid {uid}", path.display())]
    Foreign { path: PathBuf, owner: u32, uid: u32 },
}

/// Creates the data directory `dir` with mode 0700 when it is missing,
/// refuses one that is a symlink, not a directory or not ours, and returns
/// its canonical path, since [`Store::open`] refuses a path through any
/// symlink, including a symlinked home.
pub fn prepare_data_dir(dir: &Path) -> Result<PathBuf, DataDirError> {
    let metadata = match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(io_error(dir))?;
            fs::symlink_metadata(dir).map_err(io_error(dir))?
        }
        metadata => metadata.map_err(io_error(dir))?,
    };
    if metadata.file_type().is_symlink() {
        return Err(DataDirError::Symlink(dir.to_owned()));
    }
    if !metadata.is_dir() {
        return Err(DataDirError::NotADirectory(dir.to_owned()));
    }
    let uid = effective_uid()?;
    if metadata.uid() != uid {
        return Err(DataDirError::Foreign {
            path: dir.to_owned(),
            owner: metadata.uid(),
            uid,
        });
    }
    fs::canonicalize(dir).map_err(io_error(dir))
}

fn effective_uid() -> Result<u32, DataDirError> {
    let path = Path::new("/proc/self/status");
    fs::read_to_string(path)
        .map_err(io_error(path))?
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|ids| ids.split_whitespace().nth(1)?.parse().ok())
        .ok_or_else(|| DataDirError::Io {
            path: path.to_owned(),
            source: io::Error::new(io::ErrorKind::InvalidData, "no effective uid"),
        })
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> DataDirError + '_ {
    move |source| DataDirError::Io {
        path: path.to_owned(),
        source,
    }
}

/// A reading of the pad's cumulative counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub time_s: u32,
    pub dist_10m: u32,
    pub steps: u32,
}

/// The counters a pad showed when it was last seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Baseline {
    pub counters: Counters,
    pub seen_at: Timestamp,
}

/// Activity over some stretch of time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Activity {
    pub active_s: u32,
    pub steps: u32,
    pub distance_m: u32,
    pub kcal: f64,
}

impl AddAssign for Activity {
    fn add_assign(&mut self, other: Activity) {
        self.active_s = self.active_s.saturating_add(other.active_s);
        self.steps = self.steps.saturating_add(other.steps);
        self.distance_m = self.distance_m.saturating_add(other.distance_m);
        self.kcal += other.kcal;
    }
}

impl Sum for Activity {
    fn sum<I: Iterator<Item = Activity>>(activities: I) -> Activity {
        activities.fold(Activity::default(), |mut total, activity| {
            total += activity;
            total
        })
    }
}

/// A stretch of consecutive walking.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub id: i64,
    pub started_at: Timestamp,
    /// The time of the last activity.
    pub updated_at: Timestamp,
    /// `None` while the session is open.
    pub ended_at: Option<Timestamp>,
    pub activity: Activity,
    pub max_kmh: f64,
    /// Whether the session was caught up long after the pad was last seen,
    /// so that only its activity is known, not when it happened.
    pub estimated: bool,
}

impl Session {
    /// The average belt speed over the time the belt moved.
    pub fn avg_kmh(&self) -> f64 {
        if self.activity.active_s == 0 {
            return 0.0;
        }
        f64::from(self.activity.distance_m) / f64::from(self.activity.active_s) * 3.6
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub address: String,
    pub name: Option<String>,
}

pub struct Store {
    conn: Connection,
}

/// Makes the store calls issued while it is alive one transaction, rolled
/// back unless committed. Store calls that run their own transaction fail
/// while it is alive.
pub struct Transaction<'a>(rusqlite::Transaction<'a>);

impl Transaction<'_> {
    pub fn commit(self) -> Result<()> {
        Ok(self.0.commit()?)
    }
}

impl Store {
    /// Opens the database at `path`, creating and migrating it as needed.
    /// `path` must not lead through a symlink.
    pub fn open(path: &Path) -> Result<Store> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let conn = Connection::open_with_flags(path, flags)?;
        let mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        if mode != "wal" {
            return Err(Error::JournalMode(mode));
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Store::migrate(conn)
    }

    pub fn open_in_memory() -> Result<Store> {
        Store::migrate(Connection::open_in_memory()?)
    }

    fn migrate(mut conn: Connection) -> Result<Store> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let found: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let known = MIGRATIONS.len();
        let applied = usize::try_from(found)
            .ok()
            .filter(|&applied| applied <= known)
            .ok_or(Error::Schema { found, known })?;
        for (version, sql) in (1_i64..).zip(MIGRATIONS).skip(applied) {
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", version)?;
        }
        tx.commit()?;
        Ok(Store { conn })
    }

    pub fn transaction(&self) -> Result<Transaction<'_>> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        Ok(Transaction(tx))
    }

    pub fn weight_kg(&self) -> Result<Option<f64>> {
        self.number("weight_kg")
    }

    pub fn set_weight_kg(&self, kg: f64) -> Result<()> {
        self.put("weight_kg", kg)
    }

    pub fn auto_connect(&self) -> Result<Option<bool>> {
        self.setting("auto_connect")
    }

    pub fn set_auto_connect(&self, on: bool) -> Result<()> {
        self.put("auto_connect", on)
    }

    pub fn target_kmh(&self) -> Result<Option<f64>> {
        self.number("target_kmh")
    }

    pub fn set_target_kmh(&self, kmh: f64) -> Result<()> {
        self.put("target_kmh", kmh)
    }

    /// The pad to connect to automatically.
    pub fn remembered_device(&self) -> Result<Option<Device>> {
        let device = self
            .conn
            .query_row(
                "SELECT s.value, d.name FROM settings s LEFT JOIN devices d ON d.address = s.value
                 WHERE s.key = 'device'",
                [],
                |row| {
                    Ok(Device {
                        address: row.get(0)?,
                        name: row.get(1)?,
                    })
                },
            )
            .optional()?;
        Ok(device)
    }

    /// Remembers `address` in place of any other pad, keeping the name
    /// already known for it when `name` is `None`.
    pub fn remember_device(&self, address: &str, name: Option<&str>) -> Result<()> {
        let tx = self.transaction()?;
        self.conn.execute(
            "INSERT INTO devices (address, name) VALUES (?1, ?2)
             ON CONFLICT (address) DO UPDATE SET name = coalesce(excluded.name, name)",
            params![address, name],
        )?;
        self.put("device", address)?;
        tx.commit()
    }

    pub fn forget_device(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM settings WHERE key = 'device'", [])?;
        Ok(())
    }

    /// The counters last seen from the pad at `address`, and when.
    pub fn baseline(&self, address: &str) -> Result<Option<Baseline>> {
        let baseline = self
            .conn
            .query_row(
                "SELECT pad_time, pad_dist, pad_steps, seen_at FROM devices WHERE address = ?1",
                [address],
                |row| {
                    let seen_at: Option<i64> = row.get(3)?;
                    let seen_at = seen_at.map(|second| timestamp(3, second)).transpose()?;
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, seen_at))
                },
            )
            .optional()?;
        let Some((Some(time_s), Some(dist_10m), Some(steps), Some(seen_at))) = baseline else {
            return Ok(None);
        };
        Ok(Some(Baseline {
            counters: Counters {
                time_s,
                dist_10m,
                steps,
            },
            seen_at,
        }))
    }

    pub fn set_baseline(
        &self,
        address: &str,
        counters: Counters,
        seen_at: Timestamp,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO devices (address, pad_time, pad_dist, pad_steps, seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (address) DO UPDATE SET pad_time = excluded.pad_time,
               pad_dist = excluded.pad_dist, pad_steps = excluded.pad_steps,
               seen_at = excluded.seen_at",
            params![
                address,
                counters.time_s,
                counters.dist_10m,
                counters.steps,
                seen_at.as_second()
            ],
        )?;
        Ok(())
    }

    /// Inserts `session`, ignoring its `id`, and returns the id it got.
    pub fn insert_session(&self, session: &Session, device: &str) -> Result<i64> {
        let activity = &session.activity;
        self.conn.execute(
            "INSERT INTO sessions (started_at, ended_at, updated_at, active_s, steps, distance_m,
               kcal, max_kmh, estimated, device)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                session.started_at.as_second(),
                session.ended_at.map(Timestamp::as_second),
                session.updated_at.as_second(),
                activity.active_s,
                activity.steps,
                activity.distance_m,
                activity.kcal,
                session.max_kmh,
                session.estimated,
                device
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_session(&self, session: &Session) -> Result<()> {
        let activity = &session.activity;
        self.conn.execute(
            "UPDATE sessions SET ended_at = ?2, updated_at = ?3, active_s = ?4, steps = ?5,
               distance_m = ?6, kcal = ?7, max_kmh = ?8
             WHERE id = ?1",
            params![
                session.id,
                session.ended_at.map(Timestamp::as_second),
                session.updated_at.as_second(),
                activity.active_s,
                activity.steps,
                activity.distance_m,
                activity.kcal,
                session.max_kmh
            ],
        )?;
        Ok(())
    }

    /// Ends every open session at its last activity, and returns how many
    /// there were.
    pub fn close_open_sessions(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE sessions SET ended_at = updated_at WHERE ended_at IS NULL",
            [],
        )?)
    }

    pub fn latest_closed_session(&self) -> Result<Option<Session>> {
        let session = self
            .conn
            .query_row(
                &format!(
                    "SELECT {SESSION} FROM sessions WHERE ended_at IS NOT NULL
                     ORDER BY ended_at DESC, id DESC LIMIT 1"
                ),
                [],
                session,
            )
            .optional()?;
        Ok(session)
    }

    pub fn sessions_started(&self, range: Range<Timestamp>) -> Result<u32> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM sessions WHERE started_at >= ?1 AND started_at < ?2",
            [range.start.as_second(), range.end.as_second()],
            |row| row.get(0),
        )?)
    }

    /// Adds `activity` to the UTC minute containing `at`.
    pub fn add_minute(&self, at: Timestamp, activity: &Activity) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT INTO minutes (minute, active_s, steps, distance_m, kcal)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (minute) DO UPDATE SET active_s = active_s + excluded.active_s,
                   steps = steps + excluded.steps, distance_m = distance_m + excluded.distance_m,
                   kcal = kcal + excluded.kcal",
            )?
            .execute(params![
                at.as_second().div_euclid(60),
                activity.active_s,
                activity.steps,
                activity.distance_m,
                activity.kcal
            ])?;
        Ok(())
    }

    /// The UTC minutes that start within `range`, in order, with their
    /// starts.
    pub fn minutes(&self, range: Range<Timestamp>) -> Result<Vec<(Timestamp, Activity)>> {
        let first = |at: Timestamp| (at.as_second() + 59).div_euclid(60);
        let mut statement = self.conn.prepare_cached(
            "SELECT minute, active_s, steps, distance_m, kcal FROM minutes
             WHERE minute >= ?1 AND minute < ?2 ORDER BY minute",
        )?;
        let rows = statement.query_map([first(range.start), first(range.end)], |row| {
            let minute: i64 = row.get(0)?;
            let start = minute.checked_mul(60).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    Type::Integer,
                    format!("minute {minute} is out of range").into(),
                )
            })?;
            Ok((timestamp(0, start)?, activity(row, 1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn setting<T: FromStr>(&self, key: &'static str) -> Result<Option<T>> {
        let value: Option<String> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?;
        value
            .map(|value| value.parse().map_err(|_| Error::Setting { key, value }))
            .transpose()
    }

    fn number(&self, key: &'static str) -> Result<Option<f64>> {
        match self.setting::<f64>(key)? {
            Some(value) if !value.is_finite() => Err(Error::Setting {
                key,
                value: value.to_string(),
            }),
            value => Ok(value),
        }
    }

    fn put(&self, key: &str, value: impl Display) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value.to_string()],
        )?;
        Ok(())
    }
}

fn session(row: &Row<'_>) -> rusqlite::Result<Session> {
    let ended_at: Option<i64> = row.get(2)?;
    Ok(Session {
        id: row.get(0)?,
        started_at: timestamp(1, row.get(1)?)?,
        ended_at: ended_at.map(|second| timestamp(2, second)).transpose()?,
        updated_at: timestamp(3, row.get(3)?)?,
        activity: activity(row, 4)?,
        max_kmh: row.get(8)?,
        estimated: row.get(9)?,
    })
}

fn activity(row: &Row<'_>, first: usize) -> rusqlite::Result<Activity> {
    Ok(Activity {
        active_s: row.get(first)?,
        steps: row.get(first + 1)?,
        distance_m: row.get(first + 2)?,
        kcal: row.get(first + 3)?,
    })
}

fn timestamp(column: usize, second: i64) -> rusqlite::Result<Timestamp> {
    Timestamp::from_second(second)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(column, Type::Integer, Box::new(e)))
}

#[cfg(test)]
pub(crate) mod testing {
    use std::fs;
    use std::path::PathBuf;

    pub(crate) struct TempDir(pub(crate) PathBuf);

    impl TempDir {
        pub(crate) fn new(name: &str) -> TempDir {
            let path = std::env::temp_dir().join(format!("ks-fit-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        pub(crate) fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::TempDir;
    use super::*;
    use rusqlite::ffi;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(second).unwrap()
    }

    fn walk(steps: u32, kcal: f64) -> Activity {
        Activity {
            active_s: 60,
            steps,
            distance_m: steps * 3 / 4,
            kcal,
        }
    }

    fn open_session(started_at: i64, updated_at: i64) -> Session {
        Session {
            id: 0,
            started_at: at(started_at),
            updated_at: at(updated_at),
            ended_at: None,
            activity: walk(90, 4.2),
            max_kmh: 3.5,
            estimated: false,
        }
    }

    fn holding(key: &str, value: &str) -> Store {
        let store = Store::open_in_memory().unwrap();
        store.put(key, value).unwrap();
        store
    }

    fn user_version(store: &Store) -> i64 {
        store
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn a_missing_data_dir_is_created_private() {
        let root = TempDir::new("data-dir-missing");
        let dir = root.join("share/omarchy-ks-fit");
        let prepared = prepare_data_dir(&dir).unwrap();
        assert_eq!(prepared, fs::canonicalize(&dir).unwrap());
        assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
    }

    #[test]
    fn an_existing_data_dir_is_kept() {
        let root = TempDir::new("data-dir-existing");
        let dir = root.join("omarchy-ks-fit");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("ks-fit.db"), "").unwrap();
        assert_eq!(
            prepare_data_dir(&dir).unwrap(),
            fs::canonicalize(&dir).unwrap()
        );
        assert!(dir.join("ks-fit.db").exists());
    }

    #[test]
    fn a_symlinked_data_dir_is_refused() {
        let root = TempDir::new("data-dir-symlink");
        fs::create_dir(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("omarchy-ks-fit")).unwrap();
        assert!(matches!(
            prepare_data_dir(&root.join("omarchy-ks-fit")),
            Err(DataDirError::Symlink(_))
        ));
    }

    #[test]
    fn a_data_dir_that_is_a_file_is_refused() {
        let root = TempDir::new("data-dir-file");
        fs::write(root.join("omarchy-ks-fit"), "").unwrap();
        assert!(matches!(
            prepare_data_dir(&root.join("omarchy-ks-fit")),
            Err(DataDirError::NotADirectory(_))
        ));
    }

    #[test]
    fn a_data_dir_owned_by_someone_else_is_refused() {
        let foreign = Path::new("/");
        let owner = fs::metadata(foreign).unwrap().uid();
        let uid = effective_uid().unwrap();
        if owner == uid {
            return;
        }
        assert!(matches!(
            prepare_data_dir(foreign),
            Err(DataDirError::Foreign { owner: o, uid: u, .. }) if (o, u) == (owner, uid)
        ));
    }

    #[test]
    fn the_effective_uid_owns_the_files_we_create() {
        let root = TempDir::new("data-dir-uid");
        fs::write(root.join("mine"), "").unwrap();
        assert_eq!(
            effective_uid().unwrap(),
            fs::metadata(root.join("mine")).unwrap().uid()
        );
    }

    #[test]
    fn a_data_dir_under_a_symlinked_parent_opens_by_its_real_path() {
        let root = TempDir::new("data-dir-parent");
        fs::create_dir_all(root.join("real/omarchy-ks-fit")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("home")).unwrap();
        let dir = root.join("home/omarchy-ks-fit");
        assert!(Store::open(&dir.join("ks-fit.db")).is_err());
        let prepared = prepare_data_dir(&dir).unwrap();
        assert_eq!(
            prepared,
            fs::canonicalize(root.join("real/omarchy-ks-fit")).unwrap()
        );
        assert!(Store::open(&prepared.join("ks-fit.db")).is_ok());
    }

    #[test]
    fn a_new_database_is_migrated_to_the_latest_version() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(user_version(&store), 1);
        let mut statement = store
            .conn
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .unwrap();
        let tables: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(tables, ["devices", "minutes", "sessions", "settings"]);
    }

    #[test]
    fn reopening_a_database_keeps_its_data_and_version() {
        let dir = TempDir::new("store-reopen");
        let path = dir.join("ks-fit.db");
        Store::open(&path).unwrap().set_weight_kg(72.5).unwrap();
        let store = Store::open(&path).unwrap();
        assert_eq!(user_version(&store), 1);
        assert_eq!(store.weight_kg().unwrap(), Some(72.5));
    }

    #[test]
    fn file_databases_use_wal_with_normal_sync() {
        let dir = TempDir::new("store-wal");
        let store = Store::open(&dir.join("ks-fit.db")).unwrap();
        let journal_mode: String = store
            .conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        let synchronous: i64 = store
            .conn
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();
        assert_eq!((journal_mode.as_str(), synchronous), ("wal", 1));
    }

    #[test]
    fn unknown_schema_versions_are_refused() {
        let dir = TempDir::new("store-schema");
        let path = dir.join("ks-fit.db");
        for found in [2, -1] {
            Connection::open(&path)
                .unwrap()
                .pragma_update(None, "user_version", found)
                .unwrap();
            let error = Store::open(&path).err().unwrap();
            assert!(
                matches!(error, Error::Schema { found: f, known: 1 } if f == found),
                "{error}"
            );
        }
    }

    #[test]
    fn a_database_behind_a_symlink_is_refused() {
        let dir = TempDir::new("store-symlink");
        let target = dir.join("elsewhere.db");
        let link = dir.join("ks-fit.db");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let error = Store::open(&link).err().unwrap();
        let code = match &error {
            Error::Sqlite(e) => e.sqlite_error().map(|e| e.extended_code),
            _ => None,
        };
        assert_eq!(code, Some(ffi::SQLITE_CANTOPEN_SYMLINK), "{error}");
        assert!(!target.exists());
    }

    #[test]
    fn settings_start_unset_and_round_trip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.weight_kg().unwrap(), None);
        assert_eq!(store.auto_connect().unwrap(), None);
        assert_eq!(store.target_kmh().unwrap(), None);
        store.set_weight_kg(81.3).unwrap();
        store.set_auto_connect(false).unwrap();
        store.set_target_kmh(3.5).unwrap();
        store.set_target_kmh(4.1).unwrap();
        assert_eq!(store.weight_kg().unwrap(), Some(81.3));
        assert_eq!(store.auto_connect().unwrap(), Some(false));
        assert_eq!(store.target_kmh().unwrap(), Some(4.1));
    }

    #[test]
    fn an_unreadable_number_setting_is_an_error() {
        for value in ["heavy", "NaN", "inf"] {
            assert!(
                matches!(
                    holding("weight_kg", value).weight_kg(),
                    Err(Error::Setting {
                        key: "weight_kg",
                        ..
                    })
                ),
                "{value}"
            );
        }
        assert!(matches!(
            holding("target_kmh", "-inf").target_kmh(),
            Err(Error::Setting {
                key: "target_kmh",
                ..
            })
        ));
    }

    #[test]
    fn an_unreadable_switch_setting_is_an_error() {
        assert!(matches!(
            holding("auto_connect", "yes").auto_connect(),
            Err(Error::Setting {
                key: "auto_connect",
                ..
            })
        ));
    }

    #[test]
    fn remembering_a_device_replaces_the_previous_one() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.remembered_device().unwrap(), None);
        store
            .remember_device("AA:BB:CC:DD:EE:01", Some("KS-ST-A1P"))
            .unwrap();
        store.remember_device("AA:BB:CC:DD:EE:02", None).unwrap();
        assert_eq!(
            store.remembered_device().unwrap(),
            Some(Device {
                address: "AA:BB:CC:DD:EE:02".into(),
                name: None
            })
        );
    }

    #[test]
    fn remembering_a_device_without_a_name_keeps_the_known_one() {
        let store = Store::open_in_memory().unwrap();
        store
            .remember_device("AA:BB:CC:DD:EE:01", Some("KS-ST-A1P"))
            .unwrap();
        store.remember_device("AA:BB:CC:DD:EE:01", None).unwrap();
        let device = store.remembered_device().unwrap().unwrap();
        assert_eq!(device.name.as_deref(), Some("KS-ST-A1P"));
    }

    #[test]
    fn forgetting_the_device_keeps_its_baseline() {
        let store = Store::open_in_memory().unwrap();
        let counters = Counters {
            time_s: 600,
            dist_10m: 55,
            steps: 870,
        };
        store
            .set_baseline("AA:BB:CC:DD:EE:01", counters, at(1_000))
            .unwrap();
        store.remember_device("AA:BB:CC:DD:EE:01", None).unwrap();
        store.forget_device().unwrap();
        assert_eq!(store.remembered_device().unwrap(), None);
        assert_eq!(
            store.baseline("AA:BB:CC:DD:EE:01").unwrap(),
            Some(Baseline {
                counters,
                seen_at: at(1_000)
            })
        );
    }

    #[test]
    fn baselines_are_kept_per_device() {
        let store = Store::open_in_memory().unwrap();
        let first = Counters {
            time_s: 1,
            dist_10m: 2,
            steps: 3,
        };
        let second = Counters {
            time_s: 40,
            dist_10m: 50,
            steps: 60,
        };
        store.set_baseline("pad-1", first, at(1_000)).unwrap();
        store.set_baseline("pad-2", first, at(1_000)).unwrap();
        store.set_baseline("pad-2", second, at(1_001)).unwrap();
        let seen = |counters, second| {
            Some(Baseline {
                counters,
                seen_at: at(second),
            })
        };
        assert_eq!(store.baseline("pad-1").unwrap(), seen(first, 1_000));
        assert_eq!(store.baseline("pad-2").unwrap(), seen(second, 1_001));
        assert_eq!(store.baseline("pad-3").unwrap(), None);
    }

    #[test]
    fn a_device_known_only_by_name_has_no_baseline() {
        let store = Store::open_in_memory().unwrap();
        store.remember_device("pad-1", Some("KS-ST-A1P")).unwrap();
        assert_eq!(store.baseline("pad-1").unwrap(), None);
    }

    #[test]
    fn minutes_accumulate_and_come_back_in_order() {
        let store = Store::open_in_memory().unwrap();
        store.add_minute(at(6_030), &walk(100, 4.0)).unwrap();
        store.add_minute(at(6_000), &walk(20, 0.5)).unwrap();
        store.add_minute(at(5_999), &walk(7, 0.25)).unwrap();
        assert_eq!(
            store.minutes(at(5_940)..at(6_060)).unwrap(),
            [
                (at(5_940), walk(7, 0.25)),
                (
                    at(6_000),
                    Activity {
                        active_s: 120,
                        steps: 120,
                        distance_m: 75 + 15,
                        kcal: 4.5
                    }
                ),
            ]
        );
    }

    #[test]
    fn a_range_holds_only_the_minutes_starting_inside_it() {
        let store = Store::open_in_memory().unwrap();
        for second in [5_940, 6_000, 6_060] {
            store.add_minute(at(second), &walk(10, 1.0)).unwrap();
        }
        let starts = |range| -> Vec<Timestamp> {
            store
                .minutes(range)
                .unwrap()
                .into_iter()
                .map(|(start, _)| start)
                .collect()
        };
        assert_eq!(starts(at(5_941)..at(6_060)), [at(6_000)]);
        assert_eq!(
            starts(at(5_940)..at(6_061)),
            [at(5_940), at(6_000), at(6_060)]
        );
    }

    #[test]
    fn corrupt_stored_activity_is_an_error() {
        let store = Store::open_in_memory().unwrap();
        store
            .conn
            .execute(
                "INSERT INTO minutes VALUES (100, 60, -5, 10, 1.0), (101, 60, 5, 10, 1.0)",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.minutes(at(6_000)..at(6_060)),
            Err(Error::Sqlite(_))
        ));
        assert!(store.minutes(at(6_060)..at(6_120)).is_ok());
    }

    #[test]
    fn corrupt_stored_times_are_an_error() {
        let store = Store::open_in_memory().unwrap();
        store
            .conn
            .execute(
                "INSERT INTO sessions (started_at, ended_at, updated_at) VALUES (0, ?1, 0)",
                [i64::MAX],
            )
            .unwrap();
        assert!(matches!(
            store.latest_closed_session(),
            Err(Error::Sqlite(_))
        ));
    }

    #[test]
    fn sessions_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let mut session = Session {
            estimated: true,
            ..open_session(1_000, 1_600)
        };
        session.id = store.insert_session(&session, "pad-1").unwrap();
        session.activity = walk(900, 40.0);
        session.updated_at = at(1_900);
        session.ended_at = Some(at(1_900));
        session.max_kmh = 4.5;
        store.update_session(&session).unwrap();
        assert_eq!(store.latest_closed_session().unwrap(), Some(session));
    }

    #[test]
    fn the_latest_closed_session_is_the_one_that_ended_last() {
        let store = Store::open_in_memory().unwrap();
        let mut earlier = open_session(1_000, 1_600);
        earlier.ended_at = Some(at(1_600));
        store.insert_session(&earlier, "pad-1").unwrap();
        let mut later = open_session(2_000, 2_600);
        later.ended_at = Some(at(2_600));
        later.id = store.insert_session(&later, "pad-1").unwrap();
        store
            .insert_session(&open_session(3_000, 3_100), "pad-1")
            .unwrap();
        assert_eq!(store.latest_closed_session().unwrap(), Some(later));
    }

    #[test]
    fn closing_open_sessions_ends_them_at_their_last_activity() {
        let store = Store::open_in_memory().unwrap();
        let mut session = open_session(1_000, 1_600);
        session.id = store.insert_session(&session, "pad-1").unwrap();
        assert_eq!(store.close_open_sessions().unwrap(), 1);
        assert_eq!(store.close_open_sessions().unwrap(), 0);
        let closed = store.latest_closed_session().unwrap().unwrap();
        assert_eq!(
            closed,
            Session {
                ended_at: Some(at(1_600)),
                ..session
            }
        );
    }

    #[test]
    fn sessions_are_counted_by_when_they_started() {
        let store = Store::open_in_memory().unwrap();
        for started_at in [999, 1_000, 1_500, 2_000] {
            store
                .insert_session(&open_session(started_at, started_at + 60), "pad-1")
                .unwrap();
        }
        assert_eq!(store.sessions_started(at(1_000)..at(2_000)).unwrap(), 2);
    }

    #[test]
    fn activity_adds_up_without_overflowing() {
        let mut total: Activity = [walk(100, 4.0), walk(20, 0.5)].into_iter().sum();
        assert_eq!(
            total,
            Activity {
                active_s: 120,
                steps: 120,
                distance_m: 90,
                kcal: 4.5
            }
        );
        total += Activity {
            steps: u32::MAX,
            ..Activity::default()
        };
        assert_eq!(total.steps, u32::MAX);
    }

    #[test]
    fn average_speed_is_distance_over_belt_time() {
        let mut session = open_session(1_000, 1_600);
        session.activity.active_s = 1_800;
        session.activity.distance_m = 2_100;
        assert!((session.avg_kmh() - 4.2).abs() < 1e-12);
        session.activity.active_s = 0;
        assert_eq!(session.avg_kmh(), 0.0);
    }

    #[test]
    fn an_uncommitted_transaction_leaves_nothing_behind() {
        let store = Store::open_in_memory().unwrap();
        let tx = store.transaction().unwrap();
        store.add_minute(at(6_000), &walk(10, 1.0)).unwrap();
        store.set_weight_kg(90.0).unwrap();
        drop(tx);
        assert_eq!(store.minutes(at(0)..at(10_000)).unwrap(), []);
        assert_eq!(store.weight_kg().unwrap(), None);
    }

    #[test]
    fn a_committed_transaction_keeps_every_write() {
        let store = Store::open_in_memory().unwrap();
        let tx = store.transaction().unwrap();
        store.add_minute(at(6_000), &walk(10, 1.0)).unwrap();
        store.set_weight_kg(90.0).unwrap();
        tx.commit().unwrap();
        assert_eq!(store.minutes(at(0)..at(10_000)).unwrap().len(), 1);
        assert_eq!(store.weight_kg().unwrap(), Some(90.0));
    }

    #[test]
    fn a_store_call_with_its_own_transaction_fails_inside_another() {
        let store = Store::open_in_memory().unwrap();
        let _tx = store.transaction().unwrap();
        assert!(store.remember_device("pad-1", None).is_err());
    }
}
