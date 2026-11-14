//! Everything iShell does on a remote host over SSH. Submodules share this
//! module's imports and each other's helpers through `use super::*`.

#[cfg(any(windows, test))]
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    models::{
        DiskMount, NetworkSample, ProcessUsage, ServerInput, ServerRecord, ServerStatus, SftpEntry,
        UploadProgress,
    },
    openssh,
    pool::SshPool,
    store::read_secret,
    time::{now, now_fractional},
    transport::{self, TransferError, UploadAttemptError},
};

mod archive;
mod clipboard;
mod download;
mod monitor;
mod remote;
mod sftp;
mod sftp_client;
mod sftp_transfer;
mod upload;
mod upload_scripts;

use archive::*;
pub use clipboard::*;
pub use download::*;
pub(crate) use download::{local_download_name, unique_file_path};
pub use monitor::*;
pub use remote::*;
pub use sftp::*;
pub(crate) use sftp_client::forget_sftp_session;
pub use upload::*;
use upload_scripts::*;

fn emit_progress(
    app: &AppHandle,
    event_name: &str,
    transfer_id: &str,
    transferred: u64,
    total: u64,
    done: bool,
) {
    app.emit(
        event_name,
        UploadProgress {
            transfer_id: transfer_id.to_string(),
            transferred,
            total,
            done,
        },
    )
    .ok();
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MB", bytes / 1024 / 1024)
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

/// Whether the file panel reaches this server over SFTP.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn sftp_available_for_tests(app: &AppHandle, id: &str) -> bool {
    sftp_client::with_sftp(app, id, async |_| Ok(())).is_ok()
}

/// Whether transfers for this server go over SFTP, optionally forcing them to.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn sftp_transfers_for_tests(app: &AppHandle, id: &str, force: Option<bool>) -> bool {
    if let Some(over_sftp) = force {
        sftp_client::remember_exec_transfers(id, !over_sftp);
    }
    sftp_transfer::use_sftp_transfers(app, id)
}
