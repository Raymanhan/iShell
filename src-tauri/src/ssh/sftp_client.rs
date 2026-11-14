//! An SFTP client over whichever SSH backend is in use.
//!
//! `transport::open_subsystem` yields the `sftp` subsystem as a byte stream
//! (an `ssh -s` process or a `russh` channel); the protocol on top of it is
//! the same either way. One session per server is kept for browsing and
//! shared by every file operation; transfers open sessions of their own.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, OnceLock, PoisonError},
};

use futures_util::future::join_all;
use russh_sftp::{
    client::{error::Error as SftpError, RawSftpSession},
    protocol::{FileAttributes, OpenFlags, Packet, StatusCode},
};
use tauri::AppHandle;

use crate::{models::SftpEntry, transport};

const POSIX_RENAME: &str = "posix-rename@openssh.com";
const FSYNC: &str = "fsync@openssh.com";
const LIMITS: &str = "limits@openssh.com";
/// Every SFTP server must accept packets this large, so reads and writes of
/// this size work without negotiation.
const SAFE_IO_LEN: u32 = 32 * 1024;
/// Largest read or write used when the server advertises its limits.
const MAX_IO_LEN: u32 = 255 * 1024;
/// Requests kept in flight at once when reading, writing or inspecting.
pub(super) const PIPELINE: usize = 32;
const REQUEST_TIMEOUT_SECS: u64 = 30;

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;
const S_IFREG: u32 = 0o100000;

/// Why an SFTP call failed.
#[derive(Debug)]
pub(crate) enum Failure {
    /// The server offers no usable SFTP subsystem; other means may still work.
    Unavailable(String),
    /// The session broke mid-request; a fresh one may succeed.
    Disconnected(String),
    /// The server refused the operation.
    Refused { code: StatusCode, message: String },
}

impl Failure {
    pub(crate) fn message(&self) -> String {
        match self {
            Failure::Unavailable(message) | Failure::Disconnected(message) => message.clone(),
            Failure::Refused { code, message } => {
                let reason = match code {
                    StatusCode::NoSuchFile => "文件或目录不存在",
                    StatusCode::PermissionDenied => "权限不足",
                    StatusCode::OpUnsupported => "服务器不支持该操作",
                    _ => "",
                };
                match (reason.is_empty(), message.trim().is_empty()) {
                    (false, true) => reason.to_string(),
                    (false, false) => format!("{reason}（{}）", message.trim()),
                    (true, false) => message.trim().to_string(),
                    (true, true) => "服务器拒绝了该操作".to_string(),
                }
            }
        }
    }

    pub(crate) fn is(&self, expected: StatusCode) -> bool {
        matches!(self, Failure::Refused { code, .. } if *code == expected)
    }
}

impl From<SftpError> for Failure {
    fn from(error: SftpError) -> Self {
        match error {
            SftpError::Status(status) => Failure::Refused {
                code: status.status_code,
                message: status.error_message,
            },
            SftpError::Limited(message) => Failure::Refused {
                code: StatusCode::Failure,
                message,
            },
            SftpError::Timeout => Failure::Disconnected("SFTP 请求超时".into()),
            other => Failure::Disconnected(format!("SFTP 连接中断：{other}")),
        }
    }
}

pub(crate) type SftpResult<T> = Result<T, Failure>;

pub(crate) fn is_dir(attrs: &FileAttributes) -> bool {
    attrs
        .permissions
        .is_some_and(|mode| mode & S_IFMT == S_IFDIR)
}

pub(crate) fn is_symlink(attrs: &FileAttributes) -> bool {
    attrs
        .permissions
        .is_some_and(|mode| mode & S_IFMT == S_IFLNK)
}

pub(crate) fn is_regular(attrs: &FileAttributes) -> bool {
    attrs
        .permissions
        .is_some_and(|mode| mode & S_IFMT == S_IFREG)
}

/// `dir/name` without doubling the slash under the root.
pub(crate) fn join(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", dir.trim_end_matches('/'))
    }
}

/// The directory part of an absolute path.
pub(crate) fn parent(path: &str) -> String {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) | None => "/".to_string(),
        Some((dir, _)) => dir.to_string(),
    }
}

/// Owner and group names from an `ls -l` style long name, which is where
/// SFTP version 3 servers put them.
fn owner_and_group(longname: &str) -> (Option<String>, Option<String>) {
    let mut fields = longname.split_whitespace();
    let looks_like_ls = fields
        .next()
        .is_some_and(|mode| mode.len() >= 10 && "-dlcbps".contains(&mode[..1]));
    if !looks_like_ls || fields.next().is_none() {
        return (None, None);
    }
    (
        fields.next().map(str::to_string),
        fields.next().map(str::to_string),
    )
}

/// An open remote file or directory.
pub(crate) struct Handle(String);

pub(crate) struct Sftp {
    raw: RawSftpSession,
    posix_rename: bool,
    fsync: bool,
    io_len: u32,
}

impl Sftp {
    /// Open and initialise a session. `dedicated` puts it on a connection of
    /// its own, for bulk transfers.
    pub(crate) async fn connect(app: &AppHandle, id: &str, dedicated: bool) -> SftpResult<Self> {
        let stream = transport::open_subsystem(app, id, "sftp", dedicated)
            .await
            .map_err(Failure::Unavailable)?;
        let mut raw = RawSftpSession::new(stream);
        raw.set_timeout(REQUEST_TIMEOUT_SECS);
        let version = raw
            .init()
            .await
            .map_err(|err| Failure::Unavailable(format!("无法建立 SFTP 会话：{err}")))?;
        let supports = |name: &str| version.extensions.contains_key(name);
        let mut io_len = SAFE_IO_LEN;
        if supports(LIMITS) {
            if let Ok(limits) = raw.limits().await {
                let allowed = limits.max_read_len.min(limits.max_write_len);
                if allowed > 0 {
                    io_len = (allowed.min(u64::from(MAX_IO_LEN)) as u32).max(SAFE_IO_LEN);
                }
                raw.set_limits(limits.into());
            }
        }
        Ok(Self {
            posix_rename: supports(POSIX_RENAME),
            fsync: supports(FSYNC),
            io_len,
            raw,
        })
    }

    /// Bytes to ask for or send per request.
    pub(crate) fn io_len(&self) -> usize {
        self.io_len as usize
    }

    pub(crate) async fn stat(&self, path: &str) -> SftpResult<FileAttributes> {
        Ok(self.raw.stat(path).await?.attrs)
    }

    pub(crate) async fn lstat(&self, path: &str) -> SftpResult<FileAttributes> {
        Ok(self.raw.lstat(path).await?.attrs)
    }

    /// `lstat` that reports a missing path as `None`.
    pub(crate) async fn lstat_optional(&self, path: &str) -> SftpResult<Option<FileAttributes>> {
        match self.lstat(path).await {
            Ok(attrs) => Ok(Some(attrs)),
            Err(failure) if failure.is(StatusCode::NoSuchFile) => Ok(None),
            Err(failure) => Err(failure),
        }
    }

    pub(crate) async fn realpath(&self, path: &str) -> SftpResult<String> {
        let name = self.raw.realpath(path).await?;
        name.files
            .into_iter()
            .next()
            .map(|file| file.filename)
            .ok_or_else(|| Failure::Disconnected("SFTP 服务器未返回路径".into()))
    }

    pub(crate) async fn readlink(&self, path: &str) -> SftpResult<String> {
        let name = self.raw.readlink(path).await?;
        name.files
            .into_iter()
            .next()
            .map(|file| file.filename)
            .ok_or_else(|| Failure::Disconnected("SFTP 服务器未返回链接目标".into()))
    }

    /// Names and attributes of a directory's entries, without `.` and `..`.
    pub(crate) async fn read_dir(
        &self,
        path: &str,
    ) -> SftpResult<Vec<(String, String, FileAttributes)>> {
        let handle = self.raw.opendir(path).await?.handle;
        let mut entries = Vec::new();
        let outcome = loop {
            match self.raw.readdir(handle.as_str()).await {
                Ok(name) => entries.extend(
                    name.files
                        .into_iter()
                        .filter(|file| file.filename != "." && file.filename != "..")
                        .map(|file| (file.filename, file.longname, file.attrs)),
                ),
                Err(SftpError::Status(status)) if status.status_code == StatusCode::Eof => {
                    break Ok(())
                }
                Err(error) => break Err(Failure::from(error)),
            }
        };
        let _ = self.raw.close(handle).await;
        outcome.map(|()| entries)
    }

    /// A directory listing in the shape the file panel shows. Symlinks are
    /// resolved (several at a time) to tell links to directories apart.
    pub(crate) async fn list(&self, dir: &str) -> SftpResult<Vec<SftpEntry>> {
        let mut entries: Vec<SftpEntry> = self
            .read_dir(dir)
            .await?
            .into_iter()
            .map(|(name, longname, attrs)| {
                let (owner, group) = owner_and_group(&longname);
                SftpEntry {
                    path: join(dir, &name),
                    name,
                    is_dir: is_dir(&attrs),
                    is_symlink: is_symlink(&attrs),
                    link_target: None,
                    target_is_dir: None,
                    size: attrs.size,
                    uid: attrs.uid,
                    gid: attrs.gid,
                    owner: attrs.user.or(owner),
                    group: attrs.group.or(group),
                    permissions: attrs.permissions,
                    modified_at: attrs.mtime.map(u64::from),
                }
            })
            .collect();

        let links: Vec<usize> = (0..entries.len())
            .filter(|&index| entries[index].is_symlink)
            .collect();
        for batch in links.chunks(PIPELINE) {
            let resolved = join_all(batch.iter().map(|&index| {
                let path = entries[index].path.clone();
                async move {
                    let target = self.readlink(&path).await.ok();
                    let points_to_dir = self.stat(&path).await.is_ok_and(|attrs| is_dir(&attrs));
                    (target, points_to_dir)
                }
            }))
            .await;
            for (&index, (target, points_to_dir)) in batch.iter().zip(resolved) {
                let entry = &mut entries[index];
                entry.link_target = target;
                entry.target_is_dir = Some(points_to_dir);
                entry.is_dir = points_to_dir;
            }
        }
        Ok(entries)
    }

    pub(crate) async fn mkdir(&self, path: &str) -> SftpResult<()> {
        self.raw.mkdir(path, FileAttributes::empty()).await?;
        Ok(())
    }

    /// Create a directory and any missing parents. An existing directory is fine.
    pub(crate) async fn mkdir_all(&self, path: &str) -> SftpResult<()> {
        // Walk up to the nearest ancestor that exists, then create downwards.
        let mut missing = Vec::new();
        let mut current = path.trim_end_matches('/').to_string();
        if current.is_empty() {
            return Ok(());
        }
        loop {
            match self.stat(&current).await {
                Ok(attrs) if is_dir(&attrs) => break,
                Ok(_) => {
                    return Err(Failure::Refused {
                        code: StatusCode::Failure,
                        message: format!("{current} 已存在且不是目录"),
                    })
                }
                Err(failure) if failure.is(StatusCode::NoSuchFile) => {
                    let up = parent(&current);
                    missing.push(current);
                    if up == "/" {
                        break;
                    }
                    current = up;
                }
                Err(failure) => return Err(failure),
            }
        }
        for dir in missing.iter().rev() {
            if let Err(failure) = self.mkdir(dir).await {
                // Someone else may have created it meanwhile.
                if !self.stat(dir).await.is_ok_and(|attrs| is_dir(&attrs)) {
                    return Err(failure);
                }
            }
        }
        Ok(())
    }

    /// Set a path's permission bits.
    pub(crate) async fn raw_set_mode(&self, path: &str, mode: u32) -> SftpResult<()> {
        self.raw
            .setstat(
                path,
                FileAttributes {
                    permissions: Some(mode & 0o7777),
                    ..FileAttributes::empty()
                },
            )
            .await?;
        Ok(())
    }

    pub(crate) async fn remove_file(&self, path: &str) -> SftpResult<()> {
        self.raw.remove(path).await?;
        Ok(())
    }

    pub(crate) async fn remove_dir(&self, path: &str) -> SftpResult<()> {
        self.raw.rmdir(path).await?;
        Ok(())
    }

    /// Delete a directory and everything in it. Symlinks inside are removed,
    /// never followed.
    pub(crate) async fn remove_tree(&self, path: &str) -> SftpResult<()> {
        // Depth-first without recursion: directories are removed after their
        // contents, in reverse order of discovery.
        let mut pending = vec![path.to_string()];
        let mut directories = Vec::new();
        while let Some(dir) = pending.pop() {
            let entries = self.read_dir(&dir).await?;
            let mut files = Vec::new();
            for (name, _, attrs) in entries {
                let child = join(&dir, &name);
                if is_dir(&attrs) {
                    pending.push(child);
                } else {
                    files.push(child);
                }
            }
            for batch in files.chunks(PIPELINE) {
                for result in join_all(batch.iter().map(|file| self.remove_file(file))).await {
                    result?;
                }
            }
            directories.push(dir);
        }
        for dir in directories.iter().rev() {
            self.remove_dir(dir).await?;
        }
        Ok(())
    }

    /// Rename, replacing an existing file at `to` atomically when the server
    /// supports it. Returns whether the replacement was atomic.
    pub(crate) async fn rename_over(&self, from: &str, to: &str) -> SftpResult<bool> {
        if self.posix_rename {
            let mut data = Vec::with_capacity(from.len() + to.len() + 8);
            for value in [from, to] {
                data.extend_from_slice(&(value.len() as u32).to_be_bytes());
                data.extend_from_slice(value.as_bytes());
            }
            return match self.raw.extended(POSIX_RENAME, data).await? {
                Packet::Status(status) if status.status_code == StatusCode::Ok => Ok(true),
                Packet::Status(status) => Err(Failure::Refused {
                    code: status.status_code,
                    message: status.error_message,
                }),
                _ => Err(Failure::Disconnected("SFTP 服务器响应异常".into())),
            };
        }
        // Plain SFTP rename refuses to overwrite: make room first.
        if let Some(attrs) = self.lstat_optional(to).await? {
            if !is_dir(&attrs) {
                self.remove_file(to).await?;
            }
        }
        self.raw.rename(from, to).await?;
        Ok(false)
    }

    /// Whether `rename_over` replaces a file in one atomic step.
    pub(crate) fn renames_atomically(&self) -> bool {
        self.posix_rename
    }

    pub(crate) async fn open(
        &self,
        path: &str,
        flags: OpenFlags,
        attrs: FileAttributes,
    ) -> SftpResult<Handle> {
        Ok(Handle(self.raw.open(path, flags, attrs).await?.handle))
    }

    pub(crate) async fn close(&self, handle: Handle) -> SftpResult<()> {
        self.raw.close(handle.0).await?;
        Ok(())
    }

    pub(crate) async fn fstat(&self, handle: &Handle) -> SftpResult<FileAttributes> {
        Ok(self.raw.fstat(handle.0.as_str()).await?.attrs)
    }

    pub(crate) async fn fsetstat(&self, handle: &Handle, attrs: FileAttributes) -> SftpResult<()> {
        self.raw.fsetstat(handle.0.as_str(), attrs).await?;
        Ok(())
    }

    /// Flush a file to disk where the server can; a no-op elsewhere.
    pub(crate) async fn sync(&self, handle: &Handle) -> SftpResult<()> {
        if self.fsync {
            self.raw.fsync(handle.0.as_str()).await?;
        }
        Ok(())
    }

    /// Read up to `len` bytes at `offset`; empty at end of file.
    pub(crate) async fn read_at(
        &self,
        handle: &Handle,
        offset: u64,
        len: usize,
    ) -> SftpResult<Vec<u8>> {
        match self.raw.read(handle.0.as_str(), offset, len as u32).await {
            Ok(data) => Ok(data.data),
            Err(SftpError::Status(status)) if status.status_code == StatusCode::Eof => {
                Ok(Vec::new())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) async fn write_at(
        &self,
        handle: &Handle,
        offset: u64,
        data: Vec<u8>,
    ) -> SftpResult<()> {
        self.raw.write(handle.0.as_str(), offset, data).await?;
        Ok(())
    }

    /// Read a whole file, refusing anything longer than `limit` bytes.
    pub(crate) async fn read_file(&self, path: &str, limit: u64) -> SftpResult<Vec<u8>> {
        let handle = self
            .open(path, OpenFlags::READ, FileAttributes::empty())
            .await?;
        let mut content = Vec::new();
        let outcome = loop {
            match self
                .read_at(&handle, content.len() as u64, self.io_len())
                .await
            {
                Ok(chunk) if chunk.is_empty() => break Ok(()),
                Ok(chunk) => {
                    content.extend_from_slice(&chunk);
                    if content.len() as u64 > limit {
                        break Err(Failure::Refused {
                            code: StatusCode::Failure,
                            message: "文件超过大小限制".into(),
                        });
                    }
                }
                Err(failure) => break Err(failure),
            }
        };
        let _ = self.close(handle).await;
        outcome.map(|()| content)
    }

    /// Write `content` to an open file from the start, several chunks at a time.
    pub(crate) async fn write_all(&self, handle: &Handle, content: &[u8]) -> SftpResult<()> {
        let chunks: Vec<(u64, &[u8])> = content
            .chunks(self.io_len())
            .scan(0_u64, |offset, chunk| {
                let at = *offset;
                *offset += chunk.len() as u64;
                Some((at, chunk))
            })
            .collect();
        for batch in chunks.chunks(PIPELINE) {
            let writes = batch
                .iter()
                .map(|(offset, chunk)| self.write_at(handle, *offset, chunk.to_vec()));
            for result in join_all(writes).await {
                result?;
            }
        }
        Ok(())
    }
}

/// Browsing sessions by server id, and servers known to offer no SFTP.
#[derive(Default)]
struct Sessions {
    open: HashMap<String, Arc<Sftp>>,
    unavailable: HashSet<String>,
    /// Whether command-based transfers work on a server (see `sftp_transfer`).
    exec_transfers: HashMap<String, bool>,
}

fn sessions() -> std::sync::MutexGuard<'static, Sessions> {
    static SESSIONS: OnceLock<Mutex<Sessions>> = OnceLock::new();
    SESSIONS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Close the browsing session kept for a server and forget whether it has
/// SFTP, so the next operation finds out afresh.
pub(crate) fn forget_sftp_session(id: &str) {
    let mut sessions = sessions();
    sessions.open.remove(id);
    sessions.unavailable.remove(id);
    sessions.exec_transfers.remove(id);
}

pub(super) fn exec_transfers_known(id: &str) -> Option<bool> {
    sessions().exec_transfers.get(id).copied()
}

pub(super) fn remember_exec_transfers(id: &str, available: bool) {
    sessions().exec_transfers.insert(id.to_string(), available);
}

async fn shared(app: &AppHandle, id: &str) -> SftpResult<Arc<Sftp>> {
    {
        let sessions = sessions();
        if let Some(session) = sessions.open.get(id) {
            return Ok(session.clone());
        }
        if sessions.unavailable.contains(id) {
            return Err(Failure::Unavailable("服务器未提供 SFTP".into()));
        }
    }
    match Sftp::connect(app, id, false).await {
        Ok(session) => {
            let session = Arc::new(session);
            sessions().open.insert(id.to_string(), session.clone());
            Ok(session)
        }
        Err(failure) => {
            if matches!(failure, Failure::Unavailable(_)) {
                sessions().unavailable.insert(id.to_string());
            }
            Err(failure)
        }
    }
}

/// Run `action` on the server's browsing session, from blocking code. A
/// session that turns out to be dead is replaced once.
pub(crate) fn with_sftp<T>(
    app: &AppHandle,
    id: &str,
    action: impl AsyncFn(&Sftp) -> SftpResult<T>,
) -> SftpResult<T> {
    transport::runtime()
        .map_err(Failure::Unavailable)?
        .block_on(async {
            let session = shared(app, id).await?;
            match action(&session).await {
                Err(Failure::Disconnected(_)) => {
                    sessions().open.remove(id);
                    let session = shared(app, id).await?;
                    action(&session).await
                }
                result => result,
            }
        })
}

/// Run `action` on a session of its own, from blocking code.
pub(crate) fn with_dedicated_sftp<T>(
    app: &AppHandle,
    id: &str,
    action: impl AsyncFnOnce(&Sftp) -> SftpResult<T>,
) -> SftpResult<T> {
    transport::runtime()
        .map_err(Failure::Unavailable)?
        .block_on(async {
            let session = Sftp::connect(app, id, true).await?;
            action(&session).await
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_and_splits_remote_paths() {
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(join("/etc/", "hosts"), "/etc/hosts");
        assert_eq!(parent("/etc/hosts"), "/etc");
        assert_eq!(parent("/etc/"), "/");
        assert_eq!(parent("/etc"), "/");
        assert_eq!(parent("/"), "/");
    }

    #[test]
    fn reads_owner_and_group_from_long_names() {
        assert_eq!(
            owner_and_group("-rw-r--r--    1 deploy   www-data      348 Mar 25 14:29 app.conf"),
            (Some("deploy".into()), Some("www-data".into()))
        );
        assert_eq!(
            owner_and_group("drwxr-xr-x+ 12 1000 1000 4096 Jan  1  2024 srv"),
            (Some("1000".into()), Some("1000".into()))
        );
        // Not an `ls -l` line: say nothing rather than guess.
        assert_eq!(owner_and_group("app.conf"), (None, None));
        assert_eq!(owner_and_group(""), (None, None));
    }

    #[test]
    fn refusals_read_as_plain_reasons() {
        let denied = Failure::Refused {
            code: StatusCode::PermissionDenied,
            message: "Permission denied".into(),
        };
        assert_eq!(denied.message(), "权限不足（Permission denied）");
        assert!(denied.is(StatusCode::PermissionDenied));
        let silent = Failure::Refused {
            code: StatusCode::Failure,
            message: " ".into(),
        };
        assert_eq!(silent.message(), "服务器拒绝了该操作");
    }
}
