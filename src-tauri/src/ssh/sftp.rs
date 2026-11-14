//! Remote file operations for the file panel: listing, create/rename/delete,
//! and reading or writing small text files for the built-in editor.
//!
//! Each operation goes over the SFTP subsystem. Servers that offer none fall
//! back to running shell commands (and, for listings, a small Python script),
//! which is how iShell worked before it spoke SFTP.

use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};

use super::sftp_client::{
    is_dir, is_regular, is_symlink, parent, with_sftp, Failure, Sftp, SftpResult,
};
use super::*;
use crate::error::{AppError, AppResult};

pub(super) const MAX_TEXT_EDIT_BYTES: u64 = 1024 * 1024;

pub(super) const LIST_SCRIPT: &str = r#"import grp
import json
import os
import pwd
import stat
import sys

path = sys.argv[1] if len(sys.argv) > 1 and sys.argv[1] else "/"
items = []
with os.scandir(path) as entries:
    for entry in entries:
        try:
            st = entry.stat(follow_symlinks=False)
        except OSError:
            continue
        is_symlink = stat.S_ISLNK(st.st_mode)
        target_is_dir = False
        link_target = None
        if is_symlink:
            try:
                target = entry.stat(follow_symlinks=True)
                target_is_dir = stat.S_ISDIR(target.st_mode)
            except OSError:
                target_is_dir = False
            try:
                link_target = os.readlink(entry.path)
            except OSError:
                link_target = None
        try:
            owner = pwd.getpwuid(st.st_uid).pw_name
        except KeyError:
            owner = None
        try:
            group = grp.getgrgid(st.st_gid).gr_name
        except KeyError:
            group = None
        full_path = ("/" + entry.name) if path == "/" else path.rstrip("/") + "/" + entry.name
        items.append({
            "name": entry.name,
            "path": full_path,
            "isDir": stat.S_ISDIR(st.st_mode) or target_is_dir,
            "isSymlink": is_symlink,
            "linkTarget": link_target,
            "targetIsDir": target_is_dir if is_symlink else None,
            "size": st.st_size,
            "uid": st.st_uid,
            "gid": st.st_gid,
            "owner": owner,
            "group": group,
            "permissions": st.st_mode,
            "modifiedAt": int(st.st_mtime),
        })
print(json.dumps(items, ensure_ascii=False, separators=(",", ":")))
"#;

const RESOLVE_DIRECTORY_SCRIPT: &str = r#"import json
import os
import sys

path = os.path.expanduser(sys.argv[1])
if not os.path.isabs(path):
    raise ValueError("Cannot resolve the remote directory")
path = os.path.abspath(path)
if not os.path.isdir(path):
    raise NotADirectoryError(path)
print(json.dumps(path, ensure_ascii=False))
"#;

/// Run `action` over SFTP, or `fallback` on a server without SFTP.
fn sftp_or<T>(
    app: &AppHandle,
    id: &str,
    action: impl AsyncFn(&Sftp) -> SftpResult<T>,
    fallback: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    match with_sftp(app, id, action) {
        Ok(value) => Ok(value),
        Err(Failure::Unavailable(_)) => fallback(),
        Err(failure) => Err(failure.message()),
    }
}

fn refused(message: impl Into<String>) -> Failure {
    Failure::Refused {
        code: StatusCode::Failure,
        message: message.into(),
    }
}

pub fn list_sftp(
    _pool: &SshPool,
    app: &AppHandle,
    id: &str,
    path: &str,
) -> Result<Vec<SftpEntry>, String> {
    let current_path = if path.trim().is_empty() {
        "/"
    } else {
        path.trim()
    };
    let mut entries = sftp_or(
        app,
        id,
        async |sftp| sftp.list(current_path).await,
        || {
            let raw = run_remote_python(app, id, LIST_SCRIPT, &[current_path])?;
            serde_json::from_str(&raw).map_err(|err| format!("无法解析远程目录列表：{err}"))
        },
    )?;
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// What SFTP `realpath` should be asked for a path as a shell prompt shows
/// it. `None` for forms SFTP cannot expand, such as `~user`.
fn sftp_directory_query(path: &str) -> Option<&str> {
    match path {
        // The SFTP server starts in the home directory.
        "~" | "~/" => Some("."),
        _ if path.starts_with('/') => Some(path),
        _ => path.strip_prefix("~/"),
    }
}

/// The absolute path of a directory named the way a shell prompt shows it
/// (`~`, `~/src`, `/var/log`).
pub fn resolve_sftp_directory(app: &AppHandle, id: &str, path: &str) -> Result<String, String> {
    let fallback = || {
        let raw = run_remote_python(app, id, RESOLVE_DIRECTORY_SCRIPT, &[path])?;
        serde_json::from_str(&raw).map_err(|err| format!("无法解析远程目录路径：{err}"))
    };
    let Some(query) = sftp_directory_query(path) else {
        return fallback();
    };
    sftp_or(
        app,
        id,
        async |sftp| {
            let resolved = sftp.realpath(query).await?;
            if !is_dir(&sftp.stat(&resolved).await?) {
                return Err(refused(format!("{resolved} 不是目录")));
            }
            Ok(resolved)
        },
        fallback,
    )
}

pub fn make_directory(
    _pool: &SshPool,
    app: &AppHandle,
    id: &str,
    path: &str,
) -> Result<(), String> {
    sftp_or(
        app,
        id,
        async |sftp| sftp.mkdir_all(path).await,
        || {
            run_remote_shell(
                app,
                id,
                &format!("mkdir -p -- {}", openssh::shell_quote(path)),
            )
            .map(|_| ())
        },
    )
    .map_err(|err| format!("无法创建目录：{err}"))
}

pub fn remove_entry(
    _pool: &SshPool,
    app: &AppHandle,
    id: &str,
    path: &str,
    is_dir_hint: bool,
) -> Result<(), String> {
    let shell_remove = || {
        let flags = if is_dir_hint { "-rf" } else { "-f" };
        run_remote_shell(
            app,
            id,
            &format!("rm {flags} -- {}", openssh::shell_quote(path)),
        )
        .map(|_| ())
    };
    // What is at `path`: nothing, a directory, or something to unlink. A
    // symlink is unlinked even when it points at a directory.
    let kind = sftp_or(
        app,
        id,
        async |sftp| {
            Ok(match sftp.lstat_optional(path).await? {
                None => Some("missing"),
                Some(attrs) if is_dir(&attrs) && !is_symlink(&attrs) => {
                    // An empty directory needs nothing more.
                    if sftp.remove_dir(path).await.is_ok() {
                        Some("missing")
                    } else {
                        Some("directory")
                    }
                }
                Some(_) => {
                    sftp.remove_file(path).await?;
                    Some("missing")
                }
            })
        },
        || Ok(None),
    )
    .map_err(|err| format!("无法删除：{err}"))?;

    match kind {
        Some("missing") => Ok(()),
        // No SFTP: the shell is the only way.
        None => shell_remove().map_err(|err| format!("无法删除：{err}")),
        // A large tree goes much faster as one `rm -rf` than file by file;
        // walk it over SFTP only where no shell will do it.
        Some(_) => {
            if shell_remove().is_ok() {
                return Ok(());
            }
            with_sftp(app, id, async |sftp| {
                match sftp.lstat_optional(path).await? {
                    None => Ok(()),
                    Some(_) => sftp.remove_tree(path).await,
                }
            })
            .map_err(|failure| format!("无法删除：{}", failure.message()))
        }
    }
}

pub fn rename_entry(
    _pool: &SshPool,
    app: &AppHandle,
    id: &str,
    from: &str,
    to: &str,
) -> Result<(), String> {
    sftp_or(
        app,
        id,
        async |sftp| sftp.rename_over(from, to).await.map(|_| ()),
        || {
            run_remote_shell(
                app,
                id,
                &format!(
                    "mv -f -- {} {}",
                    openssh::shell_quote(from),
                    openssh::shell_quote(to)
                ),
            )
            .map(|_| ())
        },
    )
    .map_err(|err| format!("无法重命名：{err}"))
}

/// A text file opened in the editor. `revision` identifies the version that
/// was read, so a save can tell whether the file changed in the meantime.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextFile {
    pub content: String,
    pub revision: String,
}

fn too_large() -> String {
    format!(
        "文件超过可编辑大小限制（最大 {}）",
        format_bytes(MAX_TEXT_EDIT_BYTES)
    )
}

/// Modification time and size: what SFTP reports for every file.
fn revision_of(attrs: &FileAttributes) -> String {
    format!(
        "{}:{}",
        attrs.mtime.unwrap_or_default(),
        attrs.size.unwrap_or_default()
    )
}

/// The same revision from a shell, where there is no SFTP.
fn shell_revision(app: &AppHandle, id: &str, path: &str) -> Result<String, String> {
    let quoted = openssh::shell_quote(path);
    let raw = run_remote_shell(
        app,
        id,
        &format!("stat -c %Y:%s -- {quoted} 2>/dev/null || stat -f %m:%z -- {quoted}"),
    )?;
    Ok(raw.trim().to_string())
}

pub fn read_text_file(
    _pool: &SshPool,
    app: &AppHandle,
    id: &str,
    path: &str,
) -> Result<TextFile, String> {
    let (raw, revision) = sftp_or(
        app,
        id,
        async |sftp| {
            let attrs = sftp.stat(path).await?;
            if !is_regular(&attrs) {
                return Err(refused("只能编辑普通文件"));
            }
            if attrs.size.unwrap_or_default() > MAX_TEXT_EDIT_BYTES {
                return Err(refused(too_large()));
            }
            let content = sftp
                .read_file(path, MAX_TEXT_EDIT_BYTES)
                .await
                .map_err(|failure| match failure {
                    Failure::Refused { message, .. } if message == "文件超过大小限制" => {
                        refused(too_large())
                    }
                    other => other,
                })?;
            Ok((content, revision_of(&attrs)))
        },
        || {
            if remote_file_size(app, id, path)? > MAX_TEXT_EDIT_BYTES {
                return Err(too_large());
            }
            let revision = shell_revision(app, id, path)?;
            let raw = read_remote_file_bytes(app, id, path)?;
            if raw.len() as u64 > MAX_TEXT_EDIT_BYTES {
                return Err(too_large());
            }
            Ok((raw, revision))
        },
    )?;
    Ok(TextFile {
        content: text_from_bytes(raw)?,
        revision,
    })
}

fn file_changed() -> AppError {
    AppError::new(
        AppError::FILE_CHANGED,
        "远程文件在打开后已被其他人或程序修改",
    )
}

/// How a save reached the disk.
#[derive(Debug, PartialEq, Eq)]
enum Saved {
    /// Written beside the file and renamed over it: readers never see a
    /// partial file, and a dropped connection leaves the old one intact.
    Replaced,
    /// Written into the existing file, where replacing it was not possible
    /// without changing who owns it.
    InPlace,
}

/// Write `content` over the file at `target` (already resolved through any
/// symlink) whose attributes are `existing`.
async fn save_over_sftp(
    sftp: &Sftp,
    target: &str,
    existing: &FileAttributes,
    content: &[u8],
) -> SftpResult<Saved> {
    let name = target.rsplit('/').next().unwrap_or("file");
    let temp = format!(
        "{}/.{name}.ishell-{}.tmp",
        parent(target).trim_end_matches('/'),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    // Without an atomic rename a replace would briefly leave no file at all.
    let staged = if sftp.renames_atomically() {
        let private = FileAttributes {
            permissions: Some(0o600),
            ..FileAttributes::empty()
        };
        sftp.open(
            &temp,
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
            private,
        )
        .await
        .ok()
    } else {
        None
    };

    if let Some(handle) = staged {
        let staged = async {
            sftp.write_all(&handle, content).await?;
            // The new file must end up with the old one's owner and mode. A
            // user who cannot give it the old owner must not replace the file.
            let created = sftp.fstat(&handle).await?;
            let same_owner = created.uid == existing.uid && created.gid == existing.gid;
            let owner_kept = same_owner
                || sftp
                    .fsetstat(
                        &handle,
                        FileAttributes {
                            uid: existing.uid,
                            gid: existing.gid,
                            ..FileAttributes::empty()
                        },
                    )
                    .await
                    .is_ok();
            if owner_kept {
                sftp.fsetstat(
                    &handle,
                    FileAttributes {
                        permissions: existing.permissions.map(|mode| mode & 0o7777),
                        ..FileAttributes::empty()
                    },
                )
                .await?;
                sftp.sync(&handle).await?;
            }
            Ok::<bool, Failure>(owner_kept)
        }
        .await;
        let closed = sftp.close(handle).await;
        let committed = match (staged, closed) {
            (Ok(true), Ok(())) => sftp.rename_over(&temp, target).await.map(|_| true),
            (Ok(false), _) => Ok(false),
            (Err(failure), _) | (_, Err(failure)) => Err(failure),
        };
        match committed {
            Ok(true) => return Ok(Saved::Replaced),
            Ok(false) => {
                let _ = sftp.remove_file(&temp).await;
            }
            Err(failure) => {
                let _ = sftp.remove_file(&temp).await;
                return Err(failure);
            }
        }
    }

    let handle = sftp
        .open(
            target,
            OpenFlags::WRITE | OpenFlags::TRUNCATE,
            FileAttributes::empty(),
        )
        .await?;
    let written = sftp.write_all(&handle, content).await;
    let closed = sftp.close(handle).await;
    written.and(closed).map(|()| Saved::InPlace)
}

/// Save the editor's content. With `expected_revision`, the save is refused
/// (code `file_changed`) if the file is no longer the version that was read.
/// Returns the revision of the saved file.
pub fn write_text_file(
    _pool: &SshPool,
    app: &AppHandle,
    id: &str,
    path: &str,
    content: &str,
    expected_revision: Option<&str>,
) -> AppResult<String> {
    if content.len() as u64 > MAX_TEXT_EDIT_BYTES {
        return Err(format!(
            "内容超过可编辑大小限制（最大 {}）",
            format_bytes(MAX_TEXT_EDIT_BYTES)
        )
        .into());
    }
    if content.as_bytes().contains(&0) {
        return Err("仅支持保存文本内容".into());
    }

    let over_sftp = with_sftp(app, id, async |sftp| {
        // Edit the file a symlink points at, not the link.
        let target = sftp.realpath(path).await?;
        let existing = sftp.stat(&target).await?;
        if !is_regular(&existing) {
            return Err(refused("只能保存到普通文件"));
        }
        if existing.size.unwrap_or_default() > MAX_TEXT_EDIT_BYTES {
            return Err(refused(too_large()));
        }
        if expected_revision.is_some_and(|expected| expected != revision_of(&existing)) {
            return Ok(None);
        }
        save_over_sftp(sftp, &target, &existing, content.as_bytes()).await?;
        Ok(Some(revision_of(&sftp.stat(&target).await?)))
    });
    match over_sftp {
        Ok(Some(revision)) => return Ok(revision),
        Ok(None) => return Err(file_changed()),
        Err(Failure::Unavailable(_)) => {}
        Err(failure) => return Err(format!("保存文件失败：{}", failure.message()).into()),
    }

    // No SFTP: overwrite through the shell, as before.
    let size = remote_file_size(app, id, path).map_err(|err| format!("无法确认远程文件：{err}"))?;
    if size > MAX_TEXT_EDIT_BYTES {
        return Err(too_large().into());
    }
    if let Some(expected) = expected_revision {
        if shell_revision(app, id, path)? != expected {
            return Err(file_changed());
        }
    }
    text_from_bytes(read_remote_file_bytes(app, id, path)?)?;
    let remote_command = format!("cat > {}", openssh::shell_quote(path));
    run_remote_command(app, id, &remote_command, Some(content.as_bytes()))
        .map_err(|err| format!("保存文件失败：{err}"))?;
    Ok(shell_revision(app, id, path)?)
}

pub(super) fn text_from_bytes(raw: Vec<u8>) -> Result<String, String> {
    if raw.contains(&0) {
        return Err("仅支持编辑文本文件，当前文件像是二进制文件".to_string());
    }
    String::from_utf8(raw).map_err(|_| "仅支持编辑 UTF-8 文本文件".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_paths_map_to_sftp_queries() {
        assert_eq!(sftp_directory_query("~"), Some("."));
        assert_eq!(sftp_directory_query("~/src/app"), Some("src/app"));
        assert_eq!(sftp_directory_query("/var/log"), Some("/var/log"));
        // SFTP cannot expand another user's home; the shell fallback can.
        assert_eq!(sftp_directory_query("~deploy/app"), None);
        assert_eq!(sftp_directory_query("relative"), None);
    }

    #[test]
    fn revisions_change_with_time_or_size() {
        let attrs = |mtime, size| FileAttributes {
            mtime: Some(mtime),
            size: Some(size),
            ..FileAttributes::empty()
        };
        assert_eq!(revision_of(&attrs(10, 4)), "10:4");
        assert_ne!(revision_of(&attrs(10, 4)), revision_of(&attrs(11, 4)));
        assert_ne!(revision_of(&attrs(10, 4)), revision_of(&attrs(10, 5)));
    }
}
