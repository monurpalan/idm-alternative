//! The MCP server as a client runs it: JSON-RPC lines on stdin and stdout, the app at the
//! other end of the pipe (here a stand-in served by the test).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use idma_bridge::{Reply, Request};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

struct Profile(PathBuf);

impl Profile {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir()
            .join("idma-mcp-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create");
        Self(dir)
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Stands in for the app: one download in the list, and every request echoed back so the
/// test sees what arrived.
fn serve_app(dir: &Path) {
    let token = idma_bridge::new_token(dir).expect("token");
    let pipe = idma_bridge::pipe_name(dir);
    tokio::spawn(async move {
        idma_bridge::serve(&pipe, token, |req| async move {
            match req {
                Request::Tasks => Reply {
                    data: Some(json!([{ "id": 7, "url": "https://x/a.zip", "status": "paused" }])),
                    ..Reply::ok()
                },
                Request::Pause { ids } if ids == [404] => Reply::error("download 404: not found"),
                other => Reply {
                    data: Some(serde_json::to_value(&other).expect("json")),
                    ..Reply::ok()
                },
            }
        })
        .await
        .ok();
    });
}

struct Mcp {
    _child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

impl Mcp {
    async fn start(dir: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_idma-mcp"))
            .env("IDMA_HOME", dir)
            .env("IDMA_APP", "unused")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout")).lines();
        let mut mcp = Self {
            _child: child,
            stdin,
            stdout,
            next_id: 1,
        };
        let init = mcp
            .call(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "0" }
                }),
            )
            .await;
        assert_eq!(init["serverInfo"]["name"], "idm-alternative");
        mcp.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        mcp
    }

    async fn send(&mut self, msg: &Value) {
        let mut line = serde_json::to_vec(msg).expect("json");
        line.push(b'\n');
        self.stdin.write_all(&line).await.expect("write");
        self.stdin.flush().await.expect("flush");
    }

    /// Sends a request and returns its `result`.
    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        loop {
            let line = tokio::time::timeout(Duration::from_secs(30), self.stdout.next_line())
                .await
                .expect("server hung")
                .expect("read")
                .expect("server closed");
            let msg: Value = serde_json::from_str(&line).expect("json line");
            if msg["id"] == id {
                assert!(msg.get("error").is_none(), "{msg}");
                return msg["result"].clone();
            }
        }
    }

    /// Calls a tool and returns whether it failed and the JSON (or text) it gave back.
    async fn tool(&mut self, name: &str, arguments: Value) -> (bool, Value) {
        let r = self
            .call(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await;
        let failed = r["isError"].as_bool().unwrap_or(false);
        let text = r["content"][0]["text"].as_str().expect("text").to_owned();
        (
            failed,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_reach_the_app() {
    let p = Profile::new("tools");
    serve_app(&p.0);
    let mut mcp = Mcp::start(&p.0).await;

    let tools = mcp.call("tools/list", json!({})).await;
    let mut names: Vec<&str> = tools["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "add_download",
            "list_downloads",
            "pause_downloads",
            "remove_downloads",
            "resume_downloads"
        ]
    );

    let (failed, list) = mcp.tool("list_downloads", json!({})).await;
    assert!(!failed);
    assert_eq!(list[0]["id"], 7);

    let (failed, added) = mcp
        .tool(
            "add_download",
            json!({ "url": "https://x/b.zip", "name": "b.zip" }),
        )
        .await;
    assert!(!failed);
    assert_eq!(
        added,
        json!({ "type": "add", "url": "https://x/b.zip", "dir": null, "name": "b.zip", "start": null, "mirrors": [] })
    );

    let (_, removed) = mcp
        .tool(
            "remove_downloads",
            json!({ "ids": [7], "delete_files": true }),
        )
        .await;
    assert_eq!(
        removed,
        json!({ "type": "remove", "ids": [7], "delete_files": true })
    );

    // The app's refusal comes back as a failed tool call, not a protocol error.
    let (failed, error) = mcp.tool("pause_downloads", json!({ "ids": [404] })).await;
    assert!(failed);
    assert_eq!(error, "download 404: not found");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_app_is_reported() {
    let p = Profile::new("no-app");
    // No app and nothing to start: the tool fails with an explanation.
    let mut mcp = Mcp::start(&p.0).await;
    let (failed, error) = mcp.tool("list_downloads", json!({})).await;
    assert!(failed);
    assert!(
        error
            .as_str()
            .expect("text")
            .starts_with("could not reach idm-alternative"),
        "{error}"
    );
}
