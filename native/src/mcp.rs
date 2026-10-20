//! 上游 `lightcraft-mcp` 与宿主 MCP 契约之间的薄适配。
//!
//! 上游已经公开 `Server`，所以这里让专用线程持有唯一 `Server`，宿主调用经 channel
//! 串行进入：`command_run → doc_inspect` 等序列共享同一个 headless Session。
//! 工具表、参数校验、注解和错误格式都复用上游实现。
//!
//! 上游 `tools/list` 默认带每个命令一个 `cmd_*` 工具（本版本共 388 个）。这里保留
//! 完整工具面，由宿主的白名单控制默认暴露；不按名字裁剪。

use lightcraft_mcp::{Headless, Server};
use serde_json::{json, Value};

enum WorkerRequest {
    Mcp {
        method: String,
        params: Value,
        reply: std::sync::mpsc::Sender<Result<Value, String>>,
    },
    Reset {
        reply: std::sync::mpsc::Sender<Result<Value, String>>,
    },
}

/// `Server` 拥有 `Box<dyn Backend>`，没有 `Send` bound；固定在专用线程里，
/// 避免 unsafe，并让所有插件调用天然串行。
static WORKER: std::sync::OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<WorkerRequest>>> =
    std::sync::OnceLock::new();

fn worker() -> &'static std::sync::Mutex<std::sync::mpsc::Sender<WorkerRequest>> {
    WORKER.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("lightcraft-mcp".to_string())
            .spawn(move || worker_loop(receiver))
            .expect("failed to start LightCraft MCP worker");
        std::sync::Mutex::new(sender)
    })
}

fn worker_loop(receiver: std::sync::mpsc::Receiver<WorkerRequest>) {
    let mut server = Server::new(Box::new(Headless::default()));
    while let Ok(message) = receiver.recv() {
        match message {
            WorkerRequest::Mcp {
                method,
                params,
                reply,
            } => {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    request(&mut server, &method, params)
                }));
                let result = match result {
                    Ok(result) => result,
                    Err(panic) => {
                        // A panic can leave the headless session half-mutated. A normal
                        // tool error is part of the session, so only rebuild on panic.
                        server = Server::new(Box::new(Headless::default()));
                        Err(format!("internal error: {}", panic_message(panic)))
                    }
                };
                let _ = reply.send(result);
            }
            WorkerRequest::Reset { reply } => {
                server = Server::new(Box::new(Headless::default()));
                let _ = reply.send(Ok(json!({ "ok": true })));
            }
        }
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

fn invoke_worker(method: &str, params: Value) -> Result<Value, String> {
    let sender = worker()
        .lock()
        .map_err(|_| "MCP worker channel is poisoned".to_string())?;
    let (reply, response) = std::sync::mpsc::channel();
    sender
        .send(WorkerRequest::Mcp {
            method: method.to_string(),
            params,
            reply,
        })
        .map_err(|_| "MCP worker is not running".to_string())?;
    response
        .recv()
        .map_err(|_| "MCP worker stopped before replying".to_string())?
}

fn request(server: &mut Server, method: &str, params: Value) -> Result<Value, String> {
    let line = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    })
    .to_string();
    let reply = server
        .handle_line(&line)
        .ok_or_else(|| format!("MCP method `{method}` did not return a response"))?;
    let mut value: Value = serde_json::from_str(&reply)
        .map_err(|error| format!("MCP response is not JSON: {error}"))?;
    if let Some(error) = value.get_mut("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("upstream MCP error")
            .to_string();
        return Err(message);
    }
    Ok(value
        .get_mut("result")
        .map(Value::take)
        .unwrap_or(Value::Null))
}

pub fn tools_payload() -> Value {
    invoke_worker("tools/list", json!({})).unwrap_or_else(|error| {
        json!({
            "tools": [],
            "error": error,
        })
    })
}

pub fn call_payload(name: &str, arguments: &Value) -> Value {
    match invoke_worker(
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    ) {
        Ok(value) => value,
        Err(error) => json!({
            "content": [{ "type": "text", "text": error }],
            "isError": true,
        }),
    }
}

pub fn shutdown_headless() {
    let Ok(sender) = worker().lock() else {
        return;
    };
    let (reply, response) = std::sync::mpsc::channel();
    if sender.send(WorkerRequest::Reset { reply }).is_ok() {
        let _ = response.recv();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn tools_payload_preserves_full_upstream_surface() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let value = tools_payload();
        let tools = value["tools"].as_array().expect("tools array");
        assert!(tools.len() > 300, "got {}", tools.len());
        assert!(tools.iter().any(|t| t["name"] == "command_list"));
        assert!(tools.iter().any(|t| t["name"] == "cmd_library_source"));
    }

    #[test]
    fn upstream_annotations_are_preserved() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let value = tools_payload();
        let tools = value["tools"].as_array().expect("tools array");
        let find = |name: &str| {
            tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("{name} missing"))
        };
        assert_eq!(find("command_list")["annotations"]["readOnlyHint"], true);
        assert_eq!(find("command_run")["annotations"]["readOnlyHint"], false);
    }

    #[test]
    fn call_unknown_tool_returns_is_error() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let result = call_payload("definitely_not_a_tool", &json!({}));
        assert_eq!(result["isError"], true, "result: {result}");
    }

    #[test]
    fn call_read_only_tool_succeeds_headless() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let result = call_payload("command_list", &json!({}));
        assert_eq!(result["isError"], false, "result: {result}");
        assert!(result["content"].is_array(), "result: {result}");
    }

    #[test]
    fn session_state_persists_across_calls() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // A command changes undo state; doc_inspect must see it in the next call.
        let ran = call_payload(
            "command_run",
            &json!({"id":"library.source","params":{"source":"all"}}),
        );
        assert_ne!(ran["isError"], true, "command failed: {ran}");
        let inspected = call_payload("doc_inspect", &json!({}));
        assert_ne!(inspected["isError"], true, "inspect failed: {inspected}");
        let text = inspected["content"][0]["text"].as_str().unwrap_or_default();
        assert!(text.contains("library"), "inspect missing library: {text}");
    }

    #[test]
    fn tool_error_does_not_reset_session() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        shutdown_headless();
        let ran = call_payload(
            "command_run",
            &json!({"id":"library.source","params":{"source":"all"}}),
        );
        assert_ne!(ran["isError"], true, "command failed: {ran}");

        // Missing required arguments is an upstream tool error, not a backend panic.
        // The next call must still see the session mutated above.
        let bad = call_payload("command_run", &json!({}));
        assert_eq!(bad["isError"], true, "bad call should fail: {bad}");

        let inspected = call_payload("doc_inspect", &json!({}));
        assert_ne!(inspected["isError"], true, "inspect failed: {inspected}");
        let text = inspected["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            text.contains("library"),
            "tool error reset the headless session: {text}"
        );
    }

    #[test]
    fn reset_clears_session() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        shutdown_headless();
        let result = call_payload("doc_inspect", &json!({}));
        assert_ne!(result["isError"], true, "result: {result}");
    }
}
