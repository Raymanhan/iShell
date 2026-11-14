//! The local master key and the AES-256-GCM encryption built on it.
//!
//! Saved passwords, the AI API key and AI conversations are encrypted with one
//! random 256-bit master key. The key lives in the operating system's
//! credential store (macOS Keychain, Windows Credential Manager, the Secret
//! Service on Linux), so copying the app data directory is not enough to read
//! them. Where no credential store is available the key falls back to a
//! `secret.key` file with 0600 permissions next to the database.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use tauri::{AppHandle, Manager};

const KEY_FILE: &str = "secret.key";
/// Written once the master key has been moved into the credential store. With
/// it present a missing or unreadable store entry is an error: generating a
/// fresh key would silently make every saved secret undecryptable.
const KEYCHAIN_MARKER: &str = "secret.key.keychain";
const KEYCHAIN_SERVICE: &str = "com.iShell.desktop";
const KEYCHAIN_ACCOUNT: &str = "master-key";
/// Set to `file` to keep the master key in `secret.key` and never touch the
/// credential store (headless machines, or to avoid keychain prompts).
const STORAGE_ENV: &str = "ISHELL_SECRET_STORAGE";

pub type MasterKey = [u8; 32];

/// Where the master key is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    Keychain,
    File,
}

impl Storage {
    pub fn as_str(self) -> &'static str {
        match self {
            Storage::Keychain => "keychain",
            Storage::File => "file",
        }
    }
}

/// The credential store holding the master key.
pub trait KeyVault {
    /// `Ok(None)` when the store works but has no key yet.
    fn load(&self) -> Result<Option<Vec<u8>>, String>;
    fn store(&self, key: &[u8]) -> Result<(), String>;
}

/// The operating system's credential store.
struct OsVault;

impl OsVault {
    /// Credential store calls can block on a D-Bus round trip or an unlock
    /// prompt and must not run inside an async runtime, so each one gets its
    /// own thread.
    fn call<T: Send + 'static>(
        action: impl FnOnce(&keyring::Entry) -> keyring::Result<T> + Send + 'static,
    ) -> keyring::Result<T> {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
                .and_then(|entry| action(&entry));
            let _ = sender.send(result);
        });
        // Only Linux gets a deadline: a session without a Secret Service can
        // leave the D-Bus call hanging, while macOS and Windows may be showing
        // the user an unlock prompt.
        #[cfg(all(unix, not(target_os = "macos")))]
        let received = receiver
            .recv_timeout(std::time::Duration::from_secs(30))
            .ok();
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        let received = receiver.recv().ok();
        received.unwrap_or_else(|| {
            Err(keyring::Error::Invalid(
                "credential store".into(),
                "did not answer".into(),
            ))
        })
    }
}

impl KeyVault for OsVault {
    fn load(&self) -> Result<Option<Vec<u8>>, String> {
        match Self::call(|entry| entry.get_password()) {
            Ok(encoded) => BASE64
                .decode(encoded.trim())
                .map(Some)
                .map_err(|_| "系统钥匙串中的 iShell 主密钥格式无效".to_string()),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(err.to_string()),
        }
    }

    fn store(&self, key: &[u8]) -> Result<(), String> {
        let encoded = BASE64.encode(key);
        Self::call(move |entry| entry.set_password(&encoded)).map_err(|err| err.to_string())
    }
}

/// A vault that always fails, for `ISHELL_SECRET_STORAGE=file`.
struct NoVault;

impl KeyVault for NoVault {
    fn load(&self) -> Result<Option<Vec<u8>>, String> {
        Err("credential store disabled".into())
    }

    fn store(&self, _key: &[u8]) -> Result<(), String> {
        Err("credential store disabled".into())
    }
}

fn as_key(bytes: &[u8]) -> Option<MasterKey> {
    <MasterKey>::try_from(bytes).ok()
}

fn read_key_file(path: &Path) -> Option<MasterKey> {
    fs::read(path).ok().and_then(|bytes| as_key(&bytes))
}

fn write_key_file(path: &Path, key: &MasterKey) -> Result<(), String> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|err| format!("无法写入本地主密钥：{err}"))?;
    std::io::Write::write_all(&mut file, key)
        .map_err(|err| format!("无法写入本地主密钥：{err}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Put `key` into the vault and confirm it reads back unchanged.
fn store_verified(vault: &dyn KeyVault, key: &MasterKey) -> bool {
    vault.store(key).is_ok()
        && matches!(vault.load(), Ok(Some(saved)) if saved.as_slice() == key.as_slice())
}

/// Find the master key, creating it on first use and moving a key file left by
/// an earlier version into the vault when the vault works.
pub fn resolve_master_key(
    dir: &Path,
    vault: &dyn KeyVault,
) -> Result<(MasterKey, Storage), String> {
    let file = dir.join(KEY_FILE);
    let marker = dir.join(KEYCHAIN_MARKER);

    if marker.exists() {
        return match vault.load() {
            Ok(Some(bytes)) => as_key(&bytes)
                .map(|key| (key, Storage::Keychain))
                .ok_or_else(|| "系统钥匙串中的 iShell 主密钥长度无效".to_string()),
            // A key file restored from a backup still opens the data.
            Ok(None) | Err(_) if file.exists() => read_key_file(&file)
                .map(|key| (key, Storage::File))
                .ok_or_else(|| "本地主密钥文件已损坏".to_string()),
            Ok(None) => Err(
                "系统钥匙串中找不到 iShell 主密钥，已保存的密码无法解密；请恢复钥匙串条目，或重新保存各主机的密码"
                    .into(),
            ),
            Err(err) => Err(format!(
                "无法从系统钥匙串读取 iShell 主密钥：{err}。请解锁钥匙串并允许 iShell 访问后重试"
            )),
        };
    }

    if file.exists() {
        let key = read_key_file(&file).ok_or_else(|| "本地主密钥文件已损坏".to_string())?;
        if store_verified(vault, &key) && fs::write(&marker, b"").is_ok() {
            let _ = fs::remove_file(&file);
            return Ok((key, Storage::Keychain));
        }
        return Ok((key, Storage::File));
    }

    // No marker and no file: a first run, or the data directory was recreated
    // while the vault kept its entry.
    if let Ok(Some(bytes)) = vault.load() {
        if let Some(key) = as_key(&bytes) {
            let _ = fs::write(&marker, b"");
            return Ok((key, Storage::Keychain));
        }
    }

    let mut key = [0u8; 32];
    getrandom::getrandom(&mut key).map_err(|err| format!("无法生成本地主密钥：{err}"))?;
    if store_verified(vault, &key) && fs::write(&marker, b"").is_ok() {
        return Ok((key, Storage::Keychain));
    }
    write_key_file(&file, &key)?;
    Ok((key, Storage::File))
}

static MASTER: Mutex<Option<(MasterKey, Storage)>> = Mutex::new(None);

fn master(app: &AppHandle) -> Result<(MasterKey, Storage), String> {
    // Held across the lookup so concurrent callers share one keychain prompt.
    let mut cached = MASTER.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(value) = *cached {
        return Ok(value);
    }
    let dir = app_data_dir(app)?;
    let value = if std::env::var(STORAGE_ENV).is_ok_and(|value| value == "file") {
        resolve_master_key(&dir, &NoVault)?
    } else {
        resolve_master_key(&dir, &OsVault)?
    };
    *cached = Some(value);
    Ok(value)
}

/// Where the master key is kept, for the settings window.
pub fn storage(app: &AppHandle) -> Result<Storage, String> {
    master(app).map(|(_, storage)| storage)
}

/// Encrypt with AES-256-GCM. The output is `nonce (12 bytes) || ciphertext`.
pub fn encrypt_secret(app: &AppHandle, plaintext: &str) -> Result<Vec<u8>, String> {
    encrypt_with(&master(app)?.0, plaintext)
}

pub fn decrypt_secret(app: &AppHandle, blob: &[u8]) -> Result<String, String> {
    decrypt_with(&master(app)?.0, blob)
}

fn encrypt_with(key: &MasterKey, plaintext: &str) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; 12];
    getrandom::getrandom(&mut nonce_bytes).map_err(|err| format!("无法生成随机数：{err}"))?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
        .map_err(|_| "密码加密失败".to_string())?;

    let mut blob = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok(blob)
}

fn decrypt_with(key: &MasterKey, blob: &[u8]) -> Result<String, String> {
    if blob.len() < 12 {
        return Err("密码数据已损坏".into());
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&blob[..12]), &blob[12..])
        .map_err(|_| "密码解密失败，请重新编辑主机并保存密码".to_string())?;
    String::from_utf8(plaintext).map_err(|_| "密码解码失败".to_string())
}

pub fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("无法定位应用数据目录：{err}"))?;
    fs::create_dir_all(&dir).map_err(|err| format!("无法创建应用数据目录：{err}"))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// An in-memory vault that can be switched off.
    #[derive(Default)]
    struct MemoryVault {
        key: RefCell<Option<Vec<u8>>>,
        broken: bool,
    }

    impl KeyVault for MemoryVault {
        fn load(&self) -> Result<Option<Vec<u8>>, String> {
            if self.broken {
                return Err("locked".into());
            }
            Ok(self.key.borrow().clone())
        }

        fn store(&self, key: &[u8]) -> Result<(), String> {
            if self.broken {
                return Err("locked".into());
            }
            *self.key.borrow_mut() = Some(key.to_vec());
            Ok(())
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ishell-secrets-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn first_run_keeps_the_key_in_the_vault_only() {
        let dir = temp_dir();
        let vault = MemoryVault::default();
        let (key, storage) = resolve_master_key(&dir, &vault).unwrap();
        assert_eq!(storage, Storage::Keychain);
        assert!(!dir.join(KEY_FILE).exists());
        assert_eq!(vault.key.borrow().as_deref(), Some(key.as_slice()));
        // The next start finds the same key.
        assert_eq!(resolve_master_key(&dir, &vault).unwrap().0, key);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn existing_key_file_moves_into_the_vault_unchanged() {
        let dir = temp_dir();
        let legacy = [7u8; 32];
        fs::write(dir.join(KEY_FILE), legacy).unwrap();
        let vault = MemoryVault::default();
        let (key, storage) = resolve_master_key(&dir, &vault).unwrap();
        assert_eq!((key, storage), (legacy, Storage::Keychain));
        assert!(!dir.join(KEY_FILE).exists());
        assert!(dir.join(KEYCHAIN_MARKER).exists());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn without_a_vault_the_key_stays_in_a_private_file() {
        let dir = temp_dir();
        let vault = MemoryVault {
            broken: true,
            ..Default::default()
        };
        let (key, storage) = resolve_master_key(&dir, &vault).unwrap();
        assert_eq!(storage, Storage::File);
        assert_eq!(fs::read(dir.join(KEY_FILE)).unwrap(), key);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join(KEY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Still the same key later, and it moves once the vault works.
        assert_eq!(resolve_master_key(&dir, &vault).unwrap().0, key);
        let working = MemoryVault::default();
        assert_eq!(
            resolve_master_key(&dir, &working).unwrap(),
            (key, Storage::Keychain)
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_lost_vault_entry_is_an_error_not_a_new_key() {
        let dir = temp_dir();
        let vault = MemoryVault::default();
        resolve_master_key(&dir, &vault).unwrap();

        let locked = MemoryVault {
            broken: true,
            ..Default::default()
        };
        assert!(resolve_master_key(&dir, &locked).is_err());
        let emptied = MemoryVault::default();
        assert!(resolve_master_key(&dir, &emptied).is_err());
        assert!(emptied.key.borrow().is_none());
        assert!(!dir.join(KEY_FILE).exists());
        fs::remove_dir_all(dir).ok();
    }

    /// Talks to the real credential store, so it only runs on request:
    /// `cargo test os_vault -- --ignored`. Whatever the store's state, the
    /// lookup must end in a usable key rather than hang or panic.
    #[test]
    #[ignore]
    fn os_vault_resolves_or_falls_back() {
        let dir = temp_dir();
        let (key, storage) = resolve_master_key(&dir, &OsVault).unwrap();
        println!("master key storage: {storage:?}");
        assert_eq!(resolve_master_key(&dir, &OsVault).unwrap().0, key);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn encryption_round_trips_and_rejects_the_wrong_key() {
        let key = [3u8; 32];
        let blob = encrypt_with(&key, "pässword").unwrap();
        assert_eq!(decrypt_with(&key, &blob).unwrap(), "pässword");
        assert!(decrypt_with(&[4u8; 32], &blob).is_err());
        assert!(decrypt_with(&key, &blob[..8]).is_err());
    }
}
