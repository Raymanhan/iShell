//! Servers an AI task can operate on. A task always has its primary server
//! (the tab it was started from) and may add others that have a connected
//! terminal. Tools then take a `servers` argument naming where to run; one
//! call can fan out to several servers in parallel.

use serde_json::{json, Value};
use std::sync::{atomic::AtomicBool, Arc, Mutex};
use tauri::{
    ipc::{Channel, InvokeResponseBody},
    AppHandle,
};

use super::{tools, Event};
use crate::{models::ServerRecord, terminal::TerminalRegistry};

pub const MAX_TARGETS: usize = 16;
/// Output kept per server when a call runs on several.
const FANOUT_OUTPUT_CHARS: usize = 4_000;

/// A server the task may use, by the name the model calls it.
#[derive(Clone)]
pub struct Host {
    pub label: String,
    pub server: ServerRecord,
    pub session: String,
}

/// Names for the model to use: the server's name, made unique by its address
/// (and then a number) where two servers share a name.
pub fn labels(servers: &[ServerRecord]) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    for server in servers {
        let name = server.name.trim();
        let name = if name.is_empty() { &server.host } else { name };
        let mut label = name.to_string();
        if labels.contains(&label) {
            label = format!("{name} ({}@{})", server.username, server.host);
        }
        let base = label.clone();
        let mut n = 2;
        while labels.contains(&label) {
            label = format!("{base} #{n}");
            n += 1;
        }
        labels.push(label);
    }
    labels
}

/// The tool definitions with a `servers` argument when there is a choice.
pub fn definitions(labels: &[String]) -> Value {
    let mut definitions = tools::definitions();
    if labels.len() < 2 {
        return definitions;
    }
    for tool in definitions.as_array_mut().into_iter().flatten() {
        tool["function"]["parameters"]["properties"]["servers"] = json!({
            "type": "array",
            "items": {"type": "string", "enum": labels},
            "minItems": 1,
            "maxItems": MAX_TARGETS,
            "description": "在哪些服务器上执行（填服务器名称）。可填多个，会在各服务器上并行执行并分别返回结果；省略时只在主服务器上执行。",
        });
    }
    definitions
}

/// The hosts a call names, removing `servers` from its arguments. Without
/// `servers`, the primary host (the first).
pub fn select<'a>(hosts: &'a [Host], args: &mut Value) -> Result<Vec<&'a Host>, String> {
    let named = args
        .as_object_mut()
        .and_then(|object| object.remove("servers"));
    let Some(named) = named else {
        return Ok(hosts.iter().take(1).collect());
    };
    let names = named
        .as_array()
        .filter(|names| !names.is_empty() && names.len() <= MAX_TARGETS)
        .ok_or("servers 应为 1–16 个服务器名称")?;
    let mut selected: Vec<&Host> = Vec::new();
    for name in names {
        let name = name.as_str().ok_or("servers 应为服务器名称")?;
        let host = hosts
            .iter()
            .find(|host| host.label == name)
            .ok_or_else(|| format!("本任务不能操作服务器“{name}”"))?;
        if !selected.iter().any(|existing| existing.label == host.label) {
            selected.push(host);
        }
    }
    Ok(selected)
}

fn tail(text: &str, max_chars: usize) -> (String, bool) {
    let count = text.chars().count();
    if count <= max_chars {
        (text.to_string(), false)
    } else {
        (text.chars().skip(count - max_chars).collect(), true)
    }
}

/// One result for a call that ran on several servers: each server's own
/// result, plus an overall exit code and error the UI and model can read.
pub fn combine(states: &[(String, Value)]) -> Value {
    let done = states.iter().all(|(_, state)| state["status"] == "done");
    let mut results = Vec::new();
    let mut failed = Vec::new();
    for (label, state) in states {
        let mut entry = json!({"server": label});
        for key in ["status", "exitCode", "error"] {
            entry[key] = state[key].clone();
        }
        let mut truncated = state["truncated"].as_bool().unwrap_or(false);
        for key in ["stdout", "stderr"] {
            let (text, cut) = tail(state[key].as_str().unwrap_or_default(), FANOUT_OUTPUT_CHARS);
            truncated |= cut;
            entry[key] = json!(text);
        }
        entry["truncated"] = json!(truncated);
        let ok = state["error"].is_null() && state["exitCode"].as_i64() == Some(0);
        if state["status"] == "done" && !ok {
            failed.push(label.clone());
        }
        results.push(entry);
    }
    json!({
        "status": if done { "done" } else { "running" },
        "results": results,
        "exitCode": if !done { Value::Null } else if failed.is_empty() { json!(0) } else { json!(1) },
        "error": if failed.is_empty() { Value::Null } else { json!(format!("{} 台服务器失败：{}", failed.len(), failed.join("、"))) },
        "stdout": "",
        "stderr": "",
    })
}

/// Run a validated, approved tool call on the chosen hosts. One host keeps
/// the plain result; several run in parallel and report combined progress.
/// Blocking.
#[allow(clippy::too_many_arguments)]
pub fn run(
    app: &AppHandle,
    terminals: &Arc<TerminalRegistry>,
    cancel: &Arc<AtomicBool>,
    events: &Channel<Event>,
    id: &str,
    name: &str,
    args: &Value,
    hosts: &[&Host],
    guard_sensitive: bool,
) -> Result<Value, String> {
    let context = |host: &Host, events: Channel<Event>| tools::Context {
        app: app.clone(),
        server: host.server.id.clone(),
        session: host.session.clone(),
        terminals: terminals.clone(),
        cancel: cancel.clone(),
        events,
        guard_sensitive,
    };
    if let [host] = hosts {
        return tools::execute(
            context(host, events.clone()),
            id.into(),
            name.into(),
            args.clone(),
        );
    }

    let states: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(
        hosts
            .iter()
            .map(|host| (host.label.clone(), json!({"status": "running"})))
            .collect(),
    ));
    std::thread::scope(|scope| {
        for (position, host) in hosts.iter().enumerate() {
            let progress_states = states.clone();
            let outer = events.clone();
            let tool_id = id.to_string();
            // Each server reports into its slot; the card gets the combined view.
            let progress = Channel::<Event>::new(move |body| {
                if let InvokeResponseBody::Json(json) = body {
                    if let Ok(event) = serde_json::from_str::<Value>(&json) {
                        if event["type"] == "tool_output" {
                            let combined = {
                                let mut states =
                                    progress_states.lock().unwrap_or_else(|e| e.into_inner());
                                states[position].1 = event["result"].clone();
                                combine(&states)
                            };
                            let _ = outer.send(Event::new(
                                "tool_output",
                                json!({"id": tool_id, "result": combined}),
                            ));
                        }
                    }
                }
                Ok(())
            });
            let states = states.clone();
            let context = context(host, progress);
            let (id, name, args) = (id.to_string(), name.to_string(), args.clone());
            scope.spawn(move || {
                let result = tools::execute(context, id, name, args).unwrap_or_else(|reason| {
                    json!({"status": "done", "error": reason, "exitCode": null, "stdout": "", "stderr": ""})
                });
                let mut states = states.lock().unwrap_or_else(|e| e.into_inner());
                states[position].1 = result;
            });
        }
    });
    let states = states.lock().unwrap_or_else(|e| e.into_inner());
    Ok(combine(&states))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(id: &str, name: &str, host: &str) -> ServerRecord {
        ServerRecord {
            id: id.into(),
            name: name.into(),
            host: host.into(),
            port: 22,
            username: "ops".into(),
            group: String::new(),
            tags: vec![],
            auth_type: "key".into(),
            key_path: None,
            color: String::new(),
            notes: String::new(),
            jump_server_id: None,
            encoding: "UTF-8".into(),
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
            last_connected_at: None,
        }
    }

    fn hosts() -> Vec<Host> {
        let servers = [
            server("a", "web", "10.0.0.1"),
            server("b", "db", "10.0.0.2"),
        ];
        labels(&servers)
            .into_iter()
            .zip(servers)
            .map(|(label, server)| Host {
                label,
                session: format!("s-{}", server.id),
                server,
            })
            .collect()
    }

    #[test]
    fn labels_are_unique_and_readable() {
        let servers = [
            server("a", "web", "10.0.0.1"),
            server("b", "web", "10.0.0.2"),
            server("c", "web", "10.0.0.1"),
            server("d", " ", "10.0.0.9"),
        ];
        assert_eq!(
            labels(&servers),
            [
                "web",
                "web (ops@10.0.0.2)",
                "web (ops@10.0.0.1)",
                "10.0.0.9"
            ]
        );
        let same = [
            server("a", "x", "h"),
            server("b", "x", "h"),
            server("c", "x", "h"),
        ];
        assert_eq!(labels(&same), ["x", "x (ops@h)", "x (ops@h) #2"]);
    }

    #[test]
    fn the_servers_argument_appears_only_with_a_choice() {
        let one = definitions(&["web".into()]);
        assert!(one[0]["function"]["parameters"]["properties"]
            .get("servers")
            .is_none());
        let two = definitions(&["web".into(), "db".into()]);
        for tool in two.as_array().unwrap() {
            assert_eq!(
                tool["function"]["parameters"]["properties"]["servers"]["items"]["enum"],
                json!(["web", "db"])
            );
        }
    }

    #[test]
    fn calls_choose_servers_by_name() {
        let hosts = hosts();
        let mut args = json!({"command": "uptime"});
        let chosen = select(&hosts, &mut args).unwrap();
        assert_eq!(
            chosen.iter().map(|h| h.label.as_str()).collect::<Vec<_>>(),
            ["web"]
        );

        let mut args = json!({"command": "uptime", "servers": ["db", "web", "db"]});
        let chosen = select(&hosts, &mut args).unwrap();
        assert_eq!(
            chosen.iter().map(|h| h.label.as_str()).collect::<Vec<_>>(),
            ["db", "web"]
        );
        assert_eq!(
            args,
            json!({"command": "uptime"}),
            "servers is not passed on"
        );

        for bad in [json!([]), json!("web"), json!(["cache"]), json!([1])] {
            let mut args = json!({"command": "x", "servers": bad});
            assert!(select(&hosts, &mut args).is_err());
        }
    }

    #[test]
    fn fanned_out_results_are_combined_per_server() {
        let running = combine(&[
            (
                "web".into(),
                json!({"status": "done", "exitCode": 0, "error": null, "stdout": "ok"}),
            ),
            ("db".into(), json!({"status": "running"})),
        ]);
        assert_eq!(running["status"], "running");
        assert!(running["exitCode"].is_null() && running["error"].is_null());

        let done = combine(&[
            (
                "web".into(),
                json!({"status": "done", "exitCode": 0, "error": null, "stdout": "x".repeat(10_000)}),
            ),
            (
                "db".into(),
                json!({"status": "done", "exitCode": 3, "error": null, "stderr": "boom"}),
            ),
            (
                "cache".into(),
                json!({"status": "done", "exitCode": null, "error": "已停止"}),
            ),
        ]);
        assert_eq!(done["status"], "done");
        assert_eq!(done["exitCode"], 1);
        assert_eq!(done["error"], "2 台服务器失败：db、cache");
        let results = done["results"].as_array().unwrap();
        assert_eq!(results[0]["server"], "web");
        assert_eq!(
            results[0]["stdout"].as_str().unwrap().len(),
            FANOUT_OUTPUT_CHARS
        );
        assert_eq!(results[0]["truncated"], true);
        assert_eq!(results[1]["stderr"], "boom");
        assert_eq!(results[2]["error"], "已停止");
    }
}
