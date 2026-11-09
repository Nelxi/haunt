//! Import saved sessions from PuTTY (registry) and OpenSSH (`~/.ssh/config`).
//!
//! Termius is not supported: its local database stores hosts encrypted with a key held by the
//! Termius account, so there is nothing readable on disk to import.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::config::{Auth, Proxy, Server};

/// Sessions found in one source. `jumps` links a server to the hosts it jumps through (first hop
/// first), resolved against the saved list by [`link_jumps`]. `unsupported` counts proxies haunt
/// cannot use (SOCKS4, telnet, local command, ProxyCommand); those are imported as direct.
#[derive(Default)]
pub struct Found {
    pub servers: Vec<Server>,
    pub jumps: Vec<(Uuid, Vec<String>)>,
    pub unsupported: usize,
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
        let method: u32 = key.get_value("ProxyMethod").unwrap_or(0);
        let p_host = get("ProxyHost").trim().to_string();
        let p_port = key.get_value::<u32, _>("ProxyPort").ok().and_then(|p| u16::try_from(p).ok()).filter(|p| *p != 0);
        let p_user = get("ProxyUsername").trim().to_string();

        let mut s = Server::new();
        match method {
            0 => {}
            2 if !p_host.is_empty() => s.proxy = Proxy::Socks5 { host: p_host, port: p_port.unwrap_or(1080), user: p_user },
            3 if !p_host.is_empty() => s.proxy = Proxy::Http { host: p_host, port: p_port.unwrap_or(3128), user: p_user },
            // SSH proxy (PuTTY 0.77+): the host field may be a saved session name or user@host.
            6 if !p_host.is_empty() => {
                let mut hop = p_host;
                if !p_user.is_empty() && !hop.contains('@') {
                    hop = format!("{p_user}@{hop}");
                }
                if let Some(port) = p_port.filter(|p| *p != 22) {
                    hop = format!("{hop}:{port}");
                }
                found.jumps.push((s.id, vec![hop]));
            }
            _ => found.unsupported += 1,
        }
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
        if b[i] == b'%' && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
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
    jump: Option<Vec<String>>,
    command: bool,
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
                    "proxyjump" if target.jump.is_none() && !target.command => {
                        let hops: Vec<String> = v.split(',').map(|h| h.trim().to_string()).filter(|h| !h.is_empty()).collect();
                        // "none" switches an inherited jump off.
                        target.jump = Some(if hops.len() == 1 && hops[0].eq_ignore_ascii_case("none") { vec![] } else { hops });
                    }
                    "proxycommand" if target.jump.is_none() && !target.command => {
                        target.command = !v.eq_ignore_ascii_case("none");
                        if !target.command {
                            target.jump = Some(vec![]);
                        }
                    }
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
        match b.jump.clone().or_else(|| if b.command { None } else { defaults.jump.clone() }) {
            Some(hops) if !hops.is_empty() => found.jumps.push((s.id, hops)),
            Some(_) => {}
            None if b.command || defaults.command => found.unsupported += 1,
            None => {}
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

/// Points imported servers at their jump hosts. A hop is a saved server's name or
/// `[user@]host[:port]`; hops that match nothing saved become new servers in `group`.
/// Servers that already have a proxy are left alone.
pub fn link_jumps(servers: &mut Vec<Server>, jumps: Vec<(Uuid, Vec<String>)>, group: &str) -> usize {
    let mut linked = 0;
    for (id, hops) in jumps {
        if !servers.iter().any(|s| s.id == id && s.proxy.is_none()) {
            continue;
        }
        let mut prev: Option<Uuid> = None;
        for hop in &hops {
            let hid = find_or_add(servers, hop, group);
            if let (Some(p), Some(h)) = (prev, servers.iter_mut().find(|s| s.id == hid))
                && h.proxy.is_none() && h.id != p {
                    h.proxy = Proxy::Jump { server: p };
                }
            prev = Some(hid);
        }
        if let (Some(p), Some(s)) = (prev, servers.iter_mut().find(|s| s.id == id))
            && p != id {
                s.proxy = Proxy::Jump { server: p };
                linked += 1;
            }
    }
    linked
}

fn find_or_add(servers: &mut Vec<Server>, hop: &str, group: &str) -> Uuid {
    if let Some(s) = servers.iter().find(|s| s.name.eq_ignore_ascii_case(hop)) {
        return s.id;
    }
    let (user, rest) = match hop.rsplit_once('@') {
        Some((u, r)) => (Some(u), r),
        None => (None, hop),
    };
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() && !h.contains(':') => (h, p.parse().ok()),
        _ => (rest, None),
    };
    let host = host.trim_matches(['[', ']']);
    if let Some(s) = servers.iter().find(|s| {
        (s.host.eq_ignore_ascii_case(host) || s.name.eq_ignore_ascii_case(host))
            && port.is_none_or(|p| p == s.port)
            && user.is_none_or(|u| u == s.user)
    }) {
        return s.id;
    }
    let mut s = Server::new();
    s.name = host.to_string();
    s.host = host.to_string();
    s.port = port.unwrap_or(22);
    if let Some(u) = user {
        s.user = u.to_string();
    }
    s.group = group.to_string();
    let id = s.id;
    servers.push(s);
    id
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
        assert_eq!(f.jumps.len(), 1);
        assert_eq!(f.unsupported, 0);
    }

    #[test]
    fn jumps_link_to_saved_and_new_hosts() {
        let cfg = "\
Host web
    HostName 203.0.113.5
Host db
    HostName db.internal
    ProxyJump web
Host deep
    HostName 10.0.0.9
    ProxyJump ops@bastion.example:2222,web
Host cmd
    HostName x
    ProxyCommand nc %h %p
";
        let f = parse_ssh_config(cfg, None);
        assert_eq!(f.unsupported, 1);
        let mut all = Vec::new();
        merge(&mut all, f.servers);
        assert_eq!(link_jumps(&mut all, f.jumps, "ssh config"), 2);
        let id = |n: &str| all.iter().find(|s| s.name == n).unwrap().id;
        let get = |n: &str| all.iter().find(|s| s.name == n).unwrap();
        assert_eq!(get("db").proxy, Proxy::Jump { server: id("web") });
        assert_eq!(get("deep").proxy, Proxy::Jump { server: id("web") });
        let bastion = get("bastion.example");
        assert_eq!((bastion.user.as_str(), bastion.port), ("ops", 2222));
        // web is both a host of its own and the second hop of deep: it gets the bastion in front.
        assert_eq!(get("web").proxy, Proxy::Jump { server: id("bastion.example") });
    }

    /// Reads the real registry: `cargo test putty_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn putty_live() {
        let f = putty();
        for s in &f.servers {
            println!("{} | user {} | port {} | key {}", s.name, s.user, s.port, matches!(s.auth, Auth::Key { .. }));
        }
        println!("jumps: {} unsupported: {}", f.jumps.len(), f.unsupported);
    }
}
