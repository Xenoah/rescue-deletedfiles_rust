//! Append-only candidate storage. A batch and its scan checkpoint commit together.
//! Readers carry a high-water mark; later commits cannot replace their pages.
mod migrate;
mod query;
use crate::{Candidate, Error, Result, Session};
use rusqlite::{Connection, OpenFlags, OptionalExtension, config::DbConfig, limits::Limit, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const APP_ID: i32 = 0x52534349;
const SCHEMA: i32 = 3;
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
    Type,
    State,
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
            Self::Type => "type_key",
            Self::State => "state_key",
        }
    }
    fn rank(self) -> &'static str {
        match self {
            Self::Size => "size_rank",
            Self::Created => "created_rank",
            Self::Modified => "modified_rank",
            _ => "zero_rank",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexGroup {
    #[default]
    None,
    Folder,
    Type,
    Created,
    Modified,
    Size,
    Initial,
    State,
}
impl IndexGroup {
    fn column(self) -> &'static str {
        match self {
            Self::None => "empty_key",
            Self::Folder => "folder_key",
            Self::Type => "type_key",
            Self::Created => "created_day",
            Self::Modified => "modified_day",
            Self::Size => "size_group",
            Self::Initial => "initial_key",
            Self::State => "state_key",
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexQuery {
    pub sort: IndexSort,
    pub group: IndexGroup,
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
    pub last_group: String,
    pub last_rank: u8,
    pub last_sequence: u64,
}
#[derive(Clone, Debug, Deserialize)]
pub struct IndexPage {
    pub session_id: String,
    pub index_id: String,
    /// Number committed at the start of pagination, before applying filters.
    pub watermark: u64,
    pub candidates: Vec<Candidate>,
    pub next_cursor: Option<IndexCursor>,
}
impl Serialize for IndexPage {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            schema_version: u32,
            session_id: &'a str,
            index_id: &'a str,
            watermark: u64,
            candidates: Vec<crate::ScopedCandidate<'a>>,
            next_cursor: &'a Option<IndexCursor>,
        }
        Wire {
            schema_version: 1,
            session_id: &self.session_id,
            index_id: &self.index_id,
            watermark: self.watermark,
            candidates: self
                .candidates
                .iter()
                .map(|c| c.in_session(&self.session_id))
                .collect(),
            next_cursor: &self.next_cursor,
        }
        .serialize(serializer)
    }
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
struct QueryBudget(Connection);
impl QueryBudget {
    fn new(path: &Path, index_id: &str, cancel: Arc<AtomicBool>) -> Result<Self> {
        let mut db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        configure(&db, false)?;
        db.execute_batch("BEGIN")?;
        let found: String = db.query_row(
            "SELECT index_id FROM checkpoint WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if found != index_id {
            return Err(Error::Request("index changed before page query".into()));
        }
        let started = std::time::Instant::now();
        db.progress_handler(
            1000,
            Some(move || {
                cancel.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(2)
            }),
        )?;
        Ok(Self(db))
    }
}
fn date_key(date: &Option<crate::FileTimestamp>) -> (u8, String) {
    match date {
        None => (2, String::new()),
        Some(d) => match d.unix_seconds {
            None => (1, d.value.clone()),
            Some(seconds) => {
                let fraction = d
                    .value
                    .split_once('.')
                    .map(|(_, rest)| {
                        rest.chars()
                            .take_while(char::is_ascii_digit)
                            .take(9)
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                (
                    0,
                    format!(
                        "{:020}.{:0<9}",
                        i128::from(seconds) - i128::from(i64::MIN),
                        fraction
                    ),
                )
            }
        },
    }
}
fn day_key(date: &Option<crate::FileTimestamp>) -> String {
    date.as_ref().map_or_else(
        || "\u{10ffff}".into(),
        |d| d.value.chars().take(10).collect(),
    )
}

// A bounded set of literal trigrams only narrows candidates. instr() below is
// the authority for exact substring semantics, including punctuation/Unicode.
fn search_terms(text: &str) -> Option<String> {
    let chars: Vec<_> = text.chars().collect();
    if chars.len() < 3 || text.contains('\0') {
        return None;
    }
    let step = (chars.len() - 2).div_ceil(64);
    let terms: std::collections::BTreeSet<String> = chars
        .windows(3)
        .step_by(step)
        .map(|letters| letters.iter().collect::<String>())
        .collect();
    Some(
        terms
            .into_iter()
            .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND "),
    )
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
    /// Identify the file format without relying on a filename extension.
    pub fn is_index(path: &Path) -> Result<bool> {
        use std::io::Read;
        regular(path)?;
        let mut file = File::open(path)?;
        let mut header = [0u8; 16];
        match file.read_exact(&mut header) {
            Ok(()) => Ok(&header == b"SQLite format 3\0"),
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
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
        if !session.supported_version() || !session.candidates.is_empty() {
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
        if session.len() > MAX_ROW {
            return Err(Error::Limit("index header".into()));
        }
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
        self.page_with_cancel(query, cursor, limit, &Arc::new(AtomicBool::new(false)))
    }
    pub fn page_with_cancel(
        &self,
        query: &IndexQuery,
        cursor: Option<&IndexCursor>,
        limit: usize,
        cancel: &Arc<AtomicBool>,
    ) -> Result<IndexPage> {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        self.page_inner(query, cursor, limit, cancel)
            .map_err(|error| {
                if cancel.load(Ordering::Relaxed) {
                    Error::Cancelled
                } else {
                    error
                }
            })
    }
    fn page_inner(
        &self,
        query: &IndexQuery,
        cursor: Option<&IndexCursor>,
        limit: usize,
        cancel: &Arc<AtomicBool>,
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
                || c.last_key.len() > MAX_ROW
                || c.last_group.len() > MAX_ROW
                || c.last_rank > 2)
        {
            return Err(Error::Request(
                "cursor does not belong to this index/query/snapshot".into(),
            ));
        }
        let budget = QueryBudget::new(&self.path, &status.index_id, cancel.clone())?;
        let column = query.sort.column();
        let rank = query.sort.rank();
        let group = query.group.column();
        let direction = if query.descending { "DESC" } else { "ASC" };
        let text = query.text.to_lowercase();
        // Collect a bounded selective posting list once. Broad terms use the
        // sorted B-tree directly, avoiding a millions-row IN materialization.
        // Rowid lookup also prevents a full sort-index scan for absent terms.
        let terms = if let Some(value) = search_terms(&text) {
            let mut statement = budget.0.prepare(
                "SELECT rowid FROM candidate_search WHERE candidate_search MATCH ?1 LIMIT 50001",
            )?;
            let ids = statement
                .query_map([value], |r| r.get::<_, i64>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if ids.len() > 50_000 {
                None
            } else {
                Some(serde_json::to_string(&ids)?)
            }
        } else {
            None
        };
        let access = self.query_access(query, terms.is_some())?;
        let fixed = if query.group == IndexGroup::None {
            if rank == "zero_rank" {
                "AND empty_key='' AND zero_rank=0"
            } else {
                "AND empty_key=''"
            }
        } else {
            ""
        };
        let select = format!(
            "SELECT seq,{column},payload,{rank},{group} FROM candidate {access} WHERE seq<=?1 AND (?2='' OR instr(search_text,?2)>0) AND (?3=0 OR supported=1) {fixed} {}",
            if terms.is_some() {
                "AND seq IN(SELECT value FROM json_each(?9))"
            } else {
                ""
            }
        );
        let order = format!(
            "ORDER BY {group} ASC,{rank} ASC,{column} {direction},seq {direction} LIMIT ?6"
        );
        let sql = if cursor.is_none() {
            format!("{select} {order}")
        } else {
            // Split every tuple component, including the sequence tie-breaker.
            // SQLite stops a row-value seek before an INTEGER PRIMARY KEY that
            // is also explicitly present in this index. With millions of equal
            // sizes/dates, `(key,seq) > (?,?)` still walks the preceding rows.
            // Disjoint index ranges seek all the way to `seq` and
            // preserve ascending group/rank even for descending value order.
            let comparison = if query.descending { "<" } else { ">" };
            let sequence_range = if query.descending {
                // Two upper bounds let SQLite seek from the snapshot watermark
                // instead of the tighter cursor. Give it one exact bound.
                select.replacen("WHERE seq<=?1", "WHERE seq<=min(?1,?5-1)", 1)
            } else {
                format!("{select} AND seq>?5")
            };
            let mut ranges = vec![
                format!("{sequence_range} AND {group}=?7 AND {rank}=?8 AND {column}=?4"),
                format!("{select} AND {group}=?7 AND {rank}=?8 AND {column}{comparison}?4"),
            ];
            // Do not scan impossible constant-key ranges: SQLite can prefer
            // the equality constraint over the contradictory range predicate.
            if rank != "zero_rank" {
                ranges.push(format!("{select} AND {group}=?7 AND {rank}>?8"));
            }
            if query.group != IndexGroup::None {
                ranges.push(format!("{select} AND {group}>?7"));
            }
            format!("{} {order}", ranges.join(" UNION ALL "))
        };
        let mut statement = budget.0.prepare(&sql)?;
        // Bind named indices only when present: first pages have no cursor parameters.
        statement.raw_bind_parameter(1, watermark as i64)?;
        statement.raw_bind_parameter(2, text)?;
        statement.raw_bind_parameter(3, query.supported_only)?;
        statement.raw_bind_parameter(6, (limit + 1) as i64)?;
        if let Some(terms) = terms {
            statement.raw_bind_parameter(9, terms)?;
        }
        if let Some(c) = cursor {
            statement.raw_bind_parameter(4, &c.last_key)?;
            statement.raw_bind_parameter(5, c.last_sequence as i64)?;
            statement.raw_bind_parameter(7, &c.last_group)?;
            statement.raw_bind_parameter(8, c.last_rank)?;
        }
        let mut rows = statement.raw_query();
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
                last_rank: row.get(3)?,
                last_group: row.get(4)?,
                last_sequence: nonnegative(row.get(0)?)?,
            });
        }
        Ok(IndexPage {
            session_id: self.session()?.id,
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
CREATE TABLE candidate(seq INTEGER PRIMARY KEY,id TEXT NOT NULL UNIQUE,payload TEXT NOT NULL,discovery_key TEXT NOT NULL,name_key TEXT NOT NULL,path_key TEXT NOT NULL,size_key TEXT NOT NULL,created_key TEXT NOT NULL,modified_key TEXT NOT NULL,search_text TEXT NOT NULL,supported INTEGER NOT NULL,
type_key TEXT NOT NULL,state_key TEXT NOT NULL,zero_rank INTEGER NOT NULL CHECK(zero_rank=0),size_rank INTEGER NOT NULL,created_rank INTEGER NOT NULL,modified_rank INTEGER NOT NULL,folder_key TEXT NOT NULL,created_day TEXT NOT NULL,modified_day TEXT NOT NULL,size_group TEXT NOT NULL,initial_key TEXT NOT NULL,empty_key TEXT NOT NULL CHECK(empty_key='')) STRICT;
CREATE VIRTUAL TABLE candidate_search USING fts5(search_text,content='candidate',content_rowid='seq',tokenize='trigram case_sensitive 1',detail=none);"))?;
        for sort in [
            IndexSort::Discovery,
            IndexSort::Name,
            IndexSort::Path,
            IndexSort::Size,
            IndexSort::Created,
            IndexSort::Modified,
            IndexSort::Type,
            IndexSort::State,
        ] {
            let column = sort.column();
            let rank = sort.rank();
            db.execute_batch(&format!(
                "CREATE INDEX candidate_{column} ON candidate(empty_key,{rank},{column},seq);"
            ))?;
            if rank != "zero_rank" {
                db.execute_batch(&format!("CREATE INDEX candidate_{column}_desc ON candidate(empty_key,{rank},{column} DESC,seq DESC);"))?;
            }
        }
        for group in [
            IndexGroup::Folder,
            IndexGroup::Type,
            IndexGroup::Created,
            IndexGroup::Modified,
            IndexGroup::Size,
            IndexGroup::Initial,
            IndexGroup::State,
        ] {
            let column = group.column();
            db.execute_batch(&format!(
                "CREATE INDEX group_{column} ON candidate({column},zero_rank,name_key,seq);"
            ))?;
        }
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
            let (created_rank, created_key) = date_key(&candidate.created);
            let (modified_rank, modified_key) = date_key(&candidate.modified);
            let kind = crate::file_extension(&candidate.name, candidate.stream_name.as_deref())
                .to_uppercase();
            let state = if !candidate.supported {
                "2"
            } else if candidate.warnings.is_empty() {
                "0"
            } else {
                "1"
            };
            let path = candidate.original_path.replace('\\', "/");
            let folder = path.rsplit_once('/').map_or("", |(folder, _)| folder);
            let size_group = match candidate.size {
                Some(0) => "0",
                Some(1..=1048575) => "1",
                Some(1048576..=1073741823) => "2",
                Some(_) => "3",
                None => "4",
            };
            let initial = candidate
                .name
                .chars()
                .next()
                .map(|c| {
                    if c.is_ascii_digit() {
                        "0 – 9".into()
                    } else {
                        c.to_uppercase().to_string()
                    }
                })
                .unwrap_or_else(|| "\u{10ffff}".into());
            transaction.prepare_cached(
                "INSERT INTO candidate VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,0,?14,?15,?16,?17,?18,?19,?20,?21,'')")?.execute(
                params![sequence as i64,candidate.id,json,format!("{sequence:020}"),candidate.name.to_lowercase(),candidate.original_path.to_lowercase(),candidate.size.map_or_else(String::new,|s|format!("{s:020}")),created_key,modified_key,search.to_lowercase(),candidate.supported,kind,state,u8::from(candidate.size.is_none()),created_rank,modified_rank,folder,day_key(&candidate.created),day_key(&candidate.modified),size_group,initial],
            )?;
            transaction
                .prepare_cached("INSERT INTO candidate_search(rowid,search_text) VALUES(?1,?2)")?
                .execute(params![sequence as i64, search.to_lowercase()])?;
        }

        for row in staged {
            transaction
                .prepare_cached("INSERT INTO engine_item VALUES(?1,?2,?3)")?
                .execute(params![
                    row.namespace as i64,
                    row.sequence as i64,
                    row.payload
                ])?;
        }
        transaction.execute("UPDATE checkpoint SET session=?1,state=?2,resumable=?3,engine_state=?4,watermark=?5 WHERE singleton=1",params![header,state,resumable,checkpoint,sequence as i64])?;
        transaction.commit()?;
        Ok(())
    }
    /// JSON and earlier SQLite schemas are copied; the original is never edited.
    pub fn import_session(path: &Path, work: &Path) -> Result<PathBuf> {
        Self::import_session_with_cancel(path, work, &AtomicBool::new(false))
    }
    pub fn import_session_with_cancel(
        path: &Path,
        work: &Path,
        cancel: &AtomicBool,
    ) -> Result<PathBuf> {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if CandidateIndex::is_index(path)? {
            return Self::import_index(path, work, cancel);
        }
        let mut header = Session::load(path)?;
        crate::source_scope::check(&header.source)?;
        header.warnings.push(format!(
            "Copied from session {}; original record retained",
            header.id
        ));
        header.id = uuid::Uuid::new_v4().to_string();
        let candidates = std::mem::take(&mut header.candidates);
        let mut writer = Self::create(work, &header, "imported_json", &serde_json::Value::Null)?;
        for batch in candidates.chunks(256) {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            writer.commit(&header, batch, "running", false, &serde_json::Value::Null)?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
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
            source_info: None,
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
    fn trigram_search_preserves_literal_unicode_and_copy_migration() {
        let root = tempfile::tempdir().unwrap();
        let h = header(root.path());
        let mut writer = IndexWriter::create(root.path(), &h, "test", &0).unwrap();
        let long = "x".repeat(500) + "日本語終端.txt";
        let values = [
            candidate(1, "日本語の資料.pdf"),
            candidate(2, "a\"b [OR] * test.txt"),
            candidate(3, "École and e\u{301}cole.txt"),
            candidate(4, "\0ABC\nxy.txt"),
            candidate(5, &long),
        ];
        writer.commit(&h, &values, "completed", false, &0).unwrap();
        for text in [
            "日本語",
            "a\"b",
            "[OR]",
            "* test",
            "ÉCOLE",
            "e\u{301}cole",
            "\0ABC",
            "\nxy",
            "",
            "x",
            "不存在",
            &long,
        ] {
            for descending in [false, true] {
                let query = IndexQuery {
                    text: text.into(),
                    sort: IndexSort::Name,
                    descending,
                    ..Default::default()
                };
                let actual = writer.index.page(&query, None, 100).unwrap();
                let got: std::collections::BTreeSet<_> =
                    actual.candidates.iter().map(|c| &c.id).collect();
                let expected: std::collections::BTreeSet<_> = values
                    .iter()
                    .filter(|c| {
                        format!("{}\n{}", c.name, c.original_path)
                            .to_lowercase()
                            .contains(&text.to_lowercase())
                    })
                    .map(|c| &c.id)
                    .collect();
                assert_eq!(got, expected, "{text:?}");
            }
        }
        let original = writer.index.path.clone();
        drop(writer);
        let db = Connection::open(&original).unwrap();
        db.execute_batch("DROP TABLE candidate_search; PRAGMA user_version=2;")
            .unwrap();
        drop(db);
        let before = fs::read(&original).unwrap();
        let copy = IndexWriter::import_session(&original, root.path()).unwrap();
        assert_eq!(fs::read(&original).unwrap(), before);
        let restored = CandidateIndex::open(&copy).unwrap();
        assert_eq!(
            restored
                .page(
                    &IndexQuery {
                        text: "日本語終端".into(),
                        ..Default::default()
                    },
                    None,
                    10
                )
                .unwrap()
                .candidates[0]
                .id,
            "id-5"
        );
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
    fn grouped_pages_and_unknown_ranks_preserve_a_global_order() {
        let root = tempfile::tempdir().unwrap();
        let h = header(root.path());
        let mut w = IndexWriter::create(root.path(), &h, "test", &0).unwrap();
        let mut values = vec![
            candidate(1, "b.pdf"),
            candidate(2, "a.png"),
            candidate(3, "d.pdf"),
            candidate(4, "c.png"),
        ];
        values[0].size = Some(9);
        values[1].size = Some(100);
        values[2].size = None;
        values[3].size = Some(0);
        values[0].created = Some(crate::FileTimestamp {
            value: "1970-01-01T00:00:00.1000000Z".into(),
            unix_seconds: Some(0),
        });
        values[1].created = Some(crate::FileTimestamp {
            value: "1970-01-01T00:00:00.1100000Z".into(),
            unix_seconds: Some(0),
        });
        values[3].created = Some(crate::FileTimestamp {
            value: "1960-01-01T00:00:00".into(),
            unix_seconds: None,
        });
        w.commit(&h, &values, "completed", false, &4).unwrap();
        for (sort, descending, ids) in [
            (IndexSort::Size, false, [4, 1, 2, 3]),
            (IndexSort::Size, true, [2, 1, 4, 3]),
            (IndexSort::Created, false, [1, 2, 4, 3]),
            (IndexSort::Created, true, [2, 1, 4, 3]),
        ] {
            let query = IndexQuery {
                sort,
                descending,
                ..Default::default()
            };
            let mut cursor = None;
            let mut actual = vec![];
            loop {
                let page = w.index.page(&query, cursor.as_ref(), 1).unwrap();
                actual.extend(page.candidates.into_iter().map(|c| c.record));
                cursor = page.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            assert_eq!(actual, ids);
        }
        let query = IndexQuery {
            group: IndexGroup::Type,
            sort: IndexSort::Name,
            descending: true,
            ..Default::default()
        };
        let mut cursor = None;
        let mut actual = vec![];
        loop {
            let page = w.index.page(&query, cursor.as_ref(), 1).unwrap();
            actual.extend(page.candidates.into_iter().map(|c| c.record));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(actual, [3, 1, 4, 2]);
    }
    #[test]
    fn copy_migration_preserves_checkpoint_staging_and_the_old_database() {
        let root = tempfile::tempdir().unwrap();
        let h = header(root.path());
        let mut w = IndexWriter::create(
            root.path(),
            &h,
            "filesystem_records_v1",
            &serde_json::json!({"next":23}),
        )
        .unwrap();
        w.commit_staged(
            &h,
            &[candidate(1, "b.pdf"), candidate(2, "a.png")],
            "paused",
            true,
            &serde_json::json!({"next":23}),
            &[StagedRow {
                namespace: 0,
                sequence: 0,
                payload: "[1,2,3]".into(),
            }],
        )
        .unwrap();
        let old = w.index.path.clone();
        drop(w);
        let db = Connection::open(&old).unwrap();
        db.execute_batch("ALTER TABLE candidate RENAME TO candidate_v2; CREATE TABLE candidate AS SELECT seq,id,payload,discovery_key,name_key,path_key,size_key,created_key,modified_key,search_text,supported FROM candidate_v2; DROP TABLE candidate_v2; PRAGMA user_version=1;").unwrap();
        drop(db);
        let original = fs::read(&old).unwrap();
        assert!(CandidateIndex::open(&old).is_err());
        let copy = IndexWriter::import_session(&old, root.path()).unwrap();
        assert_eq!(fs::read(&old).unwrap(), original);
        let migrated = CandidateIndex::open(&copy).unwrap();
        let status = migrated.status().unwrap();
        assert_eq!(status.schema_version, SCHEMA as u32);
        assert_eq!(status.state, "paused");
        assert!(status.resumable);
        assert_eq!(
            migrated.checkpoint::<serde_json::Value>().unwrap(),
            serde_json::json!({"next":23})
        );
        assert_eq!(migrated.staged::<Vec<u32>>(0, 0).unwrap(), [1, 2, 3]);
        assert_eq!(
            migrated
                .page(
                    &IndexQuery {
                        group: IndexGroup::Type,
                        ..Default::default()
                    },
                    None,
                    10
                )
                .unwrap()
                .candidates
                .len(),
            2
        );
        let copied_again = IndexWriter::import_session(&copy, root.path()).unwrap();
        assert_ne!(copied_again, copy);
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
