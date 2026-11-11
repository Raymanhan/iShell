//! Local files for ZMODEM transfers in the terminal (`rz`/`sz`; the protocol
//! itself runs in the frontend). Files to send are picked here and read only
//! by the id the pick returned; received files go to the downloads folder.

use std::{
    collections::HashMap,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Mutex,
    time::UNIX_EPOCH,
};

use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

use crate::ssh::{local_download_name, unique_file_path};

/// Most bytes one read returns.
const MAX_CHUNK: u64 = 1024 * 1024;

/// Files the user picked to send, by id.
#[derive(Default)]
pub struct PickedFiles(Mutex<HashMap<String, PathBuf>>);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickedFile {
    pub id: String,
    pub name: String,
    pub size: u64,
    /// Seconds since the epoch.
    pub mtime: Option<u64>,
}

impl PickedFiles {
    /// Remember `paths` (regular files only) and describe them.
    pub fn register(&self, paths: Vec<PathBuf>) -> Result<Vec<PickedFile>, String> {
        let mut picked = Vec::new();
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        for path in paths {
            let metadata =
                fs::metadata(&path).map_err(|err| format!("无法读取 {}：{err}", path.display()))?;
            if !metadata.is_file() {
                return Err(format!("只能发送文件：{}", path.display()));
            }
            let id = Uuid::new_v4().to_string();
            picked.push(PickedFile {
                id: id.clone(),
                name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "file".into()),
                size: metadata.len(),
                mtime: metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs()),
            });
            map.insert(id, path);
        }
        Ok(picked)
    }

    /// Up to `length` bytes of a picked file from `offset`.
    pub fn read(&self, id: &str, offset: u64, length: u64) -> Result<Vec<u8>, String> {
        let path = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or("找不到要发送的文件")?;
        let mut file = fs::File::open(&path).map_err(|err| format!("无法打开文件：{err}"))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|err| format!("无法读取文件：{err}"))?;
        let mut data = Vec::new();
        file.take(length.min(MAX_CHUNK))
            .read_to_end(&mut data)
            .map_err(|err| format!("无法读取文件：{err}"))?;
        Ok(data)
    }

    /// Forget files once sent.
    pub fn forget(&self, ids: &[String]) {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        for id in ids {
            map.remove(id);
        }
    }
}

/// Ask the user for files to send. Empty when cancelled.
pub async fn pick(app: &AppHandle) -> Result<Vec<PickedFile>, String> {
    let dialog = app.dialog().file().set_title("选择要上传的文件（rz）");
    let picked = tauri::async_runtime::spawn_blocking(move || dialog.blocking_pick_files())
        .await
        .map_err(|err| err.to_string())?
        .unwrap_or_default();
    let paths = picked
        .into_iter()
        .map(|file| file.into_path().map_err(|err| err.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    app.state::<PickedFiles>().register(paths)
}

/// Save a received file in `dir` under a name that is free; returns its path.
pub fn save_in(dir: &Path, name: &str, data: &[u8]) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|err| format!("无法创建下载目录：{err}"))?;
    // Only the last component of the sender's name, never a path.
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .filter(|base| !base.is_empty() && *base != "." && *base != "..")
        .unwrap_or("received-file");
    let path = unique_file_path(dir, &local_download_name(base));
    fs::write(&path, data).map_err(|err| format!("无法保存文件：{err}"))?;
    Ok(path)
}

pub fn save(app: &AppHandle, name: &str, data: &[u8]) -> Result<String, String> {
    let dir = app
        .path()
        .download_dir()
        .map_err(|err| format!("无法定位下载目录：{err}"))?;
    save_in(&dir, name, data).map(|path| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picked_files_are_read_in_pieces_by_id_only() {
        let dir = std::env::temp_dir().join(format!("ishell-zm-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.bin");
        fs::write(&path, b"0123456789").unwrap();
        let files = PickedFiles::default();
        let picked = files.register(vec![path]).unwrap();
        assert_eq!(picked[0].name, "a.bin");
        assert_eq!(picked[0].size, 10);
        assert!(picked[0].mtime.is_some());
        assert_eq!(files.read(&picked[0].id, 3, 4).unwrap(), b"3456");
        assert_eq!(files.read(&picked[0].id, 8, 100).unwrap(), b"89");
        assert!(files.read("other", 0, 1).is_err());
        assert!(
            files.register(vec![dir.clone()]).is_err(),
            "folders are refused"
        );
        files.forget(&[picked[0].id.clone()]);
        assert!(files.read(&picked[0].id, 0, 1).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn received_files_keep_to_the_folder_and_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("ishell-zm-{}", Uuid::new_v4()));
        let first = save_in(&dir, "../../evil.txt", b"one").unwrap();
        assert_eq!(first, dir.join("evil.txt"));
        let second = save_in(&dir, "evil.txt", b"two").unwrap();
        assert_eq!(second, dir.join("evil (1).txt"));
        assert_eq!(fs::read(&first).unwrap(), b"one");
        assert_eq!(
            save_in(&dir, "..", b"x").unwrap(),
            dir.join("received-file")
        );
        fs::remove_dir_all(&dir).ok();
    }
}
