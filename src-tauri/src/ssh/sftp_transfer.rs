//! Uploads and downloads over SFTP, for servers where the faster
//! command-based transfers cannot run: no `python3` or `tar`, or an account
//! that may only use SFTP. Each transfer gets an SFTP session of its own.

use futures_util::future::join_all;
use russh_sftp::protocol::{FileAttributes, OpenFlags};

use super::sftp_client::{
    is_dir, is_regular, join, with_dedicated_sftp, Failure, Sftp, SftpResult, PIPELINE,
};
use super::*;

const UPLOAD_EVENT: &str = "sftp-upload-progress";
const DOWNLOAD_EVENT: &str = "sftp-download-progress";

/// Whether transfers for this server have to go over SFTP. Asked once per
/// connection: the command-based path needs a shell with `python3` and `tar`.
pub(super) fn use_sftp_transfers(app: &AppHandle, id: &str) -> bool {
    if let Some(known) = sftp_client::exec_transfers_known(id) {
        return !known;
    }
    // An empty stdin, so an account whose shell is the SFTP server itself
    // sees end of input and exits instead of waiting for a packet.
    let probe = transport::exec(
        app,
        id,
        "command -v python3 >/dev/null 2>&1 && command -v tar >/dev/null 2>&1 && printf ishell-ok",
        Some(b""),
    );
    let available = probe.is_ok_and(|output| output == b"ishell-ok");
    sftp_client::remember_exec_transfers(id, available);
    !available
}

/// A transfer that did not complete.
enum Stop {
    Canceled,
    Failed(String),
}

impl From<Failure> for Stop {
    fn from(failure: Failure) -> Self {
        Stop::Failed(failure.message())
    }
}

/// Throttled progress events for one transfer.
struct Progress<'a> {
    app: &'a AppHandle,
    event: &'static str,
    transfer_id: &'a str,
    total: u64,
    done: u64,
    last_emit: Instant,
}

impl<'a> Progress<'a> {
    fn start(app: &'a AppHandle, event: &'static str, transfer_id: &'a str, total: u64) -> Self {
        emit_progress(app, event, transfer_id, 0, total, false);
        Self {
            app,
            event,
            transfer_id,
            total,
            done: 0,
            last_emit: Instant::now(),
        }
    }

    fn add(&mut self, bytes: u64) {
        self.done += bytes;
        if self.last_emit.elapsed() >= Duration::from_millis(80) {
            emit_progress(
                self.app,
                self.event,
                self.transfer_id,
                self.done,
                self.total,
                false,
            );
            self.last_emit = Instant::now();
        }
    }

    fn finish(&self) {
        let total = self.total.max(self.done);
        emit_progress(self.app, self.event, self.transfer_id, total, total, true);
    }
}

/// Fill `buffer` from `reader`; fewer bytes only at end of input.
fn read_full(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

/// Copy a local file to a new remote file, several chunks in flight at once.
async fn put_file(
    sftp: &Sftp,
    local: &mut fs::File,
    remote_path: &str,
    mode: u32,
    progress: &mut Progress<'_>,
    is_canceled: &dyn Fn() -> bool,
) -> Result<(), Stop> {
    let handle = sftp
        .open(
            remote_path,
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
            FileAttributes {
                permissions: Some(mode & 0o777),
                ..FileAttributes::empty()
            },
        )
        .await?;
    let mut offset = 0_u64;
    let copied = async {
        loop {
            if is_canceled() {
                return Err(Stop::Canceled);
            }
            let mut batch = Vec::with_capacity(PIPELINE);
            for _ in 0..PIPELINE {
                let mut chunk = vec![0_u8; sftp.io_len()];
                let read = read_full(local, &mut chunk)
                    .map_err(|err| Stop::Failed(format!("读取本地文件失败：{err}")))?;
                if read == 0 {
                    break;
                }
                chunk.truncate(read);
                batch.push((offset, chunk));
                offset += read as u64;
            }
            if batch.is_empty() {
                return Ok(());
            }
            let sent: u64 = batch.iter().map(|(_, chunk)| chunk.len() as u64).sum();
            let writes = batch
                .into_iter()
                .map(|(at, chunk)| sftp.write_at(&handle, at, chunk));
            for result in join_all(writes).await {
                result?;
            }
            progress.add(sent);
        }
    }
    .await;
    let closed = sftp.close(handle).await;
    copied?;
    Ok(closed?)
}

/// Copy a remote file into `local`, several chunks in flight at once.
async fn get_file(
    sftp: &Sftp,
    remote_path: &str,
    local: &mut impl Write,
    progress: &mut Progress<'_>,
    is_canceled: &dyn Fn() -> bool,
) -> Result<(), Stop> {
    let handle = sftp
        .open(remote_path, OpenFlags::READ, FileAttributes::empty())
        .await?;
    let step = sftp.io_len();
    let mut offset = 0_u64;
    let copied = async {
        loop {
            if is_canceled() {
                return Err(Stop::Canceled);
            }
            let reads = (0..PIPELINE)
                .map(|index| sftp.read_at(&handle, offset + (index * step) as u64, step));
            let mut at_end = false;
            for (index, result) in join_all(reads).await.into_iter().enumerate() {
                let mut chunk = result?;
                if at_end {
                    continue;
                }
                // A server may answer with less than was asked for: fetch the
                // rest of this chunk before moving on to the next.
                let chunk_offset = offset + (index * step) as u64;
                while chunk.len() < step {
                    let more = sftp
                        .read_at(
                            &handle,
                            chunk_offset + chunk.len() as u64,
                            step - chunk.len(),
                        )
                        .await?;
                    if more.is_empty() {
                        at_end = true;
                        break;
                    }
                    chunk.extend_from_slice(&more);
                }
                local
                    .write_all(&chunk)
                    .map_err(|err| Stop::Failed(format!("写入本地文件失败：{err}")))?;
                progress.add(chunk.len() as u64);
            }
            if at_end {
                return Ok(());
            }
            offset += (PIPELINE * step) as u64;
        }
    }
    .await;
    let _ = sftp.close(handle).await;
    copied
}

fn local_mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        0o644
    }
}

fn finish<T>(
    outcome: SftpResult<Result<T, Stop>>,
    stopped: &str,
    failed: &str,
) -> Result<T, String> {
    match outcome {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(Stop::Canceled)) => Err(stopped.to_string()),
        Ok(Err(Stop::Failed(message))) => Err(format!("{failed}：{message}")),
        Err(failure) => Err(format!("{failed}：{}", failure.message())),
    }
}

pub(super) fn upload_file(
    app: &AppHandle,
    id: &str,
    local_path: &str,
    remote_dir: &str,
    transfer_id: &str,
    is_canceled: &dyn Fn() -> bool,
) -> Result<String, String> {
    let file_name = Path::new(local_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| "无效的本地文件路径".to_string())?;
    let remote_path = join(remote_dir, &file_name);
    let mut local = fs::File::open(local_path).map_err(|err| format!("无法打开本地文件：{err}"))?;
    let metadata = local
        .metadata()
        .map_err(|err| format!("无法读取本地文件信息：{err}"))?;
    let temp_path = upload::remote_upload_temp_path(remote_dir);

    let outcome = with_dedicated_sftp(app, id, async |sftp| {
        if sftp
            .lstat_optional(&remote_path)
            .await?
            .is_some_and(|attrs| is_dir(&attrs))
        {
            return Ok(Err(Stop::Failed(
                "远程已存在同名文件夹，无法用文件覆盖".into(),
            )));
        }
        let mut progress = Progress::start(app, UPLOAD_EVENT, transfer_id, metadata.len());
        let mut result = put_file(
            sftp,
            &mut local,
            &temp_path,
            local_mode(&metadata),
            &mut progress,
            is_canceled,
        )
        .await;
        if result.is_ok() {
            // Only a complete upload replaces what was there.
            result = sftp
                .rename_over(&temp_path, &remote_path)
                .await
                .map(|_| ())
                .map_err(Stop::from);
        }
        match result {
            Ok(()) => {
                progress.finish();
                Ok(Ok(()))
            }
            Err(stop) => {
                let _ = sftp.remove_file(&temp_path).await;
                Ok(Err(stop))
            }
        }
    });
    finish(outcome, "上传已停止", "上传失败").map(|()| remote_path)
}

/// Directories and files under a local folder, relative to it, parents first.
struct LocalTree {
    directories: Vec<PathBuf>,
    files: Vec<(PathBuf, u64)>,
}

fn walk_local(root: &Path, is_canceled: &dyn Fn() -> bool) -> Result<LocalTree, String> {
    let mut tree = LocalTree {
        directories: Vec::new(),
        files: Vec::new(),
    };
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        if is_canceled() {
            return Err("上传已停止".to_string());
        }
        let entries = fs::read_dir(root.join(&relative))
            .map_err(|err| format!("无法读取本地文件夹：{err}"))?;
        for entry in entries {
            let entry = entry.map_err(|err| format!("无法读取本地文件夹：{err}"))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err("文件夹包含非 UTF-8 文件名".to_string());
            };
            let child = relative.join(name);
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|err| format!("无法读取本地文件信息：{err}"))?;
            if metadata.is_dir() {
                tree.directories.push(child.clone());
                pending.push(child);
            } else if metadata.is_file() {
                tree.files.push((child, metadata.len()));
            } else {
                return Err(format!(
                    "文件夹包含符号链接或特殊文件，无法上传：{}",
                    child.display()
                ));
            }
        }
    }
    tree.directories.sort();
    Ok(tree)
}

fn remote_relative(base: &str, relative: &Path) -> String {
    relative
        .components()
        .fold(base.to_string(), |path, component| {
            join(&path, &component.as_os_str().to_string_lossy())
        })
}

pub(super) fn upload_folder(
    app: &AppHandle,
    id: &str,
    local_path: &str,
    remote_dir: &str,
    transfer_id: &str,
    replace_existing_folder: bool,
    is_canceled: &dyn Fn() -> bool,
) -> Result<String, String> {
    let local_root = Path::new(local_path);
    let folder_name = local_root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty() && *value != "." && *value != "..")
        .ok_or_else(|| "无效的本地文件夹名称".to_string())?
        .to_string();
    let remote_path = join(remote_dir, &folder_name);
    let root_mode = fs::metadata(local_root)
        .map(|metadata| local_mode(&metadata))
        .map_err(|err| format!("无法读取本地文件夹：{err}"))?;
    emit_progress(app, UPLOAD_EVENT, transfer_id, 0, 0, false);
    let tree = walk_local(local_root, is_canceled)?;
    let total = tree.files.iter().map(|(_, size)| size).sum();
    // Everything lands in a staging folder beside the destination, which is
    // only swapped in once the whole folder has arrived.
    let (_, staging) = upload::remote_folder_upload_paths(remote_dir);

    let outcome = with_dedicated_sftp(app, id, async |sftp| {
        match sftp.lstat_optional(&remote_path).await? {
            Some(_) if !replace_existing_folder => {
                return Ok(Err(Stop::Failed(upload::folder_upload_conflict_error())))
            }
            Some(attrs) if !is_dir(&attrs) => {
                return Ok(Err(Stop::Failed("远程目标不是可安全覆盖的文件夹".into())))
            }
            _ => {}
        }
        let mut progress = Progress::start(app, UPLOAD_EVENT, transfer_id, total);
        let staged = async {
            sftp.mkdir(&staging).await?;
            for directory in &tree.directories {
                if is_canceled() {
                    return Err(Stop::Canceled);
                }
                sftp.mkdir(&remote_relative(&staging, directory)).await?;
            }
            for (relative, _) in &tree.files {
                let source = local_root.join(relative);
                let mut local = fs::File::open(&source)
                    .map_err(|err| Stop::Failed(format!("无法打开本地文件：{err}")))?;
                let mode = local.metadata().map(|m| local_mode(&m)).unwrap_or(0o644);
                put_file(
                    sftp,
                    &mut local,
                    &remote_relative(&staging, relative),
                    mode,
                    &mut progress,
                    is_canceled,
                )
                .await?;
            }
            sftp.raw_set_mode(&staging, root_mode).await?;
            if is_canceled() {
                return Err(Stop::Canceled);
            }
            // Commit: the old folder goes only now, with the new one complete.
            if sftp.lstat_optional(&remote_path).await?.is_some() {
                sftp.remove_tree(&remote_path).await?;
            }
            sftp.rename_over(&staging, &remote_path).await?;
            Ok(())
        }
        .await;
        match staged {
            Ok(()) => {
                progress.finish();
                Ok(Ok(()))
            }
            Err(stop) => {
                if sftp.lstat_optional(&staging).await.ok().flatten().is_some() {
                    let _ = sftp.remove_tree(&staging).await;
                }
                Ok(Err(stop))
            }
        }
    });
    finish(outcome, "上传已停止", "上传失败").map(|()| remote_path)
}

/// Download one remote file into `local`.
pub(super) fn download_file(
    app: &AppHandle,
    id: &str,
    remote_path: &str,
    local: &mut fs::File,
    transfer_id: &str,
    is_canceled: &dyn Fn() -> bool,
) -> Result<(), String> {
    let outcome = with_dedicated_sftp(app, id, async |sftp| {
        let attrs = sftp.stat(remote_path).await?;
        if !is_regular(&attrs) {
            return Ok(Err(Stop::Failed("只能下载普通文件".into())));
        }
        let mut progress =
            Progress::start(app, DOWNLOAD_EVENT, transfer_id, attrs.size.unwrap_or(0));
        let result = get_file(sftp, remote_path, local, &mut progress, is_canceled).await;
        if result.is_ok() {
            progress.finish();
        }
        Ok(result)
    });
    finish(outcome, "下载已停止", "下载失败")
}

/// A name a server reported for a directory entry, if it is safe to create
/// locally: one path component, nothing that climbs out of the target.
fn safe_entry_name(name: &str) -> Option<String> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return None;
    }
    Some(download::local_download_name(name))
}

/// Download a remote folder into `local_root` (which must not exist yet),
/// keeping its structure. Symlinks and special files are left out.
pub(super) fn download_folder(
    app: &AppHandle,
    id: &str,
    remote_path: &str,
    local_root: &Path,
    transfer_id: &str,
    is_canceled: &dyn Fn() -> bool,
) -> Result<(), String> {
    let outcome = with_dedicated_sftp(app, id, async |sftp| {
        emit_progress(app, DOWNLOAD_EVENT, transfer_id, 0, 0, false);
        // Survey first, so progress has a total.
        let mut directories = vec![PathBuf::new()];
        let mut files: Vec<(String, PathBuf, u64)> = Vec::new();
        let mut pending = vec![(remote_path.to_string(), PathBuf::new())];
        while let Some((remote_dir, local_dir)) = pending.pop() {
            if is_canceled() {
                return Ok(Err(Stop::Canceled));
            }
            for (name, _, attrs) in sftp.read_dir(&remote_dir).await? {
                let Some(local_name) = safe_entry_name(&name) else {
                    return Ok(Err(Stop::Failed(format!(
                        "服务器返回了不安全的文件名：{name:?}"
                    ))));
                };
                let remote_child = join(&remote_dir, &name);
                let local_child = local_dir.join(local_name);
                if is_dir(&attrs) {
                    directories.push(local_child.clone());
                    pending.push((remote_child, local_child));
                } else if is_regular(&attrs) {
                    files.push((remote_child, local_child, attrs.size.unwrap_or(0)));
                }
            }
        }
        let total = files.iter().map(|(_, _, size)| size).sum();
        let mut progress = Progress::start(app, DOWNLOAD_EVENT, transfer_id, total);
        let copied = async {
            for directory in &directories {
                fs::create_dir_all(local_root.join(directory))
                    .map_err(|err| Stop::Failed(format!("无法创建本地文件夹：{err}")))?;
            }
            for (remote_file, local_file, _) in &files {
                let mut local = fs::File::create(local_root.join(local_file))
                    .map_err(|err| Stop::Failed(format!("无法创建本地文件：{err}")))?;
                get_file(sftp, remote_file, &mut local, &mut progress, is_canceled).await?;
            }
            Ok(())
        }
        .await;
        if copied.is_ok() {
            progress.finish();
        }
        Ok(copied)
    });
    finish(outcome, "下载已停止", "下载失败")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_entry_names_that_would_escape_the_download_folder() {
        for name in ["", ".", "..", "a/b", "..\\x", "nul\0"] {
            assert_eq!(safe_entry_name(name), None, "{name:?}");
        }
        assert_eq!(
            safe_entry_name("报告 v2.txt").as_deref(),
            Some("报告 v2.txt")
        );
    }

    #[test]
    fn walks_a_local_folder_parents_first() {
        let root = std::env::temp_dir().join(format!("ishell-walk-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("top.txt"), b"12345").unwrap();
        fs::write(root.join("a/b/deep.txt"), b"1").unwrap();

        let tree = walk_local(&root, &|| false).unwrap();
        assert_eq!(tree.directories, [PathBuf::from("a"), PathBuf::from("a/b")]);
        let mut files = tree.files.clone();
        files.sort();
        assert_eq!(
            files,
            [
                (PathBuf::from("a/b/deep.txt"), 1),
                (PathBuf::from("top.txt"), 5)
            ]
        );
        assert_eq!(
            remote_relative("/srv/.stage", Path::new("a/b")),
            "/srv/.stage/a/b"
        );
        assert!(walk_local(&root, &|| true).is_err());
        fs::remove_dir_all(root).ok();
    }
}
