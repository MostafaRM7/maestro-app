use tokio::sync::watch;

use crate::{ExitCause, ProcessError};

#[derive(Debug, Clone)]
pub(crate) enum LifecycleOutcome {
    Running,
    Exited(ExitCause),
    Failed(String),
}

#[derive(Debug, Clone)]
pub(crate) struct ProcessLifecycle {
    outcome: watch::Receiver<LifecycleOutcome>,
}

impl ProcessLifecycle {
    pub(crate) fn channel() -> (watch::Sender<LifecycleOutcome>, Self) {
        let (sender, outcome) = watch::channel(LifecycleOutcome::Running);
        (sender, Self { outcome })
    }

    pub(crate) async fn wait(&self) -> Result<ExitCause, ProcessError> {
        let mut outcome = self.outcome.clone();
        loop {
            if let Some(result) = outcome_result(&outcome.borrow()) {
                return result;
            }
            outcome.changed().await.map_err(|_| {
                ProcessError::Termination("process supervisor stopped unexpectedly".to_owned())
            })?;
        }
    }

    pub(crate) fn completed_result(&self) -> Option<Result<ExitCause, ProcessError>> {
        outcome_result(&self.outcome.borrow())
    }

    pub(crate) fn is_running(&self) -> bool {
        matches!(*self.outcome.borrow(), LifecycleOutcome::Running)
    }
}

fn outcome_result(outcome: &LifecycleOutcome) -> Option<Result<ExitCause, ProcessError>> {
    match outcome {
        LifecycleOutcome::Running => None,
        LifecycleOutcome::Exited(cause) => Some(Ok(*cause)),
        LifecycleOutcome::Failed(message) => Some(Err(ProcessError::Termination(message.clone()))),
    }
}
