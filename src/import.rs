//! Import saved sessions from PuTTY (registry) and OpenSSH (`~/.ssh/config`).
//!
//! Termius is not supported: its local database stores hosts encrypted with a key held by the
//! Termius account, so there is nothing readable on disk to import.

use std::path::{Path, PathBuf};

use crate::config::{Auth, Server};

/// Sessions found in one source. `jump` counts entries that go through a jump host / proxy;
/// they are imported as direct connections.
#[derive(Default)]
pub struct Found {
    pub servers: Vec<Server>,
    pub jump: usize,
}

/// Saved PuTTY sessions from `HKCU\Software\SimonTatham\PuTTY\Sessions`.
#[cfg(windows)]
pub fn putty() -> Found {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let mut found = Found::default();
    let Ok(root) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\SimonTatham\PuTTY\Sessions") else {
        return found;
    };
    for raw_name in root.enum_keys().flatten() {
        let Ok(key) = root.open_subkey(&raw_name) else { continue };
        let get = |name: &str| key.get_value::<String, _>(name).unwrap_or_default();
        let protocol = get("Protocol");
        if !protocol.is_empty() && protocol != "ssh" {
            continue;
        }
        let mut host = get("HostName").trim().to_string();
        if host.is_empty() {
            continue; // "Default Settings" and other templates
        }
        let mut user = get("UserName").trim().to_string();
        // PuTTY allows "user@host" in the host field.
        if let Some((u, h)) = host.clone().rsplit_once('@') {
            if user.is_empty() {
                user = u.to_string();
            }
            host = h.to_string();
        }
        let port = key.get_value::<u32, _>("PortNumber").ok().and_then(|p| u16::try_from(p).ok()).filter(|p| *p != 0);
        let key_file = get("PublicKeyFile");
        let proxy: u32 = key.get_value("ProxyMethod").unwrap_or(0);
        if proxy != 0 {
            found.jump += 1;
        }

        let mut s = Server::new();
        s.name = unescape_putty(&raw_name);
        s.host = host;
        s.port = port.unwrap_or(22);
        if !user.is_empty() {
            s.user = user;
        }
        if !key_file.trim().is_empty() {
            s.auth = Auth::Key { path: PathBuf::from(key_file.trim()) };
        }
        s.group = "PuTTY".into();
        found.servers.push(s);
    }
    found
}

#[cfg(not(windows))]
pub fn putty() -> Found {
    Found::default()
}

/// PuTTY stores session names percent-encoded ("my%20box").
fn unescape_putty(name: &str) -> String {
    let b = name.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn ssh_config_path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().join(".ssh").join("config"))
}

/// Concrete `Host` blocks from `~/.ssh/config`. Wildcard patterns are skipped; `Host *`
/// defaults for `User`/`Port`/`IdentityFile` are applied to every entry.
pub fn ssh_config() -> Found {
    let Some(path) = ssh_config_path() else { return Found::default() };
    let Ok(text) = std::fs::read_to_string(&path) else { return Found::default() };
    let home = path.parent().and_then(Path::parent).map(Path::to_path_buf);
    parse_ssh_config(&text, home.as_deref())
}

#[derive(Default, Clone)]
struct Block {
    aliases: Vec<String>,
    host: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity: Option<String>,
    jump: bool,
}

fn parse_ssh_config(text: &str, home: Option<&Path>) -> Found {
    let mut blocks: Vec<Block> = Vec::new();
    let mut defaults = Block::default();
    let mut cur: Option<Block> = None;
    let mut in_match = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (k.to_ascii_lowercase(), v.trim_start_matches(|c: char| c.is_whitespace() || c == '=').trim()),
            None => continue,
        };
        let v = v.trim_matches('"').to_string();
        match k.as_str() {
            "host" => {
                if let Some(b) = cur.take() {
                    blocks.push(b);
                }
                in_match = false;
                let pats: Vec<String> = v.split_whitespace().map(str::to_string).collect();
                if pats.iter().all(|p| p == "*") {
                    // `Host *`: following options become defaults for every entry.
                    continue;
                }
                let concrete: Vec<String> = pats.into_iter().filter(|p| !p.contains(['*', '?', '!'])).collect();
                cur = Some(Block { aliases: concrete, ..Default::default() });
                if cur.as_ref().is_some_and(|b| b.aliases.is_empty()) {
                    in_match = true; // pattern-only block: ignore its options
                }
            }
            "match" => {
                if let Some(b) = cur.take() {
                    blocks.push(b);
                }
                in_match = true;
            }
            _ if in_match => {}
            _ => {
                let target = match cur.as_mut() {
                    Some(b) => b,
                    None => &mut defaults,
                };
                match k.as_str() {
                    "hostname" if target.host.is_none() => target.host = Some(v),
                    "user" if target.user.is_none() => target.user = Some(v),
                    "port" if target.port.is_none() => target.port = v.parse().ok(),
                    "identityfile" if target.identity.is_none() => target.identity = Some(v),
                    "proxyjump" | "proxycommand" => target.jump = true,
                    _ => {}
                }
            }
        }
    }
    if let Some(b) = cur.take() {
        blocks.push(b);
    }

    let mut found = Found::default();
    for b in blocks {
        if b.aliases.is_empty() {
            continue;
        }
        let alias = b.aliases[0].clone();
        let mut s = Server::new();
        s.name = alias.clone();
        s.host = b.host.unwrap_or(alias).replace("%%", "%");
        s.port = b.port.or(defaults.port).unwrap_or(22);
        if let Some(u) = b.user.or_else(|| defaults.user.clone()) {
            s.user = u;
        }
        if let Some(id) = b.identity.or_else(|| defaults.identity.clone()) {
            s.auth = Auth::Key { path: expand_home(&id, home) };
        }
        if b.jump || defaults.jump {
            found.jump += 1;
        }
        s.group = "ssh config".into();
        found.servers.push(s);
    }
    found
}

fn expand_home(p: &str, home: Option<&Path>) -> PathBuf {
    match (p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")), home) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => PathBuf::from(p),
    }
}

/// Appends servers that are not already saved (same host, port and user). Returns how many were added.
pub fn merge(existing: &mut Vec<Server>, new: Vec<Server>) -> usize {
    let mut added = 0;
    for s in new {
        let dup = existing
            .iter()
            .any(|e| e.host.eq_ignore_ascii_case(&s.host) && e.port == s.port && e.user == s.user);
        if !dup {
            existing.push(s);
            added += 1;
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn putty_names() {
        assert_eq!(unescape_putty("my%20box"), "my box");
        assert_eq!(unescape_putty("100%"), "100%");
        assert_eq!(unescape_putty("a%2"), "a%2");
    }

    #[test]
    fn ssh_config_blocks() {
        let cfg = "\
Host *
    User admin
    IdentityFile ~/.ssh/id_ed25519

Host web web.prod
    HostName 203.0.113.5
    Port 2200

Host db
    HostName db.internal
    User postgres
    ProxyJump web

Host *.lan
    User pi

Match host foo
    User nobody
";
        let f = parse_ssh_config(cfg, Some(Path::new("C:/Users/me")));
        assert_eq!(f.servers.len(), 2);
        let web = &f.servers[0];
        assert_eq!((web.name.as_str(), web.host.as_str(), web.port, web.user.as_str()), ("web", "203.0.113.5", 2200, "admin"));
        assert_eq!(web.auth, Auth::Key { path: Path::new("C:/Users/me").join(".ssh/id_ed25519") });
        let db = &f.servers[1];
        assert_eq!((db.host.as_str(), db.port, db.user.as_str()), ("db.internal", 22, "postgres"));
        assert_eq!(f.jump, 1);
    }

    /// Reads the real registry: `cargo test putty_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn putty_live() {
        let f = putty();
        for s in &f.servers {
            println!("{} | user {} | port {} | key {}", s.name, s.user, s.port, matches!(s.auth, Auth::Key { .. }));
        }
        println!("jump: {}", f.jump);
    }
}
