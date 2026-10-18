//! MCP server for idm-alternative. An AI assistant's MCP client starts it and talks to it
//! over stdin/stdout; it passes each tool call to the app over the same named pipe the
//! browser extension's host uses, starting the app in the tray when it is not running.
//! Nothing listens on a network port.
//!
//! Register it with the client as a stdio server whose command is
//! `%LOCALAPPDATA%\idm-alternative\idma-mcp.exe`.

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
    #[allow(dead_code, reason = "used by the tool_handler macro")]
    tool_router: ToolRouter<Self>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AddParams {
    /// Direct link to the file (http or https), not a web page that shows it.
    url: String,
    /// Folder to save into. The app's download folder when omitted.
    dir: Option<String>,
    /// File name. The server's suggestion when omitted.
    name: Option<String>,
    /// Start now (default) or add it paused.
    start: Option<bool>,
    /// Other direct links to the same file (mirrors), downloaded from at the same time once
    /// each is shown to serve the same bytes.
    mirrors: Option<Vec<String>>,
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
struct RemoveParams {
    /// Download ids, as `list_downloads` shows them.
    ids: Vec<i64>,
    /// Also delete the downloaded (or partly downloaded) files. Default false.
    delete_files: Option<bool>,
}

#[tool_router]
impl Downloads {
    fn new(app: Client) -> Self {
        Self {
            app: Arc::new(Mutex::new(app)),
            tool_router: Self::tool_router(),
        }
    }

    async fn ask(&self, request: Request) -> Result<CallToolResult, ErrorData> {
        let reply: Result<Reply, String> = self.app.lock().await.request(&request).await;
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

    #[tool(
        description = "List the downloads in idm-alternative with id, url, file name, path, status (queued, downloading, paused, done, error), error, size and received bytes, speed in bytes per second."
    )]
    async fn list_downloads(&self) -> Result<CallToolResult, ErrorData> {
        self.ask(Request::Tasks).await
    }

    #[tool(
        description = "Add a download to idm-alternative and start it (unless start is false). Returns the new download with its id."
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
        }))
        .await
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
                "Controls the idm-alternative download manager on this computer: add direct \
                 download links, list downloads and their progress, pause, resume and remove them.",
            )
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let Some(data) = idma_bridge::data_dir() else {
        eprintln!("idma-mcp: APPDATA is not set");
        return std::process::ExitCode::FAILURE;
    };
    let server = match Downloads::new(Client::new(data))
        .serve(rmcp::transport::stdio())
        .await
    {
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
