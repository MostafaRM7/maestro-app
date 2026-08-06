//! Authenticated, per-user Unix-socket daemon foundation.
//!
//! The daemon owns encrypted storage, project services, child process groups,
//! structured fake-agent sessions, terminal PTYs, and authenticated local IPC.

pub mod fake_session;
mod ipc;
mod paths;
mod project;
mod server;
mod storage_runtime;
mod terminal;

pub use ipc::{DaemonClient, IpcError, MultiplexedDaemonClient};
pub use paths::{DaemonPaths, SecretToken};
pub use server::{DaemonConfig, DaemonError, DaemonServer, ShutdownHandle};
