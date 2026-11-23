use clap::Parser;
use mcp_server::http::proxy_jsonrpc_http;
use mcp_server::mcp::handle_jsonrpc;
use mcp_server::{SharedLensState, DEFAULT_MCP_PORT};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Parser)]
#[command(
    name = "android-lens-mcp",
    about = "Android Lens MCP server (bridges to live Desktop UI or runs standalone over stdio)"
)]
struct Args {
    /// Local HTTP MCP port exposed by a running Android Lens Desktop instance
    #[arg(long, default_value_t = DEFAULT_MCP_PORT)]
    desktop_port: u16,

    /// Always run standalone (do not proxy to a running Desktop UI instance)
    #[arg(long)]
    standalone: bool,

    /// Optional Android project folder for source snippets in standalone mode
    #[arg(long)]
    project: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let local_state = SharedLensState::shared();
    if let Some(proj) = args.project {
        local_state.set_project_root(Some(proj));
    }

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let err = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") }
                });
                let out = serde_json::to_string(&err)?;
                stdout.write_all(out.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
                continue;
            }
        };

        let response = if !args.standalone {
            match proxy_jsonrpc_http(args.desktop_port, &req).await {
                Ok(resp) => resp,
                Err(_) => handle_jsonrpc(&local_state, req).await,
            }
        } else {
            handle_jsonrpc(&local_state, req).await
        };

        if let Some(resp) = response {
            let out = serde_json::to_string(&resp)?;
            stdout.write_all(out.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}
