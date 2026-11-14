// MCP server (rmcp, stdio). Each tool turns its arguments into a
// `worker::Request`, hands it to the browser thread, and converts the reply
// into a tool result:
//
//   observation → two text items: the trusted browser block, then the
//                 untrusted page block (docs/concept.md "관찰")
//   error       → isError result: one JSON item {error, message, hint,
//                 repeat_count}, plus the observation when there is one
//   screenshot  → an image item (PNG)

use std::sync::mpsc::Sender;

use base64::Engine as _;
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use tokio::sync::oneshot;

use crate::prompts;
use crate::worker::{ErrorReport, Job, Reply, Request};

#[derive(Clone)]
pub struct BrowserServer {
    jobs: Sender<Job>,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NavigateParams {
    #[schemars(description = "Absolute http(s) URL, e.g. \"http://localhost:3000/\"")]
    pub url: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ObserveParams {
    #[schemars(description = "\"viewport\" (default) or \"full\"")]
    pub scope: Option<String>,
    #[schemars(description = "Value of `next_cursor` from the previous observation")]
    pub cursor: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClickParams {
    #[schemars(description = "Element id from the latest observation, e.g. \"e21\"")]
    pub id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TypeParams {
    #[schemars(description = "Text field id from the latest observation, e.g. \"e12\"")]
    pub id: String,
    #[schemars(description = "Text to type")]
    pub text: String,
    #[schemars(description = "Empty the field first (default false)")]
    pub clear: Option<bool>,
    #[schemars(description = "Press Enter afterwards (default false)")]
    pub submit: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PressParams {
    #[schemars(
        description = "Enter, Backspace, Tab, Escape, ArrowUp, ArrowDown, PageUp or PageDown"
    )]
    pub key: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScrollParams {
    #[schemars(description = "CSS pixels; positive scrolls down, negative up")]
    pub dy: f64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WaitForParams {
    #[schemars(description = "Wait until this text appears (page text, element name or value)")]
    pub text: Option<String>,
    #[schemars(description = "Or: wait until this element id is rendered")]
    pub id: Option<String>,
    #[schemars(description = "Give up after this many ms (default 5000, max 60000)")]
    pub timeout_ms: Option<u64>,
}

const DEFAULT_WAIT_MS: u64 = 5_000;
const MAX_WAIT_MS: u64 = 60_000;

#[tool_router]
impl BrowserServer {
    pub fn new(jobs: Sender<Job>) -> Self {
        Self {
            jobs,
            tool_router: Self::tool_router(),
        }
    }

    async fn call(&self, request: Request) -> Result<CallToolResult, McpError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.jobs
            .send(Job {
                request,
                reply: reply_tx,
            })
            .map_err(|_| McpError::internal_error("browser thread is gone", None))?;
        let reply = reply_rx
            .await
            .map_err(|_| McpError::internal_error("browser thread dropped the request", None))?;
        Ok(to_result(reply))
    }

    #[tool(description = prompts::NAVIGATE)]
    async fn navigate(
        &self,
        Parameters(NavigateParams { url }): Parameters<NavigateParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call(Request::Navigate { url }).await
    }

    #[tool(description = prompts::OBSERVE)]
    async fn observe(
        &self,
        Parameters(ObserveParams { scope, cursor }): Parameters<ObserveParams>,
    ) -> Result<CallToolResult, McpError> {
        let full = match scope.as_deref() {
            None | Some("viewport") => false,
            Some("full") => true,
            Some(_) => {
                return Ok(to_result(Reply::Error(invalid_arguments(
                    "`scope` must be \"viewport\" or \"full\".",
                ))));
            }
        };
        self.call(Request::Observe {
            full,
            cursor: cursor.unwrap_or(0),
        })
        .await
    }

    #[tool(description = prompts::CLICK)]
    async fn click(
        &self,
        Parameters(ClickParams { id }): Parameters<ClickParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call(Request::Click { id }).await
    }

    #[tool(name = "type", description = prompts::TYPE)]
    async fn type_text(
        &self,
        Parameters(params): Parameters<TypeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call(Request::Type {
            id: params.id,
            text: params.text,
            clear: params.clear.unwrap_or(false),
            submit: params.submit.unwrap_or(false),
        })
        .await
    }

    #[tool(description = prompts::PRESS)]
    async fn press(
        &self,
        Parameters(PressParams { key }): Parameters<PressParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call(Request::Press { key }).await
    }

    #[tool(description = prompts::SCROLL)]
    async fn scroll(
        &self,
        Parameters(ScrollParams { dy }): Parameters<ScrollParams>,
    ) -> Result<CallToolResult, McpError> {
        if !dy.is_finite() {
            return Ok(to_result(Reply::Error(invalid_arguments(
                "`dy` must be a finite number.",
            ))));
        }
        self.call(Request::Scroll { dy }).await
    }

    #[tool(description = prompts::BACK)]
    async fn back(&self) -> Result<CallToolResult, McpError> {
        self.call(Request::Back).await
    }

    #[tool(description = prompts::FORWARD)]
    async fn forward(&self) -> Result<CallToolResult, McpError> {
        self.call(Request::Forward).await
    }

    #[tool(description = prompts::SCREENSHOT)]
    async fn screenshot(&self) -> Result<CallToolResult, McpError> {
        self.call(Request::Screenshot).await
    }

    #[tool(description = prompts::WAIT_FOR)]
    async fn wait_for(
        &self,
        Parameters(params): Parameters<WaitForParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call(Request::WaitFor {
            text: params.text,
            id: params.id,
            timeout_ms: params
                .timeout_ms
                .unwrap_or(DEFAULT_WAIT_MS)
                .min(MAX_WAIT_MS),
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for BrowserServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(prompts::SERVER_INSTRUCTIONS)
            .with_server_info(Implementation::new(
                "mini-browser",
                env!("CARGO_PKG_VERSION"),
            ))
    }
}

fn invalid_arguments(message: &str) -> ErrorReport {
    ErrorReport {
        code: "invalid_arguments".into(),
        message: message.into(),
        hint: "Check the tool's parameters.".into(),
        repeat_count: 1,
        observation: None,
    }
}

fn to_result(reply: Reply) -> CallToolResult {
    match reply {
        Reply::Observation { browser, page } => {
            CallToolResult::success(vec![ContentBlock::text(browser), ContentBlock::text(page)])
        }
        Reply::Screenshot { png, width, height } => CallToolResult::success(vec![
            ContentBlock::image(
                base64::engine::general_purpose::STANDARD.encode(png),
                "image/png",
            ),
            ContentBlock::text(format!(
                "Screenshot of the page area, {width}x{height} CSS px (browser chrome excluded)."
            )),
        ]),
        Reply::Error(report) => {
            let json = serde_json::json!({
                "error": report.code,
                "message": report.message,
                "hint": report.hint,
                "repeat_count": report.repeat_count,
            });
            let mut content = vec![ContentBlock::text(json.to_string())];
            if let Some((browser, page)) = report.observation {
                content.push(ContentBlock::text(browser));
                content.push(ContentBlock::text(page));
            }
            CallToolResult::error(content)
        }
    }
}
