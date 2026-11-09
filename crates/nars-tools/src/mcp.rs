//! MCP (Model Context Protocol) stdio client and tool adapter.
//!
//! The adapter connects to an external MCP server over newline-delimited
//! JSON-RPC 2.0, converts `tools/list` entries into regular [`ToolManifest`]
//! values, and executes `tools/call` through the existing [`Tool`] trait. Once
//! registered, MCP tools are indistinguishable from built-ins to NARS, the
//! planner, the broker, and the UI.
//!
//! Security defaults are deliberately strict: external tools are classified as
//! [`SideEffect::ExternalSideEffect`] and require approval unless the operator
//! explicitly relaxes the configuration.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nars_agent_ir::{Artifact, SideEffect, ToolCall, ToolManifest, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::manifest::{Tool, ToolContext};

const PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_PAGES: usize = 10;

/// MCP client errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum McpError {
    /// The server process could not be started.
    #[error("failed to start MCP server `{server}`: {message}")]
    Spawn {
        /// Configured server name.
        server: String,
        /// Spawn error.
        message: String,
    },
    /// A write to the server failed.
    #[error("MCP transport I/O error: {0}")]
    Io(String),
    /// The server closed its stdout.
    #[error("MCP server closed the connection")]
    Closed,
    /// The server did not answer within the configured timeout.
    #[error("MCP request timed out after {0} ms")]
    Timeout(u64),
    /// The response did not match the protocol.
    #[error("MCP protocol error: {0}")]
    Protocol(String),
    /// The server returned a JSON-RPC error.
    #[error("MCP server error {code}: {message}")]
    Rpc {
        /// JSON-RPC error code.
        code: i64,
        /// JSON-RPC error message.
        message: String,
    },
    /// A tool description cannot be represented safely.
    #[error("invalid MCP tool `{0}`")]
    InvalidTool(String),
}

/// Configuration for one stdio MCP server.
#[derive(Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Server name used as a tool-name prefix.
    pub name: String,
    /// Executable to spawn for stdio transport.
    #[serde(default)]
    pub command: String,
    /// Streamable HTTP endpoint. When set, `command` is ignored.
    #[serde(default)]
    pub url: Option<String>,
    /// Extra HTTP headers for the HTTP transport.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Command arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Side effect classification for all tools from this server.
    #[serde(default = "external_side_effect")]
    pub side_effect: SideEffect,
    /// Per-request timeout in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Whether calls require explicit approval.
    #[serde(default = "default_requires_approval")]
    pub requires_approval: bool,
}

fn external_side_effect() -> SideEffect {
    SideEffect::ExternalSideEffect
}

const fn default_timeout_ms() -> u64 {
    60_000
}

const fn default_requires_approval() -> bool {
    true
}

impl std::fmt::Debug for McpServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpServerConfig")
            .field("name", &self.name)
            .field("command", &self.command)
            .field("args", &self.args)
            .field("url", &self.url)
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .field("side_effect", &self.side_effect)
            .field("timeout_ms", &self.timeout_ms)
            .field("requires_approval", &self.requires_approval)
            .finish()
    }
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: "mcp".to_owned(),
            command: String::new(),
            url: None,
            headers: BTreeMap::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            side_effect: SideEffect::ExternalSideEffect,
            timeout_ms: default_timeout_ms(),
            requires_approval: true,
        }
    }
}

/// A tool description returned by `tools/list`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpToolDescription {
    /// Name inside the MCP server.
    pub name: String,
    /// Optional description.
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema for the arguments.
    #[serde(rename = "inputSchema", alias = "input_schema", default)]
    pub input_schema: Option<Value>,
}

/// Parsed result of `tools/call`.
#[derive(Clone, Debug, PartialEq)]
pub struct McpCallResult {
    /// Concatenated text content.
    pub text: String,
    /// Whether the server marked the call as an error.
    pub is_error: bool,
    /// Artifacts discovered in the content array.
    pub artifacts: Vec<Artifact>,
    /// Optional structured content.
    pub structured: Option<Value>,
}

/// Byte-stream transport used by [`McpClient`].
pub trait McpTransport: Send {
    /// Sends one JSON-RPC message.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the message cannot be written.
    fn send(&mut self, message: &Value) -> Result<(), McpError>;
    /// Receives one JSON-RPC message.
    ///
    /// # Errors
    ///
    /// Returns a timeout, closed-connection, or protocol error.
    fn receive(&mut self, timeout: Duration) -> Result<Value, McpError>;
}

/// Client surface used by [`McpTool`], enabling deterministic tests.
pub trait McpClientApi: Send + Sync {
    /// Configured server name.
    fn server_name(&self) -> &str;
    /// Lists all tools.
    ///
    /// # Errors
    ///
    /// Returns a protocol or transport error.
    fn list_tools(&self) -> Result<Vec<McpToolDescription>, McpError>;
    /// Calls one tool.
    ///
    /// # Errors
    ///
    /// Returns a protocol, transport, or remote error.
    fn call_tool(&self, name: &str, arguments: &Value) -> Result<McpCallResult, McpError>;
}

/// JSON-RPC client for one MCP server.
pub struct McpClient<T: McpTransport> {
    name: String,
    transport: Mutex<T>,
    next_id: AtomicU64,
    timeout: Duration,
    server_info: Value,
}

impl<T: McpTransport> McpClient<T> {
    /// Performs the MCP initialize handshake.
    ///
    /// # Errors
    ///
    /// Returns an error when the server rejects initialization or the
    /// transport fails.
    pub fn connect(
        name: impl Into<String>,
        transport: T,
        timeout: Duration,
    ) -> Result<Self, McpError> {
        let name = name.into();
        let next_id = AtomicU64::new(1);
        let client = Self {
            name,
            transport: Mutex::new(transport),
            next_id,
            timeout,
            server_info: Value::Null,
        };
        let initialize = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "nars-agent",
                "version": env!("CARGO_PKG_VERSION")
            }
        });
        let result = client.request("initialize", initialize)?;
        client.notify("notifications/initialized", json!({}))?;
        Ok(Self {
            server_info: result,
            ..client
        })
    }

    /// Server information returned by `initialize`.
    #[must_use]
    pub fn server_info(&self) -> &Value {
        &self.server_info
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        let mut transport = self.transport.lock().unwrap();
        transport.send(&message)?;
        for _ in 0..100 {
            let response = transport.receive(self.timeout)?;
            let response_id = response.get("id").and_then(Value::as_u64);
            if response_id != Some(id) {
                // Server notifications and unrelated responses are ignored in
                // this one-request-at-a-time client.
                continue;
            }
            if let Some(error) = response.get("error") {
                return Err(McpError::Rpc {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(-1),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_owned(),
                });
            }
            return Ok(response.get("result").cloned().unwrap_or(Value::Null));
        }
        Err(McpError::Protocol(
            "too many unrelated messages before the response".to_owned(),
        ))
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        self.transport.lock().unwrap().send(&message)
    }
}

impl<T: McpTransport + 'static> McpClientApi for McpClient<T> {
    fn server_name(&self) -> &str {
        &self.name
    }

    fn list_tools(&self) -> Result<Vec<McpToolDescription>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
            let result = self.request("tools/list", params)?;
            let page = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    McpError::Protocol("tools/list result has no tools array".to_owned())
                })?;
            for item in page {
                tools.push(serde_json::from_value(item.clone()).map_err(|error| {
                    McpError::Protocol(format!("invalid tool description: {error}"))
                })?);
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        Err(McpError::Protocol(
            "tools/list exceeded pagination limit".to_owned(),
        ))
    }

    fn call_tool(&self, name: &str, arguments: &Value) -> Result<McpCallResult, McpError> {
        let result = self.request("tools/call", json!({"name": name, "arguments": arguments}))?;
        parse_call_result(&result)
    }
}

fn parse_call_result(result: &Value) -> Result<McpCallResult, McpError> {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut text = String::new();
    let mut artifacts = Vec::new();
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for (index, item) in content.iter().enumerate() {
            match item.get("type").and_then(Value::as_str).unwrap_or("text") {
                "text" => {
                    if let Some(value) = item.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(value);
                    }
                }
                "image" => {
                    artifacts.push(Artifact {
                        id: format!("mcp-image-{index}"),
                        kind: "image".to_owned(),
                        location: "mcp://content".to_owned(),
                        description: item
                            .get("mimeType")
                            .and_then(Value::as_str)
                            .unwrap_or("image")
                            .to_owned(),
                        bytes: None,
                    });
                }
                "resource" => {
                    if let Some(resource) = item.get("resource") {
                        if let Some(uri) = resource.get("uri").and_then(Value::as_str) {
                            artifacts.push(Artifact {
                                id: format!("mcp-resource-{index}"),
                                kind: "resource".to_owned(),
                                location: uri.to_owned(),
                                description: resource
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("MCP resource")
                                    .to_owned(),
                                bytes: None,
                            });
                        }
                    }
                }
                other => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&format!("[unsupported MCP content: {other}]"));
                }
            }
        }
    }
    Ok(McpCallResult {
        text,
        is_error,
        artifacts,
        structured: result.get("structuredContent").cloned(),
    })
}

/// An MCP tool registered in the local registry.
pub struct McpTool {
    client: Arc<dyn McpClientApi>,
    remote_name: String,
    manifest: ToolManifest,
}

impl McpTool {
    /// Converts one MCP description into a local tool.
    ///
    /// # Errors
    ///
    /// Returns an error when the MCP tool name cannot be represented.
    pub fn new(
        client: Arc<dyn McpClientApi>,
        description: &McpToolDescription,
        config: &McpServerConfig,
    ) -> Result<Self, McpError> {
        let server = sanitize(&config.name);
        let tool = sanitize(&description.name);
        if server.is_empty() || tool.is_empty() {
            return Err(McpError::InvalidTool(description.name.clone()));
        }
        let name = format!("mcp_{server}_{tool}");
        let description_text = format!(
            "{}\n\nProvided by MCP server `{}` as `{}`.",
            description
                .description
                .clone()
                .unwrap_or_else(|| "External MCP tool.".to_owned()),
            config.name,
            description.name
        );
        let input_schema = description.input_schema.clone().unwrap_or_else(
            || json!({"type": "object", "properties": {}, "additionalProperties": true}),
        );
        let manifest = ToolManifest {
            name,
            description: description_text,
            input_schema,
            side_effect: config.side_effect,
            timeout_ms: config.timeout_ms,
            requires_approval: config.requires_approval,
            version: "0.1.0".to_owned(),
            tags: vec!["mcp".to_owned(), format!("mcp:{}", config.name)],
        };
        Ok(Self {
            client,
            remote_name: description.name.clone(),
            manifest,
        })
    }

    /// The name used by the MCP server.
    #[must_use]
    pub fn remote_name(&self) -> &str {
        &self.remote_name
    }
}

impl Tool for McpTool {
    fn manifest(&self) -> &ToolManifest {
        &self.manifest
    }

    fn execute(&self, call: &ToolCall, _context: &ToolContext) -> ToolResult {
        match self.client.call_tool(&self.remote_name, &call.arguments) {
            Ok(result) => {
                let mut tool_result = if result.is_error {
                    ToolResult::failure(
                        call.id.clone(),
                        if result.text.is_empty() {
                            "MCP tool reported an error".to_owned()
                        } else {
                            result.text.clone()
                        },
                    )
                } else {
                    ToolResult::success(call.id.clone(), result.text)
                };
                tool_result.artifacts = result.artifacts;
                tool_result
                    .metadata
                    .insert("mcp_server".to_owned(), json!(self.client.server_name()));
                tool_result
                    .metadata
                    .insert("mcp_tool".to_owned(), json!(self.remote_name));
                if let Some(structured) = result.structured {
                    tool_result
                        .metadata
                        .insert("structured".to_owned(), structured);
                }
                tool_result
            }
            Err(error) => ToolResult::failure(call.id.clone(), error.to_string()),
        }
    }
}

/// Connects to an MCP server over stdio or HTTP.
///
/// # Errors
///
/// Returns an error when the transport cannot start, initialization fails, or
/// `tools/list` is malformed.
pub fn connect(config: &McpServerConfig) -> Result<Vec<Arc<dyn Tool>>, McpError> {
    if let Some(url) = &config.url {
        let transport = HttpTransport::new(url, &config.headers, config.timeout_ms)?;
        return connect_transport(config, transport);
    }
    connect_stdio(config)
}

/// Connects to a stdio MCP server and converts all of its tools.
///
/// # Errors
///
/// Returns an error when the process cannot be spawned, initialization fails,
/// or `tools/list` is malformed.
pub fn connect_stdio(config: &McpServerConfig) -> Result<Vec<Arc<dyn Tool>>, McpError> {
    let transport = StdioTransport::spawn(config)?;
    connect_transport(config, transport)
}

fn connect_transport<T: McpTransport + 'static>(
    config: &McpServerConfig,
    transport: T,
) -> Result<Vec<Arc<dyn Tool>>, McpError> {
    let client = Arc::new(McpClient::connect(
        config.name.clone(),
        transport,
        Duration::from_millis(config.timeout_ms.max(1)),
    )?);
    let descriptions = client.list_tools()?;
    let mut tools: Vec<Arc<dyn Tool>> = Vec::with_capacity(descriptions.len());
    for description in &descriptions {
        let tool = McpTool::new(
            Arc::clone(&client) as Arc<dyn McpClientApi>,
            description,
            config,
        )?;
        tools.push(Arc::new(tool));
    }
    Ok(tools)
}

/// Parses MCP server configuration from JSON.
///
/// Accepts either a bare array or `{ "servers": [...] }`.
///
/// # Errors
///
/// Returns the serde error for malformed JSON.
pub fn parse_servers(json_text: &str) -> Result<Vec<McpServerConfig>, serde_json::Error> {
    let value: Value = serde_json::from_str(json_text)?;
    if value.is_array() {
        serde_json::from_value(value)
    } else {
        serde_json::from_value(
            value
                .get("servers")
                .cloned()
                .unwrap_or(Value::Array(Vec::new())),
        )
    }
}

fn sanitize(value: &str) -> String {
    let mut output = String::new();
    let mut underscore = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            underscore = false;
            output.push(character.to_ascii_lowercase());
        } else if !underscore {
            underscore = true;
            output.push('_');
        }
        if output.len() >= 40 {
            break;
        }
    }
    output.trim_matches('_').to_owned()
}

/// Streamable HTTP transport for MCP.
///
/// Sends JSON-RPC messages with `Accept: application/json, text/event-stream`,
/// handles both JSON and SSE response bodies, and keeps the `Mcp-Session-Id`
/// returned by `initialize`.
pub struct HttpTransport {
    client: reqwest::blocking::Client,
    url: String,
    headers: reqwest::header::HeaderMap,
    session: Option<String>,
    queue: VecDeque<Value>,
}

impl std::fmt::Debug for HttpTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpTransport")
            .field("url", &self.url)
            .field("session", &self.session.is_some())
            .finish()
    }
}

impl HttpTransport {
    /// Builds an HTTP transport for one endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid URLs, headers, or client setup.
    pub fn new(
        url: &str,
        headers: &BTreeMap<String, String>,
        timeout_ms: u64,
    ) -> Result<Self, McpError> {
        if url.trim().is_empty() {
            return Err(McpError::Protocol("MCP HTTP URL is empty".to_owned()));
        }
        let mut header_map = reqwest::header::HeaderMap::new();
        for (key, value) in headers {
            let name =
                reqwest::header::HeaderName::from_bytes(key.as_bytes()).map_err(|error| {
                    McpError::Protocol(format!("invalid MCP header `{key}`: {error}"))
                })?;
            let value = reqwest::header::HeaderValue::from_str(value).map_err(|error| {
                McpError::Protocol(format!("invalid MCP header value: {error}"))
            })?;
            header_map.insert(name, value);
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(timeout_ms.max(1)))
            .build()
            .map_err(|error| McpError::Io(format!("failed to build MCP HTTP client: {error}")))?;
        Ok(Self {
            client,
            url: url.to_owned(),
            headers: header_map,
            session: None,
            queue: VecDeque::new(),
        })
    }
}

impl McpTransport for HttpTransport {
    fn send(&mut self, message: &Value) -> Result<(), McpError> {
        let id = message.get("id").and_then(Value::as_u64);
        let mut request = self
            .client
            .post(&self.url)
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .headers(self.headers.clone())
            .json(message);
        if let Some(session) = &self.session {
            request = request.header("Mcp-Session-Id", session);
        }
        let response = request.send().map_err(|error| {
            if error.is_timeout() {
                McpError::Io(format!("MCP HTTP request timed out: {error}"))
            } else {
                McpError::Io(format!("MCP HTTP request failed: {error}"))
            }
        })?;
        if let Some(session) = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|value| value.to_str().ok())
        {
            self.session = Some(session.to_owned());
        }
        let status = response.status();
        if status.as_u16() == 202 {
            return Ok(());
        }
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            let excerpt = body.chars().take(500).collect::<String>();
            return Err(McpError::Protocol(format!("MCP HTTP {status}: {excerpt}")));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.contains("text/event-stream") {
            self.read_sse(response, id)?;
        } else {
            let text = response
                .text()
                .map_err(|error| McpError::Io(error.to_string()))?;
            let value: Value = serde_json::from_str(&text)
                .map_err(|error| McpError::Protocol(format!("invalid MCP HTTP JSON: {error}")))?;
            if let Some(items) = value.as_array() {
                for item in items {
                    self.queue.push_back(item.clone());
                }
            } else {
                self.queue.push_back(value);
            }
        }
        Ok(())
    }

    fn receive(&mut self, _timeout: Duration) -> Result<Value, McpError> {
        self.queue
            .pop_front()
            .ok_or_else(|| McpError::Protocol("MCP HTTP response queue is empty".to_owned()))
    }
}

impl HttpTransport {
    fn read_sse(
        &mut self,
        response: reqwest::blocking::Response,
        expected_id: Option<u64>,
    ) -> Result<(), McpError> {
        let reader = BufReader::new(response);
        let mut data = String::new();
        for line in reader.lines() {
            let line = line.map_err(|error| McpError::Io(error.to_string()))?;
            if line.is_empty() {
                if !data.is_empty() {
                    if let Ok(value) = serde_json::from_str::<Value>(&data) {
                        let matched = expected_id
                            .is_some_and(|id| value.get("id").and_then(Value::as_u64) == Some(id));
                        self.queue.push_back(value);
                        data.clear();
                        if matched {
                            return Ok(());
                        }
                    } else {
                        data.clear();
                    }
                }
            } else if let Some(rest) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest.trim_start());
            }
        }
        if !data.is_empty()
            && let Ok(value) = serde_json::from_str::<Value>(&data)
        {
            self.queue.push_back(value);
        }
        Ok(())
    }
}

/// Newline-delimited JSON-RPC transport over a child process.
pub struct StdioTransport {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    receiver: Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
    stderr: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for StdioTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StdioTransport")
            .field("running", &self.child.is_some())
            .finish()
    }
}

impl StdioTransport {
    /// Spawns a stdio MCP server.
    ///
    /// # Errors
    ///
    /// Returns an error when the process or its pipes cannot be created.
    pub fn spawn(config: &McpServerConfig) -> Result<Self, McpError> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &config.env {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|error| McpError::Spawn {
            server: config.name.clone(),
            message: error.to_string(),
        })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (sender, receiver) = channel();
        let reader = stdout.map(|stdout| {
            std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    match line {
                        Ok(line) => {
                            if sender.send(line).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
        });
        let stderr = stderr.map(|stderr| {
            let server = config.name.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    tracing::debug!(target: "mcp", server = %server, "{}", line);
                }
            })
        });
        Ok(Self {
            child: Some(child),
            stdin,
            receiver,
            reader,
            stderr,
        })
    }
}

impl McpTransport for StdioTransport {
    fn send(&mut self, message: &Value) -> Result<(), McpError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| McpError::Io("MCP stdin is closed".to_owned()))?;
        let line = serde_json::to_string(message)
            .map_err(|error| McpError::Protocol(error.to_string()))?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .map_err(|error| McpError::Io(error.to_string()))
    }

    fn receive(&mut self, timeout: Duration) -> Result<Value, McpError> {
        match self.receiver.recv_timeout(timeout) {
            Ok(line) => serde_json::from_str(&line)
                .map_err(|error| McpError::Protocol(format!("invalid JSON from server: {error}"))),
            Err(RecvTimeoutError::Timeout) => Err(McpError::Timeout(
                u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            )),
            Err(RecvTimeoutError::Disconnected) => Err(McpError::Closed),
        }
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct MockTransport {
        responses: VecDeque<Value>,
        sent: Arc<Mutex<Vec<Value>>>,
    }

    impl McpTransport for MockTransport {
        fn send(&mut self, message: &Value) -> Result<(), McpError> {
            self.sent.lock().unwrap().push(message.clone());
            Ok(())
        }

        fn receive(&mut self, _timeout: Duration) -> Result<Value, McpError> {
            let mut response = self
                .responses
                .pop_front()
                .ok_or_else(|| McpError::Protocol("mock has no response".to_owned()))?;
            let id = self
                .sent
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find_map(|message| message.get("id").cloned());
            if let (Some(id), Some(object)) = (id, response.as_object_mut()) {
                object.insert("id".to_owned(), id);
            }
            Ok(response)
        }
    }

    fn mock_client() -> (McpClient<MockTransport>, Arc<Mutex<Vec<Value>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let transport = MockTransport {
            responses: VecDeque::from(vec![
                json!({"jsonrpc":"2.0","result":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{},"serverInfo":{"name":"mock","version":"0"}}}),
                json!({"jsonrpc":"2.0","result":{"tools":[{"name":"echo","description":"echo text","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}}),
                json!({"jsonrpc":"2.0","result":{"content":[{"type":"text","text":"hello"}],"isError":false}}),
            ]),
            sent: Arc::clone(&sent),
        };
        let client = McpClient::connect("mock", transport, Duration::from_secs(1)).unwrap();
        (client, sent)
    }

    #[test]
    fn handshake_lists_and_calls_tools() {
        let (client, sent) = mock_client();
        let tools = client.list_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        let result = client.call_tool("echo", &json!({"text": "hi"})).unwrap();
        assert_eq!(result.text, "hello");
        assert!(!result.is_error);
        let sent = sent.lock().unwrap();
        assert_eq!(sent[0]["method"], "initialize");
        assert_eq!(sent[1]["method"], "notifications/initialized");
        assert_eq!(sent[2]["method"], "tools/list");
        assert_eq!(sent[3]["method"], "tools/call");
    }

    #[test]
    fn manifest_is_namespaced_and_external() {
        let (client, _) = mock_client();
        let config = McpServerConfig {
            name: "files server".to_owned(),
            command: "unused".to_owned(),
            ..McpServerConfig::default()
        };
        let description = McpToolDescription {
            name: "read/path".to_owned(),
            description: Some("reads".to_owned()),
            input_schema: Some(json!({"type": "object"})),
        };
        let tool = McpTool::new(Arc::new(client), &description, &config).unwrap();
        assert_eq!(tool.manifest().name, "mcp_files_server_read_path");
        assert_eq!(tool.manifest().side_effect, SideEffect::ExternalSideEffect);
        assert!(tool.manifest().requires_approval);
        assert!(
            tool.manifest()
                .tags
                .contains(&"mcp:files server".to_owned())
        );
    }

    struct FakeClient {
        error: bool,
    }

    impl McpClientApi for FakeClient {
        fn server_name(&self) -> &str {
            "fake"
        }

        fn list_tools(&self) -> Result<Vec<McpToolDescription>, McpError> {
            Ok(Vec::new())
        }

        fn call_tool(&self, _name: &str, _arguments: &Value) -> Result<McpCallResult, McpError> {
            Ok(McpCallResult {
                text: "result".to_owned(),
                is_error: self.error,
                artifacts: Vec::new(),
                structured: Some(json!({"ok": !self.error})),
            })
        }
    }

    fn tool_call() -> ToolCall {
        ToolCall {
            id: nars_agent_ir::ToolCallId::new(),
            run_id: nars_agent_ir::RunId::from_static("run_1"),
            tool: "mcp_fake_echo".to_owned(),
            arguments: json!({}),
            side_effect: SideEffect::ExternalSideEffect,
            requires_approval: true,
            created_at_ms: 0,
        }
    }

    #[test]
    fn tool_execution_maps_text_and_errors() {
        let config = McpServerConfig {
            name: "fake".to_owned(),
            command: "unused".to_owned(),
            ..McpServerConfig::default()
        };
        let description = McpToolDescription {
            name: "echo".to_owned(),
            description: None,
            input_schema: None,
        };
        let sandbox = tempfile::tempdir().unwrap();
        let context = ToolContext::new(crate::sandbox::Sandbox::new(sandbox.path()).unwrap());
        let tool =
            McpTool::new(Arc::new(FakeClient { error: false }), &description, &config).unwrap();
        let result = tool.execute(&tool_call(), &context);
        assert!(result.is_success());
        assert_eq!(result.stdout, "result");
        assert_eq!(result.metadata["mcp_server"], "fake");

        let tool =
            McpTool::new(Arc::new(FakeClient { error: true }), &description, &config).unwrap();
        let result = tool.execute(&tool_call(), &context);
        assert!(!result.is_success());
    }

    #[test]
    fn parses_http_server_without_command() {
        let object = r#"{"servers":[{"name":"remote","url":"http://127.0.0.1:1234/mcp","side_effect":"read_only"}]}"#;
        let servers = parse_servers(object).unwrap();
        assert_eq!(servers[0].url.as_deref(), Some("http://127.0.0.1:1234/mcp"));
        assert_eq!(servers[0].side_effect, SideEffect::ReadOnly);
        assert!(servers[0].command.is_empty());
    }

    #[test]
    fn http_round_trip_with_python_server() {
        let python_ok = std::process::Command::new("python3")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !python_ok {
            return;
        }
        let script = r#"
import http.server, json, sys

class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get('Content-Length', 0))
        message = json.loads(self.rfile.read(length))
        if message.get('id') is None:
            self.send_response(202)
            self.end_headers()
            return
        method = message.get('method')
        if method == 'initialize':
            result = {'protocolVersion': '2024-11-05', 'capabilities': {}, 'serverInfo': {'name': 'pyhttp', 'version': '0'}}
        elif method == 'tools/list':
            result = {'tools': [{'name': 'echo', 'description': 'echo', 'inputSchema': {'type': 'object'}}]}
        else:
            result = {}
        body = json.dumps({'jsonrpc': '2.0', 'id': message['id'], 'result': result}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Mcp-Session-Id', 'session-1')
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
"#;
        let mut child = std::process::Command::new("python3")
            .args(["-u", "-c", script])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut port_line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut port_line)
            .unwrap();
        let port: u16 = port_line.trim().parse().unwrap();
        let config = McpServerConfig {
            name: "pyhttp".to_owned(),
            command: String::new(),
            url: Some(format!("http://127.0.0.1:{port}/mcp")),
            timeout_ms: 5_000,
            ..McpServerConfig::default()
        };
        let tools = connect(&config).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].manifest().name, "mcp_pyhttp_echo");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn parses_servers_from_array_and_object() {
        let array = r#"[{"name":"a","command":"echo"}]"#;
        assert_eq!(parse_servers(array).unwrap()[0].name, "a");
        let object = r#"{"servers":[{"name":"b","command":"echo","side_effect":"read_only"}]}"#;
        let servers = parse_servers(object).unwrap();
        assert_eq!(servers[0].name, "b");
        assert_eq!(servers[0].side_effect, SideEffect::ReadOnly);
    }

    #[test]
    fn stdio_round_trip_with_python_server() {
        let python_ok = std::process::Command::new("python3")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !python_ok {
            return;
        }
        let script = r#"
import sys, json
for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"py","version":"0"}}}), flush=True)
    elif method == "tools/list":
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object"}}]}}), flush=True)
    elif method == "tools/call":
        text = msg.get("params", {}).get("arguments", {}).get("text", "")
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"content":[{"type":"text","text":"py:" + text}]}}), flush=True)
"#;
        let config = McpServerConfig {
            name: "python".to_owned(),
            command: "python3".to_owned(),
            args: vec!["-u".to_owned(), "-c".to_owned(), script.to_owned()],
            timeout_ms: 5_000,
            ..McpServerConfig::default()
        };
        let tools = connect_stdio(&config).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].manifest().name, "mcp_python_echo");
    }
}
