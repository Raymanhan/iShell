//! The backend built on the pure-Rust `russh` client. One authenticated
//! session per server is pooled and shared by channel; bulk transfers open a
//! session of their own.

use std::{
    fs,
    io::Write,
    time::{Duration, Instant},
};

use tauri::{AppHandle, Emitter};

use super::{
    Backend, ForwardEnded, ForwardRunning, StopForward, Subsystem, TransferError,
    UploadAttemptError,
};
use crate::{
    models::{PortForward, ServerRecord, UploadProgress},
    russh_transport::{self, UploadError},
    store::{get_server, read_secret},
};

pub(crate) struct PureRust;

fn credentials(app: &AppHandle, id: &str) -> Result<(ServerRecord, Option<String>), String> {
    let server = get_server(app, id)?;
    let secret = read_secret(app, id).ok().filter(|value| !value.is_empty());
    Ok((server, secret))
}

impl Backend for PureRust {
    fn exec(
        app: &AppHandle,
        id: &str,
        command: &str,
        stdin: Option<&[u8]>,
    ) -> Result<Vec<u8>, String> {
        let (server, secret) = credentials(app, id)?;
        russh_transport::run_command(&server, secret.as_deref(), command, stdin)
    }

    fn stream(
        app: &AppHandle,
        id: &str,
        command: &str,
        is_canceled: &dyn Fn() -> bool,
        on_data: &mut dyn FnMut(&[u8]),
    ) -> Result<(), String> {
        let (server, secret) = credentials(app, id)?;
        russh_transport::stream_command(&server, secret.as_deref(), command, is_canceled, on_data)
    }

    fn download(
        app: &AppHandle,
        id: &str,
        command: &str,
        sink: &mut dyn Write,
        is_canceled: &dyn Fn() -> bool,
        on_progress: &mut dyn FnMut(u64),
    ) -> Result<(), TransferError> {
        let (server, secret) = credentials(app, id).map_err(TransferError::Failed)?;
        russh_transport::download(
            &server,
            secret.as_deref(),
            command,
            sink,
            is_canceled,
            on_progress,
        )
    }

    fn upload_attempt(
        app: &AppHandle,
        id: &str,
        command: &str,
        local: &mut fs::File,
        total: u64,
        transfer_id: &str,
        is_canceled: &dyn Fn() -> bool,
        cancel_while_waiting: bool,
    ) -> Result<(), UploadAttemptError> {
        let (server, secret) =
            credentials(app, id).map_err(|message| UploadAttemptError::Failed {
                message: format!("上传失败：{message}"),
                retryable: false,
            })?;
        let emit = |transferred: u64| {
            let _ = app.emit(
                "sftp-upload-progress",
                UploadProgress {
                    transfer_id: transfer_id.to_string(),
                    transferred,
                    total,
                    done: false,
                },
            );
        };
        emit(0);
        let mut last_emit = Instant::now();
        let mut on_progress = |transferred: u64| {
            if transferred >= total || last_emit.elapsed() >= Duration::from_millis(80) {
                emit(transferred);
                last_emit = Instant::now();
            }
        };
        russh_transport::upload(
            &server,
            secret.as_deref(),
            command,
            local,
            !cancel_while_waiting,
            is_canceled,
            &mut on_progress,
        )
        .map_err(|error| match error {
            UploadError::Canceled => UploadAttemptError::Canceled,
            UploadError::Failed { message, retryable } => UploadAttemptError::Failed {
                message: format!(
                    "上传失败：{}",
                    crate::ssh::humanize_upload_error_message(&message)
                ),
                retryable,
            },
        })
    }

    async fn open_subsystem(
        app: &AppHandle,
        id: &str,
        name: &str,
        dedicated: bool,
    ) -> Result<Subsystem, String> {
        let (server, secret) = credentials(app, id)?;
        russh_transport::open_subsystem(&server, secret.as_deref(), name, dedicated).await
    }

    fn test_connection(
        _app: &AppHandle,
        server: &ServerRecord,
        secret: Option<&str>,
    ) -> Result<(), String> {
        let result = russh_transport::run_command(server, secret, "printf ishell-test", None);
        // A test uses a throwaway id; do not leave its session pooled.
        russh_transport::invalidate(&server.id);
        result.map(|_| ())
    }

    fn start_forward(
        app: &AppHandle,
        forward: &PortForward,
        on_running: ForwardRunning,
        on_end: ForwardEnded,
    ) -> Result<StopForward, String> {
        let (server, secret) = credentials(app, &forward.server_id)?;
        russh_transport::start_port_forward(server, secret, forward.clone(), on_running, on_end)
    }

    fn invalidate(id: &str) {
        russh_transport::invalidate(id);
    }
}
