//! `narsd`: the nars-agent daemon.
//!
//! Serves the HTTP/SSE API and, when a build exists, the React web UI.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use nars_agent_runtime::ActorConfig;
use nars_model::from_env;
use nars_server::{AppState, ServerConfig, build_router};
use nars_session::{FileExperienceStore, FileSessionStore};
use nars_tools::policy::ApprovalMode;
use tracing_subscriber::EnvFilter;

#[derive(Debug)]
struct Args {
    port: u16,
    store: PathBuf,
    static_dir: Option<PathBuf>,
    approval: ApprovalMode,
    max_steps: u32,
    workspace_root: Option<PathBuf>,
    token: Option<String>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            port: 8787,
            store: PathBuf::from(".nars-agent/sessions"),
            static_dir: Some(PathBuf::from("web/dist")),
            approval: ApprovalMode::Ask,
            max_steps: 24,
            workspace_root: None,
            token: std::env::var("NARS_AGENT_TOKEN").ok(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = parse_args()?;
    let provider = from_env()?;
    let provider_name = provider.name().to_owned();
    let model = std::env::var("NARS_AGENT_MODEL").unwrap_or_else(|_| {
        if provider_name == "echo" || provider_name == "demo" {
            provider_name.clone()
        } else {
            "gpt-4.1-mini".to_owned()
        }
    });

    let store = Arc::new(FileSessionStore::new(&args.store)?);
    let experience_root = args.store.parent().map_or_else(
        || PathBuf::from(".nars-agent/experience"),
        |parent| parent.join("experience"),
    );
    let experience = Arc::new(FileExperienceStore::new(experience_root)?);
    let mut config = ServerConfig {
        provider_name: provider_name.clone(),
        token: args.token,
        approval_mode: args.approval,
        max_steps: args.max_steps,
        actor: ActorConfig {
            model,
            ..ActorConfig::default()
        },
        workspace_root: args.workspace_root,
        experience: Some(experience),
        ..ServerConfig::default()
    };
    if let Some(static_dir) = &args.static_dir
        && static_dir.is_dir()
    {
        config.static_dir = Some(static_dir.clone());
    }

    let state = AppState::new(store, provider, config);
    let app = build_router(state);
    let address = SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, provider = %provider_name, "narsd listening");
    println!("narsd listening on http://127.0.0.1:{}", args.port);
    if provider_name == "demo" || provider_name == "echo" {
        println!(
            "offline {} provider active: set NARS_AGENT_PROVIDER=openai|anthropic|ollama for real work",
            provider_name
        );
    }
    axum::serve(listener, app).await?;
    Ok(())
}

fn parse_args() -> Result<Args, Box<dyn std::error::Error>> {
    let mut args = Args::default();
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--port" => {
                args.port = iter
                    .next()
                    .ok_or("--port requires a value")?
                    .parse()
                    .map_err(|_| "--port must be a number")?;
            }
            "--store" => {
                args.store = PathBuf::from(iter.next().ok_or("--store requires a path")?);
            }
            "--static" => {
                args.static_dir = Some(PathBuf::from(
                    iter.next().ok_or("--static requires a path")?,
                ));
            }
            "--no-static" => args.static_dir = None,
            "--approval" => {
                args.approval = match iter.next().ok_or("--approval requires a value")?.as_str() {
                    "ask" => ApprovalMode::Ask,
                    "auto" => ApprovalMode::AutoApprove,
                    "deny" => ApprovalMode::Deny,
                    other => return Err(format!("unknown approval mode `{other}`").into()),
                };
            }
            "--max-steps" => {
                args.max_steps = iter
                    .next()
                    .ok_or("--max-steps requires a value")?
                    .parse()
                    .map_err(|_| "--max-steps must be a number")?;
            }
            "--workspace-root" => {
                args.workspace_root = Some(PathBuf::from(
                    iter.next().ok_or("--workspace-root requires a path")?,
                ));
            }
            "--token" => args.token = Some(iter.next().ok_or("--token requires a value")?),
            "--help" | "-h" => {
                println!(
                    "narsd — NARS agent daemon\n\n\
                     USAGE:\n  narsd [OPTIONS]\n\n\
                     OPTIONS:\n\
                     \x20 --port <PORT>              HTTP port (default 8787)\n\
                     \x20 --store <PATH>             session store (default .nars-agent/sessions)\n\
                     \x20 --static <PATH>            web assets (default web/dist)\n\
                     \x20 --no-static                disable static file serving\n\
                     \x20 --approval <ask|auto|deny>  approval policy (default ask)\n\
                     \x20 --max-steps <N>            default step budget (default 24)\n\
                     \x20 --workspace-root <PATH>    restrict session workspaces\n\
                     \x20 --token <TOKEN>            require a bearer token\n\n\
                     ENVIRONMENT:\n\
                     \x20 NARS_AGENT_PROVIDER=openai|anthropic|ollama|demo|echo\n\
                     \x20 NARS_AGENT_MODEL=<model>\n\
                     \x20 NARS_AGENT_BASE_URL=<url>\n\
                     \x20 NARS_AGENT_API_KEY=<key>\n\
                     \x20 NARS_AGENT_JSON_SCHEMA=1\n"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}` (try --help)").into()),
        }
    }
    Ok(args)
}
