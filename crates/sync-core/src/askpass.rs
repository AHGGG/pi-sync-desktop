//! OpenSSH askpass bridge. A per-process random capability authenticates bounded
//! loopback requests; neither credentials nor capabilities are written to disk.
use crate::{
    connection::ConnectionConfig,
    transport::{AuthAnswer, AuthPrompt},
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

const ENDPOINT: &str = "PI_SYNC_ASKPASS_ENDPOINT";
const CAPABILITY: &str = "PI_SYNC_ASKPASS_CAPABILITY";
const MAX_PROMPT: usize = 8192;
const MAX_SECRET: usize = 16384;
const IO_TIMEOUT: Duration = Duration::from_millis(250);

pub struct Askpass {
    listener: TcpListener,
    capability: Zeroizing<[u8; 32]>,
    config: ConnectionConfig,
    executable: PathBuf,
}
impl Askpass {
    pub fn new(config: &ConnectionConfig, executable: &Path) -> Result<Self> {
        config.validate()?;
        ensure!(
            executable.is_absolute(),
            "Askpass executable must be absolute"
        );
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .context("Cannot start authentication bridge")?;
        listener.set_nonblocking(true)?;
        let mut capability = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut *capability)
            .map_err(|_| anyhow::anyhow!("Cannot create authentication capability"))?;
        Ok(Self {
            listener,
            capability,
            config: config.clone(),
            executable: executable.to_owned(),
        })
    }
    pub fn configure(&self, command: &mut Command) {
        command
            .env("SSH_ASKPASS", &self.executable)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("DISPLAY", "pi-sync-askpass")
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .env(
                ENDPOINT,
                self.listener
                    .local_addr()
                    .expect("bound listener")
                    .to_string(),
            )
            .env(CAPABILITY, STANDARD_NO_PAD.encode(*self.capability));
    }
    /// Service at most one bounded request. Returns true for a user cancellation
    /// or a rejected/unrecognized authenticated prompt; caller must kill SSH.
    pub fn poll(&mut self, prompt: &mut impl FnMut(AuthPrompt) -> AuthAnswer) -> Result<bool> {
        let (mut stream, _) = match self.listener.accept() {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        // BSD/macOS can inherit O_NONBLOCK from the listening socket. Explicitly
        // restore blocking reads before applying the absolute request deadline.
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let mut supplied = Zeroizing::new([0u8; 32]);
        let deadline = Instant::now() + IO_TIMEOUT;
        if read_before(&mut stream, &mut *supplied, deadline).is_err() {
            return Ok(false);
        }
        let difference = supplied
            .iter()
            .zip(self.capability.iter())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b));
        if difference != 0 {
            return Ok(false);
        }
        let raw = match read_request(&mut stream, deadline) {
            Ok(raw) => raw,
            Err(_) => return Ok(true),
        };
        let parsed = std::str::from_utf8(&raw)
            .ok()
            .and_then(|raw| parse_prompt(raw, &self.config));
        let Some(parsed) = parsed else {
            let _ = stream.write_all(&[0]);
            return Ok(true);
        };
        let is_trust = matches!(parsed, AuthPrompt::TrustHost { .. });
        let response = match prompt(parsed) {
            AuthAnswer::Trust(true) if is_trust => Zeroizing::new("yes".to_owned()),
            AuthAnswer::Secret(secret)
                if !is_trust
                    && secret.len() <= MAX_SECRET
                    && !secret.contains(['\n', '\r', '\0']) =>
            {
                secret
            }
            _ => {
                let _ = stream.write_all(&[0]);
                return Ok(true);
            }
        };
        stream.write_all(&[1])?;
        write_frame(&mut stream, response.as_bytes())?;
        Ok(false)
    }
}

/// Call before GUI/runtime startup in every executable that launches transport.
/// No diagnostic includes prompts, environment values, or authentication data.
pub fn run_helper_if_requested() -> Option<i32> {
    std::env::var_os(ENDPOINT)?;
    Some(if run_helper().is_ok() { 0 } else { 1 })
}
fn run_helper() -> Result<()> {
    let endpoint: SocketAddr = std::env::var(ENDPOINT)?.parse()?;
    ensure!(
        endpoint.ip() == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        "Invalid authentication endpoint"
    );
    let encoded = Zeroizing::new(std::env::var(CAPABILITY)?);
    let capability = Zeroizing::new(STANDARD_NO_PAD.decode(encoded.as_bytes())?);
    ensure!(capability.len() == 32, "Invalid authentication capability");
    let raw = Zeroizing::new(
        std::env::args_os()
            .nth(1)
            .context("Missing authentication prompt")?
            .into_string()
            .map_err(|_| anyhow::anyhow!("Invalid authentication prompt"))?,
    );
    ensure!(raw.len() <= MAX_PROMPT, "Authentication prompt too large");
    let mut stream = TcpStream::connect_timeout(&endpoint, Duration::from_secs(2))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    // User interaction can take minutes. Parent owns cancellation and kills SSH;
    // a dropped active bridge closes this socket so the helper exits too.
    stream.set_read_timeout(Some(Duration::from_secs(600)))?;
    stream.write_all(&capability)?;
    write_frame(&mut stream, raw.as_bytes())?;
    let mut status = [0];
    stream.read_exact(&mut status)?;
    ensure!(status == [1], "Authentication cancelled");
    let response = read_frame(&mut stream, MAX_SECRET)?;
    ensure!(
        !response.iter().any(|b| matches!(b, b'\n' | b'\r' | 0)),
        "Invalid authentication response"
    );
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&response)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
fn read_before(stream: &mut TcpStream, mut bytes: &mut [u8], deadline: Instant) -> Result<()> {
    while !bytes.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .context("Authentication request timed out")?;
        ensure!(!remaining.is_zero(), "Authentication request timed out");
        stream.set_read_timeout(Some(remaining))?;
        let count = stream.read(bytes)?;
        ensure!(count != 0, "Authentication request closed");
        bytes = &mut bytes[count..];
    }
    Ok(())
}
fn read_request(stream: &mut TcpStream, deadline: Instant) -> Result<Zeroizing<Vec<u8>>> {
    let mut size = [0u8; 4];
    read_before(stream, &mut size, deadline)?;
    let size = u32::from_be_bytes(size) as usize;
    ensure!(size <= MAX_PROMPT, "Authentication frame too large");
    let mut data = Zeroizing::new(vec![0; size]);
    read_before(stream, &mut data, deadline)?;
    Ok(data)
}
fn read_frame(stream: &mut impl Read, limit: usize) -> Result<Zeroizing<Vec<u8>>> {
    let mut size = [0u8; 4];
    stream.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    ensure!(size <= limit, "Authentication frame too large");
    let mut data = Zeroizing::new(vec![0; size]);
    stream.read_exact(&mut data)?;
    Ok(data)
}
fn write_frame(stream: &mut impl Write, bytes: &[u8]) -> Result<()> {
    stream.write_all(&(u32::try_from(bytes.len())?).to_be_bytes())?;
    stream.write_all(bytes)?;
    Ok(())
}

fn safe_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 1024
        && host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-_[]:()%".contains(&c) || c == b' ')
}
fn parse_prompt(raw: &str, config: &ConnectionConfig) -> Option<AuthPrompt> {
    if raw.len() > MAX_PROMPT || raw.chars().any(|c| c.is_control() && c != '\n') {
        return None;
    }
    let raw = raw.trim();
    if let Some(destination) = raw.strip_suffix("'s password:") {
        let (username, host) = destination.split_once('@')?;
        let safe_user = !username.is_empty()
            && username.len() <= 64
            && username
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c));
        if safe_user && (config.use_config_user || username == config.username) && safe_host(host) {
            return Some(AuthPrompt::Password {
                username: username.to_owned(),
            });
        }
        return None;
    }
    if let Some(path) = raw
        .strip_prefix("Enter passphrase for key '")
        .and_then(|s| s.strip_suffix("':"))
    {
        if path.is_empty() || path.contains('\n') || path.chars().any(char::is_control) {
            return None;
        }
        return Some(AuthPrompt::KeyPassphrase {
            key_path: path.into(),
        });
    }
    let host = raw
        .strip_prefix("The authenticity of host '")?
        .split_once("' can't be established.\n")?;
    if !safe_host(host.0)
        || !host
            .1
            .ends_with("Are you sure you want to continue connecting (yes/no/[fingerprint])?")
    {
        return None;
    }
    let line = host.1.lines().next()?;
    let (algorithm, fingerprint) = line.split_once(" key fingerprint is SHA256:")?;
    if !matches!(
        algorithm,
        "ED25519" | "RSA" | "ECDSA" | "DSA" | "ECDSA-SK" | "ED25519-SK"
    ) {
        return None;
    }
    let fingerprint = fingerprint.strip_suffix('.')?;
    let decoded = STANDARD_NO_PAD.decode(fingerprint).ok()?;
    if decoded.len() != 32 || STANDARD_NO_PAD.encode(&decoded) != fingerprint {
        return None;
    }
    Some(AuthPrompt::TrustHost {
        host: host.0.to_owned(),
        port: config.port,
        fingerprint: format!("SHA256:{fingerprint}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> ConnectionConfig {
        ConnectionConfig::parse("ssh pi@example.com").unwrap()
    }
    const TEST_IO_TIMEOUT: Duration = Duration::from_secs(2);

    fn connect_client(bridge: &Askpass) -> TcpStream {
        let stream =
            TcpStream::connect_timeout(&bridge.listener.local_addr().unwrap(), TEST_IO_TIMEOUT)
                .unwrap();
        stream.set_read_timeout(Some(TEST_IO_TIMEOUT)).unwrap();
        stream.set_write_timeout(Some(TEST_IO_TIMEOUT)).unwrap();
        stream
    }

    // A nonblocking poll can legitimately return false before accept is ready.
    // Wait for actual processing, not merely a false result that also denotes
    // successful authentication. Client I/O is bounded separately above.
    fn poll_request(
        bridge: &mut Askpass,
        prompt: &mut impl FnMut(AuthPrompt) -> AuthAnswer,
    ) -> bool {
        let deadline = Instant::now() + TEST_IO_TIMEOUT;
        loop {
            let mut prompted = false;
            let cancelled = bridge
                .poll(&mut |request| {
                    prompted = true;
                    prompt(request)
                })
                .unwrap();
            if prompted || cancelled {
                return cancelled;
            }
            assert!(
                Instant::now() < deadline,
                "askpass request was not serviced"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn host_prompt() -> String {
        format!(
            "The authenticity of host 'example.com (127.0.0.1)' can't be established.\nED25519 key fingerprint is SHA256:{}.\nThis key is not known by any other names.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? ",
            STANDARD_NO_PAD.encode([7; 32])
        )
    }
    #[test]
    fn parses_only_known_prompts() {
        assert!(matches!(
            parse_prompt("pi@example.com's password: ", &config()),
            Some(AuthPrompt::Password { .. })
        ));
        assert!(matches!(
            parse_prompt(
                "Enter passphrase for key '/home/pi/.ssh/id_ed25519': ",
                &config()
            ),
            Some(AuthPrompt::KeyPassphrase { .. })
        ));
        assert!(matches!(
            parse_prompt(&host_prompt(), &config()),
            Some(AuthPrompt::TrustHost { .. })
        ));
        for bad in [
            "Password:",
            "Enter OTP:",
            "root@example.com's password:",
            "\x1b[1mPassword:",
            "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!",
            "Enter passphrase for key 'bad\npath':",
        ] {
            assert!(parse_prompt(bad, &config()).is_none(), "{bad}");
        }
        assert!(parse_prompt(&host_prompt().replace("SHA256:", "MD5:"), &config()).is_none());
        assert!(parse_prompt(&host_prompt().replace("ED25519", "UNKNOWN"), &config()).is_none());
    }
    #[test]
    fn bounded_frames() {
        assert!(read_frame(&mut &u32::MAX.to_be_bytes()[..], MAX_PROMPT).is_err());
        let mut buffer = Vec::new();
        write_frame(&mut buffer, b"abc").unwrap();
        assert_eq!(&**read_frame(&mut &buffer[..], 3).unwrap(), b"abc");
        assert!(read_frame(&mut &buffer[..], 2).is_err());
    }
    #[test]
    fn unauthenticated_peers_cannot_prompt() {
        let mut bridge = Askpass::new(
            &config(),
            Path::new(if cfg!(windows) {
                "C:\\helper.exe"
            } else {
                "/helper"
            }),
        )
        .unwrap();
        let mut stream = connect_client(&bridge);
        stream.write_all(&[0; 32]).unwrap();
        assert!(!bridge.poll(&mut |_| panic!("untrusted prompt")).unwrap());
    }
    #[test]
    fn authenticated_round_trip_and_cancel() {
        for cancel in [false, true] {
            let mut bridge = Askpass::new(
                &config(),
                Path::new(if cfg!(windows) {
                    "C:\\helper.exe"
                } else {
                    "/helper"
                }),
            )
            .unwrap();
            let mut stream = connect_client(&bridge);
            stream.write_all(&*bridge.capability).unwrap();
            write_frame(&mut stream, b"pi@example.com's password: ").unwrap();
            assert_eq!(
                poll_request(&mut bridge, &mut |_| if cancel {
                    AuthAnswer::Cancel
                } else {
                    AuthAnswer::Secret(Zeroizing::new("test-secret".into()))
                }),
                cancel
            );
            let mut status = [0];
            stream.read_exact(&mut status).unwrap();
            assert_eq!(status, [u8::from(!cancel)]);
            if !cancel {
                assert_eq!(
                    &**read_frame(&mut stream, MAX_SECRET).unwrap(),
                    b"test-secret"
                );
            }
        }
    }
    fn bridge() -> Askpass {
        Askpass::new(
            &config(),
            Path::new(if cfg!(windows) {
                "C:\\helper.exe"
            } else {
                "/helper"
            }),
        )
        .unwrap()
    }
    #[test]
    fn configured_alias_uses_effective_user_and_host() {
        let alias = ConnectionConfig::parse("ssh dev-pi").unwrap();
        assert!(
            matches!(parse_prompt("actual@resolved.example's password:", &alias), Some(AuthPrompt::Password { username }) if username == "actual")
        );
        assert!(parse_prompt("bad user@resolved.example's password:", &alias).is_none());
        assert!(parse_prompt("actual@bad\nhost's password:", &alias).is_none());
    }
    #[test]
    fn fragmented_authenticated_request_waits_within_deadline() {
        let mut bridge = bridge();
        let mut stream = connect_client(&bridge);
        let capability = bridge.capability.clone();
        stream.write_all(&capability[..16]).unwrap();
        let client = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            stream.write_all(&capability[16..]).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            write_frame(&mut stream, b"pi@example.com's password:").unwrap();
            let mut status = [0];
            stream.read_exact(&mut status).unwrap();
            assert_eq!(status, [1]);
            assert_eq!(
                &**read_frame(&mut stream, MAX_SECRET).unwrap(),
                b"fixture answer"
            );
        });
        assert!(!poll_request(&mut bridge, &mut |_| {
            AuthAnswer::Secret(Zeroizing::new("fixture answer".into()))
        }));
        client.join().unwrap();
    }
    #[test]
    fn delayed_connection_is_serviced_before_reading_the_response() {
        let mut bridge = bridge();
        assert!(!bridge.poll(&mut |_| panic!("no client yet")).unwrap());
        let endpoint = bridge.listener.local_addr().unwrap();
        let capability = bridge.capability.clone();
        let client = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            let mut stream = TcpStream::connect_timeout(&endpoint, TEST_IO_TIMEOUT).unwrap();
            stream.set_read_timeout(Some(TEST_IO_TIMEOUT)).unwrap();
            stream.set_write_timeout(Some(TEST_IO_TIMEOUT)).unwrap();
            stream.write_all(&*capability).unwrap();
            write_frame(&mut stream, b"pi@example.com's password:").unwrap();
            let mut status = [0];
            stream.read_exact(&mut status).unwrap();
            assert_eq!(status, [1]);
            assert_eq!(
                &**read_frame(&mut stream, MAX_SECRET).unwrap(),
                b"delayed answer"
            );
        });
        assert!(!poll_request(&mut bridge, &mut |_| {
            AuthAnswer::Secret(Zeroizing::new("delayed answer".into()))
        }));
        client.join().unwrap();
    }

    #[test]
    fn rejects_wrong_answer_types_and_multiline_secrets() {
        for (raw, answer) in [
            (
                host_prompt(),
                AuthAnswer::Secret(Zeroizing::new("yes".into())),
            ),
            (host_prompt(), AuthAnswer::Trust(false)),
            ("pi@example.com's password:".into(), AuthAnswer::Trust(true)),
            (
                "pi@example.com's password:".into(),
                AuthAnswer::Secret(Zeroizing::new("bad\nsecret".into())),
            ),
        ] {
            let mut bridge = bridge();
            let mut stream = connect_client(&bridge);
            stream.write_all(&*bridge.capability).unwrap();
            write_frame(&mut stream, raw.as_bytes()).unwrap();
            let mut answer = Some(answer);
            assert!(poll_request(&mut bridge, &mut |_| answer.take().unwrap()));
            let mut status = [255];
            stream.read_exact(&mut status).unwrap();
            assert_eq!(status, [0]);
        }
    }
    #[test]
    fn explicit_host_acceptance_returns_yes() {
        let mut bridge = bridge();
        let mut stream = connect_client(&bridge);
        stream.write_all(&*bridge.capability).unwrap();
        write_frame(&mut stream, host_prompt().as_bytes()).unwrap();
        assert!(!poll_request(&mut bridge, &mut |prompt| {
            assert!(matches!(prompt, AuthPrompt::TrustHost { .. }));
            AuthAnswer::Trust(true)
        }));
        let mut status = [0];
        stream.read_exact(&mut status).unwrap();
        assert_eq!(status, [1]);
        assert_eq!(&**read_frame(&mut stream, MAX_SECRET).unwrap(), b"yes");
    }
    #[test]
    fn authenticated_oversized_and_unknown_prompts_cancel_without_ui() {
        for bytes in [u32::MAX.to_be_bytes().to_vec(), {
            let mut frame = Vec::new();
            write_frame(&mut frame, b"Enter OTP:").unwrap();
            frame
        }] {
            let mut bridge = bridge();
            let mut stream = connect_client(&bridge);
            stream.write_all(&*bridge.capability).unwrap();
            stream.write_all(&bytes).unwrap();
            assert!(poll_request(&mut bridge, &mut |_| panic!(
                "unknown prompt displayed"
            )));
        }
    }
    #[test]
    fn stalled_local_peer_is_bounded() {
        let mut bridge = bridge();
        let _stream = connect_client(&bridge);
        let start = Instant::now();
        assert!(
            !bridge
                .poll(&mut |_| panic!("stalled peer prompted"))
                .unwrap()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
