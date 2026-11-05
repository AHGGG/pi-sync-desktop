//! Authenticated SSH sessions. Only a fixed Node worker launcher can be executed.
//! All calls are blocking: invoke on a worker thread, and route prompts to the UI.
use crate::{connection::ConnectionConfig, profiles::write_private_atomic};
use anyhow::{Context, Result, bail, ensure};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ssh2::{CheckResult, ExtendedData, KnownHostFileKind, Session};
use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

#[derive(Clone, Debug)]
pub enum AuthPrompt {
    TrustHost {
        host: String,
        port: u16,
        fingerprint: String,
    },
    Password {
        username: String,
    },
    KeyPassphrase {
        key_path: PathBuf,
    },
}
// Intentionally neither Debug nor Serialize: answers must not reach logs or disk.
pub enum AuthAnswer {
    Trust(bool),
    Secret(Zeroizing<String>),
    Cancel,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostIdentity {
    Trusted,
    Unknown,
    Changed,
    Unverifiable,
}
/// A changed or unreadable key is never converted into a trust prompt.
pub fn require_host_trust(identity: HostIdentity, explicitly_accepted: bool) -> Result<()> {
    match identity {
        HostIdentity::Trusted => Ok(()),
        HostIdentity::Unknown if explicitly_accepted => Ok(()),
        HostIdentity::Unknown => bail!("Host identity was not accepted"),
        HostIdentity::Changed => {
            bail!("Host key changed. Connection blocked; verify the server independently")
        }
        HostIdentity::Unverifiable => bail!("Cannot verify host identity"),
    }
}
pub struct SshTransport {
    session: Session,
}
impl SshTransport {
    pub fn connect(
        config: &ConnectionConfig,
        known_hosts_path: &Path,
        mut prompt: impl FnMut(AuthPrompt) -> AuthAnswer,
    ) -> Result<Self> {
        config.validate()?;
        let addresses = (config.host.as_str(), config.port)
            .to_socket_addrs()
            .context("Cannot resolve SSH host")?;
        let mut socket = None;
        for address in addresses.take(8) {
            if let Ok(s) = TcpStream::connect_timeout(&address, Duration::from_secs(10)) {
                socket = Some(s);
                break;
            }
        }
        let socket = socket.context("Cannot connect to SSH host")?;
        socket.set_read_timeout(Some(Duration::from_secs(30)))?;
        socket.set_write_timeout(Some(Duration::from_secs(30)))?;
        let mut session = Session::new()?;
        session.set_tcp_stream(socket);
        session.set_timeout(30_000);
        session.handshake().context("SSH handshake failed")?;
        let (key, key_type) = session
            .host_key()
            .context("SSH server did not provide a host key")?;
        let fingerprint = format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(key)));
        let mut hosts = session.known_hosts()?;
        // Honor existing OpenSSH trust as well as the app's explicit trust store.
        if let Some(base) = directories::BaseDirs::new() {
            let system = base.home_dir().join(".ssh/known_hosts");
            read_known_hosts(&mut hosts, &system)?;
        }
        read_known_hosts(&mut hosts, known_hosts_path)?;
        let identity = match hosts.check_port(&config.host, config.port, key) {
            CheckResult::Match => HostIdentity::Trusted,
            CheckResult::NotFound => HostIdentity::Unknown,
            CheckResult::Mismatch => HostIdentity::Changed,
            CheckResult::Failure => HostIdentity::Unverifiable,
        };
        let accepted = if identity == HostIdentity::Unknown {
            matches!(
                prompt(AuthPrompt::TrustHost {
                    host: config.host.clone(),
                    port: config.port,
                    fingerprint
                }),
                AuthAnswer::Trust(true)
            )
        } else {
            false
        };
        require_host_trust(identity, accepted)?;
        if identity == HostIdentity::Unknown {
            let host = if config.port == 22 {
                config.host.clone()
            } else {
                format!("[{}]:{}", config.host, config.port)
            };
            hosts.add(&host, key, "pi-sync-desktop", key_type.into())?;
            let mut contents = String::new();
            for entry in hosts.iter()? {
                contents.push_str(&hosts.write_string(&entry, KnownHostFileKind::OpenSSH)?);
                if !contents.ends_with('\n') {
                    contents.push('\n');
                }
            }
            write_private_atomic(known_hosts_path, contents.as_bytes())?;
        }
        // Key material is read by libssh2. No identity/password enters child argv.
        if let Some(path) = &config.key_path {
            let path = expand_home(path)?;
            if session
                .userauth_pubkey_file(&config.username, None, &path, None)
                .is_err()
            {
                match prompt(AuthPrompt::KeyPassphrase {
                    key_path: path.clone(),
                }) {
                    AuthAnswer::Secret(secret) => {
                        session
                            .userauth_pubkey_file(
                                &config.username,
                                None,
                                &path,
                                Some(secret.as_str()),
                            )
                            .map_err(|_| anyhow::anyhow!("SSH key authentication failed"))?;
                    }
                    _ => bail!("Authentication cancelled"),
                }
            }
        } else {
            if let Ok(mut agent) = session.agent()
                && agent.connect().is_ok()
                && agent.list_identities().is_ok()
                && let Ok(identities) = agent.identities()
            {
                for identity in identities {
                    if agent.userauth(&config.username, &identity).is_ok() {
                        break;
                    }
                }
            }
            if !session.authenticated() {
                // Try conventional unencrypted keys before prompting for a password.
                if let Some(base) = directories::BaseDirs::new() {
                    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
                        let path = base.home_dir().join(".ssh").join(name);
                        if path.is_file()
                            && session
                                .userauth_pubkey_file(&config.username, None, &path, None)
                                .is_ok()
                        {
                            break;
                        }
                    }
                }
            }
            if !session.authenticated() {
                match prompt(AuthPrompt::Password {
                    username: config.username.clone(),
                }) {
                    AuthAnswer::Secret(secret) => session
                        .userauth_password(&config.username, secret.as_str())
                        .map_err(|_| anyhow::anyhow!("SSH password authentication failed"))?,
                    _ => bail!("Authentication cancelled"),
                }
            }
        }
        ensure!(session.authenticated(), "SSH authentication failed");
        drop(hosts);
        Ok(Self { session })
    }
    pub fn run_worker(&mut self, worker_source: &str, request: &Value) -> Result<Value> {
        // Keep in sync with backend/worker.mjs MAX_PROTOCOL_BYTES. This bounds the
        // combined audited documents, models and base64 custom-file envelope.
        const MAX: usize = 48 * 1024 * 1024;
        // Fixed launcher only; user input and worker source travel solely over stdin.
        const COMMAND: &str = "node -e \"let s='';process.stdin.setEncoding('utf8');process.stdin.on('data',c=>{s+=c;if(s.length>50331648)process.exit(2)});process.stdin.on('end',async()=>{try{let p=JSON.parse(s);let m=await import('data:text/javascript;base64,'+p.code);console.log(JSON.stringify(await m.handle(p.request)))}catch{console.log(JSON.stringify({ok:false,error:{code:'REMOTE_FAILED'}}))}})\"";
        let payload = Zeroizing::new(serde_json::to_vec(
            &json!({"code":STANDARD.encode(worker_source),"request":request}),
        )?);
        ensure!(payload.len() <= MAX, "Remote request exceeds size limit");
        let mut channel = self
            .session
            .channel_session()
            .context("Cannot open SSH worker channel")?;
        channel.handle_extended_data(ExtendedData::Ignore)?;
        channel
            .exec(COMMAND)
            .context("Cannot start remote Node worker")?;
        channel
            .write_all(&payload)
            .context("Cannot send remote request")?;
        channel.send_eof()?;
        let start = Instant::now();
        let mut bytes = Zeroizing::new(Vec::new());
        let mut chunk = Zeroizing::new([0u8; 8192]);
        loop {
            ensure!(
                start.elapsed() < Duration::from_secs(60),
                "Remote worker timed out"
            );
            let n = channel
                .read(&mut chunk[..])
                .context("Cannot read remote worker response")?;
            if n == 0 {
                break;
            }
            ensure!(
                bytes.len() + n <= MAX,
                "Remote worker response exceeds size limit"
            );
            bytes.extend_from_slice(&chunk[..n]);
        }
        channel.wait_close()?;
        ensure!(
            channel.exit_status()? == 0,
            "Remote worker failed; check Node.js installation on the server"
        );
        let response: Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("Invalid remote worker response"))?;
        ensure!(
            response.get("ok").is_some_and(Value::is_boolean),
            "Invalid remote response envelope"
        );
        Ok(response)
    }
}
fn read_known_hosts(hosts: &mut ssh2::KnownHosts, path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            hosts
                .read_file(path, KnownHostFileKind::OpenSSH)
                .context("Cannot read known hosts; connection blocked")?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => bail!("Cannot inspect known hosts; connection blocked"),
    }
    Ok(())
}
fn expand_home(path: &Path) -> Result<PathBuf> {
    if let Ok(suffix) = path.strip_prefix("~") {
        Ok(directories::BaseDirs::new()
            .context("Cannot locate home directory")?
            .home_dir()
            .join(suffix))
    } else {
        Ok(path.to_owned())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_identity_requires_explicit_acceptance() {
        assert!(require_host_trust(HostIdentity::Unknown, false).is_err());
        assert!(require_host_trust(HostIdentity::Unknown, true).is_ok());
        assert!(require_host_trust(HostIdentity::Trusted, false).is_ok());
    }
    #[test]
    fn changed_and_unverifiable_identity_cannot_be_overridden() {
        for status in [HostIdentity::Changed, HostIdentity::Unverifiable] {
            for accepted in [true, false] {
                assert!(require_host_trust(status, accepted).is_err());
            }
        }
    }
}
