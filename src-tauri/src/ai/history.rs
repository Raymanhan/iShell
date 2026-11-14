//! Saved AI conversations. Bodies are encrypted with the local master key and
//! loaded one at a time; the history list comes from a small encrypted summary
//! and plain timestamps, so it stays fast however long the conversations get.

use crate::store::{decrypt_secret, encrypt_secret, open_db};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::AppHandle;

const MAX_CONVERSATION_BYTES: usize = 32 * 1024 * 1024;
const TITLE_CHARS: usize = 80;

/// One row of the history list.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    id: String,
    server_id: String,
    title: String,
    /// Messages the user sent.
    message_count: u64,
    created_at: u64,
    updated_at: u64,
}

/// A validated conversation ready to store.
struct Record<'a> {
    id: &'a str,
    server_id: &'a str,
    title: String,
    message_count: u64,
    created_at: u64,
    updated_at: u64,
    json: String,
}

/// The first user message, on one line.
fn title(input: &Value) -> String {
    input["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["kind"] == "user")
        .and_then(|entry| entry["text"].as_str())
        .map(|text| {
            text.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(TITLE_CHARS)
                .collect()
        })
        .unwrap_or_default()
}

fn record(input: &Value) -> Result<Record<'_>, String> {
    let id = input["id"].as_str().ok_or("历史记录 ID 无效")?;
    uuid::Uuid::parse_str(id).map_err(|_| "历史记录 ID 无效")?;
    let server_id = input["serverId"].as_str().ok_or("历史记录服务器无效")?;
    if server_id.is_empty()
        || server_id.len() > 200
        || !input["entries"].is_array()
        || !input["messages"].is_array()
    {
        return Err("历史记录格式无效".into());
    }
    let json = serde_json::to_string(input).map_err(|e| e.to_string())?;
    if json.len() > MAX_CONVERSATION_BYTES {
        return Err("单条历史记录超过 32 MB，无法保存".into());
    }
    Ok(Record {
        id,
        server_id,
        title: title(input),
        message_count: input["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| entry["kind"] == "user")
            .count() as u64,
        created_at: input["createdAt"].as_u64().unwrap_or_default(),
        updated_at: input["updatedAt"].as_u64().unwrap_or_default(),
        json,
    })
}

fn store(app: &AppHandle, record: &Record) -> Result<(), String> {
    // Encrypt before taking the database: see `store::open_db`.
    let body = encrypt_secret(app, &record.json)?;
    let summary = encrypt_secret(
        app,
        &json!({ "title": record.title, "messages": record.message_count }).to_string(),
    )?;
    open_db(app)?
        .execute(
            "INSERT INTO ai_history (id, encrypted, server_id, summary, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
               encrypted=excluded.encrypted, server_id=excluded.server_id,
               summary=excluded.summary, created_at=excluded.created_at,
               updated_at=excluded.updated_at",
            params![
                record.id,
                body,
                record.server_id,
                summary,
                record.created_at as i64,
                record.updated_at as i64
            ],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Conversations saved before summaries existed have only the encrypted
/// body: decrypt each once and write its summary columns.
fn backfill_summaries(app: &AppHandle) -> Result<(), String> {
    let pending: Vec<(String, Vec<u8>)> = {
        let conn = open_db(app)?;
        let mut statement = conn
            .prepare("SELECT id, encrypted FROM ai_history WHERE server_id IS NULL")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };
    for (_id, blob) in pending {
        let saved = decrypt_secret(app, &blob)
            .ok()
            .and_then(|json| serde_json::from_str::<Value>(&json).ok());
        match saved.as_ref().map(record) {
            Some(Ok(record)) => store(app, &record)?,
            // Unreadable with this key or malformed. Leave it untouched:
            // the key may come back (a locked keychain), and one bad row
            // must not take the rest of the history with it.
            _ => continue,
        }
    }
    Ok(())
}

/// id, server id, encrypted summary, created, updated.
type SummaryRow = (String, String, Option<Vec<u8>>, i64, i64);

fn list(app: &AppHandle) -> Result<Vec<Summary>, String> {
    backfill_summaries(app)?;
    let rows: Vec<SummaryRow> = {
        let conn = open_db(app)?;
        let mut statement = conn
            .prepare(
                "SELECT id, server_id, summary, created_at, updated_at FROM ai_history
                 WHERE server_id IS NOT NULL ORDER BY updated_at DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };
    Ok(rows
        .into_iter()
        .map(|(id, server_id, summary, created_at, updated_at)| {
            let summary = summary
                .and_then(|blob| decrypt_secret(app, &blob).ok())
                .and_then(|json| serde_json::from_str::<Value>(&json).ok())
                .unwrap_or_default();
            Summary {
                id,
                server_id,
                title: summary["title"].as_str().unwrap_or_default().to_string(),
                message_count: summary["messages"].as_u64().unwrap_or_default(),
                created_at: created_at.max(0) as u64,
                updated_at: updated_at.max(0) as u64,
            }
        })
        .collect())
}

fn load(app: &AppHandle, id: &str) -> Result<Value, String> {
    let blob: Option<Vec<u8>> = open_db(app)?
        .query_row(
            "SELECT encrypted FROM ai_history WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let blob = blob.ok_or("找不到这条历史记录")?;
    serde_json::from_str(&decrypt_secret(app, &blob)?).map_err(|_| "AI 历史记录无法读取".into())
}

async fn blocking<T: Send + 'static>(
    action: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(action)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn ai_list_history(app: AppHandle) -> Result<Vec<Summary>, String> {
    blocking(move || list(&app)).await
}

#[tauri::command]
pub async fn ai_load_history(app: AppHandle, id: String) -> Result<Value, String> {
    uuid::Uuid::parse_str(&id).map_err(|_| "历史记录 ID 无效")?;
    blocking(move || load(&app, &id)).await
}

#[tauri::command]
pub async fn ai_save_history(app: AppHandle, input: Value) -> Result<(), String> {
    blocking(move || store(&app, &record(&input)?)).await
}

#[tauri::command]
pub async fn ai_delete_history(app: AppHandle, id: String) -> Result<(), String> {
    uuid::Uuid::parse_str(&id).map_err(|_| "历史记录 ID 无效")?;
    blocking(move || {
        open_db(&app)?
            .execute("DELETE FROM ai_history WHERE id=?1", [id])
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_history_before_storage() {
        let mut value = json!({"id": uuid::Uuid::new_v4().to_string(), "serverId": "server", "entries": [], "messages": []});
        assert!(record(&value).is_ok());
        value["serverId"] = "".into();
        assert!(record(&value).is_err());
        value["serverId"] = "server".into();
        value["id"] = "bad".into();
        assert!(record(&value).is_err());
    }

    #[test]
    fn summary_comes_from_the_first_user_message() {
        let value = json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "serverId": "server",
            "createdAt": 10,
            "updatedAt": 20,
            "messages": [],
            "entries": [
                {"kind": "assistant", "text": "ignored"},
                {"kind": "user", "text": "  排查\n nginx   502  "},
                {"kind": "user", "text": "later"}
            ]
        });
        let record = record(&value).unwrap();
        assert_eq!(record.title, "排查 nginx 502");
        assert_eq!(record.message_count, 2);
        assert_eq!((record.created_at, record.updated_at), (10, 20));

        let long = json!({
            "id": uuid::Uuid::new_v4().to_string(), "serverId": "s", "messages": [],
            "entries": [{"kind": "user", "text": "字".repeat(200)}]
        });
        assert_eq!(title(&long).chars().count(), TITLE_CHARS);
        assert_eq!(title(&json!({"entries": []})), "");
    }
}
