use super::*;

#[derive(Clone)]
pub(super) struct Task {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub options: rescue_core::AcquisitionOptions,
}
impl Server {
    pub(super) fn image_job(&mut self, args: &Value) -> ToolResult {
        let source =
            rescue_core::canonical_source_path(Path::new(field(args, "source")?)).map_err(err)?;
        if !self.config.sources.contains(&source) {
            return Err("source is outside the allowlist".into());
        }
        let destination = PathBuf::from(field(args, "destination")?);
        let block_size = args
            .get("block_size")
            .map(|v| {
                v.as_u64()
                    .filter(|n| (512..=16 * 1024 * 1024).contains(n))
                    .ok_or("invalid block_size")
            })
            .transpose()?
            .unwrap_or(4 * 1024 * 1024) as u32;
        let retries = args
            .get("retries")
            .map(|v| v.as_u64().filter(|n| *n <= 3).ok_or("invalid retries"))
            .transpose()?
            .unwrap_or(2) as u8;
        let resume = args
            .get("resume")
            .map(|v| v.as_bool().ok_or("invalid resume"))
            .transpose()?
            .unwrap_or(false);
        self.start_image_job(Task {
            source,
            destination,
            options: rescue_core::AcquisitionOptions {
                block_size,
                retries,
                resume,
            },
        })
    }
    pub(super) fn start_image_job(&mut self, task: Task) -> ToolResult {
        let roots = self.output_roots.clone();
        let (id, cancel) = self.create_job(JobKind::Imaging)?;
        let job_id = id.clone();
        let shared = self.state.clone();
        let grants = self.source_grants();
        self.threads.push(thread::spawn(move || {
            let _scope = rescue_core::SourceScope::enter(grants);
            let result = rescue_core::acquire_image_with_roots(
                &task.source,
                &task.destination,
                task.options.clone(),
                Some(&roots),
                &cancel,
                |progress| {
                    let mut state = shared.lock().unwrap();
                    let session_id = progress.map.image_id.clone();
                    state.acquisitions.insert(session_id.clone(), task.clone());
                    let job = state.jobs.get_mut(&job_id).unwrap();
                    job.progress = Progress {
                        scanned_records: progress.completed_bytes,
                        total_records: progress.total_bytes,
                        candidates: 0,
                        unit: rescue_core::ProgressUnit::Bytes,
                    };
                    let mut details = serde_json::to_value(progress).unwrap();
                    details["pause_supported"] = json!(true);
                    details["session_id"] = json!(session_id);
                    details["resume_mode"] = json!("durable_read_map");
                    job.details = details;
                },
            );
            let mut state = shared.lock().unwrap();
            match result {
                Ok(result) => {
                    let session_id = result.summary.image_id.clone();
                    state.acquisitions.insert(session_id.clone(), task);
                    state.acquired_images.insert(result.image.clone());
                    let job = state.jobs.get_mut(&job_id).unwrap();
                    job.state = if result.state == "cancelled" && job.pause_requested {
                        "paused".into()
                    } else {
                        result.state.clone()
                    };
                    job.result = serde_json::to_value(result).unwrap();
                    job.result["session_id"] = json!(session_id);
                    job.details["pause_supported"] = json!(true);
                    job.details["session_id"] = json!(session_id);
                    job.details["resume_mode"] = json!("durable_read_map");
                    job.details.as_object_mut().unwrap().remove("pause_reason");
                }
                Err(error) => {
                    let job = state.jobs.get_mut(&job_id).unwrap();
                    job.state = if matches!(error, Error::Cancelled) {
                        "cancelled"
                    } else {
                        "failed"
                    }
                    .into();
                    job.result = json!({"error":error.report("imaging")});
                }
            }
        }));
        Ok(json!({"job_id":id,"state":"running"}))
    }
}
