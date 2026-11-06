use super::*;
impl Server {
    /// Raw export of candidates that could not be recovered as verified files.
    pub(super) fn export_unrecovered_job(&mut self, args: &Value) -> ToolResult {
        if self.output_roots.is_empty() {
            return Err("raw export requires an allowed output root".into());
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
        let ids: Vec<String> = args
            .get("candidate_ids")
            .and_then(Value::as_array)
            .ok_or("missing or invalid candidate_ids")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("candidate_ids must be strings")
            })
            .collect::<std::result::Result<_, _>>()?;
        if ids.is_empty() || ids.len() > 1000 {
            return Err("select 1..1000 candidate_ids".into());
        }
        let session_id = field(args, "session_id")?;
        let indexed = self.state.lock().unwrap().indexes.get(session_id).cloned();
        let session = if let Some(path) = indexed {
            rescue_core::CandidateIndex::open(&path)
                .and_then(|index| index.selected_session(&ids))
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
        session.guard_output(&destination).map_err(err)?;
        let (id, cancel) = self.create_job(JobKind::Recovery)?;
        let job_id = id.clone();
        let shared = self.state.clone();
        let grants = self.source_grants();
        self.state
            .lock()
            .unwrap()
            .jobs
            .get_mut(&id)
            .unwrap()
            .details =
            json!({"kind":"raw_export","verified_recovery":false,"pause_supported":false});
        self.threads.push(thread::spawn(move || {
            let _scope = rescue_core::SourceScope::enter(grants);
            let result = rescue_core::export_unrecovered(&session, &ids, &destination, &cancel);
            let mut state = shared.lock().unwrap();
            let job = state.jobs.get_mut(&job_id).unwrap();
            match result {
                Ok(exports) => {
                    job.state = "completed".into();
                    job.result = json!({"verified_recovery":false,"exports":exports});
                }
                Err(error) => {
                    job.state = if matches!(error, Error::Cancelled) {
                        "cancelled"
                    } else {
                        "failed"
                    }
                    .into();
                    job.result = json!({"error":error.report("raw_export".to_string())});
                }
            }
        }));
        Ok(json!({"job_id":id,"state":"running"}))
    }
}
