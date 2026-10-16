//! The SQLite file behind the knowledge base. Several aibrowse processes (one per
//! profile) may share it, so writes are short immediate transactions in WAL mode.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::record::Record;

/// Schema migrations, in order: the database's `user_version` counts those applied.
/// Append only; never edit a released step.
pub(super) const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE endpoints (
    site TEXT NOT NULL,
    method TEXT NOT NULL,
    -- Template as JSON: origin, path segments, selectors, GraphQL operation.
    template TEXT NOT NULL,
    mutating INTEGER NOT NULL,
    third_party INTEGER NOT NULL,
    query_keys TEXT NOT NULL,
    content_type TEXT,
    request_shape TEXT,
    response_shape TEXT,
    triggers TEXT NOT NULL,
    -- Cookie-sourced header names and their cookie's name; never values.
    headers TEXT NOT NULL,
    with_cookies INTEGER NOT NULL,
    calls INTEGER NOT NULL,
    first_seen INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    PRIMARY KEY (site, method, template)
);
"#,
    r#"
-- Navigation paths that reached a goal (kb/paths.rs): structure only.
CREATE TABLE paths (
    site TEXT NOT NULL,
    -- A hash of the normalized goal; goals are free text and may be personal.
    goal TEXT NOT NULL,
    -- JSON array of element labels, in order.
    steps TEXT NOT NULL,
    uses INTEGER NOT NULL,
    last_used INTEGER NOT NULL,
    PRIMARY KEY (site, goal)
);
"#,
    r#"
-- Earlier builds kept any `q=` value in an endpoint's identity, so search text could be
-- stored ("q": "ada lovelace"). Only identifier-like values (`findAlerts`) name an
-- operation; endpoints keyed by anything else are removed and will be relearned.
DELETE FROM endpoints WHERE EXISTS (
    SELECT 1 FROM json_each(template, '$.selectors') AS s
    WHERE json_extract(s.value, '$[0]') = 'q'
      AND NOT (json_extract(s.value, '$[1]') GLOB '[a-z]*[A-Z]*'
               AND json_extract(s.value, '$[1]') NOT GLOB '*[^a-zA-Z0-9]*')
);
"#,
];

/// Endpoints not seen for this long are dropped: the site's frontend has moved on.
const RETENTION: i64 = 180 * 24 * 3600;
/// Endpoints loaded per site, most recently seen first.
const LOAD_LIMIT: i64 = 300;
/// How long a write waits for another process holding the database.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const COLUMNS: &str = "site, method, template, mutating, third_party, query_keys, content_type, \
    request_shape, response_shape, triggers, headers, with_cookies, calls, last_seen";

#[derive(Debug)]
pub(crate) enum StoreError {
    Io(std::io::Error),
    Sql(rusqlite::Error),
    Json(serde_json::Error),
    /// Written by a newer aibrowse; left alone rather than misread.
    NewerSchema(i64),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Sql(err) => write!(f, "{err}"),
            Self::Json(err) => write!(f, "malformed stored value: {err}"),
            Self::NewerSchema(version) => write!(
                f,
                "schema version {version} is newer than this build knows ({})",
                MIGRATIONS.len()
            ),
        }
    }
}

impl From<std::io::Error> for StoreError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sql(err)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

type Result<T> = std::result::Result<T, StoreError>;

pub(crate) struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating if needed) the database at `path`, migrates it to the current
    /// schema, and drops endpoints nobody has seen in a long time.
    pub(crate) fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
        let mut store = Self { conn };
        store.migrate()?;
        let cutoff = super::unix_now() - RETENTION;
        store.conn.execute(
            "DELETE FROM endpoints WHERE last_seen < ?1",
            params![cutoff],
        )?;
        store
            .conn
            .execute("DELETE FROM paths WHERE last_used < ?1", params![cutoff])?;
        Ok(store)
    }

    /// The stored path to `goal` on `site`.
    pub(crate) fn path(&self, site: &str, goal: &str) -> Result<Option<Vec<String>>> {
        let steps: Option<String> = self
            .conn
            .query_row(
                "SELECT steps FROM paths WHERE site = ?1 AND goal = ?2",
                params![site, goal],
                |row| row.get(0),
            )
            .optional()?;
        Ok(steps.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    /// Drops the path to `goal` on `site`.
    pub(crate) fn delete_path(&mut self, site: &str, goal: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM paths WHERE site = ?1 AND goal = ?2",
            params![site, goal],
        )?;
        Ok(())
    }

    /// Stores (or replaces) the path to `goal` on `site`.
    pub(crate) fn save_path(
        &mut self,
        site: &str,
        goal: &str,
        steps: &[String],
        now: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO paths (site, goal, steps, uses, last_used) VALUES (?1, ?2, ?3, 1, ?4)
             ON CONFLICT (site, goal) DO UPDATE SET steps = ?3, uses = uses + 1, last_used = ?4",
            params![site, goal, serde_json::to_string(steps)?, now],
        )?;
        Ok(())
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?;
        let known = MIGRATIONS.len() as i64;
        if version > known {
            return Err(StoreError::NewerSchema(version));
        }
        for (step, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Another process may have migrated while we waited for the lock.
            let current: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
            if current > step as i64 {
                continue;
            }
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", step as i64 + 1)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// The endpoints known for `site`, most recently seen first.
    pub(crate) fn load(&self, site: &str) -> Result<Vec<Record>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM endpoints WHERE site = ?1 ORDER BY last_seen DESC LIMIT ?2"
        ))?;
        let rows = statement.query_map(params![site, LOAD_LIMIT], |row| Ok(read_row(row)))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row??);
        }
        Ok(records)
    }

    /// Merges what a session learned into the stored records, in one transaction.
    pub(crate) fn save(&mut self, records: Vec<Record>, now: i64) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for record in records {
            let template = serde_json::to_string(&record.template)?;
            let existing = tx
                .query_row(
                    &format!(
                        "SELECT {COLUMNS}, first_seen FROM endpoints WHERE site = ?1 AND method = ?2 AND template = ?3"
                    ),
                    params![record.site, record.method, template],
                    |row| Ok((read_row(row), row.get::<_, i64>(14)?)),
                )
                .optional()?;
            let (merged, first_seen) = match existing {
                Some((stored, first_seen)) => (stored?.absorb(record), first_seen),
                None => (record, now),
            };
            tx.execute(
                &format!(
                    "INSERT OR REPLACE INTO endpoints ({COLUMNS}, first_seen) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"
                ),
                params![
                    merged.site,
                    merged.method,
                    template,
                    merged.mutating,
                    merged.third_party,
                    json(&merged.query_keys)?,
                    merged.content_type,
                    merged.request_shape.as_ref().map(json).transpose()?,
                    merged.response_shape.as_ref().map(json).transpose()?,
                    json(&merged.triggers)?,
                    json(&merged.headers)?,
                    merged.with_cookies,
                    i64::try_from(merged.calls).unwrap_or(i64::MAX),
                    now,
                    first_seen,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// A row in `COLUMNS` order. The outer error is SQLite's, the inner one a malformed
/// stored value.
fn read_row(row: &Row<'_>) -> Result<Record> {
    Ok(Record {
        site: row.get(0)?,
        method: row.get(1)?,
        template: parse(&row.get::<_, String>(2)?)?,
        mutating: row.get(3)?,
        third_party: row.get(4)?,
        query_keys: parse(&row.get::<_, String>(5)?)?,
        content_type: row.get(6)?,
        request_shape: row
            .get::<_, Option<String>>(7)?
            .as_deref()
            .map(parse)
            .transpose()?,
        response_shape: row
            .get::<_, Option<String>>(8)?
            .as_deref()
            .map(parse)
            .transpose()?,
        triggers: parse(&row.get::<_, String>(9)?)?,
        headers: parse(&row.get::<_, String>(10)?)?,
        with_cookies: row.get(11)?,
        calls: u64::try_from(row.get::<_, i64>(12)?).unwrap_or(0),
        last_seen: row.get(13)?,
    })
}

fn json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn parse<T: DeserializeOwned>(text: &str) -> Result<T> {
    Ok(serde_json::from_str(text)?)
}
