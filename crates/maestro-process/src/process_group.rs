use std::{
    sync::{Arc, Mutex, MutexGuard, TryLockError},
    time::Duration,
};

use tokio::time::sleep;

use crate::ProcessError;

const LEADER_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug)]
struct GroupIdentity {
    active: bool,
}

/// A process-group identifier whose reuse is prevented by an unreaped group
/// leader for as long as `active` is true.
///
/// All signals are serialized with `seal`. `seal` sends the final `SIGKILL`
/// and closes the identity gate before the supervisor reaps the leader. This
/// prevents a later caller from treating a reused numeric PGID as ours.
#[derive(Debug, Clone)]
pub(crate) struct OwnedProcessGroup {
    pid: u32,
    identity: Arc<Mutex<GroupIdentity>>,
}

impl OwnedProcessGroup {
    pub(crate) fn claim(pid: u32) -> Result<Self, ProcessError> {
        let group = Self {
            pid,
            identity: Arc::new(Mutex::new(GroupIdentity { active: true })),
        };
        if let Err(error) = verify_process_group_leader(pid) {
            // `pid` belongs to the child we just spawned, so it cannot name an
            // unrelated live group. Close the partial-initialization window
            // before the child handle itself is dropped.
            group.try_seal();
            return Err(error);
        }
        Ok(group)
    }

    #[cfg(test)]
    fn claimed_for_test(pid: u32) -> Self {
        Self {
            pid,
            identity: Arc::new(Mutex::new(GroupIdentity { active: true })),
        }
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    #[cfg(unix)]
    pub(crate) fn terminate(&self) -> Result<bool, ProcessError> {
        self.signal(nix::sys::signal::Signal::SIGTERM)
    }

    #[cfg(not(unix))]
    pub(crate) fn terminate(&self) -> Result<bool, ProcessError> {
        Err(ProcessError::Termination(
            "process groups are only supported on Unix targets".to_owned(),
        ))
    }

    /// Sends the last group-wide signal while the leader still anchors this
    /// PGID, then prevents every future signal through this owner.
    #[cfg(unix)]
    pub(crate) fn seal(&self) -> Result<(), ProcessError> {
        self.seal_with(|pid| send_signal(pid, nix::sys::signal::Signal::SIGKILL).map(|_| ()))
    }

    #[cfg(not(unix))]
    pub(crate) fn seal(&self) -> Result<(), ProcessError> {
        Err(ProcessError::Termination(
            "process groups are only supported on Unix targets".to_owned(),
        ))
    }

    fn seal_with(
        &self,
        signal: impl FnOnce(u32) -> Result<(), ProcessError>,
    ) -> Result<(), ProcessError> {
        let mut identity = self.lock_identity();
        if !identity.active {
            return Ok(());
        }
        let result = signal(self.pid);
        identity.active = false;
        result
    }

    /// Best-effort, syscall-only cleanup suitable for `Drop`.
    pub(crate) fn try_seal(&self) {
        let mut identity = match self.identity.try_lock() {
            Ok(identity) => identity,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        if identity.active {
            #[cfg(unix)]
            let _ = send_signal(self.pid, nix::sys::signal::Signal::SIGKILL);
            identity.active = false;
        }
    }

    #[cfg(unix)]
    fn signal(&self, signal: nix::sys::signal::Signal) -> Result<bool, ProcessError> {
        self.signal_with(|pid| send_signal(pid, signal))
            .map(|result| result.unwrap_or(false))
    }

    fn signal_with<T>(
        &self,
        signal: impl FnOnce(u32) -> Result<T, ProcessError>,
    ) -> Result<Option<T>, ProcessError> {
        let identity = self.lock_identity();
        if !identity.active {
            return Ok(None);
        }
        signal(self.pid).map(Some)
    }

    fn lock_identity(&self) -> MutexGuard<'_, GroupIdentity> {
        self.identity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(unix)]
fn process_group_id(pid: u32) -> Result<nix::unistd::Pid, ProcessError> {
    let pid = i32::try_from(pid).map_err(|_| {
        ProcessError::Termination("owned process group identifier is invalid".to_owned())
    })?;
    Ok(nix::unistd::Pid::from_raw(pid))
}

#[cfg(unix)]
fn rustix_process_id(pid: u32) -> Result<rustix::process::Pid, ProcessError> {
    let pid = i32::try_from(pid).map_err(|_| {
        ProcessError::Termination("owned process group identifier is invalid".to_owned())
    })?;
    rustix::process::Pid::from_raw(pid).ok_or_else(|| {
        ProcessError::Termination("owned process group identifier is invalid".to_owned())
    })
}

#[cfg(unix)]
fn verify_process_group_leader(pid: u32) -> Result<(), ProcessError> {
    let pid = rustix_process_id(pid)?;
    let group = match rustix::process::getpgid(Some(pid)) {
        Ok(group) => group,
        // macOS no longer exposes process-group metadata for an exited,
        // unreaped child. The just-spawned child handle still owns the PID,
        // and both spawn paths establish it as leader before `exec`.
        Err(rustix::io::Errno::SRCH) => return Ok(()),
        Err(error) => {
            return Err(ProcessError::Termination(format!(
                "could not verify the owned process group: {error}"
            )));
        }
    };
    if group == pid {
        Ok(())
    } else {
        Err(ProcessError::Termination(
            "spawned process is not its process-group leader".to_owned(),
        ))
    }
}

#[cfg(not(unix))]
fn verify_process_group_leader(_pid: u32) -> Result<(), ProcessError> {
    Err(ProcessError::Termination(
        "process groups are only supported on Unix targets".to_owned(),
    ))
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: nix::sys::signal::Signal) -> Result<bool, ProcessError> {
    use nix::{errno::Errno, sys::signal::killpg};

    match killpg(process_group_id(pid)?, Some(signal)) {
        Ok(()) => Ok(true),
        // macOS reports EPERM when only an exited, unreaped leader remains.
        // Every live member of a Maestro-owned group has our UID, so there is
        // no legitimate cross-user member whose permission failure to hide.
        Err(Errno::ESRCH | Errno::EPERM) => Ok(false),
        Err(error) => Err(ProcessError::Termination(format!(
            "could not signal the owned process group: {error}"
        ))),
    }
}

#[cfg(unix)]
fn leader_has_exited(pid: u32) -> Result<bool, ProcessError> {
    use rustix::process::{WaitId, WaitIdOptions, waitid};

    waitid(
        WaitId::Pid(rustix_process_id(pid)?),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )
    .map(|status| status.is_some())
    .map_err(|error| {
        ProcessError::Termination(format!("could not observe process leader exit: {error}"))
    })
}

#[cfg(not(unix))]
fn leader_has_exited(_pid: u32) -> Result<bool, ProcessError> {
    Err(ProcessError::Termination(
        "process groups are only supported on Unix targets".to_owned(),
    ))
}

/// Observes leader exit without reaping it. The resulting zombie remains the
/// process-group identity sentinel until [`OwnedProcessGroup::seal`] runs.
pub(crate) async fn wait_for_leader_exit(pid: u32) -> Result<(), ProcessError> {
    while !leader_has_exited(pid)? {
        sleep(LEADER_POLL_INTERVAL).await;
    }
    Ok(())
}

pub(crate) fn wait_for_leader_exit_blocking(pid: u32) -> Result<(), ProcessError> {
    while !leader_has_exited(pid)? {
        std::thread::sleep(LEADER_POLL_INTERVAL);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::OwnedProcessGroup;

    #[test]
    fn sealed_identity_never_signals_a_reused_numeric_group() {
        let group = OwnedProcessGroup::claimed_for_test(42);
        group.seal_with(|_| Ok(())).expect("identity seals");
        let signals = AtomicUsize::new(0);

        let result = group
            .signal_with(|_| {
                signals.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .expect("closed identity is harmless");

        assert!(result.is_none());
        assert_eq!(signals.load(Ordering::SeqCst), 0);
    }
}
