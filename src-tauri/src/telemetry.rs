use reqwest::{blocking::Client, redirect::Policy, Url};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

const TICK: Duration = Duration::from_secs(5);
const HEARTBEAT: Duration = Duration::from_secs(60);
const QUEUE_LIMIT: usize = 256;
const BATCH_LIMIT: usize = 32;

pub struct Telemetry(Option<SyncSender<PendingEvent>>);

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    SshLogin,
    SshTest,
    CommandRecorded,
    ServerSaved,
    ServerDeleted,
    SftpUpload,
    SftpDownload,
    SftpMkdir,
    SftpRemove,
    SftpRename,
    SftpWrite,
}

struct PendingEvent {
    kind: EventKind,
    succeeded: bool,
    // Only a parsed internal UUID can enter the worker. Imported legacy IDs
    // that are not UUIDs are omitted rather than sent as arbitrary strings.
    connection_id: Option<Uuid>,
    occurred_at: u64,
}

#[derive(Serialize)]
struct Event {
    id: String,
    kind: EventKind,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    connection_ref: Option<String>,
    occurred_at: u64,
}

#[derive(Serialize)]
struct Report<'a> {
    platform: &'static str,
    app_version: &'static str,
    events: Vec<&'a Event>,
}

impl Telemetry {
    pub fn start(app: &AppHandle) -> Self {
        if std::env::var_os("ISHELL_TELEMETRY_DISABLED").is_some() {
            return Self(None);
        }
        let endpoint = std::env::var("ISHELL_MANAGEMENT_URL")
            .ok()
            .or_else(|| option_env!("ISHELL_MANAGEMENT_URL").map(str::to_owned));
        let Some(endpoint) = endpoint.as_deref().and_then(telemetry_endpoint) else {
            return Self(None);
        };
        let Ok(dir) = app.path().app_data_dir() else {
            return Self(None);
        };
        let (tx, rx) = mpsc::sync_channel(QUEUE_LIMIT);
        let started = thread::Builder::new()
            .name("ishell-telemetry".into())
            .spawn(move || {
                if !matches!(dir.join("telemetry.disabled").try_exists(), Ok(false)) {
                    return;
                }
                let Ok(token) = installation_token(&dir) else {
                    return;
                };
                let _ = rustls::crypto::ring::default_provider().install_default();
                let Ok(client) = Client::builder()
                    .timeout(Duration::from_secs(4))
                    .redirect(Policy::none())
                    .build()
                else {
                    return;
                };
                report_loop(client, endpoint, &token, rx);
            });
        Self(started.ok().map(|_| tx))
    }
}

pub fn record(app: &AppHandle, kind: EventKind, connection_id: Option<&str>, succeeded: bool) {
    if let Some(state) = app.try_state::<Telemetry>() {
        if let Some(sender) = &state.0 {
            let _ = sender.try_send(PendingEvent {
                kind,
                succeeded,
                connection_id: connection_id.and_then(|id| Uuid::parse_str(id).ok()),
                occurred_at: crate::time::now(),
            });
        }
    }
}

pub fn observe<T, E>(
    app: &AppHandle,
    kind: EventKind,
    connection_id: Option<&str>,
    result: Result<T, E>,
) -> Result<T, E> {
    record(app, kind, connection_id, result.is_ok());
    result
}

fn telemetry_endpoint(raw: &str) -> Option<Url> {
    let mut url = Url::parse(raw.trim()).ok()?;
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return None;
    }
    url.set_path("/api/v1/telemetry");
    Some(url)
}

fn installation_token(dir: &Path) -> io::Result<String> {
    fs::create_dir_all(dir)?;
    let path = dir.join("telemetry.token");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut raw = [0_u8; 32];
    getrandom::getrandom(&mut raw).map_err(|_| io::Error::other("installation RNG failed"))?;
    let token = hex(&raw);
    match options.open(&path) {
        Ok(mut file) => {
            if let Err(err) = file.write_all(token.as_bytes()) {
                let _ = fs::remove_file(&path);
                return Err(err);
            }
            Ok(token)
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            let token = fs::read_to_string(path)?;
            if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
                Ok(token)
            } else {
                Err(io::Error::other("invalid installation token"))
            }
        }
        Err(err) => Err(err),
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

impl PendingEvent {
    fn prepare(self, token: &str) -> Event {
        let connection_ref = self.connection_id.map(|id| {
            let mut hash = Sha256::new();
            hash.update(b"ishell-connection-v1");
            hash.update(token.as_bytes());
            hash.update(id.as_bytes());
            hex(&hash.finalize()[..16])
        });
        Event {
            id: Uuid::new_v4().to_string(),
            kind: self.kind,
            outcome: if self.succeeded { "success" } else { "failure" },
            connection_ref,
            occurred_at: self.occurred_at,
        }
    }
}

fn report_loop(client: Client, endpoint: Url, token: &str, rx: Receiver<PendingEvent>) {
    let platform = match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        "linux" => "linux",
        _ => "other",
    };
    let mut pending = VecDeque::new();
    let mut tick = Instant::now();
    let mut heartbeat = tick;
    let mut retry_after = tick;
    let mut backoff = TICK;
    loop {
        match rx.recv_timeout(tick.saturating_duration_since(Instant::now())) {
            Ok(event) => {
                if pending.len() < QUEUE_LIMIT {
                    pending.push_back(event.prepare(token));
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        if now < tick {
            continue;
        }
        tick = now + TICK;
        if now < retry_after || (pending.is_empty() && now < heartbeat) {
            continue;
        }
        let events: Vec<_> = pending.iter().take(BATCH_LIMIT).collect();
        let count = events.len();
        let response = client
            .post(endpoint.clone())
            .bearer_auth(token)
            .json(&Report {
                platform,
                app_version: env!("CARGO_PKG_VERSION"),
                events,
            })
            .send();
        match response {
            Ok(response) if response.status().is_success() => {
                pending.drain(..count);
                heartbeat = Instant::now() + HEARTBEAT;
                backoff = TICK;
            }
            Ok(response)
                if response.status().is_client_error()
                    && !matches!(response.status().as_u16(), 408 | 429) =>
            {
                pending.drain(..count);
                heartbeat = Instant::now() + HEARTBEAT;
                backoff = TICK;
            }
            _ => {
                retry_after = Instant::now() + backoff;
                backoff = (backoff * 2).min(HEARTBEAT);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_requires_https_except_for_local_development() {
        assert!(telemetry_endpoint("http://127.0.0.1:8080").is_some());
        assert!(telemetry_endpoint("http://[::1]:8080").is_some());
        assert!(telemetry_endpoint("https://example.com").is_some());
        assert!(telemetry_endpoint("http://example.com").is_none());
        assert!(telemetry_endpoint("https://secret@example.com").is_none());
        assert!(telemetry_endpoint("https://example.com/?secret=abc").is_none());
    }

    #[test]
    fn connection_refs_correlate_only_within_one_installation() {
        let id = Uuid::new_v4();
        let prepare = |token| {
            PendingEvent {
                kind: EventKind::SshLogin,
                succeeded: true,
                connection_id: Some(id),
                occurred_at: 1,
            }
            .prepare(token)
        };
        let first = prepare("device-one");
        let json = serde_json::to_string(&first).unwrap();
        assert!(!json.contains(&id.to_string()));
        assert!(!json.contains("device-one"));
        assert_eq!(first.connection_ref, prepare("device-one").connection_ref);
        assert_ne!(first.connection_ref, prepare("device-two").connection_ref);
    }
}
