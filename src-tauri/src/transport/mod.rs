//! The seam between iShell and its SSH backends.
//!
//! Two backends exist: the system `ssh` binary (macOS, Linux) and the
//! pure-Rust `russh` client (Windows, or anywhere with `--features
//! force-russh`). This module is the only place that knows which one is
//! compiled in. Both implement [`Backend`], so they cannot drift apart in
//! what they offer, and the rest of the app calls the functions below.

use std::{fs, io::Write, sync::OnceLock};

use tauri::AppHandle;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    runtime::Runtime,
};

use crate::models::{PortForward, ServerRecord};

#[cfg(russh_backend)]
mod pure_rust;
#[cfg(not(russh_backend))]
mod system_ssh;

#[cfg(russh_backend)]
use pure_rust::PureRust as Active;
#[cfg(not(russh_backend))]
use system_ssh::SystemSsh as Active;
#[cfg(not(russh_backend))]
pub(crate) use system_ssh::{cleanup_askpass_helper, spawn_remote_ex};

/// A streaming download that did not finish.
pub(crate) enum TransferError {
    Canceled,
    Failed(String),
}

/// One failed attempt at streaming a local file into a remote command.
/// `retryable` is set when the connection, not the request, was at fault.
#[derive(Debug)]
pub(crate) enum UploadAttemptError {
    Canceled,
    Failed { message: String, retryable: bool },
}

/// A bidirectional byte stream to an SSH subsystem such as `sftp`.
pub(crate) trait SubsystemIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> SubsystemIo for T {}
pub(crate) type Subsystem = Box<dyn SubsystemIo>;

/// Stops a running port forward.
pub(crate) type StopForward = Box<dyn Fn() + Send + Sync>;
/// Called each time a port forward is confirmed to be listening.
pub(crate) type ForwardRunning = Box<dyn Fn() + Send + Sync>;
/// Called once when a port forward ends on its own, with the reason.
pub(crate) type ForwardEnded = Box<dyn FnOnce(String) + Send>;

/// What an SSH backend provides. Servers are named by their saved id; the
/// backend looks up the address, credentials and jump hosts itself.
pub(crate) trait Backend {
    /// Run a command to completion and return its stdout. A non-zero exit is
    /// an error carrying stderr.
    fn exec(
        app: &AppHandle,
        id: &str,
        command: &str,
        stdin: Option<&[u8]>,
    ) -> Result<Vec<u8>, String>;

    /// Run a long-lived command, handing stdout to `on_data` as it arrives,
    /// until it exits or `is_canceled` turns true (which is not an error).
    fn stream(
        app: &AppHandle,
        id: &str,
        command: &str,
        is_canceled: &dyn Fn() -> bool,
        on_data: &mut dyn FnMut(&[u8]),
    ) -> Result<(), String>;

    /// Copy a command's stdout into `sink` over a connection of its own, so
    /// bulk data does not compete with the terminal. `on_progress` receives
    /// the running byte count.
    fn download(
        app: &AppHandle,
        id: &str,
        command: &str,
        sink: &mut dyn Write,
        is_canceled: &dyn Fn() -> bool,
        on_progress: &mut dyn FnMut(u64),
    ) -> Result<(), TransferError>;

    /// Feed `local` to a command's stdin over a connection of its own,
    /// emitting `sftp-upload-progress` events for `transfer_id`. With
    /// `cancel_while_waiting` false, a cancel that arrives after all bytes
    /// were sent is ignored: the remote command is committing them.
    #[allow(clippy::too_many_arguments)]
    fn upload_attempt(
        app: &AppHandle,
        id: &str,
        command: &str,
        local: &mut fs::File,
        total: u64,
        transfer_id: &str,
        is_canceled: &dyn Fn() -> bool,
        cancel_while_waiting: bool,
    ) -> Result<(), UploadAttemptError>;

    /// Open a subsystem channel. `dedicated` gives it a connection of its
    /// own instead of the one shared with the terminal. Must be awaited on
    /// [`runtime`].
    fn open_subsystem(
        app: &AppHandle,
        id: &str,
        name: &str,
        dedicated: bool,
    ) -> impl std::future::Future<Output = Result<Subsystem, String>> + Send;

    /// Check that a server, saved or not, accepts a login.
    fn test_connection(
        app: &AppHandle,
        server: &ServerRecord,
        secret: Option<&str>,
    ) -> Result<(), String>;

    /// Start a port forward; it runs until stopped or until it fails.
    fn start_forward(
        app: &AppHandle,
        forward: &PortForward,
        on_running: ForwardRunning,
        on_end: ForwardEnded,
    ) -> Result<StopForward, String>;

    /// Forget any connection kept open for this server.
    fn invalidate(id: &str);
}

/// The runtime all SSH I/O runs on. Blocking callers use `block_on`; it must
/// not be entered from one of its own tasks.
pub(crate) fn runtime() -> Result<&'static Runtime, String> {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    if let Some(rt) = RUNTIME.get() {
        return Ok(rt);
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("无法创建异步运行时：{err}"))?;
    Ok(RUNTIME.get_or_init(|| rt))
}

pub(crate) fn exec(
    app: &AppHandle,
    id: &str,
    command: &str,
    stdin: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    Active::exec(app, id, command, stdin)
}

pub(crate) fn stream(
    app: &AppHandle,
    id: &str,
    command: &str,
    is_canceled: &dyn Fn() -> bool,
    on_data: &mut dyn FnMut(&[u8]),
) -> Result<(), String> {
    Active::stream(app, id, command, is_canceled, on_data)
}

pub(crate) fn download(
    app: &AppHandle,
    id: &str,
    command: &str,
    sink: &mut dyn Write,
    is_canceled: &dyn Fn() -> bool,
    on_progress: &mut dyn FnMut(u64),
) -> Result<(), TransferError> {
    Active::download(app, id, command, sink, is_canceled, on_progress)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn upload_attempt(
    app: &AppHandle,
    id: &str,
    command: &str,
    local: &mut fs::File,
    total: u64,
    transfer_id: &str,
    is_canceled: &dyn Fn() -> bool,
    cancel_while_waiting: bool,
) -> Result<(), UploadAttemptError> {
    Active::upload_attempt(
        app,
        id,
        command,
        local,
        total,
        transfer_id,
        is_canceled,
        cancel_while_waiting,
    )
}

pub(crate) async fn open_subsystem(
    app: &AppHandle,
    id: &str,
    name: &str,
    dedicated: bool,
) -> Result<Subsystem, String> {
    Active::open_subsystem(app, id, name, dedicated).await
}

pub(crate) fn test_connection(
    app: &AppHandle,
    server: &ServerRecord,
    secret: Option<&str>,
) -> Result<(), String> {
    Active::test_connection(app, server, secret)
}

pub(crate) fn start_forward(
    app: &AppHandle,
    forward: &PortForward,
    on_running: ForwardRunning,
    on_end: ForwardEnded,
) -> Result<StopForward, String> {
    Active::start_forward(app, forward, on_running, on_end)
}

pub(crate) fn invalidate(id: &str) {
    Active::invalidate(id);
    crate::ssh::forget_sftp_session(id);
}
