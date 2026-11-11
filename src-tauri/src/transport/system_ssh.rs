//! The backend that drives the system `ssh` binary. Commands sharing a host
//! are multiplexed over one ControlMaster connection (see `crate::openssh`);
//! bulk transfers and port forwards get a process and connection of their own.

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    pin::Pin,
    process::{Command, Stdio},
    sync::{mpsc, Arc, Mutex},
    task::{Context, Poll},
    thread,
    time::Duration,
};

use tauri::AppHandle;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::{
    Backend, ForwardEnded, ForwardRunning, StopForward, Subsystem, TransferError,
    UploadAttemptError,
};
use crate::{
    models::{PortForward, ServerRecord},
    openssh,
    store::{get_server, jump_hops, read_secret},
};

pub(crate) struct SystemSsh;

/// Spawn `ssh <server> <remote_command>`. `multiplex = false` gives the
/// command its own connection instead of the shared ControlMaster one. The
/// returned path is the askpass helper to remove once ssh has exited.
pub(crate) fn spawn_remote_ex(
    app: &AppHandle,
    id: &str,
    remote_command: &str,
    stdin_piped: bool,
    stdout_piped: bool,
    stderr_piped: bool,
    multiplex: bool,
) -> Result<(std::process::Child, Option<PathBuf>), String> {
    let server = get_server(app, id)?;
    let saved_secret = read_secret(app, id).ok().filter(|value| !value.is_empty());
    let hops = jump_hops(app, &server)?;
    let invocation = openssh::SshInvocation::prepare(
        &server,
        saved_secret.as_deref(),
        &hops,
        multiplex,
        true,
        id,
    )?;
    let helper_path = invocation.askpass.clone();
    let mut command = Command::new(openssh::ssh_binary());
    command.envs(invocation.env.iter().cloned());
    command.args(&invocation.args);
    command.arg(&server.host);
    command.arg(remote_command);
    let pipe = |piped: bool| if piped { Stdio::piped() } else { Stdio::null() };
    command.stdin(pipe(stdin_piped));
    command.stdout(pipe(stdout_piped));
    command.stderr(pipe(stderr_piped));

    match command.spawn() {
        Ok(child) => Ok((child, helper_path)),
        Err(err) => {
            cleanup_askpass_helper(helper_path.as_ref());
            Err(format!("无法启动 OpenSSH：{err}"))
        }
    }
}

pub(crate) fn cleanup_askpass_helper(path: Option<&PathBuf>) {
    if let Some(path) = path {
        let _ = fs::remove_file(path);
    }
}

/// What ssh or the remote command printed, or `fallback` when it said nothing.
fn stderr_message(stderr: &[u8], fallback: &str) -> String {
    let message = String::from_utf8_lossy(stderr).trim().to_string();
    if message.is_empty() {
        fallback.to_string()
    } else {
        message
    }
}

/// `host:port` for an ssh forward spec; IPv6 literals need brackets.
fn spec_address(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn forward_args(forward: &PortForward) -> Vec<String> {
    let bind = spec_address(&forward.bind_host, forward.bind_port);
    let dest = spec_address(&forward.dest_host, forward.dest_port);
    match forward.kind.as_str() {
        "remote" => vec!["-R".into(), format!("{bind}:{dest}")],
        "dynamic" => vec!["-D".into(), bind],
        _ => vec!["-L".into(), format!("{bind}:{dest}")],
    }
}

/// The most useful line of what ssh printed before it gave up.
fn forward_failure_reason(lines: &[String]) -> Option<String> {
    // Most specific first.
    const TELLING: [&str; 8] = [
        "Address already in use",
        "Permission denied",
        "Host key verification failed",
        "Could not resolve",
        "Connection refused",
        "timed out",
        "forwarding failed",
        "Could not request",
    ];
    TELLING
        .iter()
        .find_map(|needle| lines.iter().rev().find(|line| line.contains(needle)))
        .or_else(|| lines.last())
        .cloned()
}

/// An `ssh -s <subsystem>` child process as one duplex stream.
struct ChildIo {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    askpass: Option<PathBuf>,
}

impl AsyncRead for ChildIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for ChildIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stdin).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}

impl Drop for ChildIo {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        cleanup_askpass_helper(self.askpass.as_ref());
    }
}

impl Backend for SystemSsh {
    fn exec(
        app: &AppHandle,
        id: &str,
        command: &str,
        stdin: Option<&[u8]>,
    ) -> Result<Vec<u8>, String> {
        let (mut child, helper_path) =
            spawn_remote_ex(app, id, command, stdin.is_some(), true, true, true)?;
        let written = match (stdin, child.stdin.take()) {
            (Some(input), Some(mut child_stdin)) => child_stdin
                .write_all(input)
                .map_err(|err| format!("写入 OpenSSH 输入失败：{err}")),
            (Some(_), None) => Err("无法写入 OpenSSH 输入".to_string()),
            (None, _) => Ok(()),
        };
        if written.is_err() {
            let _ = child.kill();
        }
        let output = child
            .wait_with_output()
            .map_err(|err| format!("等待 OpenSSH 命令结束失败：{err}"));
        cleanup_askpass_helper(helper_path.as_ref());
        let output = output?;
        if !output.status.success() {
            // The remote command's own complaint beats "broken pipe" from the
            // write that it cut short.
            return Err(stderr_message(
                &output.stderr,
                &written
                    .err()
                    .unwrap_or_else(|| format!("OpenSSH 命令失败（{}）", output.status)),
            ));
        }
        written?;
        Ok(output.stdout)
    }

    fn stream(
        app: &AppHandle,
        id: &str,
        command: &str,
        is_canceled: &dyn Fn() -> bool,
        on_data: &mut dyn FnMut(&[u8]),
    ) -> Result<(), String> {
        let (mut child, helper_path) = spawn_remote_ex(app, id, command, false, true, true, true)?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let (chunk_sender, chunk_receiver) = mpsc::channel::<Vec<u8>>();
        let stdout_reader = thread::spawn(move || -> std::io::Result<()> {
            let Some(mut stdout) = stdout else {
                return Ok(());
            };
            loop {
                let mut chunk = vec![0_u8; 16 * 1024];
                let size = stdout.read(&mut chunk)?;
                if size == 0 {
                    return Ok(());
                }
                chunk.truncate(size);
                if chunk_sender.send(chunk).is_err() {
                    return Ok(());
                }
            }
        });
        let stderr_reader = thread::spawn(move || {
            let mut output = Vec::new();
            if let Some(mut stderr) = stderr {
                let _ = stderr.read_to_end(&mut output);
            }
            output
        });

        let mut canceled = false;
        loop {
            if is_canceled() {
                canceled = true;
                let _ = child.kill();
                break;
            }
            match chunk_receiver.recv_timeout(Duration::from_millis(250)) {
                Ok(chunk) => on_data(&chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                // stdout closed: the command has exited or is about to.
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        let status = child
            .wait()
            .map_err(|err| format!("等待 OpenSSH 命令结束失败：{err}"));
        cleanup_askpass_helper(helper_path.as_ref());
        let read = stdout_reader.join();
        let stderr = stderr_reader.join().unwrap_or_default();
        if canceled {
            return Ok(());
        }
        match read {
            Ok(Ok(())) => {}
            Ok(Err(err)) => return Err(format!("读取 OpenSSH 输出失败：{err}")),
            Err(_) => return Err("OpenSSH 输出线程异常退出".to_string()),
        }
        let status = status?;
        if !status.success() {
            return Err(stderr_message(
                &stderr,
                &format!("OpenSSH 命令失败（{status}）"),
            ));
        }
        Ok(())
    }

    fn download(
        app: &AppHandle,
        id: &str,
        command: &str,
        sink: &mut dyn Write,
        is_canceled: &dyn Fn() -> bool,
        on_progress: &mut dyn FnMut(u64),
    ) -> Result<(), TransferError> {
        let (mut child, helper_path) = spawn_remote_ex(app, id, command, false, true, true, false)
            .map_err(TransferError::Failed)?;
        let mut stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stderr_reader = thread::spawn(move || {
            let mut output = Vec::new();
            if let Some(mut stderr) = stderr {
                let _ = stderr.read_to_end(&mut output);
            }
            output
        });

        let mut copy = || -> Result<(), TransferError> {
            let stdout = stdout
                .as_mut()
                .ok_or_else(|| TransferError::Failed("无法读取 OpenSSH 下载输出".into()))?;
            let mut buffer = vec![0u8; 64 * 1024];
            let mut transferred = 0_u64;
            loop {
                if is_canceled() {
                    return Err(TransferError::Canceled);
                }
                let read = match stdout.read(&mut buffer) {
                    Ok(read) => read,
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(err) => return Err(TransferError::Failed(err.to_string())),
                };
                if read == 0 {
                    return Ok(());
                }
                if is_canceled() {
                    return Err(TransferError::Canceled);
                }
                sink.write_all(&buffer[..read])
                    .map_err(|err| TransferError::Failed(format!("写入本地文件失败：{err}")))?;
                transferred += read as u64;
                on_progress(transferred);
            }
        };
        let copied = copy();
        if copied.is_err() {
            let _ = child.kill();
        }
        let status = child.wait();
        cleanup_askpass_helper(helper_path.as_ref());
        let stderr = stderr_reader.join().unwrap_or_default();
        copied?;
        let status = status
            .map_err(|err| TransferError::Failed(format!("等待 OpenSSH 下载结束失败：{err}")))?;
        if !status.success() {
            return Err(TransferError::Failed(stderr_message(
                &stderr,
                &format!("OpenSSH 已退出（{status}）"),
            )));
        }
        Ok(())
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
        crate::ssh::openssh_upload_attempt(
            app,
            id,
            command,
            local,
            total,
            transfer_id,
            is_canceled,
            cancel_while_waiting,
        )
    }

    async fn open_subsystem(
        app: &AppHandle,
        id: &str,
        name: &str,
        dedicated: bool,
    ) -> Result<Subsystem, String> {
        let server = get_server(app, id)?;
        let saved_secret = read_secret(app, id).ok().filter(|value| !value.is_empty());
        let hops = jump_hops(app, &server)?;
        let invocation = openssh::SshInvocation::prepare(
            &server,
            saved_secret.as_deref(),
            &hops,
            !dedicated,
            true,
            id,
        )?;
        let mut command = tokio::process::Command::new(openssh::ssh_binary());
        command
            .envs(invocation.env.iter().cloned())
            .args(&invocation.args)
            .arg("-s")
            .arg(&server.host)
            .arg(name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // A failed login surfaces as the stream closing; the caller then
            // reports the subsystem as unavailable.
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|err| {
            invocation.cleanup();
            format!("无法启动 OpenSSH：{err}")
        })?;
        match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => Ok(Box::new(ChildIo {
                child,
                stdin,
                stdout,
                askpass: invocation.askpass.clone(),
            })),
            _ => {
                let _ = child.start_kill();
                invocation.cleanup();
                Err("无法连接 OpenSSH 子系统的输入输出".into())
            }
        }
    }

    fn test_connection(
        app: &AppHandle,
        server: &ServerRecord,
        secret: Option<&str>,
    ) -> Result<(), String> {
        let hops = jump_hops(app, server)?;
        // A connection of its own: reusing a multiplexed one would skip the
        // login being tested.
        let invocation =
            openssh::SshInvocation::prepare(server, secret, &hops, false, true, &server.id)?;
        let mut command = Command::new(openssh::ssh_binary());
        command.envs(invocation.env.iter().cloned());
        command.args(&invocation.args);
        command.arg("-o");
        command.arg("ConnectTimeout=10");
        command.arg(&server.host);
        command.arg("printf ishell-test");
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        let output = command
            .output()
            .map_err(|err| format!("无法启动 OpenSSH：{err}"));
        invocation.cleanup();
        let output = output?;
        if !output.status.success() {
            return Err(stderr_message(
                &output.stderr,
                &format!("OpenSSH 已退出（{}）", output.status),
            ));
        }
        Ok(())
    }

    fn start_forward(
        app: &AppHandle,
        forward: &PortForward,
        on_running: ForwardRunning,
        on_end: ForwardEnded,
    ) -> Result<StopForward, String> {
        let server = get_server(app, &forward.server_id)?;
        let secret = read_secret(app, &forward.server_id)
            .ok()
            .filter(|value| !value.is_empty());
        let hops = jump_hops(app, &server)?;
        let invocation = openssh::SshInvocation::prepare(
            &server,
            secret.as_deref(),
            &hops,
            false,
            true,
            &format!("forward-{}", forward.id),
        )?;

        let mut command = Command::new(openssh::ssh_binary());
        command.envs(invocation.env.iter().cloned());
        command.args(&invocation.args);
        // -v: the debug lines tell when the forward is actually listening.
        command.args(["-N", "-T", "-v", "-o", "ExitOnForwardFailure=yes"]);
        command.args(forward_args(forward));
        command.arg(&server.host);
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());
        command.stderr(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                invocation.cleanup();
                return Err(format!("无法启动 OpenSSH：{err}"));
            }
        };
        let stderr = child.stderr.take();
        let child = Arc::new(Mutex::new(child));

        let remote = forward.kind == "remote";
        let watched = child.clone();
        thread::spawn(move || {
            let mut errors: Vec<String> = Vec::new();
            if let Some(stderr) = stderr {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    // Local and dynamic listeners exist once the session starts;
                    // a remote one once the server confirms it.
                    let listening = if remote {
                        line.contains("remote forward success")
                    } else {
                        line.contains("Entering interactive session")
                    };
                    if listening {
                        on_running();
                    }
                    if !line.starts_with("debug") && !line.trim().is_empty() {
                        errors.push(line.trim().to_string());
                    }
                }
            }
            let status = watched.lock().ok().and_then(|mut child| child.wait().ok());
            invocation.cleanup();
            let reason = forward_failure_reason(&errors)
                .or_else(|| status.map(|status| format!("OpenSSH 已退出（{status}）")))
                .unwrap_or_else(|| "端口转发已结束".to_string());
            on_end(reason);
        });

        Ok(Box::new(move || {
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
            }
        }))
    }

    fn invalidate(_id: &str) {
        // ControlMaster connections expire on their own (ControlPersist).
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forward(kind: &str, bind_host: &str, dest_host: &str) -> PortForward {
        PortForward {
            id: "f".into(),
            server_id: "s".into(),
            kind: kind.into(),
            bind_host: bind_host.into(),
            bind_port: 8080,
            dest_host: dest_host.into(),
            dest_port: 80,
            description: String::new(),
            auto_start: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn builds_ssh_forward_specs() {
        assert_eq!(
            forward_args(&forward("local", "127.0.0.1", "db.internal")),
            ["-L", "127.0.0.1:8080:db.internal:80"]
        );
        assert_eq!(
            forward_args(&forward("remote", "0.0.0.0", "::1")),
            ["-R", "0.0.0.0:8080:[::1]:80"]
        );
        assert_eq!(
            forward_args(&forward("dynamic", "127.0.0.1", "")),
            ["-D", "127.0.0.1:8080"]
        );
    }

    #[test]
    fn prefers_the_line_that_explains_the_failure() {
        let lines = vec![
            "Warning: Permanently added '[127.0.0.1]:22' to the list of known hosts.".to_string(),
            "bind [127.0.0.1]:8080: Address already in use".to_string(),
            "channel_setup_fwd_listener_tcpip: cannot listen to port: 8080".to_string(),
            "Could not request local forwarding.".to_string(),
        ];
        assert_eq!(
            forward_failure_reason(&lines).as_deref(),
            Some("bind [127.0.0.1]:8080: Address already in use")
        );
    }

    #[test]
    fn askpass_helpers_are_unique_per_ssh_invocation() {
        let first = openssh::create_askpass_helper("same-server").unwrap();
        let second = openssh::create_askpass_helper("same-server").unwrap();
        assert_ne!(first, second);
        assert!(first.exists());
        assert!(second.exists());
        cleanup_askpass_helper(Some(&first));
        cleanup_askpass_helper(Some(&second));
    }
}
