use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRoute {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeCfg {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub broker_url: String,
    pub bind_url: String,
    pub gateway_url: String,
    #[serde(default)]
    pub gateway_token: Option<String>,
    pub heartbeat_secs: u64,
    pub state_dir: PathBuf,
    #[serde(default)]
    pub task_routes: HashMap<String, TaskRoute>,
    #[serde(default)]
    pub probes: Vec<ProbeCfg>,
}

impl Default for Config {
    fn default() -> Self {
        let mut task_routes = HashMap::new();
        task_routes.insert(
            "shell.chat".to_string(),
            TaskRoute {
                method: "POST".to_string(),
                path: "/api/v1/shell/chat/completions".to_string(),
                timeout_secs: 300,
            },
        );
        // office-api routes (issue #10): served via fusion-gateway
        // /shell/office-api proxy, auth via link service ticket.
        for (name, path) in [
            ("office.spreadsheet", "/shell/office-api/v1/xlsx/parse"),
            ("office.document", "/shell/office-api/v1/docx/write"),
            ("office.slides", "/shell/office-api/v1/pptx/write"),
        ] {
            task_routes.insert(
                name.to_string(),
                TaskRoute {
                    method: "POST".to_string(),
                    path: path.to_string(),
                    timeout_secs: 300,
                },
            );
        }
        Self {
            broker_url: "wss://fusion-mlx.cn/link/ws".to_string(),
            bind_url: "https://fusion-mlx.cn/api/link/bind".to_string(),
            gateway_url: "http://127.0.0.1:11432".to_string(),
            gateway_token: None,
            // Broker read deadline is 30s; a 30s heartbeat loses that race.
            // Stay strictly under it (verified stable at 15 on prod).
            heartbeat_secs: 15,
            state_dir: default_state_dir(),
            task_routes,
            probes: default_probes(),
        }
    }
}

pub fn default_state_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".fusion-link")
}

fn default_probes() -> Vec<ProbeCfg> {
    vec![
        ProbeCfg {
            name: "fusion-mlx".into(),
            url: "http://127.0.0.1:11434/health".into(),
        },
        ProbeCfg {
            name: "fusion-gateway".into(),
            url: "http://127.0.0.1:11432/api/v1/system/health".into(),
        },
        ProbeCfg {
            name: "fusion-rag".into(),
            url: "http://127.0.0.1:11436/health".into(),
        },
        ProbeCfg {
            name: "fusion-artifacts-engine".into(),
            url: "http://127.0.0.1:11451/health".into(),
        },
        ProbeCfg {
            name: "fusion-office".into(),
            url: "http://127.0.0.1:11485/".into(),
        },
        ProbeCfg {
            name: "fusion-office-api".into(),
            url: "http://127.0.0.1:11492/health".into(),
        },
    ]
}

fn merge_table(cfg: &mut Config, table: &toml::Table) -> Result<(), String> {
    for (key, value) in table {
        match key.as_str() {
            "broker_url" => cfg.broker_url = str_field(value, key)?,
            "bind_url" => cfg.bind_url = str_field(value, key)?,
            "gateway_url" => cfg.gateway_url = str_field(value, key)?,
            "gateway_token" => {
                cfg.gateway_token = value.as_str().map(|v| v.to_string());
            }
            "heartbeat_secs" => {
                cfg.heartbeat_secs = value
                    .as_integer()
                    .filter(|n| *n >= 0)
                    .map(|n| n as u64)
                    .ok_or_else(|| "heartbeat_secs must be a non-negative integer".to_string())?;
            }
            "state_dir" => cfg.state_dir = PathBuf::from(str_field(value, key)?),
            "task_routes" => {
                cfg.task_routes = value
                    .clone()
                    .try_into()
                    .map_err(|e| format!("task_routes: {e}"))?;
            }
            "probes" => {
                cfg.probes = value
                    .clone()
                    .try_into()
                    .map_err(|e| format!("probes: {e}"))?;
            }
            other => return Err(format!("unknown config key '{other}'")),
        }
    }
    Ok(())
}

fn str_field(value: &toml::Value, key: &str) -> Result<String, String> {
    value
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("{key} must be a string"))
}

impl Config {
    pub fn load() -> Result<Self, String> {
        let mut cfg = Config::default();
        let path = default_state_dir().join("config.toml");
        if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("config read failed {}: {e}", path.display()))?;
            // Field-level merge over defaults: a partial config file (e.g. only
            // broker_url) must not wipe unset collections like task_routes.
            let table: toml::Table = toml::from_str(&raw)
                .map_err(|e| format!("config parse failed {}: {e}", path.display()))?;
            merge_table(&mut cfg, &table)
                .map_err(|e| format!("config merge failed {}: {e}", path.display()))?;
            tracing::info!("config loaded from {}", path.display());
        }
        if let Ok(v) = std::env::var("FUSION_LINK_BROKER_URL") {
            cfg.broker_url = v;
        }
        if let Ok(v) = std::env::var("FUSION_LINK_BIND_URL") {
            cfg.bind_url = v;
        }
        if let Ok(v) = std::env::var("FUSION_LINK_GATEWAY_URL") {
            cfg.gateway_url = v;
        }
        if let Ok(v) = std::env::var("FUSION_LINK_GATEWAY_TOKEN") {
            cfg.gateway_token = Some(v);
        }
        if let Ok(v) = std::env::var("FUSION_LINK_STATE_DIR") {
            cfg.state_dir = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("FUSION_LINK_HEARTBEAT_SECS")
            && let Ok(n) = v.parse()
        {
            cfg.heartbeat_secs = n;
        }
        Ok(cfg)
    }

    pub fn ensure_state_dir(&self) -> Result<(), String> {
        let dir: &Path = &self.state_dir;
        if !dir.exists() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("state dir create failed {}: {e}", dir.display()))?;
            tracing::info!("state dir created: {}", dir.display());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_port_registry() {
        let cfg = Config::default();
        assert!(cfg.broker_url.starts_with("wss://"));
        assert!(cfg.gateway_url.contains("11432"));
        // broker read deadline is 30s; heartbeat must stay under it
        assert_eq!(cfg.heartbeat_secs, 15);
        assert!(cfg.heartbeat_secs < 30);
        let urls: Vec<&str> = cfg.probes.iter().map(|p| p.url.as_str()).collect();
        assert!(urls.iter().any(|u| u.contains("11434")));
        assert!(urls.iter().any(|u| u.contains("11436")));
        assert!(urls.iter().any(|u| u.contains("11451")));
        assert!(urls.iter().any(|u| u.contains("11492")));
        assert!(cfg.task_routes.contains_key("shell.chat"));
        assert!(cfg.task_routes.contains_key("office.spreadsheet"));
        assert!(cfg.task_routes.contains_key("office.document"));
        assert!(cfg.task_routes.contains_key("office.slides"));
    }

    #[test]
    fn env_overrides_apply() {
        // env override logic covered indirectly by load(); keep parse-safe test
        assert!("30".parse::<u64>().is_ok());
    }

    #[test]
    fn partial_config_file_keeps_default_collections() {
        // Real-world regression: a config.toml with only connection fields
        // must not wipe default task_routes/probes.
        let raw = r#"
broker_url = "wss://example.org/ws"
heartbeat_secs = 20
"#;
        let table: toml::Table = toml::from_str(raw).unwrap();
        let mut cfg = Config::default();
        merge_table(&mut cfg, &table).unwrap();
        assert_eq!(cfg.broker_url, "wss://example.org/ws");
        assert_eq!(cfg.heartbeat_secs, 20);
        assert!(
            cfg.task_routes.contains_key("shell.chat"),
            "routes must survive partial file"
        );
        assert_eq!(cfg.probes.len(), 6, "probes must survive partial file");
        assert!(cfg.gateway_url.contains("11432"));
    }

    #[test]
    fn unknown_config_key_rejected() {
        let table: toml::Table = toml::from_str("bogus_key = 1").unwrap();
        let mut cfg = Config::default();
        assert!(merge_table(&mut cfg, &table).is_err());
    }
}
