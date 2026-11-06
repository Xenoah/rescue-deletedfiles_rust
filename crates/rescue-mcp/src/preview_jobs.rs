use super::*;
impl Server {
    pub(super) fn preview_job(&mut self, args: &Value) -> ToolResult {
        if self.output_roots.is_empty() {
            return Err("preview export requires an allowed output root".into());
        }
        let destination = PathBuf::from(field(args, "destination")?);
        let absolute = rescue_core::canonical_output_path(&destination).map_err(err)?;
        if !self
            .config
            .output_roots
            .iter()
            .any(|root| absolute.starts_with(root))
        {
            return Err("destination is outside allowed output roots".into());
        }
        let candidate_id = field(args, "candidate_id")?.to_owned();
        let session_id = field(args, "session_id")?;
        let indexed = self.state.lock().unwrap().indexes.get(session_id).cloned();
        let mut session = if let Some(path) = indexed {
            rescue_core::CandidateIndex::open(&path)
                .and_then(|index| index.selected_session(std::slice::from_ref(&candidate_id)))
                .map_err(err)?
        } else {
            self.state
                .lock()
                .unwrap()
                .sessions
                .get(session_id)
                .cloned()
                .ok_or("unknown session")?
        };
        session.candidates.retain(|c| c.id == candidate_id);
        if session.candidates.len() != 1 {
            return Err("unknown candidate".into());
        }
        session.guard_output(&destination).map_err(err)?;
        let (id, cancel) = self.create_job(JobKind::Preview)?;
        let job_id = id.clone();
        let shared = self.state.clone();
        let grants = self.source_grants();
        let roots = self.output_roots.clone();
        self.state
            .lock()
            .unwrap()
            .jobs
            .get_mut(&id)
            .unwrap()
            .details = json!({"kind":"preview","pause_supported":false,"pause_reason":"disposable preview; retry creates a new worker","limits":{"wall_seconds":5,"cpu_seconds":3,"memory_bytes":536870912,"source_read_bytes":67108864,"encoded_image_bytes":8388608,"pixels":16777216,"thumbnail_side":512,"text_prefix_bytes":65536}});
        self.threads.push(thread::spawn(move || {
            let _scope = rescue_core::SourceScope::enter(grants);
            let result = rescue_core::preview_candidate(session.clone(), &candidate_id, &cancel)
                .and_then(|preview| {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(Error::Cancelled);
                    }
                    rescue_core::save_preview(&session, &preview, &destination, Some(&roots))
                });
            let mut state = shared.lock().unwrap();
            let job = state.jobs.get_mut(&job_id).unwrap();
            match result {
                Ok(result) => {
                    job.state = "completed".into();
                    job.result = json!(result);
                }
                Err(error) => {
                    job.state = if matches!(error, Error::Cancelled) {
                        "cancelled"
                    } else {
                        "failed"
                    }
                    .into();
                    job.result = json!({"error":error.report(format!("candidate:{candidate_id}"))});
                }
            }
        }));
        Ok(json!({"job_id":id,"state":"running"}))
    }
}
