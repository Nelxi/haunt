//! Passwords and key passphrases in the OS credential store
//! (Windows Credential Manager). Nothing secret is ever written to config.toml.

use uuid::Uuid;

const SERVICE: &str = "haunt";

fn entry(server: Uuid) -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, &server.to_string())
        .map_err(|e| log::warn!("credential store unavailable: {e}"))
        .ok()
}

pub fn get(server: Uuid) -> Option<String> {
    entry(server)?.get_password().ok()
}

pub fn set(server: Uuid, secret: &str) -> Result<(), String> {
    entry(server)
        .ok_or_else(|| "credential store unavailable".to_string())?
        .set_password(secret)
        .map_err(|e| e.to_string())
}

pub fn delete(server: Uuid) {
    if let Some(e) = entry(server) {
        let _ = e.delete_credential();
    }
}
