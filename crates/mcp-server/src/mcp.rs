use std::sync::Arc;

use jdwp_client::StepDepth;
use logcat_stream::LogLevel;
use serde_json::{json, Value};

use crate::session::read_source_snippet;
use crate::SharedLensState;

pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Handles a single MCP JSON-RPC 2.0 request and returns an optional JSON-RPC response
/// (`None` for notifications that do not expect a reply).
pub async fn handle_jsonrpc(state: &Arc<SharedLensState>, req: Value) -> Option<Value> {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

    // Notifications (no `id`) do not receive a response per JSON-RPC 2.0.
    if id.is_none() || method.starts_with("notifications/") {
        return None;
    }
    let id = id.unwrap();

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": { "listChanged": false }
            },
            "serverInfo": {
                "name": "android-lens",
                "version": env!("CARGO_PKG_VERSION")
            }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call_tool(state, name, args).await {
                Ok(data) => Ok(tool_success(data)),
                Err(msg) => Ok(tool_error(&msg)),
            }
        }
        other => Err((-32601, format!("method not found: {other}"))),
    };

    Some(match result {
        Ok(res) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": res
        }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        }),
    })
}

fn tool_success(data: Value) -> Value {
    let text = serde_json::to_string_pretty(&data).unwrap_or_else(|_| data.to_string());
    json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "structuredContent": data,
        "isError": false
    })
}

fn tool_error(msg: &str) -> Value {
    json!({
        "content": [
            { "type": "text", "text": msg }
        ],
        "isError": true
    })
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "debug_attach",
            "description": "Attach Android Lens JDWP debugger to a debuggable Android app (or fresh-launch with am start -D) and start package-scoped logcat.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "package": {
                        "type": "string",
                        "description": "Android application package name (e.g. com.everybit.sholattv.native.dev)"
                    },
                    "serial": {
                        "type": "string",
                        "description": "Optional ADB device serial; defaults to the first online device/emulator"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["auto", "attach", "launch"],
                        "description": "auto (attach if running, else launch -D), attach, or launch"
                    },
                    "activity": {
                        "type": "string",
                        "description": "Optional activity name when mode is launch"
                    },
                    "port": {
                        "type": "integer",
                        "description": "Optional local TCP forward port (default 5005)"
                    }
                },
                "required": ["package"]
            }
        }),
        json!({
            "name": "debug_set_breakpoint",
            "description": "Set or remove a source-line breakpoint (e.g. MainActivity.kt:27). Automatically registers a pending ClassPrepare breakpoint if the class is not loaded yet.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "Source file name (e.g. BoardViewModel.kt)"
                    },
                    "line": {
                        "type": "integer",
                        "description": "1-based source line number"
                    },
                    "action": {
                        "type": "string",
                        "enum": ["set", "remove"],
                        "description": "Whether to set (default) or remove the breakpoint"
                    }
                },
                "required": ["file", "line"]
            }
        }),
        json!({
            "name": "debug_step",
            "description": "Control execution of the attached Android app: step over, step into, step out, resume, or pause.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["over", "into", "out", "resume", "pause"],
                        "description": "Debugger execution action"
                    }
                },
                "required": ["action"]
            }
        }),
        json!({
            "name": "debug_get_state",
            "description": "Get live debug session state: VM status, paused thread, call stack, source snippet around the active line, local variables, and breakpoints.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "frame_index": {
                        "type": "integer",
                        "description": "Stack frame index to inspect locals for (default 0)"
                    },
                    "context_lines": {
                        "type": "integer",
                        "description": "Lines of source code above and below the active line (default 8)"
                    }
                }
            }
        }),
        json!({
            "name": "debug_inspect_object",
            "description": "Inspect fields (including superclass fields) or array elements of a live JVM/ART object reference by its hex object_id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "object_id": {
                        "type": "string",
                        "description": "Hex object ID (e.g. 0x72b6) from debug_get_state locals"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Recursive expansion depth (1..=3, default 1)"
                    },
                    "max_array_elements": {
                        "type": "integer",
                        "description": "Max array elements to read (default 50)"
                    }
                },
                "required": ["object_id"]
            }
        }),
        json!({
            "name": "logcat_tail",
            "description": "Tail recent package-scoped Android logcat entries and grouped fatal crashes from the live ring buffer.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "package": {
                        "type": "string",
                        "description": "Android package name (defaults to the active debug/logcat package)"
                    },
                    "level": {
                        "type": "string",
                        "enum": ["verbose", "debug", "info", "warn", "error", "fatal"],
                        "description": "Minimum log level filter"
                    },
                    "tag": {
                        "type": "string",
                        "description": "Optional substring filter on log tag"
                    },
                    "query": {
                        "type": "string",
                        "description": "Optional substring filter on message or tag"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum log entries to return (default 80, max 500)"
                    }
                }
            }
        }),
        json!({
            "name": "get_pinned_context",
            "description": "Return stack frames, variables, logcat entries, and crashes that the developer explicitly pinned to the AI agent from the Android Lens UI.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clear": {
                        "type": "boolean",
                        "description": "If true, clear pinned items after reading"
                    }
                }
            }
        }),
    ]
}

async fn call_tool(state: &Arc<SharedLensState>, name: &str, args: Value) -> Result<Value, String> {
    match name {
        "debug_attach" => {
            let package = args
                .get("package")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing required argument 'package'".to_string())?
                .to_string();
            let serial = args
                .get("serial")
                .and_then(Value::as_str)
                .map(str::to_string);
            let mode = args.get("mode").and_then(Value::as_str).unwrap_or("auto");
            let activity = args
                .get("activity")
                .and_then(Value::as_str)
                .map(str::to_string);
            let port = args
                .get("port")
                .and_then(Value::as_u64)
                .map(|p| p as u16)
                .unwrap_or(5005);

            let status = state
                .debug
                .attach(serial.clone(), package.clone(), mode, activity, port)
                .await?;
            let _ = state.start_logcat(serial, package).await;
            Ok(json!({ "status": status }))
        }
        "debug_set_breakpoint" => {
            let file = args
                .get("file")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing required argument 'file'".to_string())?
                .to_string();
            let line = args
                .get("line")
                .and_then(Value::as_i64)
                .ok_or_else(|| "missing required argument 'line'".to_string())?
                as i32;
            let action = args.get("action").and_then(Value::as_str).unwrap_or("set");

            if action == "remove" {
                let bps = state
                    .debug
                    .remove_breakpoint_by_location(&file, line)
                    .await?;
                Ok(json!({ "removed": format!("{file}:{line}"), "breakpoints": bps }))
            } else {
                let bp = state.debug.set_breakpoint(file, line).await?;
                Ok(json!({ "breakpoint": bp }))
            }
        }
        "debug_step" => {
            let action = args
                .get("action")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing required argument 'action'".to_string())?;
            match action {
                "over" => {
                    state.debug.step(StepDepth::Over).await?;
                    Ok(json!({ "action": "over", "status": state.debug.status().await }))
                }
                "into" => {
                    state.debug.step(StepDepth::Into).await?;
                    Ok(json!({ "action": "into", "status": state.debug.status().await }))
                }
                "out" => {
                    state.debug.step(StepDepth::Out).await?;
                    Ok(json!({ "action": "out", "status": state.debug.status().await }))
                }
                "resume" => {
                    let status = state.debug.resume().await?;
                    Ok(json!({ "action": "resume", "status": status }))
                }
                "pause" => {
                    let status = state.debug.pause().await?;
                    Ok(json!({ "action": "pause", "status": status }))
                }
                other => Err(format!("invalid step action '{other}'")),
            }
        }
        "debug_get_state" => {
            let frame_index = args.get("frame_index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let context_lines = args
                .get("context_lines")
                .and_then(Value::as_i64)
                .unwrap_or(8) as i32;

            let status = state.debug.status().await;
            let breakpoints = state.debug.list_breakpoints().await;

            if status.state != "paused" {
                return Ok(json!({
                    "status": status,
                    "breakpoints": breakpoints,
                    "call_stack": [],
                    "locals": [],
                    "source_snippet": null
                }));
            }

            let call_stack = state.debug.get_call_stack(None).await.unwrap_or_default();
            let locals = state
                .debug
                .get_frame_locals(None, frame_index)
                .await
                .unwrap_or_default();

            let snippet = state.project_root().and_then(|root| {
                let target_frame = call_stack.get(frame_index)?;
                let file = target_frame.source_file.as_deref()?;
                let line = target_frame.line_number?;
                read_source_snippet(
                    &root,
                    file,
                    Some(&target_frame.class_name),
                    line,
                    context_lines,
                )
            });

            Ok(json!({
                "status": status,
                "frame_index": frame_index,
                "call_stack": call_stack,
                "locals": locals,
                "source_snippet": snippet,
                "breakpoints": breakpoints
            }))
        }
        "debug_inspect_object" => {
            let object_id = args
                .get("object_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing required argument 'object_id'".to_string())?;
            let depth = args.get("depth").and_then(Value::as_u64).unwrap_or(1) as usize;
            let max_array = args
                .get("max_array_elements")
                .and_then(Value::as_u64)
                .unwrap_or(50) as usize;

            let fields = state
                .debug
                .inspect_object_depth(object_id, depth, max_array)
                .await?;
            Ok(json!({
                "object_id": object_id,
                "depth": depth.clamp(1, 3),
                "fields": fields
            }))
        }
        "logcat_tail" => {
            let package = args
                .get("package")
                .and_then(Value::as_str)
                .map(str::to_string);
            let level = args
                .get("level")
                .and_then(Value::as_str)
                .and_then(parse_level);
            let tag = args.get("tag").and_then(Value::as_str).map(str::to_string);
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .map(str::to_string);
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(80) as usize;

            let (pkg, entries, crashes) =
                state.tail_logcat(package, level, tag, query, limit).await?;
            Ok(json!({
                "package": pkg,
                "count": entries.len(),
                "entries": entries,
                "crashes": crashes
            }))
        }
        "get_pinned_context" => {
            let clear = args.get("clear").and_then(Value::as_bool).unwrap_or(false);
            let items = state.pins.list();
            if clear {
                state.pins.clear();
            }
            Ok(json!({
                "count": items.len(),
                "items": items
            }))
        }
        other => Err(format!("unknown tool '{other}'")),
    }
}

fn parse_level(s: &str) -> Option<LogLevel> {
    match s.to_ascii_lowercase().as_str() {
        "v" | "verbose" => Some(LogLevel::Verbose),
        "d" | "debug" => Some(LogLevel::Debug),
        "i" | "info" => Some(LogLevel::Info),
        "w" | "warn" | "warning" => Some(LogLevel::Warn),
        "e" | "error" => Some(LogLevel::Error),
        "f" | "fatal" => Some(LogLevel::Fatal),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pins::{NewPinRequest, PinKind};

    #[tokio::test]
    async fn mcp_initialize_and_tools_list() {
        let state = SharedLensState::shared();
        let init = handle_jsonrpc(
            &state,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "android-lens");

        let list = handle_jsonrpc(
            &state,
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
        )
        .await
        .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"debug_attach"));
        assert!(names.contains(&"debug_set_breakpoint"));
        assert!(names.contains(&"debug_step"));
        assert!(names.contains(&"debug_get_state"));
        assert!(names.contains(&"debug_inspect_object"));
        assert!(names.contains(&"logcat_tail"));
        assert!(names.contains(&"get_pinned_context"));
    }

    #[tokio::test]
    async fn mcp_pinned_context_and_queued_breakpoint_round_trip() {
        let state = SharedLensState::shared();
        state.pins.add(NewPinRequest {
            kind: PinKind::Variable,
            title: "settings : TvSettings".into(),
            summary: "selectedTheme = \"Theme 1\"".into(),
            note: Some("Check theme mismatch".into()),
            payload: json!({ "object_id": "0x72b6" }),
        });

        let pinned = handle_jsonrpc(
            &state,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": { "name": "get_pinned_context", "arguments": { "clear": false } }
            }),
        )
        .await
        .unwrap();
        assert_eq!(pinned["result"]["structuredContent"]["count"], 1);
        assert_eq!(
            pinned["result"]["structuredContent"]["items"][0]["title"],
            "settings : TvSettings"
        );

        let bp = handle_jsonrpc(
            &state,
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "debug_set_breakpoint",
                    "arguments": { "file": "MainActivity.kt", "line": 27 }
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            bp["result"]["structuredContent"]["breakpoint"]["status"],
            "pending"
        );

        let dbg_state = handle_jsonrpc(
            &state,
            json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": { "name": "debug_get_state", "arguments": {} }
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            dbg_state["result"]["structuredContent"]["status"]["state"],
            "disconnected"
        );
        assert_eq!(
            dbg_state["result"]["structuredContent"]["breakpoints"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
