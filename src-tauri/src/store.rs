use std::{
    fs,
    path::PathBuf,
    sync::{Mutex, MutexGuard, OnceLock, PoisonError},
};

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use tauri::AppHandle;

pub use crate::secrets::{decrypt_secret, encrypt_secret};
use crate::{
    models::{
        JumpHop, PortForward, PortForwardInput, ServerInput, ServerRecord, Snippet, SnippetInput,
    },
    secrets::app_data_dir,
    time::now,
};

const DB_FILE: &str = "ishell.db";
const LEGACY_JSON: &str = "servers.json";
const COMMAND_HISTORY_LIMIT: i64 = 10_000;
/// Jump hosts allowed in front of a server (counting the server's own jump).
pub const MAX_JUMP_HOPS: usize = 3;

/// Schema migrations, applied in order inside one transaction each. The
/// database's `user_version` is the number of migrations it has had. Never
/// edit a released migration: add a new one.
const MIGRATIONS: &[fn(&Transaction) -> rusqlite::Result<()>] = &[
    migration_baseline,
    migration_history_scopes,
    migration_terminal_encoding,
];

/// Everything created before migrations were versioned. Databases from those
/// versions already have some or all of it, hence `IF NOT EXISTS` and the
/// column checks.
fn migration_baseline(tx: &Transaction) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS servers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            host TEXT NOT NULL,
            port INTEGER NOT NULL,
            username TEXT NOT NULL,
            \"group\" TEXT NOT NULL,
            tags TEXT NOT NULL,
            auth_type TEXT NOT NULL,
            key_path TEXT,
            color TEXT NOT NULL,
            notes TEXT NOT NULL,
            secret BLOB,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            last_connected_at INTEGER,
            sort_order INTEGER NOT NULL DEFAULT 0,
            jump_server_id TEXT
        );
        CREATE TABLE IF NOT EXISTS command_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            command TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_command_history_created_at
            ON command_history(created_at DESC);
        CREATE TABLE IF NOT EXISTS snippets (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            command TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            run INTEGER NOT NULL DEFAULT 0,
            sort_order INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS port_forwards (
            id TEXT PRIMARY KEY,
            server_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            bind_host TEXT NOT NULL,
            bind_port INTEGER NOT NULL,
            dest_host TEXT NOT NULL DEFAULT '',
            dest_port INTEGER NOT NULL DEFAULT 0,
            description TEXT NOT NULL DEFAULT '',
            auto_start INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS ai_settings (
            id INTEGER PRIMARY KEY CHECK(id=1),
            encrypted BLOB NOT NULL
        );
        CREATE TABLE IF NOT EXISTS ai_history (
            id TEXT PRIMARY KEY,
            encrypted BLOB NOT NULL
        );",
    )?;
    add_column(tx, "servers", "sort_order", "INTEGER NOT NULL DEFAULT 0")?;
    add_column(tx, "servers", "jump_server_id", "TEXT")
}

/// Command history per server, and AI conversations listed without
/// decrypting their bodies.
fn migration_history_scopes(tx: &Transaction) -> rusqlite::Result<()> {
    // NULL server_id: typed before history was kept per server.
    add_column(tx, "command_history", "server_id", "TEXT")?;
    // NULL server_id: saved before these columns existed; filled in the first
    // time the history is listed, which is when the master key is at hand.
    add_column(tx, "ai_history", "server_id", "TEXT")?;
    add_column(tx, "ai_history", "summary", "BLOB")?;
    add_column(tx, "ai_history", "created_at", "INTEGER NOT NULL DEFAULT 0")?;
    add_column(tx, "ai_history", "updated_at", "INTEGER NOT NULL DEFAULT 0")?;
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_command_history_server
            ON command_history(server_id, id DESC);
        CREATE INDEX IF NOT EXISTS idx_ai_history_server
            ON ai_history(server_id, updated_at DESC);",
    )
}

/// Each server's terminal character encoding.
fn migration_terminal_encoding(tx: &Transaction) -> rusqlite::Result<()> {
    add_column(tx, "servers", "encoding", "TEXT NOT NULL DEFAULT 'UTF-8'")
}

/// Add a column unless the table already has it.
fn add_column(
    tx: &Transaction,
    table: &str,
    column: &str,
    definition: &str,
) -> rusqlite::Result<()> {
    let exists = tx
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))?
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(Result::ok)
        .any(|name| name == column);
    if !exists {
        tx.execute(
            &format!("ALTER TABLE \"{table}\" ADD COLUMN \"{column}\" {definition}"),
            [],
        )?;
    }
    Ok(())
}

/// Connection settings and pending migrations for a freshly opened database.
fn prepare(conn: &mut Connection) -> rusqlite::Result<()> {
    // WAL keeps readers and the writer from blocking each other and survives a
    // crash mid-write; it is a no-op for in-memory databases.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;

    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    // A newer iShell may have migrated further; its additions are left alone.
    for (index, migration) in MIGRATIONS.iter().enumerate().skip(version.max(0) as usize) {
        let tx = conn.transaction()?;
        migration(&tx)?;
        tx.pragma_update(None, "user_version", index as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

const FORWARD_COLUMNS: &str = "id, server_id, kind, bind_host, bind_port, dest_host, dest_port, description, auto_start, created_at, updated_at";

const COLUMNS: &str = "id,name,host,port,username,\"group\",tags,auth_type,key_path,color,notes,created_at,updated_at,last_connected_at,sort_order,jump_server_id,encoding";

pub fn list_servers(app: &AppHandle) -> Result<Vec<ServerRecord>, String> {
    let conn = open_db(app)?;
    let sql = format!("SELECT {COLUMNS} FROM servers ORDER BY \"group\", sort_order, name");
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let rows = stmt.query_map([], row_to_record).map_err(db_err)?;

    let mut servers = Vec::new();
    for row in rows {
        servers.push(row.map_err(db_err)?);
    }
    Ok(servers)
}

pub fn get_server(app: &AppHandle, id: &str) -> Result<ServerRecord, String> {
    let conn = open_db(app)?;
    let sql = format!("SELECT {COLUMNS} FROM servers WHERE id = ?1");
    conn.query_row(&sql, [id], row_to_record)
        .optional()
        .map_err(db_err)?
        .ok_or_else(|| "找不到该服务器".into())
}

/// Insert or update a server. When `secret` is `Some`, the password/passphrase
/// is encrypted (AES-256-GCM) and stored in the local SQLite database; when
/// `None`, the existing stored secret is preserved.
pub fn upsert_server(
    app: &AppHandle,
    record: &ServerRecord,
    secret: Option<&str>,
) -> Result<(), String> {
    // Encrypt first: the master key lookup can wait on a keychain prompt and
    // must not hold the database meanwhile.
    let blob = secret
        .map(|plaintext| encrypt_secret(app, plaintext))
        .transpose()?;
    let tags = serde_json::to_string(&record.tags).map_err(|err| err.to_string())?;
    let conn = open_db(app)?;

    conn.execute(
        &format!(
            "INSERT INTO servers ({COLUMNS}, secret)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17, NULL)
             ON CONFLICT(id) DO UPDATE SET
               name=excluded.name, host=excluded.host, port=excluded.port,
               username=excluded.username, \"group\"=excluded.\"group\",
               tags=excluded.tags, auth_type=excluded.auth_type,
               key_path=excluded.key_path, color=excluded.color,
               notes=excluded.notes, updated_at=excluded.updated_at,
               last_connected_at=excluded.last_connected_at,
               sort_order=excluded.sort_order,
               jump_server_id=excluded.jump_server_id,
               encoding=excluded.encoding"
        ),
        params![
            record.id,
            record.name,
            record.host,
            record.port as i64,
            record.username,
            record.group,
            tags,
            record.auth_type,
            record.key_path,
            record.color,
            record.notes,
            record.created_at as i64,
            record.updated_at as i64,
            record.last_connected_at.map(|value| value as i64),
            record.sort_order as i64,
            record.jump_server_id,
            record.encoding,
        ],
    )
    .map_err(db_err)?;

    if let Some(blob) = blob {
        conn.execute(
            "UPDATE servers SET secret = ?1 WHERE id = ?2",
            params![blob, record.id],
        )
        .map_err(db_err)?;
    }

    Ok(())
}

pub fn delete_server(app: &AppHandle, id: &str) -> Result<(), String> {
    let conn = open_db(app)?;
    conn.execute("DELETE FROM servers WHERE id = ?1", [id])
        .map_err(db_err)?;
    conn.execute("DELETE FROM port_forwards WHERE server_id = ?1", [id])
        .map_err(db_err)?;
    conn.execute("DELETE FROM command_history WHERE server_id = ?1", [id])
        .map_err(db_err)?;
    conn.execute("DELETE FROM ai_history WHERE server_id = ?1", [id])
        .map_err(db_err)?;
    // Servers that went through this one now connect directly.
    conn.execute(
        "UPDATE servers SET jump_server_id = NULL WHERE jump_server_id = ?1",
        [id],
    )
    .map_err(db_err)?;
    Ok(())
}

/// The jump hosts in front of `server`, the one dialled first first. Fails on
/// a missing jump server, a loop, or a chain longer than `MAX_JUMP_HOPS`.
pub fn jump_hops(app: &AppHandle, server: &ServerRecord) -> Result<Vec<JumpHop>, String> {
    let mut hops = Vec::new();
    let mut seen = vec![server.id.clone()];
    let mut next = server.jump_server_id.clone();
    while let Some(jump_id) = next.filter(|id| !id.is_empty()) {
        if seen.contains(&jump_id) {
            return Err("跳板机设置出现循环，请检查各服务器的跳板机".into());
        }
        if hops.len() >= MAX_JUMP_HOPS {
            return Err(format!("跳板机最多串联 {MAX_JUMP_HOPS} 层"));
        }
        let jump = get_server(app, &jump_id)
            .map_err(|_| "找不到设置的跳板机，它可能已被删除".to_string())?;
        let secret = read_secret(app, &jump_id)
            .ok()
            .filter(|value| !value.is_empty());
        seen.push(jump_id);
        next = jump.jump_server_id.clone();
        hops.push(JumpHop {
            server: jump,
            secret,
        });
    }
    hops.reverse();
    Ok(hops)
}

pub fn reorder_servers(
    app: &AppHandle,
    items: &[crate::models::ServerOrderInput],
) -> Result<(), String> {
    let mut conn = open_db(app)?;
    let tx = conn.transaction().map_err(db_err)?;
    let stamp = now() as i64;
    for item in items {
        tx.execute(
            "UPDATE servers SET \"group\" = ?1, sort_order = ?2, updated_at = ?3 WHERE id = ?4",
            params![item.group.trim(), item.sort_order as i64, stamp, item.id],
        )
        .map_err(db_err)?;
    }
    tx.commit().map_err(db_err)?;
    Ok(())
}

pub fn mark_connected(app: &AppHandle, id: &str) -> Result<(), String> {
    let conn = open_db(app)?;
    let stamp = now() as i64;
    conn.execute(
        "UPDATE servers SET last_connected_at = ?1, updated_at = ?2 WHERE id = ?3",
        params![stamp, stamp, id],
    )
    .map_err(db_err)?;
    Ok(())
}

pub fn read_secret(app: &AppHandle, id: &str) -> Result<String, String> {
    let blob: Option<Vec<u8>> = open_db(app)?
        .query_row("SELECT secret FROM servers WHERE id = ?1", [id], |row| {
            row.get::<_, Option<Vec<u8>>>(0)
        })
        .optional()
        .map_err(db_err)?
        .flatten();

    match blob {
        Some(bytes) => decrypt_secret(app, &bytes),
        None => Err("尚未保存该主机的密码，请编辑主机并填写密码后重试".into()),
    }
}

/// Commands typed on `server_id`, newest first. Commands recorded before
/// history was kept per server belong to every server. Without a server, the
/// whole history.
pub fn list_command_history(
    app: &AppHandle,
    server_id: Option<&str>,
) -> Result<Vec<String>, String> {
    let conn = open_db(app)?;
    query_command_history(&conn, server_id).map_err(db_err)
}

fn query_command_history(
    conn: &Connection,
    server_id: Option<&str>,
) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT command
         FROM command_history
         WHERE ?1 IS NULL OR server_id IS NULL OR server_id = ?1
         ORDER BY id DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![server_id, COMMAND_HISTORY_LIMIT], |row| {
        row.get::<_, String>(0)
    })?;
    rows.collect()
}

pub fn append_command_history(
    app: &AppHandle,
    server_id: Option<&str>,
    command_text: &str,
) -> Result<(), String> {
    let command_text = command_text.trim();
    if command_text.is_empty() {
        return Ok(());
    }
    let mut conn = open_db(app)?;
    insert_command_history(&mut conn, server_id, command_text).map_err(db_err)
}

fn insert_command_history(
    conn: &mut Connection,
    server_id: Option<&str>,
    command_text: &str,
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO command_history (command, created_at, server_id) VALUES (?1, ?2, ?3)",
        params![command_text, now() as i64, server_id],
    )?;
    tx.execute(
        "DELETE FROM command_history
         WHERE id NOT IN (
           SELECT id FROM command_history ORDER BY id DESC LIMIT ?1
         )",
        [COMMAND_HISTORY_LIMIT],
    )?;
    tx.commit()
}

pub fn list_snippets(app: &AppHandle) -> Result<Vec<Snippet>, String> {
    let conn = open_db(app)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, name, command, description, run, sort_order, created_at, updated_at
             FROM snippets
             ORDER BY sort_order, created_at",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Snippet {
                id: row.get(0)?,
                name: row.get(1)?,
                command: row.get(2)?,
                description: row.get(3)?,
                run: row.get::<_, i64>(4)? != 0,
                sort_order: row.get::<_, i64>(5)? as u64,
                created_at: row.get::<_, i64>(6)? as u64,
                updated_at: row.get::<_, i64>(7)? as u64,
            })
        })
        .map_err(db_err)?;

    let mut snippets = Vec::new();
    for row in rows {
        snippets.push(row.map_err(db_err)?);
    }
    Ok(snippets)
}

/// Insert a new snippet (at the end of the list) or update an existing one.
pub fn save_snippet(app: &AppHandle, input: SnippetInput) -> Result<Snippet, String> {
    let name = input.name.trim().to_string();
    let command = input.command.trim_end().to_string();
    if command.trim().is_empty() {
        return Err("命令不能为空".into());
    }
    let name = if name.is_empty() {
        command
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        name
    };

    let conn = open_db(app)?;
    let stamp = now();
    let existing = match input.id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => conn
            .query_row(
                "SELECT sort_order, created_at FROM snippets WHERE id = ?1",
                [id],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
            )
            .optional()
            .map_err(db_err)?,
        None => None,
    };
    let (id, sort_order, created_at) = match (input.id, existing) {
        (Some(id), Some((sort_order, created_at))) => (id, sort_order, created_at),
        _ => {
            let next_order: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(sort_order) + 1, 0) FROM snippets",
                    [],
                    |row| row.get(0),
                )
                .map_err(db_err)?;
            (uuid::Uuid::new_v4().to_string(), next_order as u64, stamp)
        }
    };

    let snippet = Snippet {
        id,
        name,
        command,
        description: input.description.trim().to_string(),
        run: input.run,
        sort_order,
        created_at,
        updated_at: stamp,
    };
    conn.execute(
        "INSERT INTO snippets (id, name, command, description, run, sort_order, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
           name=excluded.name, command=excluded.command, description=excluded.description,
           run=excluded.run, updated_at=excluded.updated_at",
        params![
            snippet.id,
            snippet.name,
            snippet.command,
            snippet.description,
            snippet.run as i64,
            snippet.sort_order as i64,
            snippet.created_at as i64,
            snippet.updated_at as i64,
        ],
    )
    .map_err(db_err)?;
    Ok(snippet)
}

pub fn delete_snippet(app: &AppHandle, id: &str) -> Result<(), String> {
    let conn = open_db(app)?;
    conn.execute("DELETE FROM snippets WHERE id = ?1", [id])
        .map_err(db_err)?;
    Ok(())
}

fn row_to_forward(row: &rusqlite::Row) -> rusqlite::Result<PortForward> {
    Ok(PortForward {
        id: row.get(0)?,
        server_id: row.get(1)?,
        kind: row.get(2)?,
        bind_host: row.get(3)?,
        bind_port: row.get::<_, i64>(4)? as u16,
        dest_host: row.get(5)?,
        dest_port: row.get::<_, i64>(6)? as u16,
        description: row.get(7)?,
        auto_start: row.get::<_, i64>(8)? != 0,
        created_at: row.get::<_, i64>(9)? as u64,
        updated_at: row.get::<_, i64>(10)? as u64,
    })
}

pub fn list_port_forwards(app: &AppHandle) -> Result<Vec<PortForward>, String> {
    let conn = open_db(app)?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {FORWARD_COLUMNS} FROM port_forwards ORDER BY created_at"
        ))
        .map_err(db_err)?;
    let rows = stmt.query_map([], row_to_forward).map_err(db_err)?;
    let mut forwards = Vec::new();
    for row in rows {
        forwards.push(row.map_err(db_err)?);
    }
    Ok(forwards)
}

pub fn get_port_forward(app: &AppHandle, id: &str) -> Result<PortForward, String> {
    let conn = open_db(app)?;
    conn.query_row(
        &format!("SELECT {FORWARD_COLUMNS} FROM port_forwards WHERE id = ?1"),
        [id],
        row_to_forward,
    )
    .optional()
    .map_err(db_err)?
    .ok_or_else(|| "找不到该端口转发".into())
}

pub fn validate_port_forward(input: &PortForwardInput) -> Result<(), String> {
    if !matches!(input.kind.as_str(), "local" | "remote" | "dynamic") {
        return Err("转发类型无效".into());
    }
    if input.bind_port == 0 {
        return Err("监听端口必须在 1–65535 之间".into());
    }
    if input.kind != "dynamic" {
        if input.dest_host.trim().is_empty() {
            return Err("请填写目标主机".into());
        }
        if input.dest_port == 0 {
            return Err("目标端口必须在 1–65535 之间".into());
        }
    }
    Ok(())
}

pub fn save_port_forward(app: &AppHandle, input: PortForwardInput) -> Result<PortForward, String> {
    validate_port_forward(&input)?;
    get_server(app, &input.server_id)?;
    let conn = open_db(app)?;
    let stamp = now();
    let created_at = match input.id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => conn
            .query_row(
                "SELECT created_at FROM port_forwards WHERE id = ?1",
                [id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(db_err)?
            .map(|value| value as u64),
        None => None,
    };
    let bind_host = input.bind_host.trim();
    let forward = PortForward {
        id: input
            .id
            .filter(|id| !id.is_empty() && created_at.is_some())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        server_id: input.server_id,
        bind_host: if bind_host.is_empty() {
            "127.0.0.1".into()
        } else {
            bind_host.into()
        },
        bind_port: input.bind_port,
        dest_host: if input.kind == "dynamic" {
            String::new()
        } else {
            input.dest_host.trim().into()
        },
        dest_port: if input.kind == "dynamic" {
            0
        } else {
            input.dest_port
        },
        kind: input.kind,
        description: input.description.trim().into(),
        auto_start: input.auto_start,
        created_at: created_at.unwrap_or(stamp),
        updated_at: stamp,
    };
    conn.execute(
        &format!(
            "INSERT INTO port_forwards ({FORWARD_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
               kind=excluded.kind, bind_host=excluded.bind_host, bind_port=excluded.bind_port,
               dest_host=excluded.dest_host, dest_port=excluded.dest_port,
               description=excluded.description, auto_start=excluded.auto_start,
               updated_at=excluded.updated_at"
        ),
        params![
            forward.id,
            forward.server_id,
            forward.kind,
            forward.bind_host,
            forward.bind_port as i64,
            forward.dest_host,
            forward.dest_port as i64,
            forward.description,
            forward.auto_start as i64,
            forward.created_at as i64,
            forward.updated_at as i64,
        ],
    )
    .map_err(db_err)?;
    Ok(forward)
}

pub fn delete_port_forward(app: &AppHandle, id: &str) -> Result<(), String> {
    let conn = open_db(app)?;
    conn.execute("DELETE FROM port_forwards WHERE id = ?1", [id])
        .map_err(db_err)?;
    Ok(())
}

pub fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    tags.into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect()
}

pub fn validate_server(input: &ServerInput) -> Result<(), String> {
    if input.name.trim().is_empty() {
        return Err("服务器名称不能为空".into());
    }
    if input.host.trim().is_empty() {
        return Err("主机地址不能为空".into());
    }
    if input.username.trim().is_empty() {
        return Err("用户名不能为空".into());
    }
    if input.port == 0 {
        return Err("端口必须大于 0".into());
    }
    if input.auth_type != "password" && input.auth_type != "key" {
        return Err("认证方式无效".into());
    }
    if input.auth_type == "key" && input.key_path.as_deref().unwrap_or("").trim().is_empty() {
        return Err("密钥认证需要填写私钥路径".into());
    }
    Ok(())
}

// ===== internals =====

fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<ServerRecord> {
    let tags_json: String = row.get(6)?;
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

    Ok(ServerRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        host: row.get(2)?,
        port: row.get::<_, i64>(3)? as u16,
        username: row.get(4)?,
        group: row.get(5)?,
        tags,
        auth_type: row.get(7)?,
        key_path: row.get(8)?,
        color: row.get(9)?,
        notes: row.get(10)?,
        created_at: row.get::<_, i64>(11)? as u64,
        updated_at: row.get::<_, i64>(12)? as u64,
        last_connected_at: row.get::<_, Option<i64>>(13)?.map(|value| value as u64),
        sort_order: row.get::<_, i64>(14)? as u64,
        jump_server_id: row.get(15)?,
        encoding: row.get(16)?,
    })
}

/// The app's single database connection, opened and migrated on first use.
/// The guard serializes access: drop it before calling anything else that
/// opens the database.
pub(crate) fn open_db(app: &AppHandle) -> Result<MutexGuard<'static, Connection>, String> {
    static DB: OnceLock<Mutex<Connection>> = OnceLock::new();
    static OPENING: Mutex<()> = Mutex::new(());

    if DB.get().is_none() {
        let _opening = OPENING.lock().unwrap_or_else(PoisonError::into_inner);
        if DB.get().is_none() {
            let mut conn = Connection::open(db_path(app)?).map_err(db_err)?;
            prepare(&mut conn).map_err(db_err)?;
            migrate_legacy_json(app, &conn)?;
            let _ = DB.set(Mutex::new(conn));
        }
    }
    Ok(DB
        .get()
        .expect("database initialized above")
        .lock()
        .unwrap_or_else(PoisonError::into_inner))
}

/// Best-effort one-time import of host metadata from the old `servers.json`
/// store. Passwords are not migrated (they lived in the OS keychain) and must
/// be re-entered once.
fn migrate_legacy_json(app: &AppHandle, conn: &Connection) -> Result<(), String> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM servers", [], |row| row.get(0))
        .map_err(db_err)?;
    if count > 0 {
        return Ok(());
    }

    let json_path = app_data_dir(app)?.join(LEGACY_JSON);
    if !json_path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(&json_path).unwrap_or_default();
    if content.trim().is_empty() {
        return Ok(());
    }
    let records: Vec<ServerRecord> = serde_json::from_str(&content).unwrap_or_default();

    for record in &records {
        let tags = serde_json::to_string(&record.tags).unwrap_or_else(|_| "[]".into());
        let _ = conn.execute(
            &format!(
                "INSERT OR IGNORE INTO servers ({COLUMNS}, secret)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17, NULL)"
            ),
            params![
                record.id,
                record.name,
                record.host,
                record.port as i64,
                record.username,
                record.group,
                tags,
                record.auth_type,
                record.key_path,
                record.color,
                record.notes,
                record.created_at as i64,
                record.updated_at as i64,
                record.last_connected_at.map(|value| value as i64),
                record.sort_order as i64,
                record.jump_server_id,
                record.encoding,
            ],
        );
    }

    let _ = fs::rename(&json_path, json_path.with_extension("json.imported"));
    Ok(())
}

fn db_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app_data_dir(app)?.join(DB_FILE))
}

fn db_err(err: rusqlite::Error) -> String {
    format!("本地数据库错误：{err}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(conn: &Connection) -> i64 {
        conn.pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap()
    }

    fn columns(conn: &Connection, table: &str) -> Vec<String> {
        conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn a_new_database_gets_every_migration_once() {
        let mut conn = Connection::open_in_memory().unwrap();
        prepare(&mut conn).unwrap();
        assert_eq!(version(&conn), MIGRATIONS.len() as i64);
        assert!(columns(&conn, "command_history").contains(&"server_id".to_string()));
        assert!(columns(&conn, "ai_history").contains(&"summary".to_string()));
        // Opening again changes nothing.
        prepare(&mut conn).unwrap();
        assert_eq!(version(&conn), MIGRATIONS.len() as i64);
    }

    /// A database from before `sort_order`, `jump_server_id` and versioning.
    #[test]
    fn an_unversioned_database_is_upgraded_in_place() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE servers (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, host TEXT NOT NULL,
                port INTEGER NOT NULL, username TEXT NOT NULL, \"group\" TEXT NOT NULL,
                tags TEXT NOT NULL, auth_type TEXT NOT NULL, key_path TEXT,
                color TEXT NOT NULL, notes TEXT NOT NULL, secret BLOB,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                last_connected_at INTEGER
            );
            INSERT INTO servers VALUES
                ('a','web','10.0.0.1',22,'root','Default','[]','password',NULL,'','',x'0102',1,1,NULL);
            CREATE TABLE command_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT, command TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            INSERT INTO command_history (command, created_at) VALUES ('uptime', 1);
            CREATE TABLE ai_history (id TEXT PRIMARY KEY, encrypted BLOB NOT NULL);
            INSERT INTO ai_history VALUES ('h', x'00');",
        )
        .unwrap();

        prepare(&mut conn).unwrap();

        assert_eq!(version(&conn), MIGRATIONS.len() as i64);
        let sql = format!("SELECT {COLUMNS} FROM servers WHERE id = 'a'");
        let server = conn.query_row(&sql, [], row_to_record).unwrap();
        assert_eq!((server.name.as_str(), server.sort_order), ("web", 0));
        assert_eq!(server.jump_server_id, None);
        let secret: Vec<u8> = conn
            .query_row("SELECT secret FROM servers WHERE id = 'a'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(secret, [1, 2]);
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ai_history WHERE server_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1);
    }

    #[test]
    fn command_history_is_kept_per_server() {
        let mut conn = Connection::open_in_memory().unwrap();
        prepare(&mut conn).unwrap();
        insert_command_history(&mut conn, None, "legacy").unwrap();
        insert_command_history(&mut conn, Some("a"), "on-a").unwrap();
        insert_command_history(&mut conn, Some("b"), "on-b").unwrap();

        assert_eq!(
            query_command_history(&conn, Some("a")).unwrap(),
            ["on-a", "legacy"]
        );
        assert_eq!(
            query_command_history(&conn, Some("b")).unwrap(),
            ["on-b", "legacy"]
        );
        assert_eq!(
            query_command_history(&conn, None).unwrap(),
            ["on-b", "on-a", "legacy"]
        );
    }
}
