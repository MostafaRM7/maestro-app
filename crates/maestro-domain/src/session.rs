use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationMode {
    Structured,
    CliManaged,
    PtyTui,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Created,
    Starting,
    Ready,
    Running,
    AwaitingPermission,
    AwaitingUserInput,
    Background,
    Interrupting,
    Completed,
    Stopped,
    Failed,
    Interrupted,
    Recoverable,
    Incompatible,
}

impl SessionState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Stopped | Self::Incompatible)
    }

    pub const fn can_transition_to(self, next: Self) -> bool {
        use SessionState::{
            AwaitingPermission, AwaitingUserInput, Background, Completed, Created, Failed,
            Incompatible, Interrupted, Interrupting, Ready, Recoverable, Running, Starting,
            Stopped,
        };

        matches!(
            (self, next),
            (Created, Starting | Stopped)
                | (
                    Starting,
                    Ready | Running | Failed | Interrupted | Incompatible | Stopped
                )
                | (Ready, Running | Background | Stopped | Failed)
                | (
                    Running,
                    AwaitingPermission
                        | AwaitingUserInput
                        | Background
                        | Interrupting
                        | Completed
                        | Failed
                        | Interrupted
                        | Stopped
                )
                | (
                    AwaitingPermission | AwaitingUserInput,
                    Running | Background | Interrupting | Failed | Interrupted | Stopped
                )
                | (
                    Background,
                    Ready
                        | Running
                        | AwaitingPermission
                        | AwaitingUserInput
                        | Interrupting
                        | Completed
                        | Failed
                        | Interrupted
                        | Stopped
                )
                | (
                    Interrupting,
                    Ready | Interrupted | Completed | Failed | Stopped
                )
                | (Failed | Interrupted, Recoverable | Starting | Stopped)
                | (Recoverable, Starting | Stopped | Incompatible)
        )
    }

    /// Applies a validated state transition.
    ///
    /// # Errors
    ///
    /// Returns [`SessionTransitionError`] when `next` is not reachable from the
    /// current state according to Maestro's durable session state machine.
    pub fn transition_to(self, next: Self) -> Result<Self, SessionTransitionError> {
        if self == next || self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(SessionTransitionError {
                from: self,
                to: next,
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("invalid session state transition from {from:?} to {to:?}")]
pub struct SessionTransitionError {
    pub from: SessionState,
    pub to: SessionState,
}

#[cfg(test)]
mod tests {
    use super::SessionState;

    #[test]
    fn running_session_can_wait_for_permission() {
        assert_eq!(
            SessionState::Running.transition_to(SessionState::AwaitingPermission),
            Ok(SessionState::AwaitingPermission)
        );
    }

    #[test]
    fn completed_session_cannot_restart_in_place() {
        assert!(
            SessionState::Completed
                .transition_to(SessionState::Starting)
                .is_err()
        );
    }
}
