use super::*;

pub(super) enum Task {
    Scan {
        source: PathBuf,
        options: ScanOptions,
        settings: SearchSettings,
    },
    Resume(PathBuf),
}
impl Server {
    pub(super) fn prepare_results(&mut self, args: &Value) -> ToolResult {
        let session_id = field(args, "session_id")?.to_owned();
        let path = self
            .state
            .lock()
            .unwrap()
            .indexes
            .get(&session_id)
            .cloned()
            .ok_or("unknown indexed session")?;
        let query: rescue_core::IndexQuery =
            serde_json::from_value(args.get("query").cloned().ok_or("missing query")?)
                .map_err(err)?;
        let (id, cancel) = self.create_job()?;
        let job_id = id.clone();
        let shared = self.state.clone();
        let grants = self.source_grants();
        self.threads.push(thread::spawn(move || {
            let _scope = rescue_core::SourceScope::enter(grants);
            let result = rescue_core::CandidateIndex::open(&path).and_then(|index| index.prepare_query(&query, &cancel));
            let mut state = shared.lock().unwrap();
            let job = state.jobs.get_mut(&job_id).unwrap();
            match result {
                Ok(created) => {
                    job.state = "completed".into();
                    job.result = json!({"session_id":session_id,"query":query,"order_index_created":created});
                }
                Err(error) => {
                    job.state = if matches!(error, Error::Cancelled) { "cancelled" } else { "failed" }.into();
                    job.result = json!({"error":{"code":error.code(),"message":error.to_string(),"retryable":true,"affected_scope":"result_order"}});
                }
            }
        }));
        Ok(json!({"job_id":id,"state":"running"}))
    }
    pub(super) fn start_index_job(&mut self, task: Task) -> ToolResult {
        if !self.persist_sessions {
            return Err(
                "indexed scanning needs a safe work directory on a different physical source disk"
                    .into(),
            );
        }
        if self.state.lock().unwrap().indexes.len() >= 128 {
            return Err(
                "128 open indexes; restart server and open_session to select saved indexes".into(),
            );
        }
        let (id, cancel) = self.create_job()?;
        let job_id = id.clone();
        let shared = self.state.clone();
        let work = self.config.work_dir.clone();
        let grants = self.source_grants();
        self.threads.push(thread::spawn(move||{
            let _scope=rescue_core::SourceScope::enter(grants);
            let progress=|p:rescue_core::IndexedProgress| {
                let mut state=shared.lock().unwrap();
                state.indexes.insert(p.session_id.clone(),p.index.clone());
                let job=state.jobs.get_mut(&job_id).unwrap();
                job.progress=p.progress;
                job.details=json!({"index":p.index,"session_id":p.session_id,"committed_candidates":p.committed_candidates,"pause_supported":true,"resume_mode":"durable_checkpoint"});
            };
            let operation=match task {
                Task::Scan{source,options,settings}=>rescue_core::scan_indexed(&source,&work,options,&settings,&cancel,progress),
                Task::Resume(path)=>rescue_core::resume_indexed_scan(&path,&cancel,progress),
            }.and_then(|path|{
                let index=rescue_core::CandidateIndex::open(&path)?;
                Ok((path,index.session()?,index.status()?))
            });
            let mut state=shared.lock().unwrap();
            match operation {
                Ok((path,session,status))=>{
                    state.indexes.insert(session.id.clone(),path.clone());
                    let job=state.jobs.get_mut(&job_id).unwrap();
                    job.state=if status.state=="cancelled" && job.pause_requested {"paused".into()}else{status.state.clone()};
                    job.result=json!({"session_id":session.id,"session_file":path,"candidates":status.candidates,"warnings":session.warnings,"truncated":session.truncated,"index":status});
                }
                Err(error)=>{
                    let job=state.jobs.get_mut(&job_id).unwrap();job.state=if matches!(error,Error::Cancelled){"cancelled"}else{"failed"}.into();
                    job.result=json!({"error":{"code":error.code(),"message":error.to_string(),"retryable":matches!(error,Error::Io(_)|Error::Index(_)),"affected_scope":"scan_index"}});
                }
            }
        }));
        Ok(json!({"job_id":id,"state":"running"}))
    }
    pub(super) fn open_index(&mut self, path: &Path) -> ToolResult {
        if !self.persist_sessions {
            return Err("work directory is unavailable for indexes".into());
        }
        let path = path.canonicalize().map_err(err)?;
        if !path.starts_with(&self.config.work_dir) {
            return Err("session is outside the work directory".into());
        }
        let index = rescue_core::CandidateIndex::open(&path).map_err(err)?;
        let header = index.session().map_err(err)?;
        if !self.source_grants().contains(&header.source) {
            return Err("session source is outside the allowlist".into());
        }
        rescue_core::validate_source(&header.source).map_err(err)?;
        let status = index.status().map_err(err)?;
        let mut state = self.state.lock().unwrap();
        if state.indexes.len() >= 128 && !state.indexes.contains_key(&header.id) {
            return Err("128 open indexes".into());
        }
        if let Some(previous) = state.indexes.get(&header.id)
            && previous != &path
        {
            return Err("a different index with this session ID is already open".into());
        }
        state.indexes.insert(header.id.clone(), path.clone());
        Ok(json!({"session_id":header.id,"session_file":path,"index":status}))
    }
    pub(super) fn indexed_results(&self, path: &Path, name: &str, args: &Value) -> ToolResult {
        let index = rescue_core::CandidateIndex::open(path).map_err(err)?;
        if name == "get_report" {
            return Ok(
                json!({"index":index.status().map_err(err)?,"session":index.session().map_err(err)?}),
            );
        }
        if args.get("offset").is_some_and(|v| v.as_u64() != Some(0)) {
            return Err("indexed results use cursor instead of offset".into());
        }
        let limit = args
            .get("limit")
            .map(|n| n.as_u64().ok_or("invalid limit"))
            .transpose()?
            .unwrap_or(100);
        let query = args
            .get("query")
            .map(|v| serde_json::from_value(v.clone()).map_err(err))
            .transpose()?
            .unwrap_or_default();
        let cursor: Option<rescue_core::IndexCursor> = args
            .get("cursor")
            .map(|v| serde_json::from_value(v.clone()).map_err(err))
            .transpose()?;
        serde_json::to_value(
            index
                .page(
                    &query,
                    cursor.as_ref(),
                    usize::try_from(limit).map_err(err)?,
                )
                .map_err(err)?,
        )
        .map_err(err)
    }
}
