//! A shell on this computer, in a terminal tab like a server's. It runs in a
//! local PTY (ConPTY on Windows), so it works the same with either SSH
//! backend.

use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::{mpsc, Arc},
    thread,
    time::Duration,
};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::terminal::{TerminalControl, TerminalRegistry};

/// The server id of local terminal tabs; never a saved server's (those are UUIDs).
pub const LOCAL_SERVER_ID: &str = "local";

/// The user's shell: `ISHELL_LOCAL_SHELL` if set, else `$SHELL` (Unix) or
/// PowerShell (Windows).
fn shell_command() -> CommandBuilder {
    if let Some(shell) = std::env::var_os("ISHELL_LOCAL_SHELL").filter(|s| !s.is_empty()) {
        return CommandBuilder::new(shell);
    }
    #[cfg(windows)]
    {
        CommandBuilder::new("powershell.exe")
    }
    #[cfg(not(windows))]
    {
        let shell = std::env::var_os("SHELL")
            .filter(|shell| !shell.is_empty())
            .unwrap_or_else(|| {
                if cfg!(target_os = "macos") {
                    "/bin/zsh".into()
                } else {
                    "/bin/sh".into()
                }
            });
        let mut command = CommandBuilder::new(shell);
        // A login shell, as a new terminal window would start, so the
        // user's profile (PATH and the like) applies; an app started from
        // the Dock does not inherit it.
        command.arg("-l");
        command
    }
}

fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
}

/// Run a local shell for `session_id` until it exits or the tab closes.
pub(crate) fn run(
    registry: Arc<TerminalRegistry>,
    session_id: String,
    receiver: mpsc::Receiver<TerminalControl>,
) -> Result<(), String> {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 32,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| format!("无法创建本地 PTY：{err}"))?;
    let mut command = shell_command();
    if let Some(home) = home_dir() {
        command.cwd(home);
    }
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("TERM_PROGRAM", "iShell");
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|err| format!("无法启动本地 Shell：{err}"))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|err| format!("无法读取本地 PTY：{err}"))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|err| format!("无法写入本地 PTY：{err}"))?;

    let (done_sender, done_receiver) = mpsc::channel::<String>();
    let read_registry = registry.clone();
    let read_session_id = session_id.clone();
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(size) => read_registry.output(&read_session_id, &buffer[..size]),
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                // Linux reports EIO once the shell has exited.
                Err(_) => break,
            }
        }
        let _ = done_sender.send("本地 Shell 已退出".to_string());
    });
    registry.mark_ready(&session_id);

    loop {
        if let Ok(reason) = done_receiver.try_recv() {
            let _ = child.wait();
            return Err(reason);
        }
        match receiver.recv_timeout(Duration::from_millis(80)) {
            Ok(TerminalControl::Input(data)) => {
                writer
                    .write_all(&data)
                    .and_then(|_| writer.flush())
                    .map_err(|err| format!("终端写入失败：{err}"))?;
            }
            Ok(TerminalControl::Resize { cols, rows }) => {
                pair.master
                    .resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .map_err(|err| format!("无法调整终端尺寸：{err}"))?;
            }
            Ok(TerminalControl::Close) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::terminal;

    #[test]
    fn a_local_shell_runs_commands_and_reports_its_exit() {
        std::env::set_var("ISHELL_LOCAL_SHELL", "/bin/sh");
        let registry = Arc::new(TerminalRegistry::default());
        let session = terminal::open_local(&registry).unwrap();
        terminal::terminal_input(
            &registry,
            &session,
            b"echo local-$((20+1)); stty size\n".to_vec(),
        )
        .unwrap();
        terminal::terminal_resize(&registry, &session, 90, 20).unwrap();
        terminal::terminal_input(&registry, &session, b"stty size; exit\n".to_vec()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let text = registry.recent_text(&session, 20).unwrap_or_default();
            if text.contains("local-21") && text.contains("20 90") && !registry.is_open(&session) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "local shell output: {text}"
            );
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !registry.connected_to(&session, LOCAL_SERVER_ID),
            "an exited shell is not connected"
        );
        terminal::close_terminal(&registry, &session);
    }
}
