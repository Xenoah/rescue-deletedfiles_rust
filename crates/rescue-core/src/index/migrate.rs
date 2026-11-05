//! Copy migration keeps the source database and its resumable scan state intact.
use super::*;
impl IndexWriter {
    pub(super) fn import_index(path: &Path, work: &Path) -> Result<PathBuf> {
        regular(path)?;
        let path = path.canonicalize()?;
        let _owner = Self::owner(&path)?;
        let db = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        configure(&db, false)?;
        db.execute_batch("BEGIN")?;
        let app: i32 = db.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let schema: i32 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if app != APP_ID || !(1..=SCHEMA).contains(&schema) {
            return Err(Error::Unsupported("unknown index migration schema".into()));
        }
        let source = CandidateIndex { db, path };
        let header = source.session()?;
        uuid::Uuid::parse_str(&header.id).map_err(|_| Error::Request("index session ID".into()))?;
        if !(1..=9).contains(&header.schema_version) || !header.candidates.is_empty() {
            return Err(Error::Request("migration header".into()));
        }
        let status = source.status()?;
        if status.candidates > 10_000_000 {
            return Err(Error::Limit("migration candidate count".into()));
        }
        let checkpoint: serde_json::Value = source.checkpoint()?;
        let mut writer = Self::create(work, &header, &status.engine, &checkpoint)?;
        // The new copy is not resumable until every committed candidate and
        // internal staging row from this single read snapshot has been copied.
        writer.commit(&header, &[], "running", false, &checkpoint)?;
        let mut statement = source
            .db
            .prepare("SELECT seq,payload FROM candidate WHERE seq<=?1 ORDER BY seq")?;
        let mut rows = statement.query([status.candidates as i64])?;
        let mut count = 0u64;
        let mut batch = vec![];
        let mut bytes = 0usize;
        while let Some(row) = rows.next()? {
            let sequence: i64 = row.get(0)?;
            let payload: String = row.get(1)?;
            if sequence < 1 || sequence as u64 != count + 1 || payload.len() > MAX_ROW {
                return Err(Error::Request("migration sequence or payload".into()));
            }
            if !batch.is_empty() && (batch.len() >= 128 || bytes + payload.len() > 16 * 1024 * 1024)
            {
                writer.commit(&header, &batch, "running", false, &checkpoint)?;
                batch.clear();
                bytes = 0;
            }
            bytes += payload.len();
            batch.push(serde_json::from_str::<Candidate>(&payload)?);
            count += 1;
        }
        if count != status.candidates {
            return Err(Error::Request("migration candidate watermark".into()));
        }
        writer.commit(&header, &batch, "running", false, &checkpoint)?;
        let staging: bool = source.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='engine_item')",
            [],
            |r| r.get(0),
        )?;
        if staging {
            let mut statement = source
                .db
                .prepare("SELECT namespace,seq,payload FROM engine_item ORDER BY namespace,seq")?;
            let mut rows = statement.query([])?;
            let mut batch = vec![];
            let mut bytes = 0usize;
            let mut count = 0u64;
            while let Some(row) = rows.next()? {
                let namespace = nonnegative(row.get(0)?)?;
                let sequence = nonnegative(row.get(1)?)?;
                let payload: String = row.get(2)?;
                if payload.len() > MAX_ROW || namespace >= 256 || count >= 10_000_000 {
                    return Err(Error::Limit("migration staging".into()));
                }
                if !batch.is_empty()
                    && (batch.len() >= 128 || bytes + payload.len() > 16 * 1024 * 1024)
                {
                    writer.commit_staged(&header, &[], "running", false, &checkpoint, &batch)?;
                    batch.clear();
                    bytes = 0;
                }
                bytes += payload.len();
                batch.push(StagedRow {
                    namespace,
                    sequence,
                    payload,
                });
                count += 1;
            }
            writer.commit_staged(&header, &[], "running", false, &checkpoint, &batch)?;
        }
        writer.commit(&header, &[], &status.state, status.resumable, &checkpoint)?;
        Ok(writer.index.path.clone())
    }
}
