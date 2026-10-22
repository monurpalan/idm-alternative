//! The MCP server as a client runs it: JSON-RPC lines on stdin and stdout, the app at the
//! other end of the pipe (here a stand-in served by the test).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use shardload_bridge::{Reply, Request};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

struct Profile(PathBuf);

impl Profile {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir()
            .join("shardload-mcp-tests")
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
    let token = shardload_bridge::new_token(dir).expect("token");
    let pipe = shardload_bridge::pipe_name(dir);
    tokio::spawn(async move {
        shardload_bridge::serve(&pipe, token, |req| async move {
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
        let mut child = Command::new(env!("CARGO_BIN_EXE_shardload-mcp"))
            .env("SHARDLOAD_HOME", dir)
            .env("SHARDLOAD_APP", "unused")
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
        assert_eq!(init["serverInfo"]["name"], "shardload");
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
            "find_site_files",
            "forget_site",
            "inspect_archive",
            "inspect_torrent",
            "inspect_video",
            "list_downloads",
            "list_sites",
            "move_download",
            "pause_downloads",
            "relocate_download",
            "remove_downloads",
            "rename_download",
            "resume_downloads",
            "set_speed_limit",
            "set_when_done",
            "wait_for_downloads"
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
        json!({
            "type": "add", "url": "https://x/b.zip", "dir": null, "name": "b.zip", "start": null,
            "mirrors": [], "checksum": null, "video_height": null, "audio_only": null,
            "torrent_files": null, "archive_entries": null, "allow_duplicate": null,
            "playlist_items": null, "clip_start": null, "clip_end": null, "exact_cut": null
        })
    );
    // Adding again what was downloaded before is on the caller's word.
    let (_, again) = mcp
        .tool(
            "add_download",
            json!({ "url": "https://x/b.zip", "allow_duplicate": true }),
        )
        .await;
    assert_eq!(again["allow_duplicate"], true);

    // A video page at 720p, and some files of a torrent.
    let (_, video) = mcp
        .tool(
            "add_download",
            json!({ "url": "https://video.example/watch?v=1", "video_height": 720 }),
        )
        .await;
    assert_eq!(video["video_height"], 720);
    let (_, torrent) = mcp
        .tool(
            "add_download",
            json!({ "url": "magnet:?xt=urn:btih:abc", "torrent_files": [0, 2] }),
        )
        .await;
    assert_eq!(torrent["torrent_files"], json!([0, 2]));
    let (_, zip) = mcp
        .tool(
            "add_download",
            json!({ "url": "https://x/a.zip", "archive_entries": ["docs/*.pdf"] }),
        )
        .await;
    assert_eq!(zip["archive_entries"], json!(["docs/*.pdf"]));

    // Each new tool sends its own request.
    for (tool, args, want) in [
        (
            "inspect_video",
            json!({ "url": "https://v/1" }),
            json!({ "type": "inspect_video", "url": "https://v/1", "whole_list": null }),
        ),
        (
            "inspect_video",
            json!({ "url": "https://v/1&list=2", "whole_list": true }),
            json!({ "type": "inspect_video", "url": "https://v/1&list=2", "whole_list": true }),
        ),
        (
            "add_download",
            json!({ "url": "https://www.youtube.com/@mit", "playlist_items": [1, 2, 3], "audio_only": true }),
            json!({
                "type": "add", "url": "https://www.youtube.com/@mit", "dir": null, "name": null,
                "start": null, "mirrors": [], "checksum": null, "video_height": null,
                "audio_only": true, "torrent_files": null, "archive_entries": null,
                "allow_duplicate": null, "playlist_items": [1, 2, 3], "clip_start": null, "clip_end": null,
                "exact_cut": null
            }),
        ),
        (
            "add_download",
            json!({ "url": "https://v/1", "video_height": 720, "clip_start": "1:05", "clip_end": "2:30" }),
            json!({
                "type": "add", "url": "https://v/1", "dir": null, "name": null,
                "start": null, "mirrors": [], "checksum": null, "video_height": 720,
                "audio_only": null, "torrent_files": null, "archive_entries": null,
                "allow_duplicate": null, "playlist_items": null, "clip_start": "1:05", "clip_end": "2:30",
                "exact_cut": null
            }),
        ),
        (
            "inspect_torrent",
            json!({ "url": "magnet:?x" }),
            json!({ "type": "inspect_torrent", "url": "magnet:?x" }),
        ),
        (
            "inspect_archive",
            json!({ "url": "https://x/a.zip" }),
            json!({ "type": "inspect_archive", "url": "https://x/a.zip" }),
        ),
        (
            "set_speed_limit",
            json!({ "bytes_per_second": 1024 }),
            json!({ "type": "speed_limit", "id": null, "bytes_per_second": 1024 }),
        ),
        (
            "move_download",
            json!({ "id": 7, "to": "top" }),
            json!({ "type": "move", "id": 7, "to": "top" }),
        ),
        (
            "rename_download",
            json!({ "id": 7, "name": "b.zip" }),
            json!({ "type": "rename", "id": 7, "name": "b.zip" }),
        ),
        (
            "relocate_download",
            json!({ "id": 7, "dir": "D:\\Downloads\\iso" }),
            json!({ "type": "relocate", "id": 7, "dir": "D:\\Downloads\\iso" }),
        ),
        ("list_sites", json!({}), json!({ "type": "sites" })),
        (
            "forget_site",
            json!({ "site": "example.com" }),
            json!({ "type": "forget_site", "site": "example.com" }),
        ),
        (
            "set_when_done",
            json!({ "action": "nothing" }),
            json!({ "type": "when_done", "action": "nothing" }),
        ),
        // Over a connection of its own.
        (
            "wait_for_downloads",
            json!({ "ids": [7], "timeout_secs": 5 }),
            json!({ "type": "wait", "ids": [7], "timeout_secs": 5 }),
        ),
    ] {
        let (failed, got) = mcp.tool(tool, args).await;
        assert!(!failed, "{tool}: {got}");
        assert_eq!(got, want, "{tool}");
    }

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
            .starts_with("could not reach Shardload"),
        "{error}"
    );
}
