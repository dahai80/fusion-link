use serde::{Deserialize, Serialize};

const SERVICE: &str = "fusion-link";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub device_id: String,
    pub token: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindResponse {
    pub device_id: String,
    pub token: String,
}

fn run_security(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("security")
        .args(args)
        .output()
        .map_err(|e| format!("security spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "security {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn store(cred: &Credential) -> Result<(), String> {
    let secret =
        serde_json::to_string(cred).map_err(|e| format!("credential serialize failed: {e}"))?;
    run_security(&[
        "add-generic-password",
        "-U",
        "-s",
        SERVICE,
        "-a",
        &cred.device_id,
        "-w",
        &secret,
    ])
    .map(|_| {
        tracing::info!(
            "credential stored in keychain for device {}",
            cred.device_id
        );
    })
}

pub fn load() -> Result<Credential, String> {
    let raw = run_security(&["find-generic-password", "-s", SERVICE, "-w"])?;
    serde_json::from_str(&raw).map_err(|e| format!("credential parse failed: {e}"))
}

pub fn delete() -> Result<(), String> {
    match run_security(&["delete-generic-password", "-s", SERVICE]) {
        Ok(_) => {
            tracing::info!("credential deleted from keychain");
            Ok(())
        }
        Err(e) if e.contains("could not be found") => Ok(()),
        Err(e) => Err(e),
    }
}

/// Stable device fingerprint: IOPlatformUUID (no hardware serial exposure).
pub fn device_fingerprint() -> Result<String, String> {
    let out = std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .map_err(|e| format!("ioreg spawn failed: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(idx) = line.find("IOPlatformUUID")
            && let Some(eq) = line[idx..].find('=')
        {
            let val = line[idx + eq..].trim_start_matches(['=', ' ', '"']);
            let val = val.trim_end_matches(['"', ';']);
            return Ok(format!("fp-{}", &val.to_lowercase().replace('-', "")[..16]));
        }
    }
    Err("IOPlatformUUID not found in ioreg output".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // macOS-only: security CLI + ioreg do not exist on Linux CI runners.
    #[cfg(target_os = "macos")]
    #[test]
    fn fingerprint_is_deterministic_and_prefixed() {
        let fp1 = device_fingerprint().unwrap();
        let fp2 = device_fingerprint().unwrap();
        assert_eq!(fp1, fp2);
        assert!(fp1.starts_with("fp-"));
        assert_eq!(fp1.len(), 19);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_roundtrip() {
        let cred = Credential {
            device_id: format!("test-{}", std::process::id()),
            token: "tok-abc".into(),
            fingerprint: "fp-test".into(),
        };
        store(&cred).unwrap();
        let loaded = load().unwrap();
        assert_eq!(loaded.device_id, cred.device_id);
        assert_eq!(loaded.token, cred.token);
        delete().unwrap();
        assert!(load().is_err());
    }
}
