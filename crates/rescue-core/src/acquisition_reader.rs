//! Acquisition source policy: isolate reads, and only reopen the original identity.
use crate::{
    AcquisitionIdentity, AcquisitionOptions, Error, Result, acquisition_worker::Worker,
    source::Image,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

pub(crate) enum Recovery {
    Continue { reconnects: u64 },
    Stop(String),
}
pub(crate) trait ReadSource {
    fn read(&mut self, offset: u64, length: usize, cancel: &AtomicBool) -> Result<Vec<u8>>;
    fn recover(
        &mut self,
        _error: &Error,
        _cancel: &AtomicBool,
        _notify: &mut dyn FnMut(&str),
    ) -> Result<Recovery> {
        Ok(Recovery::Continue { reconnects: 0 })
    }
}
impl<F: FnMut(u64, usize) -> Result<Vec<u8>>> ReadSource for F {
    fn read(&mut self, offset: u64, length: usize, _cancel: &AtomicBool) -> Result<Vec<u8>> {
        self(offset, length)
    }
}

pub(crate) struct Reader<'a> {
    source: &'a mut Image,
    identity: AcquisitionIdentity,
    options: &'a AcquisitionOptions,
    worker: Option<Worker>,
    consecutive_timeouts: u8,
}
impl<'a> Reader<'a> {
    pub fn new(
        source: &'a mut Image,
        identity: AcquisitionIdentity,
        options: &'a AcquisitionOptions,
    ) -> Self {
        Self {
            source,
            identity,
            options,
            worker: None,
            consecutive_timeouts: 0,
        }
    }
    fn timeout(&self) -> Duration {
        Duration::from_millis(u64::from(self.options.read_timeout_ms))
    }
    fn open(&mut self, timeout: Duration, cancel: &AtomicBool) -> Result<()> {
        if self.worker.is_none() {
            self.worker = Some(Worker::start(self.source, &self.identity, timeout, cancel)?);
        }
        Ok(())
    }
    pub fn verify(&mut self, cancel: &AtomicBool) -> Result<()> {
        if self.options.read_timeout_ms == 0 {
            return if self.source.acquisition_identity()? == self.identity {
                Ok(())
            } else {
                Err(Error::SourceChanged)
            };
        }
        let timeout = self.timeout();
        self.open(timeout, cancel)?;
        self.worker.as_mut().unwrap().verify(timeout, cancel)
    }
}
fn disconnected(error: &std::io::Error) -> bool {
    if matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::TimedOut
    ) {
        return true;
    }
    #[cfg(unix)]
    if matches!(error.raw_os_error(), Some(6 | 19)) {
        return true;
    } // ENXIO / ENODEV
    #[cfg(windows)]
    if matches!(error.raw_os_error(), Some(21 | 1167)) {
        return true;
    } // NOT_READY / DEVICE_NOT_CONNECTED
    false
}
impl ReadSource for Reader<'_> {
    fn read(&mut self, offset: u64, length: usize, cancel: &AtomicBool) -> Result<Vec<u8>> {
        if self.options.read_timeout_ms == 0 {
            return self.source.read(offset, length);
        }
        let timeout = self.timeout();
        let result = self.open(timeout, cancel).and_then(|()| {
            self.worker
                .as_mut()
                .unwrap()
                .read(offset, length, timeout, cancel)
        });
        match &result {
            Ok(_) => self.consecutive_timeouts = 0,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut => {
                self.consecutive_timeouts += 1
            }
            _ => {}
        }
        result
    }
    fn recover(
        &mut self,
        error: &Error,
        cancel: &AtomicBool,
        notify: &mut dyn FnMut(&str),
    ) -> Result<Recovery> {
        let Error::Io(error) = error else {
            return Ok(Recovery::Continue { reconnects: 0 });
        };
        if self.options.read_timeout_ms == 0 || !disconnected(error) {
            return Ok(Recovery::Continue { reconnects: 0 });
        }
        if let Some(mut worker) = self.worker.take()
            && !worker.stop()
        {
            return Ok(Recovery::Stop(
                "reader remains blocked in the OS; no replacement was started".into(),
            ));
        }
        if self.consecutive_timeouts >= 3 {
            return Ok(Recovery::Stop(
                "three consecutive read deadlines expired; resume after checking the source".into(),
            ));
        }
        if self.options.reconnect_wait_ms == 0 {
            return Ok(Recovery::Stop(
                "source read timed out or disconnected; automatic reconnect is disabled".into(),
            ));
        }
        let started = Instant::now();
        let wait = Duration::from_millis(u64::from(self.options.reconnect_wait_ms));
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let Some(left) = wait.checked_sub(started.elapsed()) else {
                return Ok(Recovery::Stop(
                    "reconnect wait expired; saved ranges are retained for resume".into(),
                ));
            };
            notify("Waiting for the original source; identity is checked before reading resumes");
            match self.open(self.timeout().min(left), cancel) {
                Ok(()) => return Ok(Recovery::Continue { reconnects: 1 }),
                Err(Error::Io(error)) if disconnected(&error) => {}
                Err(error) => return Err(error),
            }
            let pause_until = Instant::now()
                + Duration::from_millis(250).min(wait.saturating_sub(started.elapsed()));
            while Instant::now() < pause_until {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
