//! MCP server for Shardload. An AI assistant's MCP client starts it and talks to it
//! over stdin/stdout; it passes each tool call to the app over the same named pipe the
//! browser extension's host uses, starting the app in the tray when it is not running.
//! Nothing listens on a network port.
//!
//! Register it with the client as a stdio server whose command is
//! `%LOCALAPPDATA%\Shardload\shardload-mcp.exe`.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use shardload_bridge::{AddDownload, Client, Reply, Request};
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
    /// Absolute path of a folder to save into: the app's download folder or a category's
    /// folder from its settings, or a folder inside one of them. The app's download folder
    /// when omitted.
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
    /// A ZIP archive: names of the files inside it to download (from inspect_archive), or
    /// globs (`*` within a folder, `**` across folders), or folder names. Only these are
    /// fetched and extracted, into a folder named after the archive; without this the
    /// archive itself is downloaded.
    archive_entries: Option<Vec<String>>,
    /// Add it even if it was downloaded before (same link, or a file of the same name and
    /// size) or is already on the list. Without it such a request fails and says what was
    /// found: its id, path, date and whether the file is still there.
    allow_duplicate: Option<bool>,
    /// A playlist or a channel page: the numbers (from 1) of the videos to download, in
    /// inspect_video's list. Each becomes a download of its own, in one group, saved into a
    /// folder named after the list (or after `name`); video_height (1080 when omitted) or
    /// audio_only applies to each. Videos downloaded before are skipped unless
    /// allow_duplicate.
    playlist_items: Option<Vec<usize>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct VideoParams {
    url: String,
    /// The page is one video of a playlist (`watch?v=...&list=...`): list the whole
    /// playlist instead.
    whole_list: Option<bool>,
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
struct RenameParams {
    /// The download's id.
    id: i64,
    /// The new file name, with its extension; characters Windows does not allow in names
    /// become `_`. A torrent's or an archive's folder takes this name.
    name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RelocateParams {
    /// The download's id.
    id: i64,
    /// Absolute path of the folder to move it into: the app's download folder or a
    /// category's folder from its settings, or a folder inside one of them.
    dir: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SiteParams {
    /// The site as list_sites shows it: a host name, with `:port` when not the default.
    site: String,
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
        description = "List the downloads in Shardload with id, url, file name, path, status (queued, downloading, paused, done, error), error, size and received bytes, speed in bytes per second, place in the queue (queue_pos, lower starts first) and own speed limit."
    )]
    async fn list_downloads(&self) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::Tasks).await
    }

    #[tool(
        description = "Add a download to Shardload and start it (unless start is false): a direct file link, some files out of a ZIP archive (archive_entries), a magnet link or .torrent file (optionally only some of its files), a .meta4 metalink, or a page of a video site with video_height or audio_only. Returns the new download with its id."
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
            archive_entries: p.archive_entries,
            allow_duplicate: p.allow_duplicate,
            playlist_items: p.playlist_items,
        }))
        .await
    }

    #[tool(
        description = "What a page of a video site (YouTube, X, TikTok, Instagram, Twitch, ...) offers, read by yt-dlp: title, duration in seconds, the video heights available and whether audio alone is. Use one of the heights as add_download's video_height. A playlist or a channel (YouTube /playlist?list=..., /@name) comes as `playlist` with its videos (url, title, duration, thumbnail; a channel's newest first, at most 1000): pass their numbers from 1 as add_download's playlist_items. `in_playlist` true means the video belongs to a playlist too; whole_list lists it."
    )]
    async fn inspect_video(
        &self,
        Parameters(p): Parameters<VideoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::InspectVideo {
            url: p.url,
            whole_list: p.whole_list,
        })
        .await
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
        description = "List the files in a ZIP archive on the web without downloading it: name, size, packed size, compression method, whether it is a folder, and why a file cannot be extracted (encrypted, or a method other than stored and deflate). Needs a server that supports range requests; otherwise it fails and only the whole archive can be downloaded. Pass the wanted names as add_download's archive_entries."
    )]
    async fn inspect_archive(
        &self,
        Parameters(p): Parameters<UrlParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::InspectArchive { url: p.url }).await
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

    #[tool(description = "Start or resume downloads in Shardload.")]
    async fn resume_downloads(
        &self,
        Parameters(p): Parameters<IdsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::Resume { ids: p.ids }).await
    }

    #[tool(description = "Pause downloads in Shardload; they keep their progress.")]
    async fn pause_downloads(
        &self,
        Parameters(p): Parameters<IdsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::Pause { ids: p.ids }).await
    }

    #[tool(
        description = "Remove downloads from Shardload's list; with delete_files also delete their files."
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
        description = "Rename a download, at any stage: a running one pauses for a moment and continues, a finished file is renamed on disk. If another file already has the name, a number is added (name (1).ext). Returns the download with its new name and path."
    )]
    async fn rename_download(
        &self,
        Parameters(p): Parameters<RenameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::Rename {
            id: p.id,
            name: p.name,
        })
        .await
    }

    #[tool(
        description = "Move a download into another folder, at any stage, with its files: a running one pauses for a moment and continues, a finished file (or a torrent's folder) is moved, copied and checked first when it goes to another drive. Returns the download with its new path."
    )]
    async fn relocate_download(
        &self,
        Parameters(p): Parameters<RelocateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::Relocate {
            id: p.id,
            dir: p.dir,
        })
        .await
    }

    #[tool(
        description = "What Shardload learned about each site from earlier downloads, used to start the next download from it right: max_connections (it refused more; the next download tries one more), no_ranges (it sends whole files only), browser (the identity it served after refusing the one in the settings), link_lifetime (seconds its links worked before expiring) and seen_at. Forgotten 30 days after last use."
    )]
    async fn list_sites(&self) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::Sites).await
    }

    #[tool(
        description = "Forget what was learned about a site (see list_sites), for example after it changed its limits; the next download finds out again."
    )]
    async fn forget_site(
        &self,
        Parameters(p): Parameters<SiteParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::ForgetSite { site: p.site }).await
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
            "could not reach Shardload: {e}"
        ))]),
    })
}

#[tool_handler]
impl ServerHandler for Downloads {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("shardload", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Controls the Shardload download manager on this computer: add downloads \
                 (files, chosen files out of ZIP archives, torrents, metalinks, video pages), \
                 find the files a site links to, list \
                 downloads and their progress, wait for them to finish, pause, resume, reorder, \
                 rename, move and remove them, limit their speed, and choose what happens when all are done.",
            )
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let Some(data) = shardload_bridge::data_dir() else {
        eprintln!("shardload-mcp: APPDATA is not set");
        return std::process::ExitCode::FAILURE;
    };
    let server = match Downloads::new(data).serve(rmcp::transport::stdio()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("shardload-mcp: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if let Err(e) = server.waiting().await {
        eprintln!("shardload-mcp: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
