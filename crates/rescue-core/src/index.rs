//! Append-only candidate storage. A batch and its scan checkpoint commit together.
//! Readers carry a high-water mark; later commits cannot replace their pages.
use crate::{Candidate, Error, Result, Session};
use rusqlite::{Connection, OpenFlags, OptionalExtension, config::DbConfig, limits::Limit, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    time::Duration,
};

const APP_ID: i32 = 0x52534349;
const SCHEMA: i32 = 1;
const MAX_ROW: usize = 8 * 1024 * 1024;
const MAX_CHECKPOINT: usize = 32 * 1024 * 1024;
pub const MAX_PAGE: usize = 1000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexSort {
    #[default]
    Discovery,
    Name,
    Path,
    Size,
    Created,
    Modified,
}
impl IndexSort {
    fn column(self) -> &'static str {
        match self {
            Self::Discovery => "discovery_key",
            Self::Name => "name_key",
            Self::Path => "path_key",
            Self::Size => "size_key",
            Self::Created => "created_key",
            Self::Modified => "modified_key",
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexQuery {
    pub sort: IndexSort,
    pub descending: bool,
    pub text: String,
    pub supported_only: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexCursor {
    pub index_id: String,
    pub watermark: u64,
    pub query_sha256: String,
    pub last_key: String,
    pub last_sequence: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexPage {
    pub index_id: String,
    /// Number committed at the start of pagination, before applying filters.
    pub watermark: u64,
    pub candidates: Vec<Candidate>,
    pub next_cursor: Option<IndexCursor>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexStatus {
    pub schema_version: u32,
    pub index_id: String,
    pub source: PathBuf,
    pub candidates: u64,
    pub scanned_records: u64,
    pub total_records: u64,
    pub state: String,
    pub engine: String,
    pub resumable: bool,
}
pub struct CandidateIndex {
    db: Connection,
    path: PathBuf,
}
pub(crate) struct StagedRow {
    pub namespace: u64,
    pub sequence: u64,
    pub payload: String,
}
pub struct IndexWriter {
    index: CandidateIndex,
    // A separate stable inode, never removed on close: a deleted lock file would
    // allow two writers to lock different inodes after a restart.
    _owner: File,
}

fn checked_json<T: Serialize>(value: &T, maximum: usize) -> Result<String> {
    if crate::serialized_size(value)? > maximum as u64 {
        return Err(Error::Limit("index row/checkpoint size".into()));
    }
    Ok(serde_json::to_string(value)?)
}
fn configure(db: &Connection, writer: bool) -> Result<()> {
    db.busy_timeout(Duration::from_millis(100))?;
    db.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    db.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)?;
    db.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false)?;
    db.set_limit(Limit::SQLITE_LIMIT_LENGTH, 64 * 1024 * 1024)?;
    db.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 64 * 1024)?;
    db.set_limit(Limit::SQLITE_LIMIT_COLUMN, 64)?;
    db.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    db.set_limit(Limit::SQLITE_LIMIT_WORKER_THREADS, 0)?;
    db.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA cache_size=-8192; PRAGMA mmap_size=0; PRAGMA temp_store=FILE;")?;
    if writer {
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA wal_autocheckpoint=256;",
        )?;
    } else {
        db.execute_batch("PRAGMA query_only=ON;")?;
    }
    Ok(())
}
fn nonnegative(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| Error::Request("negative index sequence".into()))
}
fn regular(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_file() || m.file_type().is_symlink() {
        return Err(Error::Request(
            "index must be a regular file, not a link".into(),
        ));
    }
    Ok(())
}
impl CandidateIndex {
    pub fn open(path: &Path) -> Result<Self> {
        regular(path)?;
        let path = path.canonicalize()?;
        let db = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        configure(&db, false)?;
        let index = Self { db, path };
        index.validate()?;
        Ok(index)
    }
    fn validate(&self) -> Result<()> {
        let app: i32 = self
            .db
            .query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let schema: i32 = self.db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if app != APP_ID || schema != SCHEMA {
            return Err(Error::Unsupported(
                "index schema; keep the original and migrate into a new copy".into(),
            ));
        }
        let session = self.session()?;
        uuid::Uuid::parse_str(&session.id)
            .map_err(|_| Error::Request("index session ID".into()))?;
        if !(1..=9).contains(&session.schema_version) || !session.candidates.is_empty() {
            return Err(Error::Request("invalid index header".into()));
        }
        Ok(())
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn session(&self) -> Result<Session> {
        let json: String = self.db.query_row(
            "SELECT session FROM checkpoint WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if json.len() > MAX_ROW {
            return Err(Error::Limit("index header".into()));
        }
        Ok(serde_json::from_str(&json)?)
    }
    pub fn status(&self) -> Result<IndexStatus> {
        let (session,state,engine,resumable,count,index_id):(String,String,String,bool,i64,String)=self.db.query_row("SELECT session,state,engine,resumable,watermark,index_id FROM checkpoint WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
        let session: Session = serde_json::from_str(&session)?;
        Ok(IndexStatus {
            schema_version: SCHEMA as u32,
            index_id,
            source: session.source,
            candidates: nonnegative(count)?,
            scanned_records: session.scanned_records,
            total_records: session.total_records,
            state,
            engine,
            resumable,
        })
    }
    pub(crate) fn staged<T: for<'de> Deserialize<'de>>(
        &self,
        namespace: u64,
        sequence: u64,
    ) -> Result<T> {
        let json: String = self.db.query_row(
            "SELECT payload FROM engine_item WHERE namespace=?1 AND seq=?2",
            params![namespace as i64, sequence as i64],
            |r| r.get(0),
        )?;
        if json.len() > MAX_ROW {
            return Err(Error::Limit("staged metadata row".into()));
        }
        Ok(serde_json::from_str(&json)?)
    }
    pub fn checkpoint<T: for<'de> Deserialize<'de>>(&self) -> Result<T> {
        let value: String = self.db.query_row(
            "SELECT engine_state FROM checkpoint WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if value.len() > MAX_CHECKPOINT {
            return Err(Error::Limit("index checkpoint".into()));
        }
        Ok(serde_json::from_str(&value)?)
    }
    pub fn page(
        &self,
        query: &IndexQuery,
        cursor: Option<&IndexCursor>,
        limit: usize,
    ) -> Result<IndexPage> {
        if limit == 0 || limit > MAX_PAGE || query.text.len() > 4096 {
            return Err(Error::Request(
                "page limit must be 1..1000; query at most 4096 bytes".into(),
            ));
        }
        let status = self.status()?;
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(query)?));
        let watermark = cursor.map_or(status.candidates, |c| c.watermark);
        if let Some(c) = cursor
            && (c.index_id != status.index_id
                || c.query_sha256 != digest
                || c.watermark > status.candidates
                || c.last_sequence == 0
                || c.last_sequence > c.watermark
                || c.last_key.len() > MAX_ROW)
        {
            return Err(Error::Request(
                "cursor does not belong to this index/query/snapshot".into(),
            ));
        }
        let column = query.sort.column();
        let direction = if query.descending { "DESC" } else { "ASC" };
        let comparison = if query.descending { "<" } else { ">" };
        let sql = format!(
            "SELECT seq, {column}, payload FROM candidate WHERE seq<=?1 AND (?2='' OR instr(search_text,?2)>0) AND (?3=0 OR supported=1) {} ORDER BY {column} {direction}, seq {direction} LIMIT ?6",
            if cursor.is_some() {
                format!("AND ({column},seq) {comparison} (?4,?5)")
            } else {
                String::new()
            }
        );
        let mut statement = self.db.prepare(&sql)?;
        let mut rows = statement.query(params![
            watermark as i64,
            query.text.to_lowercase(),
            query.supported_only,
            cursor.map_or("", |c| c.last_key.as_str()),
            cursor.map_or(0, |c| c.last_sequence as i64),
            (limit + 1) as i64
        ])?;
        let mut values = Vec::with_capacity(limit);
        let mut last = None;
        let mut more = false;
        let mut bytes = 0;
        while let Some(row) = rows.next()? {
            if values.len() == limit {
                more = true;
                break;
            }
            let payload: String = row.get(2)?;
            if payload.len() > MAX_ROW {
                return Err(Error::Limit("index candidate".into()));
            }
            if bytes + payload.len() > 32 * 1024 * 1024 && !values.is_empty() {
                more = true;
                break;
            }
            bytes += payload.len();
            let candidate: Candidate = serde_json::from_str(&payload)?;
            values.push(candidate);
            last = Some(IndexCursor {
                index_id: status.index_id.clone(),
                watermark,
                query_sha256: digest.clone(),
                last_key: row.get(1)?,
                last_sequence: nonnegative(row.get(0)?)?,
            });
        }
        Ok(IndexPage {
            index_id: status.index_id,
            watermark,
            candidates: values,
            next_cursor: if more { last } else { None },
        })
    }
    /// Load only explicitly selected rows. The existing restore engine revalidates
    /// each record against the source before opening an output file.
    pub fn selected_session(&self, ids: &[String]) -> Result<Session> {
        if ids.is_empty() || ids.len() > 100_000 {
            return Err(Error::Limit(
                "select 1..100000 indexed candidates per recovery batch".into(),
            ));
        }
        let mut session = self.session()?;
        let mut seen = std::collections::BTreeSet::new();
        let mut bytes = 0;
        let mut statement = self
            .db
            .prepare("SELECT payload FROM candidate WHERE id=?1")?;
        for id in ids {
            if !seen.insert(id) {
                return Err(Error::Request("duplicate candidate ID".into()));
            }
            let json: String = statement
                .query_row([id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| Error::Request("candidate not found in index".into()))?;
            bytes += json.len();
            if bytes > crate::MAX_SESSION_BYTES as usize {
                return Err(Error::Limit(
                    "selected metadata exceeds 128 MiB; use a smaller batch".into(),
                ));
            }
            let candidate: Candidate = serde_json::from_str(&json)?;
            if &candidate.id != id {
                return Err(Error::Request("index candidate ID mismatch".into()));
            }
            session.candidates.push(candidate);
        }
        Ok(session)
    }
}
impl IndexWriter {
    /// Creates a private, uniquely named directory under an approved work area.
    pub fn create(
        work: &Path,
        session: &Session,
        engine: &str,
        checkpoint: &impl Serialize,
    ) -> Result<Self> {
        if !session.candidates.is_empty() {
            return Err(Error::Request(
                "index header must not contain candidates".into(),
            ));
        }
        session.guard_output(work)?;
        fs::create_dir_all(work)?;
        let directory = work
            .canonicalize()?
            .join(format!("{}.rescue-index", uuid::Uuid::new_v4()));
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        builder.create(&directory)?;
        let path = directory.join("session.sqlite");
        let _file = File::options().write(true).create_new(true).open(&path)?;
        let owner = Self::owner(&path)?;
        let db = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        configure(&db, true)?;
        db.execute_batch(&format!("PRAGMA application_id={APP_ID}; PRAGMA user_version={SCHEMA};
CREATE TABLE engine_item(namespace INTEGER NOT NULL,seq INTEGER NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(namespace,seq)) STRICT;
CREATE TABLE checkpoint(singleton INTEGER PRIMARY KEY CHECK(singleton=1),session TEXT NOT NULL,state TEXT NOT NULL,engine TEXT NOT NULL,resumable INTEGER NOT NULL,engine_state TEXT NOT NULL,watermark INTEGER NOT NULL CHECK(watermark>=0),index_id TEXT NOT NULL) STRICT;
CREATE TABLE candidate(seq INTEGER PRIMARY KEY,id TEXT NOT NULL UNIQUE,payload TEXT NOT NULL,discovery_key TEXT NOT NULL,name_key TEXT NOT NULL,path_key TEXT NOT NULL,size_key TEXT NOT NULL,created_key TEXT NOT NULL,modified_key TEXT NOT NULL,search_text TEXT NOT NULL,supported INTEGER NOT NULL) STRICT;
CREATE INDEX candidate_name ON candidate(name_key,seq); CREATE INDEX candidate_path ON candidate(path_key,seq);
CREATE INDEX candidate_size ON candidate(size_key,seq); CREATE INDEX candidate_created ON candidate(created_key,seq);
CREATE INDEX candidate_modified ON candidate(modified_key,seq); CREATE INDEX candidate_discovery ON candidate(discovery_key,seq);"))?;
        db.execute(
            "INSERT INTO checkpoint VALUES(1,?1,'queued',?2,1,?3,0,?4)",
            params![
                checked_json(session, MAX_ROW)?,
                engine,
                checked_json(checkpoint, MAX_CHECKPOINT)?,
                uuid::Uuid::new_v4().to_string()
            ],
        )?;
        Ok(Self {
            index: CandidateIndex { db, path },
            _owner: owner,
        })
    }
    fn owner(path: &Path) -> Result<File> {
        let path = path.with_extension("owner");
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        regular(&path)?;
        file.try_lock()
            .map_err(|_| Error::Request("index already has a writing owner".into()))?;
        Ok(file)
    }
    pub fn resume(path: &Path) -> Result<Self> {
        let reader = CandidateIndex::open(path)?;
        let session = reader.session()?;
        session.guard_output(path)?;
        let path = reader.path.clone();
        drop(reader);
        let owner = Self::owner(&path)?;
        let db = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        configure(&db, true)?;
        let index = CandidateIndex { db, path };
        index.validate()?;
        Ok(Self {
            index,
            _owner: owner,
        })
    }
    pub fn index(&self) -> &CandidateIndex {
        &self.index
    }
    /// Commit at most 256 candidates and the exact engine restart point together.
    pub fn commit(
        &mut self,
        header: &Session,
        batch: &[Candidate],
        state: &str,
        resumable: bool,
        checkpoint: &impl Serialize,
    ) -> Result<()> {
        self.commit_staged(header, batch, state, resumable, checkpoint, &[])
    }
    pub(crate) fn commit_staged(
        &mut self,
        header: &Session,
        batch: &[Candidate],
        state: &str,
        resumable: bool,
        checkpoint: &impl Serialize,
        staged: &[StagedRow],
    ) -> Result<()> {
        if staged.len() > 256
            || staged.iter().any(|row| {
                row.payload.len() > MAX_ROW
                    || row.namespace > i64::MAX as u64
                    || row.sequence > i64::MAX as u64
            })
            || staged
                .iter()
                .map(|row| row.payload.len() as u64)
                .sum::<u64>()
                > 32 * 1024 * 1024
        {
            return Err(Error::Limit("staged metadata batch".into()));
        }
        if batch.len() > 256
            || !header.candidates.is_empty()
            || !matches!(
                state,
                "queued" | "running" | "paused" | "completed" | "partial" | "failed" | "cancelled"
            )
        {
            return Err(Error::Request("index batch/state".into()));
        }
        let old = self.index.status()?;
        let previous = self.index.session()?;
        if header.id != previous.id
            || header.source != old.source
            || header.fingerprint != previous.fingerprint
            || header.filesystem != previous.filesystem
            || header.search != previous.search
            || header.scanned_records < old.scanned_records
        {
            return Err(Error::Request(
                "index checkpoint moved backwards or changed identity".into(),
            ));
        }
        if batch
            .iter()
            .try_fold(0u64, |n, c| crate::serialized_size(c).map(|size| n + size))?
            > 32 * 1024 * 1024
        {
            return Err(Error::Limit("index batch metadata exceeds 32 MiB".into()));
        }
        let header = checked_json(header, MAX_ROW)?;
        let checkpoint = checked_json(checkpoint, MAX_CHECKPOINT)?;
        let transaction = self.index.db.transaction()?;
        let mut sequence = old.candidates;
        for candidate in batch {
            let json = checked_json(candidate, MAX_ROW)?;
            sequence = sequence
                .checked_add(1)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| Error::Limit("index sequence".into()))?;
            let mut search = format!("{}\n{}", candidate.name, candidate.original_path);
            for n in &candidate.alternate_names {
                search.push('\n');
                search.push_str(&n.name);
                search.push('\n');
                search.push_str(&n.original_path);
            }
            let unknown = "\u{10ffff}";
            transaction.execute(
                "INSERT INTO candidate VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    sequence as i64,
                    candidate.id,
                    json,
                    format!("{sequence:020}"),
                    candidate.name.to_lowercase(),
                    candidate.original_path.to_lowercase(),
                    candidate
                        .size
                        .map_or_else(|| unknown.into(), |s| format!("{s:020}")),
                    candidate
                        .created
                        .as_ref()
                        .map_or(unknown, |t| t.value.as_str()),
                    candidate
                        .modified
                        .as_ref()
                        .map_or(unknown, |t| t.value.as_str()),
                    search.to_lowercase(),
                    candidate.supported
                ],
            )?;
        }
        for row in staged {
            transaction.execute(
                "INSERT INTO engine_item VALUES(?1,?2,?3)",
                params![row.namespace as i64, row.sequence as i64, row.payload],
            )?;
        }
        transaction.execute("UPDATE checkpoint SET session=?1,state=?2,resumable=?3,engine_state=?4,watermark=?5 WHERE singleton=1",params![header,state,resumable,checkpoint,sequence as i64])?;
        transaction.commit()?;
        Ok(())
    }
    /// JSON sessions are imported into a new index; the original is never edited.
    pub fn import_session(path: &Path, work: &Path) -> Result<PathBuf> {
        let mut header = Session::load(path)?;
        let candidates = std::mem::take(&mut header.candidates);
        let mut writer = Self::create(work, &header, "imported_json", &serde_json::Value::Null)?;
        for batch in candidates.chunks(256) {
            writer.commit(&header, batch, "running", false, &serde_json::Value::Null)?;
        }
        writer.commit(
            &header,
            &[],
            if header.cancelled || header.truncated {
                "partial"
            } else {
                "completed"
            },
            false,
            &serde_json::Value::Null,
        )?;
        Ok(writer.index.path.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn header(root: &Path) -> Session {
        let source = root.join("source.raw");
        fs::write(&source, vec![0u8; 8192]).unwrap();
        Session {
            raid: None,
            schema_version: 3,
            filesystem: crate::FileSystem::Ntfs,
            id: uuid::Uuid::new_v4().to_string(),
            source: source.clone(),
            fingerprint: crate::inspect_image(&source).unwrap(),
            candidates: vec![],
            scanned_records: 0,
            total_records: 100,
            cancelled: false,
            truncated: false,
            invalid_records: 0,
            warnings: vec![],
            search: None,
            region: None,
            volumes: vec![],
        }
    }
    fn candidate(id: u64, name: &str) -> Candidate {
        Candidate {
            id: format!("id-{id}"),
            record: id,
            fragment_offset: None,
            sequence: 1,
            stream_name: None,
            name: name.into(),
            original_path: format!("folder/{name}"),
            path_verified: true,
            alternate_names: vec![],
            size: Some(id * 100),
            supported: true,
            warnings: vec![],
            record_sha256: "a".repeat(64),
            extent_evidence: crate::ExtentEvidence::Metadata,
            allocation_status: crate::AllocationStatus::Unknown,
            created: None,
            modified: None,
        }
    }
    #[test]
    fn stable_pages_and_exclusive_writer_survive_new_commits() {
        let root = tempfile::tempdir().unwrap();
        let h = header(root.path());
        let mut w = IndexWriter::create(root.path(), &h, "test", &0).unwrap();
        let path = w.index.path.clone();
        w.commit(
            &h,
            &[
                candidate(1, "b.pdf"),
                candidate(2, "c.pdf"),
                candidate(3, "d.pdf"),
            ],
            "running",
            true,
            &3,
        )
        .unwrap();
        assert!(IndexWriter::resume(&path).is_err());
        let reader = CandidateIndex::open(&path).unwrap();
        let q = IndexQuery {
            sort: IndexSort::Name,
            ..Default::default()
        };
        let first = reader.page(&q, None, 1).unwrap();
        assert_eq!(first.candidates[0].name, "b.pdf");
        w.commit(
            &h,
            &[candidate(4, "a.pdf"), candidate(5, "cc.pdf")],
            "running",
            true,
            &5,
        )
        .unwrap();
        let second = reader.page(&q, first.next_cursor.as_ref(), 100).unwrap();
        assert_eq!(
            second
                .candidates
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["c.pdf", "d.pdf"]
        );
        assert_eq!(second.watermark, 3);
        assert!(second.next_cursor.is_none());
        let current = reader.page(&q, None, 100).unwrap();
        assert_eq!(current.candidates.len(), 5);
        assert_eq!(current.candidates[0].name, "a.pdf");
        let mut wrong = q.clone();
        wrong.descending = true;
        assert!(reader.page(&wrong, first.next_cursor.as_ref(), 1).is_err());
        let w2 = IndexWriter::create(root.path(), &h, "test", &0).unwrap();
        assert!(w2.index.page(&q, first.next_cursor.as_ref(), 1).is_err());
        drop(w);
        let reopened = IndexWriter::resume(&path).unwrap();
        assert_eq!(reopened.index.checkpoint::<u64>().unwrap(), 5);
    }
    #[test]
    fn failed_batch_rolls_back_candidates_and_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let mut h = header(root.path());
        let mut w = IndexWriter::create(root.path(), &h, "test", &0).unwrap();
        let c = candidate(1, "日本語.pdf");
        h.scanned_records = 1;
        w.commit(&h, std::slice::from_ref(&c), "running", true, &1)
            .unwrap();
        h.scanned_records = 3;
        assert!(
            w.commit(&h, &[candidate(2, "new.pdf"), c], "running", true, &3)
                .is_err()
        );
        assert_eq!(w.index.status().unwrap().candidates, 1);
        assert_eq!(w.index.session().unwrap().scanned_records, 1);
        assert_eq!(w.index.checkpoint::<u64>().unwrap(), 1);
        assert!(w.index.selected_session(&["id-2".into()]).is_err());
        h.fingerprint.length += 1;
        assert!(w.commit(&h, &[], "paused", true, &3).is_err());
    }
    #[test]
    fn sort_filter_and_selected_ids_keep_evidence() {
        let root = tempfile::tempdir().unwrap();
        let h = header(root.path());
        let mut w = IndexWriter::create(root.path(), &h, "test", &0).unwrap();
        let mut a = candidate(2, "日本語.PDF");
        a.alternate_names.push(crate::CandidateName {
            name: "alias.txt".into(),
            original_path: "old/ALIAS.txt".into(),
            path_verified: true,
            namespace: 1,
        });
        let mut b = candidate(1, "other.pdf");
        b.supported = false;
        w.commit(&h, &[a.clone(), b], "running", true, &2).unwrap();
        let q = IndexQuery {
            text: "OLD/alias".into(),
            supported_only: true,
            ..Default::default()
        };
        let page = w.index.page(&q, None, 1000).unwrap();
        assert_eq!(page.candidates.len(), 1);
        assert_eq!(page.candidates[0].id, a.id);
        for sort in [
            IndexSort::Discovery,
            IndexSort::Name,
            IndexSort::Path,
            IndexSort::Size,
            IndexSort::Created,
            IndexSort::Modified,
        ] {
            for descending in [false, true] {
                let query = IndexQuery {
                    sort,
                    descending,
                    ..Default::default()
                };
                let all = w.index.page(&query, None, 100).unwrap();
                let first = w.index.page(&query, None, 1).unwrap();
                let second = w.index.page(&query, first.next_cursor.as_ref(), 1).unwrap();
                assert_eq!(all.candidates[0].id, first.candidates[0].id);
                assert_eq!(all.candidates[1].id, second.candidates[0].id);
                assert!(second.next_cursor.is_none());
            }
        }
        let selected = w.index.selected_session(&[a.id.clone()]).unwrap();
        assert_eq!(
            serde_json::to_value(&selected.candidates[0]).unwrap(),
            serde_json::to_value(a).unwrap()
        );
        assert!(
            w.index
                .selected_session(&["id-1".into(), "id-1".into()])
                .is_err()
        );
        assert!(w.index.page(&q, None, 0).is_err());
        assert!(w.index.page(&q, None, 1001).is_err());
    }
    #[test]
    fn import_keeps_original_and_rejects_unknown_schema_without_mutation() {
        let root = tempfile::tempdir().unwrap();
        let mut h = header(root.path());
        h.candidates.push(candidate(1, "file.pdf"));
        let original = h.save(root.path()).unwrap();
        let before = fs::read(&original).unwrap();
        let path = IndexWriter::import_session(&original, root.path()).unwrap();
        let index = CandidateIndex::open(&path).unwrap();
        assert_eq!(index.status().unwrap().candidates, 1);
        assert!(!index.status().unwrap().resumable);
        drop(index);
        assert_eq!(fs::read(original).unwrap(), before);
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA user_version=999").unwrap();
        drop(db);
        let before = fs::read(&path).unwrap();
        assert!(matches!(
            CandidateIndex::open(&path),
            Err(Error::Unsupported(_))
        ));
        assert!(IndexWriter::resume(&path).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }
}
