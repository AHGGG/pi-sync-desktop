//! System OpenSSH with a single private, bounded worker stream per connection.
//! No shell is used locally. The only remote command is the constant launcher.
use crate::{askpass::Askpass, connection::ConnectionConfig, profiles::write_private_atomic};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

const MAX_PROTOCOL_BYTES: usize = 48 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 64 * 1024;
const READY: &[u8] = b"{\"piSyncReady\":1}\n";
// Single outstanding request; code, paths, credentials and file contents are
// never interpolated into this command. In particular, no request is replayed.
const WORKER_COMMAND: &str = r#"node -e "let b='';let active=false;process.stdin.setEncoding('utf8');process.stdin.on('data',c=>{b+=c;if(Buffer.byteLength(b)>50331649)process.exit(2);let i=b.indexOf('\n');if(i<0)return;if(active||i!==b.length-1)process.exit(2);active=true;let s=b.slice(0,i);b='';(async()=>{try{let p=JSON.parse(s);let m=await import('data:text/javascript;base64,'+p.code);let r=JSON.stringify(await m.handle(p.request));if(Buffer.byteLength(r)>50331648)process.exit(2);process.stdout.write(r+'\n')}catch{process.stdout.write(JSON.stringify({ok:false,error:{code:'REMOTE_FAILED'}})+'\n')}finally{active=false}})()});process.stdin.on('end',()=>process.exit(0));if(Number(process.versions.node.split('.')[0])<20)process.exit(2);process.stdout.write(JSON.stringify({piSyncReady:1})+'\n')""#;

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
// Intentionally neither Debug nor Serialize: answers must not reach logs/disk.
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
pub fn require_host_trust(identity: HostIdentity, accepted: bool) -> Result<()> {
    match identity {
        HostIdentity::Trusted => Ok(()),
        HostIdentity::Unknown if accepted => Ok(()),
        HostIdentity::Unknown => bail!("Host identity was not accepted"),
        HostIdentity::Changed => {
            bail!("Host key changed. Connection blocked; verify the server independently")
        }
        HostIdentity::Unverifiable => bail!("Cannot verify host identity"),
    }
}
/// Program/config injection exists for isolated tests; profiles never contain
/// executables, environment variables or arbitrary OpenSSH options.
#[derive(Clone)]
pub struct SshOptions {
    pub ssh_program: PathBuf,
    pub askpass_program: PathBuf,
    pub config_file: Option<PathBuf>,
    pub connection_timeout: Duration,
    pub request_timeout: Duration,
    pub inherit_user_known_hosts: bool,
}
impl Default for SshOptions {
    fn default() -> Self {
        Self {
            ssh_program: system_ssh(),
            askpass_program: std::env::current_exe().unwrap_or_default(),
            config_file: None,
            connection_timeout: Duration::from_secs(120),
            request_timeout: Duration::from_secs(60),
            inherit_user_known_hosts: true,
        }
    }
}
fn system_ssh() -> PathBuf {
    // Use Windows' installed OpenSSH rather than accidentally selecting a Git
    // distribution's client/agent implementation ahead of it on PATH.
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        let path = PathBuf::from(root).join("System32/OpenSSH/ssh.exe");
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from("ssh")
}

/// Describes the deterministic client choice without exposing authentication data.
pub fn ssh_client_description() -> String {
    let executable = system_ssh();
    if executable.is_absolute() {
        executable.display().to_string()
    } else {
        "ssh from PATH".into()
    }
}

pub struct SshTransport {
    child: Child,
    input: SyncSender<Zeroizing<Vec<u8>>>,
    written: Receiver<bool>,
    output: Receiver<Result<Zeroizing<Vec<u8>>, ()>>,
    diagnostics: Arc<Mutex<Zeroizing<Vec<u8>>>>,
    diagnostics_done: Receiver<()>,
    timeout: Duration,
    usable: bool,
}
impl SshTransport {
    pub fn connect(
        config: &ConnectionConfig,
        known: &Path,
        prompt: impl FnMut(AuthPrompt) -> AuthAnswer,
    ) -> Result<Self> {
        Self::connect_with_options(config, known, prompt, &SshOptions::default())
    }
    pub fn connect_with_options(
        config: &ConnectionConfig,
        known: &Path,
        mut prompt: impl FnMut(AuthPrompt) -> AuthAnswer,
        options: &SshOptions,
    ) -> Result<Self> {
        Self::connect_control_options(
            config,
            known,
            |event| match event {
                Some(prompt_event) => prompt(prompt_event),
                None => AuthAnswer::Trust(false),
            },
            options,
        )
    }
    /// `None` polls for cancellation without a credential prompt. Any answer
    /// other than Cancel continues; secrets are never requested by this poll.
    pub fn connect_with_control(
        config: &ConnectionConfig,
        known: &Path,
        control: impl FnMut(Option<AuthPrompt>) -> AuthAnswer,
    ) -> Result<Self> {
        Self::connect_control_options(config, known, control, &SshOptions::default())
    }
    fn connect_control_options(
        config: &ConnectionConfig,
        known: &Path,
        mut control: impl FnMut(Option<AuthPrompt>) -> AuthAnswer,
        options: &SshOptions,
    ) -> Result<Self> {
        config.validate()?;
        ensure!(
            !matches!(control(None), AuthAnswer::Cancel),
            "Authentication cancelled"
        );
        ensure!(
            !options.askpass_program.as_os_str().is_empty(),
            "Cannot start SSH authentication helper"
        );
        if options.inherit_user_known_hosts {
            check_native_trust_config(config, options)?;
        }
        let known_paths = prepare_known_hosts(known, options.inherit_user_known_hosts)?;
        let mut command = ssh_command(config, options, &known_paths)?;
        let mut askpass = Askpass::new(config, &options.askpass_program)?;
        askpass.configure(&mut command);
        command
            .arg(WORKER_COMMAND)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW; prompts are native app UI.
        }
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!("OpenSSH client is missing. On Windows install the OpenSSH Client optional feature; on macOS check /usr/bin/ssh and PATH; on Linux install openssh-client. Then reopen Pi Sync")
            } else { anyhow::anyhow!("Cannot start the system OpenSSH client") }
        })?;
        // Pipes requested above are guaranteed for a successfully spawned child.
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let (input, input_rx) = mpsc::sync_channel::<Zeroizing<Vec<u8>>>(1);
        let (written_tx, written) = mpsc::sync_channel(1);
        thread::spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                let ok = stdin.write_all(&bytes).and_then(|()| stdin.flush()).is_ok();
                if written_tx.send(ok).is_err() || !ok {
                    break;
                }
            }
        });
        let (output_tx, output) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut bytes = Zeroizing::new(Vec::new());
                let read = (&mut reader)
                    .take((MAX_PROTOCOL_BYTES + 2) as u64)
                    .read_until(b'\n', &mut bytes);
                match read {
                    Ok(0) => break,
                    Ok(n) if n <= MAX_PROTOCOL_BYTES + 1 && bytes.last() == Some(&b'\n') => {
                        if output_tx.send(Ok(bytes)).is_err() {
                            break;
                        }
                    }
                    _ => {
                        let _ = output_tx.send(Err(()));
                        break;
                    }
                }
            }
        });
        let diagnostics = Arc::new(Mutex::new(Zeroizing::new(Vec::new())));
        let captured = diagnostics.clone();
        let (diagnostics_done_tx, diagnostics_done) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut chunk = Zeroizing::new([0u8; 4096]);
            while let Ok(n) = stderr.read(&mut chunk[..]) {
                if n == 0 {
                    break;
                }
                if let Ok(mut bytes) = captured.lock() {
                    let remaining = MAX_DIAGNOSTIC_BYTES.saturating_sub(bytes.len());
                    bytes.extend_from_slice(&chunk[..n.min(remaining)]);
                }
                // Always drain; oversized banners cannot deadlock the worker.
            }
            let _ = diagnostics_done_tx.send(());
        });
        let transport = Self {
            child,
            input,
            written,
            output,
            diagnostics,
            diagnostics_done,
            timeout: options.request_timeout,
            usable: true,
        };
        let started = Instant::now();
        loop {
            if matches!(control(None), AuthAnswer::Cancel)
                || askpass.poll(&mut |prompt| control(Some(prompt)))?
            {
                bail!("Authentication cancelled");
            }
            match transport.output.recv_timeout(Duration::from_millis(20)) {
                Ok(Ok(frame)) if frame.as_slice() == READY => break,
                Ok(_) => bail!(
                    "Invalid remote worker startup response. Check the server's Node.js installation and shell startup output"
                ),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(transport.failure(false)),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if started.elapsed() >= options.connection_timeout {
                bail!(
                    "SSH connection timed out. Check the host, network and SSH agent; unlock Bitwarden if it is your agent"
                );
            }
        }
        Ok(transport)
    }
    pub fn run_worker(&mut self, worker_source: &str, request: &Value) -> Result<Value> {
        self.run_worker_cancellable(worker_source, request, || false)
    }
    pub fn run_worker_cancellable(
        &mut self,
        worker_source: &str,
        request: &Value,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Value> {
        ensure!(
            self.usable,
            "SSH connection is no longer usable. Prepare a fresh review before trying again"
        );
        let mut payload = Zeroizing::new(serde_json::to_vec(
            &json!({"code": STANDARD.encode(worker_source), "request": request}),
        )?);
        ensure!(
            payload.len() <= MAX_PROTOCOL_BYTES,
            "Remote request exceeds size limit"
        );
        // Once sent, failure/cancellation is potentially after a partial apply.
        // Poison the connection until a complete valid response arrives.
        self.usable = false;
        payload.push(b'\n');
        let result = (|| {
            ensure!(
                !cancelled(),
                "SSH operation cancelled. If applying, inspect the target and prepare a fresh review; changes may already have been written"
            );
            self.input.try_send(payload).map_err(|_| {
                anyhow::anyhow!(
                    "Cannot send remote request. Inspect the target before preparing again"
                )
            })?;
            let started = Instant::now();
            let mut written = false;
            loop {
                if cancelled() {
                    bail!(
                        "SSH operation cancelled. If applying, inspect the target and prepare a fresh review; changes may already have been written"
                    );
                }
                if !written {
                    match self.written.try_recv() {
                        Ok(true) => written = true,
                        Ok(false) | Err(mpsc::TryRecvError::Disconnected) => {
                            return Err(self.failure(true));
                        }
                        Err(mpsc::TryRecvError::Empty) => {}
                    }
                }
                match self.output.recv_timeout(Duration::from_millis(20)) {
                    Ok(Ok(bytes)) => {
                        let response: Value = serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("Invalid remote worker response. Inspect the target before preparing again"))?;
                        ensure!(
                            response.get("ok").is_some_and(Value::is_boolean),
                            "Invalid remote response envelope. Inspect the target before preparing again"
                        );
                        // The write acknowledgement can arrive after the response.
                        // Consume it before allowing another request.
                        if !written {
                            ensure!(
                                self.written
                                    .recv_timeout(Duration::from_secs(1))
                                    .unwrap_or(false),
                                "Cannot finish remote request. Inspect the target before preparing again"
                            );
                        }
                        return Ok(response);
                    }
                    Ok(Err(())) => bail!(
                        "Remote worker response exceeds size limit or is incomplete. Inspect the target before preparing again"
                    ),
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Err(self.failure(true)),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if started.elapsed() >= self.timeout {
                    bail!(
                        "Remote worker timed out. If applying, inspect the target and prepare a fresh review; changes may already have been written"
                    );
                }
            }
        })();
        if result.is_ok() {
            self.usable = true;
        } else {
            self.stop();
        }
        result
    }
    fn failure(&self, after_request: bool) -> anyhow::Error {
        if after_request {
            return anyhow::anyhow!(
                "SSH connection stopped before confirming the result. If applying, inspect the target and prepare a fresh review; changes may already have been written"
            );
        }
        // stdout EOF can race the stderr reader on a quick authentication error.
        let _ = self
            .diagnostics_done
            .recv_timeout(Duration::from_millis(200));
        let bytes = self.diagnostics.lock().unwrap();
        anyhow::anyhow!(classify_failure(&bytes))
    }
    fn stop(&mut self) {
        self.usable = false;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Drop for SshTransport {
    fn drop(&mut self) {
        self.stop();
    }
}
/// Only these adapter-authored messages may be presented by the GUI. No raw
/// subprocess diagnostics, server banners, payloads or OS errors are forwarded.
pub fn is_safe_error(text: &str) -> bool {
    const MESSAGES: &[&str] = &[
        "Host key changed. Connection blocked; verify the server independently",
        "Host identity was not accepted",
        "Cannot verify host identity",
        "Authentication cancelled",
        "OpenSSH client is missing. On Windows install the OpenSSH Client optional feature; on macOS check /usr/bin/ssh and PATH; on Linux install openssh-client. Then reopen Pi Sync",
        "Cannot start the system OpenSSH client",
        "Cannot start authentication bridge",
        "Cannot start SSH authentication helper",
        "Invalid remote worker startup response. Check the server's Node.js installation and shell startup output",
        "SSH connection timed out. Check the host, network and SSH agent; unlock Bitwarden if it is your agent",
        "SSH connection is no longer usable. Prepare a fresh review before trying again",
        "Remote request exceeds size limit",
        "SSH operation cancelled. If applying, inspect the target and prepare a fresh review; changes may already have been written",
        "Cannot send remote request. Inspect the target before preparing again",
        "Invalid remote worker response. Inspect the target before preparing again",
        "Invalid remote response envelope. Inspect the target before preparing again",
        "Cannot finish remote request. Inspect the target before preparing again",
        "Remote worker response exceeds size limit or is incomplete. Inspect the target before preparing again",
        "Remote worker timed out. If applying, inspect the target and prepare a fresh review; changes may already have been written",
        "SSH connection stopped before confirming the result. If applying, inspect the target and prepare a fresh review; changes may already have been written",
        "SSH authentication failed. Unlock or approve your SSH agent (including Bitwarden), or check the configured identity and password",
        "Cannot resolve SSH host. Check the hostname or alias in your OpenSSH config",
        "Cannot connect to SSH host. Check its address, port, network and SSH service",
        "OpenSSH could not start the remote worker. Check this connection in a terminal and ensure Node.js 20 or later is available to SSH sessions",
        "Cannot safely read OpenSSH configuration. Pi Sync requires OpenSSH 8.7 or later; update the client or check its configuration in a terminal",
        "Cannot safely read OpenSSH configuration",
        "This SSH alias uses a custom UserKnownHostsFile. Pi Sync cannot safely combine that trust store yet; use the standard OpenSSH known_hosts files for this alias or keep using the terminal",
        "This SSH alias uses KnownHostsCommand, which Pi Sync cannot safely combine with its trust store yet. Keep using the terminal for this alias",
        "Application SSH trust file must be private to your user",
        "Cannot create the application SSH trust file",
        "Unsupported characters in SSH trust path",
    ];
    MESSAGES.contains(&text)
}
fn classify_failure(bytes: &[u8]) -> &'static str {
    // Never forward raw diagnostics: login banners and server text are untrusted.
    let contains = |needle: &[u8]| bytes.windows(needle.len()).any(|part| part == needle);
    if contains(b"REMOTE HOST IDENTIFICATION HAS CHANGED") || contains(b"REVOKED HOST KEY DETECTED")
    {
        "Host key changed. Connection blocked; verify the server independently"
    } else if contains(b"Host key verification failed") {
        "Cannot verify host identity"
    } else if contains(b"Permission denied") || contains(b"sign_and_send_pubkey: signing failed") {
        "SSH authentication failed. Unlock or approve your SSH agent (including Bitwarden), or check the configured identity and password"
    } else if contains(b"Could not resolve hostname") {
        "Cannot resolve SSH host. Check the hostname or alias in your OpenSSH config"
    } else if contains(b"Connection refused")
        || contains(b"No route to host")
        || contains(b"Connection timed out")
    {
        "Cannot connect to SSH host. Check its address, port, network and SSH service"
    } else {
        "OpenSSH could not start the remote worker. Check this connection in a terminal and ensure Node.js 20 or later is available to SSH sessions"
    }
}
// `ssh -G` loses quoting between paths. Never reconstruct arbitrary trust
// files from it: only an exact match to the two standard paths is supported.
// A custom store is blocked before network/authentication rather than silently
// ignored and turned into a fresh trust prompt.
fn check_native_trust_config(config: &ConnectionConfig, options: &SshOptions) -> Result<()> {
    let mut command = ssh_command_mode(config, options, &[], true)?;
    command
        .arg(WORKER_COMMAND)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!("OpenSSH client is missing. On Windows install the OpenSSH Client optional feature; on macOS check /usr/bin/ssh and PATH; on Linux install openssh-client. Then reopen Pi Sync")
        } else { anyhow::anyhow!("Cannot start the system OpenSSH client") }
    })?;
    let mut stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Zeroizing::new(Vec::new());
        let ok = (&mut stdout).take(65537).read_to_end(&mut bytes).is_ok() && bytes.len() <= 65536;
        let _ = tx.send(if ok { Some(bytes) } else { None });
    });
    let started = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    rx.recv_timeout(Duration::from_secs(1)).ok().flatten()
                } else {
                    None
                };
            }
            Ok(None) if started.elapsed() < Duration::from_secs(5) => {
                thread::sleep(Duration::from_millis(20))
            }
            _ => break None,
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    let bytes = result
        .context("Cannot safely read OpenSSH configuration. Pi Sync requires OpenSSH 8.7 or later; update the client or check its configuration in a terminal")?;
    let contents =
        std::str::from_utf8(&bytes).context("Cannot safely read OpenSSH configuration")?;
    let home = directories::BaseDirs::new().context("Cannot locate home directory")?;
    let expected = format!(
        "{} {}",
        home.home_dir().join(".ssh/known_hosts").display(),
        home.home_dir().join(".ssh/known_hosts2").display()
    )
    .replace('\\', "/");
    let known = contents
        .lines()
        .find_map(|line| line.strip_prefix("userknownhostsfile "));
    ensure!(
        known.is_some_and(|value| value.replace('\\', "/") == expected
            || value == "~/.ssh/known_hosts ~/.ssh/known_hosts2"),
        "This SSH alias uses a custom UserKnownHostsFile. Pi Sync cannot safely combine that trust store yet; use the standard OpenSSH known_hosts files for this alias or keep using the terminal"
    );
    ensure!(
        !contents.lines().any(|line| line
            .strip_prefix("knownhostscommand ")
            .is_some_and(|value| value != "none")),
        "This SSH alias uses KnownHostsCommand, which Pi Sync cannot safely combine with its trust store yet. Keep using the terminal for this alias"
    );
    Ok(())
}
fn prepare_known_hosts(app: &Path, inherit: bool) -> Result<Vec<PathBuf>> {
    if !app.exists() {
        write_private_atomic(app, b"").context("Cannot create the application SSH trust file")?;
    }
    let mut paths = vec![app.to_path_buf()];
    if inherit && let Some(base) = directories::BaseDirs::new() {
        for name in ["known_hosts", "known_hosts2"] {
            paths.push(base.home_dir().join(".ssh").join(name));
        }
    }
    for (index, path) in paths.iter().enumerate() {
        match fs::symlink_metadata(path) {
            Ok(meta) => {
                ensure!(
                    meta.is_file()
                        && !meta.file_type().is_symlink()
                        && meta.len() <= 4 * 1024 * 1024,
                    "Cannot verify host identity"
                );
                let _ = fs::read(path).context("Cannot verify host identity")?;
                #[cfg(unix)]
                if index == 0 {
                    use std::os::unix::fs::PermissionsExt;
                    ensure!(
                        meta.permissions().mode() & 0o077 == 0,
                        "Application SSH trust file must be private to your user"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && index > 0 => {}
            Err(_) => bail!("Cannot verify host identity"),
        }
    }
    Ok(paths)
}
fn ssh_command(
    config: &ConnectionConfig,
    options: &SshOptions,
    known: &[PathBuf],
) -> Result<Command> {
    ssh_command_mode(config, options, known, false)
}
fn ssh_command_mode(
    config: &ConnectionConfig,
    options: &SshOptions,
    known: &[PathBuf],
    inspect: bool,
) -> Result<Command> {
    let mut command = Command::new(&options.ssh_program);
    if let Some(path) = &options.config_file {
        command.arg("-F").arg(path);
    }
    command.arg("-T");
    if inspect {
        command.arg("-G");
    } else {
        command.arg("-o").arg("KnownHostsCommand=none");
    }
    for guard in [
        "StrictHostKeyChecking=ask",
        "FingerprintHash=sha256",
        "VerifyHostKeyDNS=no",
        "UpdateHostKeys=no",
        "ForwardAgent=no",
        "GSSAPIDelegateCredentials=no",
        "ForwardX11=no",
        "ClearAllForwardings=yes",
        "Tunnel=no",
        "PermitLocalCommand=no",
        "RemoteCommand=none",
        "RequestTTY=no",
        "ControlMaster=no",
        "ControlPath=none",
        "ControlPersist=no",
        "ProxyCommand=none",
        "ProxyJump=none",
        "SessionType=default",
        "StdinNull=no",
        "ForkAfterAuthentication=no",
        "BatchMode=no",
        "AddKeysToAgent=no",
        "IgnoreUnknown=UseKeychain",
        "UseKeychain=no",
        "NumberOfPasswordPrompts=1",
        "KbdInteractiveAuthentication=no",
        "ConnectTimeout=15",
        "ConnectionAttempts=1",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=2",
        "LogLevel=ERROR",
    ] {
        command.arg("-o").arg(guard);
    }
    // OpenSSH, not a local shell, parses this option. Quote each path and reject
    // metacharacters which OpenSSH would expand or misinterpret a second time.
    let paths = known
        .iter()
        .map(|path| {
            let value = path.to_str().context("SSH trust path must be UTF-8")?;
            ensure!(
                !value.chars().any(char::is_control)
                    && !value.contains(['"', '%', '$'])
                    && (cfg!(windows) || !value.contains('\\')),
                "Unsupported characters in SSH trust path"
            );
            Ok(format!("\"{}\"", value.replace('\\', "/")))
        })
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    if !known.is_empty() {
        command.arg("-o").arg(format!("UserKnownHostsFile={paths}"));
    }
    if !config.use_config_user {
        command.arg("-l").arg(&config.username);
    }
    if !config.use_config_port {
        command.arg("-p").arg(config.port.to_string());
    }
    if let Some(path) = &config.key_path {
        command.arg("-i").arg(path);
    }
    command.arg("--").arg(&config.host);
    Ok(command)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trust_is_explicit_and_changed_keys_cannot_be_overridden() {
        assert!(require_host_trust(HostIdentity::Unknown, false).is_err());
        assert!(require_host_trust(HostIdentity::Unknown, true).is_ok());
        assert!(require_host_trust(HostIdentity::Trusted, false).is_ok());
        assert!(require_host_trust(HostIdentity::Changed, true).is_err());
        assert!(require_host_trust(HostIdentity::Unverifiable, true).is_err());
    }
    #[test]
    fn alias_and_identity_are_arguments_not_shell_syntax() {
        let config = ConnectionConfig::parse(r#"ssh mac-mini -i "C:\Users\Me\key file""#).unwrap();
        let command = ssh_command(
            &config,
            &SshOptions::default(),
            &[PathBuf::from("/tmp/known hosts")],
        )
        .unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            &args[args.len() - 4..],
            ["-i", r"C:\Users\Me\key file", "--", "mac-mini"]
        );
        assert!(!args.iter().any(|arg| arg == "-p" || arg == "-l"));
        for guard in [
            "StrictHostKeyChecking=ask",
            "FingerprintHash=sha256",
            "ForwardAgent=no",
            "GSSAPIDelegateCredentials=no",
            "ClearAllForwardings=yes",
            "ProxyCommand=none",
            "ControlPath=none",
            "StdinNull=no",
            "SessionType=default",
            "ForkAfterAuthentication=no",
        ] {
            assert!(args.iter().any(|arg| arg == guard), "Missing guard {guard}");
        }
        assert!(
            args.iter()
                .any(|arg| arg == "UserKnownHostsFile=\"/tmp/known hosts\"")
        );
    }
    #[test]
    fn diagnostics_do_not_disclose_server_text() {
        assert!(
            !classify_failure(b"banner with fixture private content").contains("private content")
        );
        assert!(
            classify_failure(b"WARNING REMOTE HOST IDENTIFICATION HAS CHANGED")
                .starts_with("Host key changed")
        );
    }
    #[test]
    fn missing_client_has_actionable_error_without_touching_trust() {
        let directory = tempfile::tempdir().unwrap();
        let options = SshOptions {
            ssh_program: directory.path().join("missing-ssh-client"),
            ..SshOptions::default()
        };
        let known = directory.path().join("known_hosts");
        let error = SshTransport::connect_with_options(
            &ConnectionConfig::parse("ssh fixture").unwrap(),
            &known,
            |_| panic!("Missing client cannot prompt"),
            &options,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("OpenSSH client is missing"));
        assert!(is_safe_error(&error));
        assert!(!known.exists());
    }
    #[test]
    fn real_openssh_preflight_rejects_custom_trust_before_network_or_prompt() {
        // Read-only `ssh -G` runs on each native CI platform, never a real host.
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config");
        let options = SshOptions {
            config_file: Some(config_path.clone()),
            ..SshOptions::default()
        };
        let config = ConnectionConfig::parse("ssh synthetic-alias").unwrap();
        fs::write(
            &config_path,
            "Host synthetic-alias\n HostName example.invalid\n User fixture\n Port 2222\n",
        )
        .unwrap();
        check_native_trust_config(&config, &options).unwrap();
        for directive in [
            "UserKnownHostsFile \"~/custom trust\"",
            "KnownHostsCommand echo",
        ] {
            fs::write(
                &config_path,
                format!("Host synthetic-alias\n HostName example.invalid\n {directive}\n"),
            )
            .unwrap();
            let known = directory.path().join("known_hosts");
            let error = SshTransport::connect_with_options(
                &config,
                &known,
                |_| panic!("Unsupported trust cannot prompt"),
                &options,
            )
            .err()
            .unwrap()
            .to_string();
            assert!(error.contains("cannot safely combine"), "{error}");
            assert!(is_safe_error(&error));
            assert!(!known.exists());
        }
    }
    #[test]
    fn preflight_uses_the_actual_remote_command_for_match_conditions() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config");
        fs::write(
            &config_path,
            "Match command \"node*\"\n UserKnownHostsFile /pi-sync-conditional-trust\n",
        )
        .unwrap();
        let config = ConnectionConfig::parse("ssh synthetic-alias").unwrap();
        let options = SshOptions {
            config_file: Some(config_path),
            ..SshOptions::default()
        };
        let error = check_native_trust_config(&config, &options)
            .unwrap_err()
            .to_string();
        // Match command is newer than our minimum client version. Older clients
        // reject the fixture directive; current clients must reject custom trust.
        assert!(
            error.contains("custom UserKnownHostsFile")
                || error.contains("check its configuration"),
            "{error}"
        );
    }
    #[test]
    fn unsafe_trust_paths_are_refused() {
        for path in ["/tmp/a\"b", "/tmp/%h", "/tmp/${HOME}", "/tmp/a\nb"] {
            assert!(
                ssh_command(
                    &ConnectionConfig::parse("ssh host").unwrap(),
                    &SshOptions::default(),
                    &[PathBuf::from(path)]
                )
                .is_err()
            );
        }
    }
}
