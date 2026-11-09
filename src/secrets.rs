//! Passwords and key passphrases in the OS credential store
//! (Windows Credential Manager). Nothing secret is ever written to config.toml.

use uuid::Uuid;

const SERVICE: &str = "haunt";

fn entry(server: Uuid) -> Option<keyring::Entry> {
    entry_named(&server.to_string())
}

/// The proxy password of a server is kept next to its own secret.
fn proxy_name(server: Uuid) -> String {
    format!("{server}:proxy")
}

fn entry_named(name: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, name)
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
    for e in [entry(server), entry_named(&proxy_name(server))].into_iter().flatten() {
        let _ = e.delete_credential();
    }
}

pub fn get_proxy(server: Uuid) -> Option<String> {
    entry_named(&proxy_name(server))?.get_password().ok()
}

pub fn set_proxy(server: Uuid, secret: &str) -> Result<(), String> {
    entry_named(&proxy_name(server))
        .ok_or_else(|| "credential store unavailable".to_string())?
        .set_password(secret)
        .map_err(|e| e.to_string())
}

pub fn delete_proxy(server: Uuid) {
    if let Some(e) = entry_named(&proxy_name(server)) {
        let _ = e.delete_credential();
    }
}
