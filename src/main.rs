use clap::{Parser, Subcommand};
use fusion_link::config::Config;
use fusion_link::keychain::{self, Credential};
use fusion_link::outbox::{Outbox, outbox_path};
use fusion_link::session;
use serde_json::json;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "fusion-link",
    version,
    about = "Fusion device-side connection daemon"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Bind this device to a fusion account using a one-time code from the site console
    Bind {
        code: String,
        #[arg(long)]
        broker_url: Option<String>,
    },
    /// Remove device credential and local outbox state
    Unbind,
    /// Show bind state and pending uplink count
    Status,
    /// Run the daemon (foreground; managed by fusion-sv / start.sh)
    Run,
    /// Answer a pending local confirmation (FR-8): approve or deny a paused task
    Confirm {
        task_id: String,
        #[arg(value_parser = ["approve", "deny"])]
        decision: String,
    },
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

#[tokio::main]
async fn main() {
    init_logging();
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Bind { code, .. } => {
            if let Err(e) = cmd_bind(&code).await {
                eprintln!("bind failed: {e}");
                std::process::exit(1);
            }
            0
        }
        Command::Unbind => {
            if let Err(e) = cmd_unbind() {
                eprintln!("unbind failed: {e}");
                std::process::exit(1);
            }
            0
        }
        Command::Status => {
            if let Err(e) = cmd_status() {
                eprintln!("status failed: {e}");
                std::process::exit(1);
            }
            0
        }
        Command::Run => {
            if let Err(e) = cmd_run().await {
                eprintln!("run failed: {e}");
                std::process::exit(1);
            }
            0
        }
        Command::Confirm { task_id, decision } => {
            let approve = decision == "approve";
            if let Err(e) = cmd_confirm(&task_id, approve) {
                eprintln!("confirm failed: {e}");
                std::process::exit(1);
            }
            0
        }
    };
    std::process::exit(code);
}

async fn cmd_bind(code: &str) -> Result<(), String> {
    let cfg = Config::load()?;
    let fingerprint = keychain::device_fingerprint()?;
    let hostname = hostname();
    tracing::info!("exchanging bind code at {}", cfg.bind_url);

    let client = reqwest::Client::new();
    let resp = client
        .post(&cfg.bind_url)
        .json(&json!({
            "code": code,
            "fingerprint": fingerprint,
            "hostname": hostname,
        }))
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("bind request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("bind rejected: http {status} {body}"));
    }
    let bind: fusion_link::keychain::BindResponse = resp
        .json()
        .await
        .map_err(|e| format!("bind response parse failed: {e}"))?;

    let cred = Credential {
        device_id: bind.device_id,
        token: bind.token,
        fingerprint,
    };
    keychain::store(&cred)?;
    println!("device bound: {}", cred.device_id);
    Ok(())
}

fn cmd_unbind() -> Result<(), String> {
    keychain::delete()?;
    let cfg = Config::load()?;
    for file in ["outbox.json", "outbox.json.tmp"] {
        let path = cfg.state_dir.join(file);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("remove {} failed: {e}", path.display()))?;
        }
    }
    println!("device unbound, local state cleared");
    Ok(())
}

fn cmd_status() -> Result<(), String> {
    let cfg = Config::load()?;
    let cred = keychain::load().ok();
    match &cred {
        Some(c) => println!("bound: yes (device_id={})", c.device_id),
        None => println!("bound: no"),
    }
    println!("broker: {}", cfg.broker_url);
    println!("gateway: {}", cfg.gateway_url);
    if let Some(cred) = &cred {
        let outbox = Outbox::load(outbox_path(&cfg.state_dir));
        println!("pending uplink: {}", outbox.pending_count());
        println!(
            "capabilities: {:?}",
            cfg.task_routes.keys().collect::<Vec<_>>()
        );
        println!("fingerprint: {}", cred.fingerprint);
    }
    Ok(())
}

async fn cmd_run() -> Result<(), String> {
    let cfg = Config::load()?;
    let cred = keychain::load()?;
    let token = tokio_util::sync::CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("SIGINT received");
        cancel.cancel();
    });
    session::run(cfg, cred, token).await
}

fn cmd_confirm(task_id: &str, approve: bool) -> Result<(), String> {
    fusion_link::confirm::write_decision(&Config::load()?.state_dir, task_id, approve)?;
    println!(
        "decision recorded: {} {}",
        if approve { "approve" } else { "deny" },
        task_id
    );
    Ok(())
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}
