//! Process execution primitives used by the daemon and built-in adapters.
//!
//! This crate deliberately accepts an executable plus an argument array. It
//! never accepts a shell command string and never reads project `.env` files.

mod environment;
mod lifecycle;
mod process_group;
mod pty;
mod structured;

pub use environment::{ControlledEnvironment, EnvironmentPolicy, EnvironmentPreview};
pub use pty::{PtyProcess, PtySize};
pub use structured::{ExitCause, ProcessSpec, StructuredProcess};

use std::sync::Arc;

use maestro_domain::RunId;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

/// Default upper bound for simultaneously owned child processes.
pub const DEFAULT_PROCESS_LIMIT: usize = 32;

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("the process limit ({limit}) has been reached")]
    Capacity { limit: usize },
    #[error("process I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("PTY operation failed: {0}")]
    Pty(String),
    #[error("process has no {0} stream")]
    MissingStream(&'static str),
    #[error("process has already exited")]
    AlreadyExited,
    #[error("process identifier is unavailable")]
    MissingProcessId,
    #[error("graceful process termination failed: {0}")]
    Termination(String),
}

/// Shared admission controller for structured and PTY children.
#[derive(Debug, Clone)]
pub struct ProcessSpawner {
    permits: Arc<Semaphore>,
    limit: usize,
}

impl Default for ProcessSpawner {
    fn default() -> Self {
        Self::new(DEFAULT_PROCESS_LIMIT)
    }
}

impl ProcessSpawner {
    /// Creates a bounded process admission controller.
    ///
    /// # Panics
    ///
    /// Panics when `limit` is zero.
    pub fn new(limit: usize) -> Self {
        assert!(limit > 0, "process limit must be nonzero");
        Self {
            permits: Arc::new(Semaphore::new(limit)),
            limit,
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn active_count(&self) -> usize {
        self.limit - self.permits.available_permits()
    }

    /// Starts a process with piped standard streams.
    ///
    /// # Errors
    ///
    /// Returns an error when capacity is closed or the process cannot start.
    pub async fn spawn_structured(
        &self,
        run_id: RunId,
        spec: ProcessSpec,
    ) -> Result<StructuredProcess, ProcessError> {
        let permit = self.acquire().await?;
        StructuredProcess::spawn(run_id, &spec, permit)
    }

    /// Starts a process attached to a newly allocated PTY.
    ///
    /// # Errors
    ///
    /// Returns an error when capacity is closed or PTY spawning fails.
    pub async fn spawn_pty(
        &self,
        run_id: RunId,
        spec: ProcessSpec,
        size: PtySize,
    ) -> Result<PtyProcess, ProcessError> {
        let permit = self.acquire().await?;
        PtyProcess::spawn(run_id, &spec, size, permit)
    }

    async fn acquire(&self) -> Result<OwnedSemaphorePermit, ProcessError> {
        self.permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ProcessError::Capacity { limit: self.limit })
    }

    /// Reserves a process slot without waiting.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Capacity`] when no slot is available.
    pub fn try_reserve(&self) -> Result<OwnedSemaphorePermit, ProcessError> {
        self.permits
            .clone()
            .try_acquire_owned()
            .map_err(|error| match error {
                TryAcquireError::NoPermits | TryAcquireError::Closed => {
                    ProcessError::Capacity { limit: self.limit }
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessSpawner;

    #[test]
    fn limiter_rejects_work_above_capacity() {
        let spawner = ProcessSpawner::new(1);
        let permit = spawner.try_reserve().expect("first slot is available");

        assert!(spawner.try_reserve().is_err());
        drop(permit);
        assert!(spawner.try_reserve().is_ok());
    }
}
