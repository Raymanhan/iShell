//! Running commands on a server: connection tests and one-shot remote shell
//! or Python commands.

use super::*;

pub fn test_connection(
    app: &AppHandle,
    input: ServerInput,
    password: Option<String>,
) -> Result<(), String> {
    let saved_id = input.id.clone();
    let id = format!("test-{}", now());
    let server = ServerRecord {
        id: id.clone(),
        name: input.name.trim().to_string(),
        host: input.host.trim().to_string(),
        port: input.port,
        username: input.username.trim().to_string(),
        group: input.group.trim().to_string(),
        tags: input.tags,
        auth_type: input.auth_type,
        key_path: input.key_path.filter(|path| !path.trim().is_empty()),
        color: input.color,
        notes: input.notes,
        jump_server_id: input
            .jump_server_id
            .filter(|jump_id| !jump_id.trim().is_empty()),
        sort_order: input.sort_order.unwrap_or_else(now),
        created_at: now(),
        updated_at: now(),
        last_connected_at: None,
    };
    let secret = password
        .filter(|value| !value.is_empty())
        .or_else(|| {
            saved_id
                .as_deref()
                .and_then(|server_id| read_secret(app, server_id).ok())
        })
        .filter(|value| !value.is_empty());
    test_connection_with_server(app, &server, secret.as_deref())
}

pub(super) fn test_connection_with_server(
    app: &AppHandle,
    server: &ServerRecord,
    secret: Option<&str>,
) -> Result<(), String> {
    transport::test_connection(app, server, secret).map_err(|err| format!("测试连接失败：{err}"))
}

pub(super) fn run_remote_shell(app: &AppHandle, id: &str, script: &str) -> Result<String, String> {
    run_remote_command(
        app,
        id,
        &format!("sh -lc {}", openssh::shell_quote(script)),
        None,
    )
}

pub(crate) fn run_remote_python(
    app: &AppHandle,
    id: &str,
    script: &str,
    args: &[&str],
) -> Result<String, String> {
    let quoted_args = args
        .iter()
        .map(|arg| openssh::shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let command = if quoted_args.is_empty() {
        "python3 -".to_string()
    } else {
        format!("python3 - {quoted_args}")
    };
    run_remote_command(app, id, &command, Some(script.as_bytes()))
}

pub(super) fn run_remote_command(
    app: &AppHandle,
    id: &str,
    remote_command: &str,
    stdin: Option<&[u8]>,
) -> Result<String, String> {
    let stdout = transport::exec(app, id, remote_command, stdin)
        .map_err(|err| format!("SSH 命令失败：{err}"))?;
    String::from_utf8(stdout).map_err(|err| format!("SSH 输出不是 UTF-8：{err}"))
}

pub(super) fn remote_file_size(
    app: &AppHandle,
    id: &str,
    remote_path: &str,
) -> Result<u64, String> {
    let quoted = openssh::shell_quote(remote_path);
    let raw = run_remote_shell(
        app,
        id,
        &format!("stat -c %s -- {quoted} 2>/dev/null || stat -f %z -- {quoted} 2>/dev/null || wc -c < {quoted}"),
    )?;
    raw.split_whitespace()
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| "无法读取远程文件大小".to_string())
}

pub(super) fn read_remote_file_bytes(
    app: &AppHandle,
    id: &str,
    remote_path: &str,
) -> Result<Vec<u8>, String> {
    let command = format!("cat -- {}", openssh::shell_quote(remote_path));
    transport::exec(app, id, &command, None).map_err(|err| format!("读取远程文件失败：{err}"))
}

pub(super) fn remove_remote_file(app: &AppHandle, id: &str, path: &str) -> Result<(), String> {
    run_remote_shell(app, id, &format!("rm -f -- {}", openssh::shell_quote(path))).map(|_| ())
}
