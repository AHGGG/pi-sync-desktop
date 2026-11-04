//! A deliberately small SSH grammar. Pasted text is data, never a command.
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionConfig {
    pub host: String,
    pub username: String,
    pub port: u16,
    pub key_path: Option<PathBuf>,
}
impl ConnectionConfig {
    pub fn parse(input: &str) -> Result<Self> {
        ensure!(input.len() <= 4096, "SSH connection text is too long");
        ensure!(
            !input
                .chars()
                .any(|c| c.is_control() || ";|&$`<>".contains(c)),
            "Shell syntax and control characters are not accepted"
        );
        let tokens = tokenize(input)?;
        ensure!(
            tokens.first().map(String::as_str) == Some("ssh"),
            "Use ssh user@host [-p port] [-i key-path]"
        );
        let mut destination = None;
        let mut port = None;
        let mut key_path = None;
        let mut i = 1;
        while i < tokens.len() {
            match tokens[i].as_str() {
                "-p" => {
                    ensure!(port.is_none(), "Duplicate port option");
                    i += 1;
                    let value = tokens
                        .get(i)
                        .ok_or_else(|| anyhow::anyhow!("Missing port"))?;
                    ensure!(
                        value.bytes().all(|c| c.is_ascii_digit()),
                        "Port must be numeric"
                    );
                    port = Some(
                        value
                            .parse::<u16>()
                            .map_err(|_| anyhow::anyhow!("Port must be 1–65535"))?,
                    );
                }
                "-i" => {
                    ensure!(key_path.is_none(), "Duplicate identity option");
                    i += 1;
                    let value = tokens
                        .get(i)
                        .ok_or_else(|| anyhow::anyhow!("Missing key path"))?;
                    ensure!(
                        !value.is_empty() && !value.starts_with('-'),
                        "Invalid key path"
                    );
                    key_path = Some(PathBuf::from(value));
                }
                value if value.starts_with('-') => bail!("Only -p and -i options are accepted"),
                value => {
                    ensure!(
                        destination.is_none(),
                        "Remote commands and extra arguments are not accepted"
                    );
                    destination = Some(value.to_owned());
                }
            }
            i += 1;
        }
        let destination = destination.ok_or_else(|| anyhow::anyhow!("Missing user@host"))?;
        let (username, host) = destination
            .split_once('@')
            .ok_or_else(|| anyhow::anyhow!("Specify user@host explicitly"))?;
        let host = if host.starts_with('[') {
            let inner = host
                .strip_prefix('[')
                .and_then(|v| v.strip_suffix(']'))
                .ok_or_else(|| anyhow::anyhow!("Invalid bracketed IPv6 host"))?;
            ensure!(
                inner.contains(':'),
                "Brackets are only accepted for IPv6 hosts"
            );
            inner
        } else {
            host
        };
        let config = Self {
            username: username.into(),
            host: host.into(),
            port: port.unwrap_or(22),
            key_path,
        };
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.port != 0, "Port must be 1–65535");
        ensure!(
            !self.username.is_empty()
                && self.username.len() <= 64
                && !self.username.starts_with('-')
                && self
                    .username
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)),
            "Invalid SSH username"
        );
        ensure!(
            !self.host.is_empty()
                && self.host.len() <= 253
                && !self.host.starts_with('-')
                && self
                    .host
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b".:-".contains(&c)),
            "Invalid SSH host"
        );
        if self.host.contains(':') {
            self.host
                .parse::<std::net::Ipv6Addr>()
                .map_err(|_| anyhow::anyhow!("Invalid IPv6 host"))?;
        }
        if let Some(path) = &self.key_path {
            let path = path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("Key path must be UTF-8"))?;
            ensure!(
                !path.is_empty() && !path.chars().any(char::is_control),
                "Invalid key path"
            );
        }
        Ok(())
    }
}
fn tokenize(input: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    for c in input.chars() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                token.push(c);
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == ' ' {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(c);
        }
    }
    ensure!(quote.is_none(), "Unclosed quote");
    if !token.is_empty() {
        tokens.push(token);
    }
    Ok(tokens)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_explicit_connection_and_quoted_key() {
        let c = ConnectionConfig::parse("ssh -i '/home/me/key file' pi@raspberry.local -p 2222")
            .unwrap();
        assert_eq!(c.host, "raspberry.local");
        assert_eq!(c.username, "pi");
        assert_eq!(c.port, 2222);
        assert_eq!(c.key_path, Some(PathBuf::from("/home/me/key file")));
    }
    #[test]
    fn accepts_literal_windows_identity_paths_only() {
        let config = ConnectionConfig::parse(r#"ssh pi@host -i "C:\Users\Me\key file""#).unwrap();
        assert_eq!(
            config.key_path,
            Some(PathBuf::from(r"C:\Users\Me\key file"))
        );
        for text in [
            r"ssh pi@ho\st",
            r"ssh p\i@host",
            r"ssh pi@host -p 2\2",
            r"ssh pi@host -i C:\key;id",
            r"ssh pi@host -i C:\key extra",
        ] {
            assert!(ConnectionConfig::parse(text).is_err());
        }
    }
    #[test]
    fn accepts_ipv6_and_default_port() {
        let c = ConnectionConfig::parse("ssh pi@[::1]").unwrap();
        assert_eq!(c.host, "::1");
        assert_eq!(c.port, 22);
    }
    #[test]
    fn refuses_commands_forwarding_proxy_and_injection() {
        for s in [
            "ssh pi@host ls",
            "ssh pi@host;id",
            "ssh pi@host $(id)",
            "ssh pi@host\nid",
            "ssh pi@host -L 80:host:80",
            "ssh -oProxyCommand=id pi@host",
            "ssh -J jump pi@host",
            "ssh pi@host -p 0",
            "ssh pi@host -p 65536",
            "ssh pi@host -p 22 -p 23",
            "ssh host",
            "ssh -bad@host",
            "ssh pi@host -i",
            "ssh pi@host -i 'unfinished",
            "ssh pi@host `id`",
            "ssh pi@[[::1]]",
            "ssh pi@[host]",
            "ssh pi@::1]",
        ] {
            assert!(ConnectionConfig::parse(s).is_err(), "accepted {s}");
        }
    }
}
