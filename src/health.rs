use crate::config::ProbeCfg;
use crate::protocol::{HealthSnapshot, ServiceHealth};
use std::time::Duration;

pub async fn snapshot(client: &reqwest::Client, probes: &[ProbeCfg]) -> HealthSnapshot {
    let mut tasks = Vec::with_capacity(probes.len());
    for probe in probes {
        tasks.push(probe_one(client, probe));
    }
    let results = futures_util::future::join_all(tasks).await;
    let services = results
        .into_iter()
        .map(|(name, ok, detail)| ServiceHealth { name, ok, detail })
        .collect();
    HealthSnapshot {
        services,
        generated_at: crate::protocol::now_millis(),
    }
}

async fn probe_one(client: &reqwest::Client, probe: &ProbeCfg) -> (String, bool, Option<String>) {
    match client
        .get(&probe.url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
    {
        Ok(resp) => {
            let ok = resp.status().is_success();
            let detail = if ok {
                None
            } else {
                Some(format!("http {}", resp.status().as_u16()))
            };
            (probe.name.clone(), ok, detail)
        }
        Err(e) => {
            tracing::debug!("probe {} failed: {e}", probe.name);
            (probe.name.clone(), false, Some("unreachable".to_string()))
        }
    }
}

pub fn new_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("health client build")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn snapshot_reports_up_and_down_services() {
        let ok_mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/health"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .mount(&ok_mock)
            .await;

        let probes = vec![
            ProbeCfg {
                name: "up-svc".into(),
                url: format!("{}/health", ok_mock.uri()),
            },
            ProbeCfg {
                name: "down-svc".into(),
                url: "http://127.0.0.1:1/health".into(),
            },
        ];
        let snap = snapshot(&new_client(), &probes).await;
        assert_eq!(snap.services.len(), 2);
        let up = snap.services.iter().find(|s| s.name == "up-svc").unwrap();
        let down = snap.services.iter().find(|s| s.name == "down-svc").unwrap();
        assert!(up.ok);
        assert!(!down.ok);
        assert!(down.detail.as_deref() == Some("unreachable"));
    }

    #[tokio::test]
    async fn http_error_status_counts_as_down() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(503))
            .mount(&mock)
            .await;
        let probes = vec![ProbeCfg {
            name: "err-svc".into(),
            url: mock.uri(),
        }];
        let snap = snapshot(&new_client(), &probes).await;
        assert!(!snap.services[0].ok);
        assert_eq!(snap.services[0].detail.as_deref(), Some("http 503"));
    }
}
