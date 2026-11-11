//! Interactive terminal sessions.
//!
//! A session's output is kept as raw bytes: the remote side decides the
//! encoding and the frontend decodes it with the session's encoding (the
//! server's setting, UTF-8 by default, switchable while it runs), so nothing
//! is lost or replaced on the way. Typed text is encoded the same way. Each pane showing
//! a session subscribes with its own IPC channel and receives binary frames;
//! attaching replays the buffered output and subscribes in one step, so no
//! output can fall between the two.
//!
//! A frame is one byte of kind, eight bytes of little-endian offset, then the
//! payload: output bytes for `FRAME_DATA` (the offset is where they start in
//! the session's output), nothing for `FRAME_READY`, and the UTF-8 reason for
//! `FRAME_CLOSED`.

use std::{
    collections::HashMap,
    sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError},
    thread,
};

#[cfg(not(russh_backend))]
use std::{
    io::{Read, Write},
    time::Duration,
};

use encoding_rs::Encoding;
#[cfg(not(russh_backend))]
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tauri::{
    ipc::{Channel, InvokeResponseBody},
    AppHandle,
};
use uuid::Uuid;

use crate::encoding;

#[cfg(not(russh_backend))]
use crate::models::ServerRecord;
#[cfg(not(russh_backend))]
use crate::openssh;
#[cfg(not(russh_backend))]
use crate::store::jump_hops;
use crate::store::{get_server, mark_connected, read_secret};

pub const FRAME_DATA: u8 = 0;
pub const FRAME_READY: u8 = 1;
pub const FRAME_CLOSED: u8 = 2;

/// Output kept per session for panes that attach later (a tab moved to
/// another window, say). Trimmed in batches once it passes the maximum.
const MAX_BUFFER_BYTES: usize = 512 * 1024;
const TARGET_BUFFER_BYTES: usize = 384 * 1024;
/// More panes than this on one session means some never detached.
const MAX_SUBSCRIBERS: usize = 8;

pub(crate) enum TerminalControl {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Close,
}

fn frame(kind: u8, offset: u64, payload: &[u8]) -> InvokeResponseBody {
    let mut bytes = Vec::with_capacity(9 + payload.len());
    bytes.push(kind);
    bytes.extend_from_slice(&offset.to_le_bytes());
    bytes.extend_from_slice(payload);
    InvokeResponseBody::Raw(bytes)
}

struct Session {
    server_id: String,
    control: mpsc::Sender<TerminalControl>,
    encoding: &'static Encoding,
    /// The most recent output; `start` is its offset in the whole output.
    buffer: Vec<u8>,
    start: u64,
    ready: bool,
    closed: Option<String>,
    subscribers: Vec<Channel>,
}

impl Session {
    fn end(&self) -> u64 {
        self.start + self.buffer.len() as u64
    }

    /// Send to every subscriber, dropping those whose window has gone.
    fn broadcast(&mut self, kind: u8, offset: u64, payload: &[u8]) {
        self.subscribers
            .retain(|channel| channel.send(frame(kind, offset, payload)).is_ok());
    }

    fn push_output(&mut self, data: &[u8]) {
        let offset = self.end();
        self.buffer.extend_from_slice(data);
        if self.buffer.len() > MAX_BUFFER_BYTES {
            let drop = self.buffer.len() - TARGET_BUFFER_BYTES;
            self.buffer.drain(..drop);
            self.start += drop as u64;
        }
        self.broadcast(FRAME_DATA, offset, data);
    }

    /// Replay what this session has to a new subscriber.
    fn replay(&self, channel: &Channel) -> Result<(), String> {
        let send = |body| channel.send(body).map_err(|err| err.to_string());
        if !self.buffer.is_empty() {
            send(frame(FRAME_DATA, self.start, &self.buffer))?;
        }
        if self.ready {
            send(frame(FRAME_READY, self.end(), &[]))?;
        }
        if let Some(reason) = &self.closed {
            send(frame(FRAME_CLOSED, self.end(), reason.as_bytes()))?;
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct TerminalRegistry {
    sessions: Mutex<HashMap<String, Arc<Mutex<Session>>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl TerminalRegistry {
    fn session(&self, session_id: &str) -> Option<Arc<Mutex<Session>>> {
        lock(&self.sessions).get(session_id).cloned()
    }

    fn with_session<T>(
        &self,
        session_id: &str,
        action: impl FnOnce(&mut Session) -> T,
    ) -> Option<T> {
        self.session(session_id)
            .map(|session| action(&mut lock(&session)))
    }

    /// Whether `session_id` is a live, logged-in terminal to `server_id`.
    pub(crate) fn connected_to(&self, session_id: &str, server_id: &str) -> bool {
        self.with_session(session_id, |session| {
            session.server_id == server_id && session.ready && session.closed.is_none()
        })
        .unwrap_or(false)
    }

    /// The last `lines` lines of a session's output as plain text, for the
    /// AI: escape sequences removed, overwritten line content resolved.
    pub(crate) fn recent_text(&self, session_id: &str, lines: usize) -> Option<String> {
        let (bytes, encoding) = self.with_session(session_id, |session| {
            let skip = session.buffer.len().saturating_sub(256 * 1024);
            (session.buffer[skip..].to_vec(), session.encoding)
        })?;
        Some(plain_lines(&encoding::decode(encoding, &bytes), lines))
    }

    fn output(&self, session_id: &str, data: &[u8]) {
        if !data.is_empty() {
            self.with_session(session_id, |session| session.push_output(data));
        }
    }

    fn mark_ready(&self, session_id: &str) {
        self.with_session(session_id, |session| {
            if !session.ready {
                session.ready = true;
                let end = session.end();
                session.broadcast(FRAME_READY, end, &[]);
            }
        });
    }

    /// The session ended on its own. It stays registered, so a pane that
    /// attaches later still shows its output and why it ended, until closed.
    fn mark_closed(&self, session_id: &str, reason: String) {
        self.with_session(session_id, |session| {
            let end = session.end();
            session.broadcast(FRAME_CLOSED, end, reason.as_bytes());
            session.closed = Some(reason);
        });
    }

    fn send(&self, session_id: &str, control: TerminalControl) -> Result<(), String> {
        let sender = self
            .with_session(session_id, |session| {
                session.closed.is_none().then(|| session.control.clone())
            })
            .ok_or_else(|| "找不到终端会话".to_string())?
            .ok_or_else(|| "终端会话已关闭，请重新连接".to_string())?;
        sender
            .send(control)
            .map_err(|_| "终端会话已关闭，请重新连接".to_string())
    }
}

pub fn open_terminal(
    app: AppHandle,
    registry: &Arc<TerminalRegistry>,
    server_id: String,
    channel: Option<Channel>,
) -> Result<String, String> {
    let session_id = Uuid::new_v4().to_string();
    let (sender, receiver) = mpsc::channel::<TerminalControl>();
    let session_encoding = get_server(&app, &server_id)
        .map(|server| encoding::lookup(&server.encoding))
        .unwrap_or(encoding_rs::UTF_8);
    lock(&registry.sessions).insert(
        session_id.clone(),
        Arc::new(Mutex::new(Session {
            server_id: server_id.clone(),
            control: sender,
            encoding: session_encoding,
            buffer: Vec::new(),
            start: 0,
            ready: false,
            closed: None,
            subscribers: channel.into_iter().collect(),
        })),
    );

    let thread_registry = registry.clone();
    let thread_session_id = session_id.clone();
    thread::spawn(move || {
        let result = run_terminal_thread(
            app,
            thread_registry.clone(),
            thread_session_id.clone(),
            server_id,
            receiver,
        );
        let reason = result.err().unwrap_or_else(|| "终端已关闭".to_string());
        thread_registry.mark_closed(&thread_session_id, reason);
    });

    Ok(session_id)
}

/// Subscribe `channel` to a session, first replaying its buffered output.
/// Returns the encoding to decode the output with.
pub fn attach_terminal(
    registry: &TerminalRegistry,
    session_id: &str,
    channel: Channel,
) -> Result<String, String> {
    registry
        .with_session(session_id, |session| {
            session.replay(&channel)?;
            if session.subscribers.len() >= MAX_SUBSCRIBERS {
                session.subscribers.remove(0);
            }
            session.subscribers.push(channel);
            Ok(session.encoding.name().to_string())
        })
        .unwrap_or_else(|| Err("找不到终端会话".to_string()))
}

/// Stop sending to the pane whose channel has this id.
pub fn detach_terminal(registry: &TerminalRegistry, session_id: &str, channel_id: u32) {
    registry.with_session(session_id, |session| {
        session
            .subscribers
            .retain(|channel| channel.id() != channel_id)
    });
}

/// Raw bytes for the remote side, sent as they are (file transfers, tests).
pub fn terminal_input(
    registry: &TerminalRegistry,
    session_id: &str,
    data: Vec<u8>,
) -> Result<(), String> {
    registry.send(session_id, TerminalControl::Input(data))
}

/// Typed text, encoded with the session's encoding.
pub fn terminal_type(
    registry: &TerminalRegistry,
    session_id: &str,
    text: &str,
) -> Result<(), String> {
    let encoding = registry
        .with_session(session_id, |session| session.encoding)
        .ok_or_else(|| "找不到终端会话".to_string())?;
    terminal_input(registry, session_id, encoding::encode(encoding, text))
}

/// Decode and encode a running session with another encoding. Returns the
/// encoding's canonical name.
pub fn set_encoding(
    registry: &TerminalRegistry,
    session_id: &str,
    label: &str,
) -> Result<String, String> {
    let name = encoding::normalize(label)?;
    registry
        .with_session(session_id, |session| {
            session.encoding = encoding::lookup(&name)
        })
        .ok_or_else(|| "找不到终端会话".to_string())?;
    Ok(name)
}

pub fn terminal_resize(
    registry: &TerminalRegistry,
    session_id: &str,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    registry.send(
        session_id,
        TerminalControl::Resize {
            cols: cols.max(1),
            rows: rows.max(1),
        },
    )
}

pub fn close_terminal(registry: &TerminalRegistry, session_id: &str) {
    if let Some(session) = lock(&registry.sessions).remove(session_id) {
        let _ = lock(&session).control.send(TerminalControl::Close);
    }
}

#[cfg(not(russh_backend))]
const CONNECTED_MARKER: &str = "[iShell] connected";

/// Whether a character takes two terminal cells (CJK, Hangul, fullwidth
/// forms, most emoji). Readline moves the cursor by cells, not characters.
fn is_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF | 0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F | 0x1F900..=0x1FAFF | 0x20000..=0x3FFFD)
}

/// One line being drawn: cells hold a character, or `None` for the second
/// half of a wide one.
#[derive(Default)]
struct LineDraw {
    cells: Vec<Option<char>>,
    cursor: usize,
}

impl LineDraw {
    fn put(&mut self, ch: char) {
        let width = if is_wide(ch) { 2 } else { 1 };
        if self.cells.len() < self.cursor + width {
            self.cells.resize(self.cursor + width, Some(' '));
        }
        self.cells[self.cursor] = Some(ch);
        if width == 2 {
            self.cells[self.cursor + 1] = None;
        }
        self.cursor += width;
    }

    fn text(&self) -> String {
        self.cells
            .iter()
            .flatten()
            .collect::<String>()
            .trim_end()
            .to_string()
    }
}

/// Terminal output as the lines a reader would see. Carriage returns,
/// backspaces and erase-in-line (`ESC [ K`) are applied, as a shell's line
/// editor and progress bars use them; other escape sequences are dropped.
fn plain_lines(output: &str, lines: usize) -> String {
    let mut done: Vec<String> = Vec::new();
    let mut line = LineDraw::default();
    let mut chars = output.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\n' => done.push(std::mem::take(&mut line).text()),
            '\r' => line.cursor = 0,
            '\x08' => line.cursor = line.cursor.saturating_sub(1),
            '\t' => {
                let next = (line.cursor / 8 + 1) * 8;
                while line.cursor < next {
                    line.put(' ');
                }
            }
            '\x1b' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    let mut params = String::new();
                    let mut last = '\0';
                    for seq in chars.by_ref() {
                        if ('@'..='~').contains(&seq) {
                            last = seq;
                            break;
                        }
                        params.push(seq);
                    }
                    if last == 'K' {
                        match params.as_str() {
                            "" | "0" => line.cells.truncate(line.cursor),
                            "2" => line.cells.clear(),
                            _ => {}
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(seq) = chars.next() {
                        if seq == '\x07' {
                            break;
                        }
                        if seq == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            ch if ch.is_control() => {}
            ch => line.put(ch),
        }
    }
    done.push(line.text());
    while done.last().is_some_and(|line| line.trim().is_empty()) {
        done.pop();
    }
    let skip = done.len().saturating_sub(lines);
    done[skip..].join("\n")
}

#[cfg(not(russh_backend))]
fn strip_ansi_sequences(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for seq in chars.by_ref() {
                        if ('@'..='~').contains(&seq) {
                            break;
                        }
                    }
                    continue;
                }
                Some(']') => {
                    chars.next();
                    while let Some(seq) = chars.next() {
                        if seq == '\x07' {
                            break;
                        }
                        if seq == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                    continue;
                }
                _ => {}
            }
        }
        output.push(ch);
    }

    output
}

/// Keep the last `max_bytes` of `value`.
#[cfg(not(russh_backend))]
fn keep_recent_tail(value: &mut Vec<u8>, max_bytes: usize) {
    if value.len() > max_bytes {
        let drop = value.len() - max_bytes;
        value.drain(..drop);
    }
}

#[cfg(not(russh_backend))]
fn looks_like_password_prompt(output_tail: &str) -> bool {
    let plain = strip_ansi_sequences(output_tail).replace('\r', "");
    let line = plain
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_lowercase();

    (line.ends_with(':') || line.ends_with('：'))
        && (line.contains("password")
            || line.contains("passphrase")
            || line.contains("密码")
            || line.contains("口令"))
}

#[cfg(not(russh_backend))]
fn looks_like_shell_ready(output_tail: &str) -> bool {
    let plain = strip_ansi_sequences(output_tail).replace('\r', "");
    let lower = plain.to_lowercase();
    if lower.contains("last login:") || lower.contains("last failed login:") {
        return true;
    }

    let line = plain.rsplit('\n').next().unwrap_or_default().trim_end();
    if line.is_empty() || looks_like_password_prompt(output_tail) {
        return false;
    }

    line.ends_with('$') || line.ends_with('#') || line.ends_with('%') || line.ends_with('>')
}

#[cfg(not(russh_backend))]
fn build_ssh_command(server: &ServerRecord, invocation: &openssh::SshInvocation) -> CommandBuilder {
    let mut command = CommandBuilder::new(openssh::ssh_binary());
    command.env("TERM", "xterm-256color");
    for (key, value) in &invocation.env {
        // Keep a real DISPLAY: askpass only needs one to be set.
        if key == "DISPLAY" && command.get_env("DISPLAY").is_some() {
            continue;
        }
        command.env(key, value);
    }
    command.arg("-tt");
    for arg in &invocation.args {
        command.arg(arg);
    }
    command.arg("-o");
    command.arg("PermitLocalCommand=yes");
    command.arg("-o");
    command.arg("LocalCommand=printf '\\r\\n[iShell] connected via OpenSSH\\r\\n'");
    command.arg(&server.host);
    command
}

#[cfg(not(russh_backend))]
fn run_terminal_thread(
    app: AppHandle,
    registry: Arc<TerminalRegistry>,
    session_id: String,
    server_id: String,
    receiver: mpsc::Receiver<TerminalControl>,
) -> Result<(), String> {
    let server = get_server(&app, &server_id)?;
    let saved_secret = read_secret(&app, &server_id)
        .ok()
        .filter(|secret| !secret.is_empty());
    let hops = jump_hops(&app, &server)?;
    // Not batch: a password that is not saved can be typed into the terminal.
    let invocation = openssh::SshInvocation::prepare(
        &server,
        saved_secret.as_deref(),
        &hops,
        true,
        false,
        &session_id,
    )?;
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 32,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| {
            invocation.cleanup();
            format!("无法创建本地 PTY：{err}")
        })?;

    let command = build_ssh_command(&server, &invocation);
    let mut child = match pair.slave.spawn_command(command) {
        Ok(child) => child,
        Err(err) => {
            invocation.cleanup();
            return Err(format!("无法启动本机 ssh：{err}"));
        }
    };
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|err| format!("无法读取本地 PTY：{err}"))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|err| format!("无法写入本地 PTY：{err}"))?;

    let (read_done_sender, read_done_receiver) = mpsc::channel::<String>();
    let (password_prompt_sender, password_prompt_receiver) = mpsc::channel::<()>();
    let read_app = app.clone();
    let read_registry = registry.clone();
    let read_session_id = session_id.clone();
    let read_server_id = server_id.clone();
    // Typing the saved password at the first prompt is a fallback for an ssh
    // that ignores SSH_ASKPASS_REQUIRE. With jump hosts the first prompt may be
    // a jump host's, so leave those to the askpass helper.
    let should_autofill_password = saved_secret.is_some() && hops.is_empty();
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        // Recent bytes while connecting, read only to spot prompts and the
        // login; what reaches the terminal is the raw output.
        let mut connection_tail = Vec::new();
        let mut password_prompt_seen = false;
        let mut connected_seen = false;
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = read_done_sender.send("远程终端已关闭".to_string());
                    break;
                }
                Ok(size) => {
                    let data = &buffer[..size];
                    if !connected_seen {
                        connection_tail.extend_from_slice(data);
                        keep_recent_tail(&mut connection_tail, 1024);
                        let tail = String::from_utf8_lossy(&connection_tail);
                        // Only answer authentication prompts while the SSH
                        // session is still connecting. Never inject a saved
                        // password into later remote prompts such as sudo.
                        if should_autofill_password
                            && !password_prompt_seen
                            && looks_like_password_prompt(&tail)
                        {
                            password_prompt_seen = true;
                            let _ = password_prompt_sender.send(());
                        }
                        if tail.contains(CONNECTED_MARKER) {
                            connected_seen = true;
                            mark_connected(&read_app, &read_server_id).ok();
                            read_registry.mark_ready(&read_session_id);
                        } else if looks_like_shell_ready(&tail) {
                            connected_seen = true;
                            mark_connected(&read_app, &read_server_id).ok();
                            read_registry.mark_ready(&read_session_id);
                            read_registry.output(&read_session_id, data);
                            read_registry.output(
                                &read_session_id,
                                b"\r\n[iShell] connected via OpenSSH\r\n",
                            );
                            continue;
                        }
                    }
                    read_registry.output(&read_session_id, data);
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => {
                    let _ = read_done_sender.send(format!("终端读取失败：{err}"));
                    break;
                }
            }
        }
    });

    registry.output(&session_id, b"\r\n[iShell] OpenSSH started\r\n");

    loop {
        if password_prompt_receiver.try_recv().is_ok() {
            if let Some(secret) = saved_secret.as_deref() {
                if let Err(err) = writer.write_all(secret.as_bytes()) {
                    invocation.cleanup();
                    return Err(format!("终端写入失败：{err}"));
                }
                if let Err(err) = writer.write_all(b"\n") {
                    invocation.cleanup();
                    return Err(format!("终端写入失败：{err}"));
                }
                writer.flush().ok();
            }
        }

        if let Ok(reason) = read_done_receiver.try_recv() {
            let _ = child.wait();
            invocation.cleanup();
            return Err(reason);
        }

        match receiver.recv_timeout(Duration::from_millis(80)) {
            Ok(TerminalControl::Input(data)) => {
                if let Err(err) = writer.write_all(&data) {
                    invocation.cleanup();
                    return Err(format!("终端写入失败：{err}"));
                }
                writer.flush().ok();
            }
            Ok(TerminalControl::Resize { cols, rows }) => {
                if let Err(err) = pair.master.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                }) {
                    invocation.cleanup();
                    return Err(format!("无法调整终端尺寸：{err}"));
                }
            }
            Ok(TerminalControl::Close) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                let _ = child.wait();
                invocation.cleanup();
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

/// Windows / russh terminal: drive an interactive shell over a pure-Rust SSH
/// PTY channel. Authentication (password or key) happens inside the transport,
/// so there is no local PTY, askpass helper, or password-prompt autofill.
#[cfg(russh_backend)]
fn run_terminal_thread(
    app: AppHandle,
    registry: Arc<TerminalRegistry>,
    session_id: String,
    server_id: String,
    receiver: mpsc::Receiver<TerminalControl>,
) -> Result<(), String> {
    let server = get_server(&app, &server_id)?;
    // For password authentication this is the account password; for key
    // authentication it is the optional private-key passphrase.
    let secret = read_secret(&app, &server_id)
        .ok()
        .filter(|password| !password.is_empty());

    registry.output(
        &session_id,
        "\r\n[iShell] 正在通过 russh 连接...\r\n".as_bytes(),
    );

    let ready_registry = registry.clone();
    let ready_session_id = session_id.clone();
    let on_ready = move || {
        mark_connected(&app, &server_id).ok();
        ready_registry.mark_ready(&ready_session_id);
        ready_registry.output(&ready_session_id, b"\r\n[iShell] connected via russh\r\n");
    };
    let on_data = move |data: &[u8]| registry.output(&session_id, data);

    crate::russh_transport::run_terminal(
        &server,
        secret.as_deref(),
        120,
        32,
        receiver,
        on_ready,
        on_data,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A channel that records the frames it is sent.
    fn recording_channel() -> (Channel, Arc<Mutex<Vec<Vec<u8>>>>) {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let sink = frames.clone();
        let channel = Channel::new(move |body| {
            if let InvokeResponseBody::Raw(bytes) = body {
                sink.lock().unwrap().push(bytes);
            }
            Ok(())
        });
        (channel, frames)
    }

    fn decode(frame: &[u8]) -> (u8, u64, &[u8]) {
        (
            frame[0],
            u64::from_le_bytes(frame[1..9].try_into().unwrap()),
            &frame[9..],
        )
    }

    fn registry_with_session(subscriber: Channel) -> (Arc<TerminalRegistry>, String) {
        let registry = Arc::new(TerminalRegistry::default());
        let (control, _receiver) = mpsc::channel();
        std::mem::forget(_receiver);
        lock(&registry.sessions).insert(
            "s".into(),
            Arc::new(Mutex::new(Session {
                server_id: "server".into(),
                control,
                encoding: encoding_rs::UTF_8,
                buffer: Vec::new(),
                start: 0,
                ready: false,
                closed: None,
                subscribers: vec![subscriber],
            })),
        );
        (registry, "s".into())
    }

    #[test]
    fn output_reaches_subscribers_as_raw_bytes_with_offsets() {
        let (channel, frames) = recording_channel();
        let (registry, id) = registry_with_session(channel);
        // Split inside a multi-byte character, and not UTF-8 at all (GBK).
        let text = "中文".as_bytes();
        registry.output(&id, &text[..2]);
        registry.output(&id, &text[2..]);
        registry.output(&id, &[0xd6, 0xd0]);
        registry.mark_ready(&id);
        registry.mark_ready(&id);

        let frames = frames.lock().unwrap();
        let decoded: Vec<_> = frames.iter().map(|frame| decode(frame)).collect();
        assert_eq!(decoded[0], (FRAME_DATA, 0, &text[..2]));
        assert_eq!(decoded[1], (FRAME_DATA, 2, &text[2..]));
        assert_eq!(decoded[2], (FRAME_DATA, 6, &[0xd6, 0xd0][..]));
        assert_eq!(decoded[3], (FRAME_READY, 8, &[][..]));
        assert_eq!(decoded.len(), 4, "ready is announced once");
        assert!(registry.connected_to(&id, "server"));
        assert!(!registry.connected_to(&id, "other"));
    }

    #[test]
    fn attaching_replays_the_buffer_and_the_session_state() {
        let (first, _) = recording_channel();
        let (registry, id) = registry_with_session(first);
        registry.output(&id, b"hello ");
        registry.mark_ready(&id);
        registry.output(&id, b"world");
        registry.mark_closed(&id, "远程终端已关闭".into());
        assert!(!registry.connected_to(&id, "server"));
        assert!(terminal_input(&registry, &id, b"x".to_vec()).is_err());

        let (late, frames) = recording_channel();
        attach_terminal(&registry, &id, late).unwrap();
        let frames = frames.lock().unwrap();
        let decoded: Vec<_> = frames.iter().map(|frame| decode(frame)).collect();
        assert_eq!(decoded[0], (FRAME_DATA, 0, &b"hello world"[..]));
        assert_eq!(decoded[1], (FRAME_READY, 11, &[][..]));
        assert_eq!(decoded[2], (FRAME_CLOSED, 11, "远程终端已关闭".as_bytes()));
        assert!(attach_terminal(&registry, "missing", recording_channel().0).is_err());
    }

    #[test]
    fn the_buffer_is_trimmed_and_offsets_keep_counting() {
        let (channel, _) = recording_channel();
        let (registry, id) = registry_with_session(channel);
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..10 {
            registry.output(&id, &chunk);
        }
        let (start, len) = registry
            .with_session(&id, |session| (session.start, session.buffer.len()))
            .unwrap();
        assert!(len <= MAX_BUFFER_BYTES);
        assert_eq!(start + len as u64, 10 * 64 * 1024);
        assert!(registry
            .with_session(&id, |session| session.buffer.ends_with(b"xxx"))
            .unwrap());

        let (late, frames) = recording_channel();
        attach_terminal(&registry, &id, late).unwrap();
        assert_eq!(decode(&frames.lock().unwrap()[0]).1, start);
    }

    #[test]
    fn a_session_switches_encoding_for_input_and_output() {
        let registry = TerminalRegistry::default();
        let (control, receiver) = mpsc::channel();
        lock(&registry.sessions).insert(
            "s".into(),
            Arc::new(Mutex::new(Session {
                server_id: "server".into(),
                control,
                encoding: encoding_rs::UTF_8,
                buffer: Vec::new(),
                start: 0,
                ready: true,
                closed: None,
                subscribers: vec![],
            })),
        );
        assert_eq!(set_encoding(&registry, "s", "gb2312").unwrap(), "GBK");
        assert!(set_encoding(&registry, "s", "utf-16").is_err());
        assert!(set_encoding(&registry, "missing", "gbk").is_err());

        terminal_type(&registry, "s", "echo 中").unwrap();
        match receiver.try_recv() {
            Ok(TerminalControl::Input(bytes)) => assert_eq!(bytes, b"echo \xd6\xd0"),
            _ => panic!("no input"),
        }
        registry.output("s", b"\xc4\xe3\xba\xc3\r\n");
        assert_eq!(registry.recent_text("s", 5).unwrap(), "你好");
    }

    #[test]
    fn recent_text_is_what_a_reader_sees() {
        let (channel, _) = recording_channel();
        let (registry, id) = registry_with_session(channel);
        registry.output(&id, b"\x1b[1;32mok\x1b[0m line one\r\n");
        registry.output(&id, b"progress 10%\rprogress 100%\r\n");
        registry.output(
            &id,
            "\x1b]0;title\x07错误：端口被占用\r\n$ \r\n\r\n".as_bytes(),
        );
        assert_eq!(
            registry.recent_text(&id, 10).unwrap(),
            "ok line one\nprogress 100%\n错误：端口被占用\n$"
        );
        assert_eq!(registry.recent_text(&id, 2).unwrap(), "错误：端口被占用\n$");
        assert!(registry.recent_text("missing", 2).is_none());

        // What bash's line editor sends for Ctrl+U after typing, with a wide
        // character counted as two cells.
        assert_eq!(
            plain_lines(
                "$ # 看端口\x08\x08\x08\x08\x08\x08\x08\x08\x1b[Kss -lt\r\n",
                5
            ),
            "$ ss -lt"
        );
        assert_eq!(plain_lines("abc\x08X\n\tz", 5), "abX\n        z");
        assert_eq!(plain_lines("keep\x1b[2K\rnew", 5), "new");
    }

    #[test]
    fn detached_panes_stop_receiving() {
        let (first, first_frames) = recording_channel();
        let first_id = first.id();
        let (registry, id) = registry_with_session(first);
        let (second, second_frames) = recording_channel();
        attach_terminal(&registry, &id, second).unwrap();
        detach_terminal(&registry, &id, first_id);
        registry.output(&id, b"after");
        assert!(first_frames.lock().unwrap().is_empty());
        assert_eq!(second_frames.lock().unwrap().len(), 1);

        close_terminal(&registry, &id);
        assert!(registry.session(&id).is_none());
    }
}
