use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::mcp::handle_jsonrpc;
use crate::SharedLensState;

const MAX_HTTP_BODY: usize = 2 * 1024 * 1024;

/// Spawns a lightweight localhost HTTP/1.1 MCP Streamable-HTTP server (`POST /mcp`, `GET /health`)
/// bound to `127.0.0.1:<port>` sharing the live `SharedLensState` with the desktop UI.
pub async fn spawn_http_mcp_server(
    state: Arc<SharedLensState>,
    port: u16,
) -> std::io::Result<(u16, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let bound_port = listener.local_addr()?.port();

    let handle = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let st = Arc::clone(&state);
            tokio::spawn(async move {
                let _ = handle_connection(stream, st).await;
            });
        }
    });

    Ok((bound_port, handle))
}

async fn handle_connection(
    mut stream: TcpStream,
    state: Arc<SharedLensState>,
) -> std::io::Result<()> {
    let Some((method, path, body)) = read_http_request(&mut stream).await? else {
        return Ok(());
    };

    if method == "OPTIONS" {
        return write_http_response(&mut stream, 204, "No Content", &[]).await;
    }

    if method == "GET" && path == "/health" {
        let status = state.debug.status().await;
        let payload = serde_json::to_vec(&json!({
            "ok": true,
            "service": "android-lens-mcp",
            "state": status.state,
            "package": status.package,
            "pins": state.pins.list().len(),
        }))
        .unwrap_or_default();
        return write_http_response(&mut stream, 200, "OK", &payload).await;
    }

    if method == "POST" && (path == "/mcp" || path == "/") {
        let req: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                let err = serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("invalid JSON: {e}") }
                }))
                .unwrap_or_default();
                return write_http_response(&mut stream, 400, "Bad Request", &err).await;
            }
        };

        match handle_jsonrpc(&state, req).await {
            Some(resp) => {
                let bytes = serde_json::to_vec(&resp).unwrap_or_default();
                write_http_response(&mut stream, 200, "OK", &bytes).await
            }
            None => write_http_response(&mut stream, 202, "Accepted", &[]).await,
        }
    } else {
        write_http_response(&mut stream, 404, "Not Found", b"{\"error\":\"not found\"}").await
    }
}

/// Forwards a single JSON-RPC request to a running local Android Lens HTTP MCP server.
/// Returns `Ok(None)` when the request was a notification (`202 Accepted`), or `Err` if unreachable.
pub async fn proxy_jsonrpc_http(port: u16, req: &Value) -> Result<Option<Value>, String> {
    let body = serde_json::to_vec(req).map_err(|e| e.to_string())?;
    let mut stream = tokio::time::timeout(
        Duration::from_millis(500),
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .map_err(|_| "connect timeout".to_string())?
    .map_err(|e| e.to_string())?;

    let header = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(header.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(&body).await.map_err(|e| e.to_string())?;
    stream.flush().await.map_err(|e| e.to_string())?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .map_err(|e| e.to_string())?;

    let Some(split) = find_header_end(&raw) else {
        return Err("malformed HTTP response from desktop MCP server".into());
    };
    let head = String::from_utf8_lossy(&raw[..split]);
    let status_code = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(500);

    if status_code == 202 || status_code == 204 {
        return Ok(None);
    }

    let resp_body = &raw[split + 4..];
    if resp_body.is_empty() {
        return Ok(None);
    }
    let val: Value = serde_json::from_slice(resp_body).map_err(|e| e.to_string())?;
    Ok(Some(val))
}

async fn read_http_request(
    stream: &mut TcpStream,
) -> std::io::Result<Option<(String, String, Vec<u8>)>> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];

    let header_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        if buf.len() > 64 * 1024 {
            return Ok(None);
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = head.lines();
    let Some(req_line) = lines.next() else {
        return Ok(None);
    };
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts
        .next()
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();

    let mut content_len: usize = 0;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_len = v.trim().parse().unwrap_or(0);
            }
        }
    }
    if content_len > MAX_HTTP_BODY {
        return Ok(None);
    }

    let body_start = header_end + 4;
    let mut body = buf[body_start..].to_vec();
    while body.len() < content_len {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_len);

    Ok(Some((method, path, body)))
}

async fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    if !body.is_empty() {
        stream.write_all(body).await?;
    }
    stream.flush().await
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn http_mcp_server_and_proxy_round_trip() {
        let state = SharedLensState::shared();
        let (port, handle) = spawn_http_mcp_server(Arc::clone(&state), 0).await.unwrap();

        let resp = proxy_jsonrpc_http(
            port,
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(resp["id"], 1);
        assert!(resp["result"]["tools"].as_array().unwrap().len() >= 7);

        let notif = proxy_jsonrpc_http(
            port,
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        )
        .await
        .unwrap();
        assert!(notif.is_none());

        handle.abort();
    }
}
