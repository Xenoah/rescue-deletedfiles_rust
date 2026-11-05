//! Optional compound orders are built in the work index, never on the source.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

impl IndexQuery {
    fn order_index(&self) -> (String, bool) {
        let column = self.sort.column();
        if self.group == IndexGroup::None {
            let suffix = if self.descending && self.sort.rank() != "zero_rank" {
                "_desc"
            } else {
                ""
            };
            (format!("candidate_{column}{suffix}"), false)
        } else if self.sort == IndexSort::Name && !self.descending {
            (format!("group_{}", self.group.column()), false)
        } else {
            (
                format!(
                    "query_{}_{}_{}",
                    self.group.column(),
                    column,
                    if self.descending { "desc" } else { "asc" }
                ),
                true,
            )
        }
    }
}

impl CandidateIndex {
    /// Build a requested compound order in the private work database. Call from
    /// a cancellable worker before `page`; the read-only page API never writes.
    /// Existing scan ownership prevents concurrent schema changes. SQLite rolls
    /// back interrupted/failed builds; completed indexes are reusable caches.
    pub fn prepare_query(&self, query: &IndexQuery, cancel: &Arc<AtomicBool>) -> Result<bool> {
        if query.text.len() > 4096 {
            return Err(Error::Limit("index query".into()));
        }
        self.checkpoint_cache(cancel);
        let (name, optional) = query.order_index();
        if !optional || self.has_order(&name)? || self.status()?.candidates <= 50_000 {
            return Ok(false);
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        self.session()?.guard_output(&self.path)?;
        let _owner = IndexWriter::owner(&self.path)?;
        let mut db = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        configure(&db, true)?;
        let expected = self.status()?.index_id;
        let stop = cancel.clone();
        let started = std::time::Instant::now();
        db.progress_handler(
            1000,
            Some(move || {
                stop.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(600)
            }),
        )?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let actual: String = tx.query_row(
            "SELECT index_id FROM checkpoint WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if actual != expected {
            return Err(Error::Request("index changed before preparation".into()));
        }
        let direction = if query.descending { "DESC" } else { "ASC" };
        let sql = format!(
            "CREATE INDEX IF NOT EXISTS {name} ON candidate({} ASC,{} ASC,{} {direction},seq {direction});",
            query.group.column(),
            query.sort.rank(),
            query.sort.column()
        );
        if let Err(error) = tx.execute_batch(&sql) {
            return Err(if cancel.load(Ordering::Relaxed) {
                Error::Cancelled
            } else {
                error.into()
            });
        }
        tx.commit()?;
        db.progress_handler(0, None::<fn() -> bool>)?;
        // A large CREATE INDEX may leave hundreds of MiB of WAL frames. A
        // future read-only process would otherwise rebuild that WAL mapping.
        let _ = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        });
        Ok(true)
    }
    fn checkpoint_cache(&self, cancel: &Arc<AtomicBool>) {
        let mut wal = self.path.as_os_str().to_os_string();
        wal.push("-wal");
        if fs::metadata(wal).map_or(true, |m| m.len() <= 32 * 1024 * 1024)
            || cancel.load(Ordering::Relaxed)
        {
            return;
        }
        // Optional maintenance must not prevent read-only access or contend
        // with an active scan. Only the already-authorized work index is written.
        let attempt = || -> Result<()> {
            self.session()?.guard_output(&self.path)?;
            let _owner = IndexWriter::owner(&self.path)?;
            let db = Connection::open_with_flags(
                &self.path,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )?;
            configure(&db, true)?;
            let id: String = db.query_row(
                "SELECT index_id FROM checkpoint WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            if id != self.status()?.index_id {
                return Err(Error::Request("index changed before maintenance".into()));
            }
            let _ = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            });
            Ok(())
        };
        let _ = attempt();
    }
    fn has_order(&self, name: &str) -> Result<bool> {
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND tbl_name='candidate' AND name=?1)", [name], |r| r.get(0))?)
    }
    pub(super) fn query_access(&self, query: &IndexQuery, selective: bool) -> Result<String> {
        if selective {
            return Ok("NOT INDEXED".into());
        }
        let (name, optional) = query.order_index();
        if !optional || self.has_order(&name)? {
            Ok(format!("INDEXED BY {name}"))
        } else if self.status()?.candidates <= 50_000 {
            Ok(String::new())
        } else {
            Err(Error::Request("compound result order needs preparation; use prepare_results in MCP, or open it through the CLI/GUI".into()))
        }
    }
}
