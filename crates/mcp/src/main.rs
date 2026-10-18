//! MCP server for idm-alternative. An AI assistant's MCP client starts it and talks to it
//! over stdin/stdout; it passes each tool call to the app over the same named pipe the
//! browser extension's host uses, starting the app in the tray when it is not running.
//! Nothing listens on a network port.
//!
//! Register it with the client as a stdio server whose command is
//! `%LOCALAPPDATA%\idm-alternative\idma-mcp.exe`.

use std::path::PathBuf;
use std::sync::Arc;

use idma_bridge::{AddDownload, Client, Reply, Request};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use tokio::sync::Mutex;

#[derive(Clone)]
struct Downloads {
    app: Arc<Mutex<Client>>,
    /// The app's data folder, for calls that get a connection of their own.
    data: PathBuf,
    #[allow(dead_code, reason = "used by the tool_handler macro")]
    tool_router: ToolRouter<Self>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AddParams {
    /// What to download: a direct link to a file (http or https), a magnet link or
    /// `.torrent` file, a `.meta4` metalink, or, with `video_height` or `audio_only`, a page of
    /// a video site (YouTube, X, TikTok, ...; see inspect_video).
    url: String,
    /// Folder to save into. The app's download folder when omitted.
    dir: Option<String>,
    /// File name. The server's suggestion (for a video, its title) when omitted.
    name: Option<String>,
    /// Start now (default) or add it paused.
    start: Option<bool>,
    /// Other direct links to the same file (mirrors), downloaded from at the same time once
    /// each is shown to serve the same bytes.
    mirrors: Option<Vec<String>>,
    /// `sha256:<hex>` (also sha512, sha1, md5) the finished file must match.
    checksum: Option<String>,
    /// A video page: the highest video height wanted, e.g. 720 (one of inspect_video's).
    video_height: Option<u32>,
    /// A video page: download its audio alone.
    audio_only: Option<bool>,
    /// A torrent: indices of the files to download, from inspect_torrent. All when omitted.
    torrent_files: Option<Vec<usize>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct UrlParams {
    url: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct GrabParams {
    /// The page to start from.
    url: String,
    /// 1 (default): the files this page links to; 2: also those of the pages it links to;
    /// up to 5.
    depth: Option<u32>,
    /// Follow only pages on the same site (default true). Files are listed from anywhere.
    same_site: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct IdsParams {
    /// Download ids, as `list_downloads` shows them.
    ids: Vec<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct WaitParams {
    /// Download ids, as `list_downloads` shows them.
    ids: Vec<i64>,
    /// Seconds to wait at most, 1 to 600; 60 when omitted.
    timeout_secs: Option<u64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RemoveParams {
    /// Download ids, as `list_downloads` shows them.
    ids: Vec<i64>,
    /// Also delete the downloaded (or partly downloaded) files. Default false.
    delete_files: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct LimitParams {
    /// Bytes per second; 0 or omitted removes the limit.
    bytes_per_second: Option<u64>,
    /// One download's id; omitted for all downloads together.
    id: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct MoveParams {
    /// The download's id.
    id: i64,
    /// `top`, `up`, `down` or `bottom` of the queue.
    to: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct WhenDoneParams {
    /// `nothing` (cancel), `exit` (close the app), `sleep`, `hibernate` or `shutdown`.
    action: String,
}

#[tool_router]
impl Downloads {
    fn new(data: PathBuf) -> Self {
        Self {
            app: Arc::new(Mutex::new(Client::new(data.clone()))),
            data,
            tool_router: Self::tool_router(),
        }
    }

    async fn ask(&self, request: Request) -> Result<CallToolResult, ErrorData> {
        let reply: Result<Reply, String> = self.app.lock().await.request(&request).await;
        answer(reply)
    }

    #[tool(
        description = "List the downloads in idm-alternative with id, url, file name, path, status (queued, downloading, paused, done, error), error, size and received bytes, speed in bytes per second, place in the queue (queue_pos, lower starts first) and own speed limit."
    )]
    async fn list_downloads(&self) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::Tasks).await
    }

    #[tool(
        description = "Add a download to idm-alternative and start it (unless start is false): a direct file link, a magnet link or .torrent file (optionally only some of its files), a .meta4 metalink, or a page of a video site with video_height or audio_only. Returns the new download with its id."
    )]
    async fn add_download(
        &self,
        Parameters(p): Parameters<AddParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::Add(AddDownload {
            url: p.url,
            dir: p.dir,
            name: p.name,
            start: p.start,
            mirrors: p.mirrors.unwrap_or_default(),
            checksum: p.checksum,
            video_height: p.video_height,
            audio_only: p.audio_only,
            torrent_files: p.torrent_files,
        }))
        .await
    }

    #[tool(
        description = "What a page of a video site (YouTube, X, TikTok, Instagram, Twitch, ...) offers, read by yt-dlp: title, duration in seconds, the video heights available and whether audio alone is. Use one of the heights as add_download's video_height."
    )]
    async fn inspect_video(
        &self,
        Parameters(p): Parameters<UrlParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::InspectVideo { url: p.url }).await
    }

    #[tool(
        description = "A torrent's name, size and files (a magnet link asks peers for them, which can take a minute). Pass the indices of the wanted files as add_download's torrent_files."
    )]
    async fn inspect_torrent(
        &self,
        Parameters(p): Parameters<UrlParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::InspectTorrent { url: p.url }).await
    }

    #[tool(
        description = "Find the files a web page links to (archives, documents, video, pictures...) and, with depth above 1, the pages below it, without downloading them. Returns url, link text and the page each was found on; add the wanted ones with add_download."
    )]
    async fn find_site_files(
        &self,
        Parameters(p): Parameters<GrabParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::Grab {
            url: p.url,
            depth: p.depth,
            same_site: p.same_site,
        })
        .await
    }

    #[tool(
        description = "Wait until none of these downloads is queued or downloading any more (each is done, failed or paused), or until timeout_secs pass. Returns finished (false on timeout) and the downloads; call again to keep waiting."
    )]
    async fn wait_for_downloads(
        &self,
        Parameters(p): Parameters<WaitParams>,
    ) -> Result<CallToolResult, ErrorData> {
        // A connection of its own, so other calls are not held up meanwhile.
        let mut client = Client::new(self.data.clone());
        let reply = client
            .request(&Request::Wait {
                ids: p.ids,
                timeout_secs: p.timeout_secs,
            })
            .await;
        answer(reply)
    }

    #[tool(description = "Start or resume downloads in idm-alternative.")]
    async fn resume_downloads(
        &self,
        Parameters(p): Parameters<IdsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::Resume { ids: p.ids }).await
    }

    #[tool(description = "Pause downloads in idm-alternative; they keep their progress.")]
    async fn pause_downloads(
        &self,
        Parameters(p): Parameters<IdsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::Pause { ids: p.ids }).await
    }

    #[tool(
        description = "Remove downloads from idm-alternative's list; with delete_files also delete their files."
    )]
    async fn remove_downloads(
        &self,
        Parameters(p): Parameters<RemoveParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::Remove {
            ids: p.ids,
            delete_files: p.delete_files.unwrap_or(false),
        })
        .await
    }

    #[tool(
        description = "Set a speed limit in bytes per second: of one download (id), or of all downloads together (no id). 0 or no value removes it."
    )]
    async fn set_speed_limit(
        &self,
        Parameters(p): Parameters<LimitParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::SpeedLimit {
            id: p.id,
            bytes_per_second: p.bytes_per_second,
        })
        .await
    }

    #[tool(description = "Move a download in the queue: to the top, up, down or to the bottom.")]
    async fn move_download(
        &self,
        Parameters(p): Parameters<MoveParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::Move { id: p.id, to: p.to }).await
    }

    #[tool(
        description = "Choose what the computer does once no download is running or waiting any more: nothing (cancels), exit (closes the app), sleep, hibernate or shutdown. Only on the user's request."
    )]
    async fn set_when_done(
        &self,
        Parameters(p): Parameters<WhenDoneParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::WhenDone { action: p.action }).await
    }
}

fn answer(reply: Result<Reply, String>) -> Result<CallToolResult, ErrorData> {
    Ok(match reply {
        Ok(r) if r.ok => CallToolResult::success(vec![match r.data {
            Some(data) => ContentBlock::json(data)?,
            None => ContentBlock::text("done"),
        }]),
        Ok(r) => CallToolResult::error(vec![ContentBlock::text(
            r.error.unwrap_or_else(|| "the app refused".into()),
        )]),
        Err(e) => CallToolResult::error(vec![ContentBlock::text(format!(
            "could not reach idm-alternative: {e}"
        ))]),
    })
}

#[tool_handler]
impl ServerHandler for Downloads {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "idm-alternative",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Controls the idm-alternative download manager on this computer: add downloads \
                 (files, torrents, metalinks, video pages), find the files a site links to, list \
                 downloads and their progress, wait for them to finish, pause, resume, reorder and \
                 remove them, limit their speed, and choose what happens when all are done.",
            )
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let Some(data) = idma_bridge::data_dir() else {
        eprintln!("idma-mcp: APPDATA is not set");
        return std::process::ExitCode::FAILURE;
    };
    let server = match Downloads::new(data).serve(rmcp::transport::stdio()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("idma-mcp: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if let Err(e) = server.waiting().await {
        eprintln!("idma-mcp: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
