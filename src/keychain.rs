use serde::{Deserialize, Serialize};

const SERVICE: &str = "fusion-link";
const KEY_ACCOUNT: &str = "device";

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

// The daemon credential lives under a fixed service+account pair so re-binds
// update in place and lookups are unambiguous even if other entries with the
// same service exist (e.g. entries written by older builds).
pub fn store(cred: &Credential) -> Result<(), String> {
    store_in(SERVICE, cred)
}

pub fn load() -> Result<Credential, String> {
    match load_from(SERVICE, KEY_ACCOUNT) {
        Ok(cred) => Ok(cred),
        Err(new_err) => match load_legacy() {
            // Older builds stored under account = device_id; migrate in place.
            Ok(legacy) => {
                tracing::info!(
                    "migrating keychain credential to fixed account (device {})",
                    legacy.device_id
                );
                store_in(SERVICE, &legacy)?;
                let _ = run_security(&[
                    "delete-generic-password",
                    "-s",
                    SERVICE,
                    "-a",
                    &legacy.device_id,
                ]);
                Ok(legacy)
            }
            Err(_) => Err(new_err),
        },
    }
}

pub fn delete() -> Result<(), String> {
    match run_security(&["delete-generic-password", "-s", SERVICE, "-a", KEY_ACCOUNT]) {
        Ok(_) => {
            tracing::info!("credential deleted from keychain");
            Ok(())
        }
        Err(e) if e.contains("could not be found") => Ok(()),
        Err(e) => Err(e),
    }
}

fn store_in(service: &str, cred: &Credential) -> Result<(), String> {
    let secret =
        serde_json::to_string(cred).map_err(|e| format!("credential serialize failed: {e}"))?;
    run_security(&[
        "add-generic-password",
        "-U",
        "-s",
        service,
        "-a",
        KEY_ACCOUNT,
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

fn load_from(service: &str, account: &str) -> Result<Credential, String> {
    let raw = run_security(&["find-generic-password", "-s", service, "-a", account, "-w"])?;
    serde_json::from_str(&raw).map_err(|e| format!("credential parse failed: {e}"))
}

fn load_legacy() -> Result<Credential, String> {
    let raw = run_security(&["find-generic-password", "-s", SERVICE, "-w"])?;
    serde_json::from_str(&raw).map_err(|e| format!("credential parse failed: {e}"))
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
    // macOS-only: security CLI + ioreg do not exist on Linux CI runners.
    #[cfg(target_os = "macos")]
    use super::*;

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
        // Isolated service: never touches the real daemon credential entry.
        let service = "fusion-link-test";
        let cred = Credential {
            device_id: format!("test-{}", std::process::id()),
            token: "tok-abc".into(),
            fingerprint: "fp-test".into(),
        };
        store_in(service, &cred).unwrap();
        let loaded = load_from(service, KEY_ACCOUNT).unwrap();
        assert_eq!(loaded.device_id, cred.device_id);
        assert_eq!(loaded.token, cred.token);
        run_security(&["delete-generic-password", "-s", service, "-a", KEY_ACCOUNT]).unwrap();
        assert!(load_from(service, KEY_ACCOUNT).is_err());
    }
}
