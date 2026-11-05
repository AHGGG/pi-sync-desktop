//! Opt-in real SSH test. Every key, authorized_keys entry, trust record, and Pi
//! file lives in a temporary directory. No system/user SSH configuration changes.
#![cfg(unix)]
use anyhow::{Context, Result, bail, ensure};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use sync_core::{
    connection::ConnectionConfig,
    transport::{AuthAnswer, AuthPrompt, SshOptions, SshTransport},
};

fn fixture_options(known: &Path) -> Result<SshOptions> {
    Ok(SshOptions {
        ssh_program: PathBuf::from("ssh"),
        askpass_program: PathBuf::from(
            std::env::var_os("PI_SYNC_TEST_ASKPASS")
                .context("Use scripts/test-ssh.sh to build the isolated askpass fixture")?,
        ),
        config_file: Some(
            known
                .parent()
                .context("Missing fixture directory")?
                .join("ssh_config"),
        ),
        connection_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(15),
        inherit_user_known_hosts: false,
    })
}
fn fixture_connect(
    config: &ConnectionConfig,
    known: &Path,
    prompt: impl FnMut(AuthPrompt) -> AuthAnswer,
) -> Result<SshTransport> {
    SshTransport::connect_with_options(config, known, prompt, &fixture_options(known)?)
}

// Logs belong only to the disposable loopback server, never the product SSH
// client. INFO logs contain authentication decisions, not private key material.
// Rust captures this output and displays it on a failed test, including `?`
// returns (not only panics). Keep the reported tail bounded.
struct Server(Child, Option<PathBuf>);
fn fixture_log_tail(path: &Path) -> std::io::Result<String> {
    const MAX_LOG_BYTES: u64 = 16 * 1024;
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(MAX_LOG_BYTES)))?;
    let mut bytes = Vec::new();
    file.take(MAX_LOG_BYTES).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        if let Some(path) = &self.1 {
            match fixture_log_tail(path) {
                Ok(log) => eprintln!("Isolated sshd diagnostics (last 16 KiB):\n{log}"),
                Err(error) => eprintln!("Cannot read isolated sshd diagnostics: {error}"),
            }
        }
    }
}
fn generate_key(path: &Path) -> Result<()> {
    generate_key_with_passphrase(path, "")
}
fn generate_key_with_passphrase(path: &Path, passphrase: &str) -> Result<()> {
    ensure!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", passphrase, "-f"])
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success(),
        "Cannot generate temporary SSH fixture key"
    );
    Ok(())
}
fn start_server(dir: &Path, host: &Path, port: u16, node_dir: &Path) -> Result<Server> {
    let config = dir.join("sshd_config");
    ensure!(
        !dir.to_string_lossy().contains(['\n', '"'])
            && !node_dir.to_string_lossy().contains(['\n', '"', ' ']),
        "Unsupported fixture path characters"
    );
    fs::write(
        &config,
        format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey \"{}\"\nPidFile \"{}\"\nAuthorizedKeysFile \"{}\"\nStrictModes yes\nUsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nPermitRootLogin no\nAllowTcpForwarding no\nAllowAgentForwarding no\nX11Forwarding no\nPermitTunnel no\nSetEnv PATH={}:/usr/bin:/bin\n",
            host.display(),
            dir.join("pid").display(),
            dir.join("authorized_keys").display(),
            node_dir.display()
        ),
    )?;
    let sshd = std::env::var_os("PI_SYNC_TEST_SSHD").unwrap_or_else(|| "/usr/sbin/sshd".into());
    let check = Command::new(&sshd)
        .args(["-t", "-f"])
        .arg(&config)
        .output()?;
    ensure!(
        check.status.success(),
        "Unprivileged SSH fixture configuration rejected: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let log_path = dir.join("sshd.log");
    let log = fs::File::create(&log_path)?;
    let child = Command::new(sshd)
        .args(["-D", "-e", "-f"])
        .arg(config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()?;
    let mut server = Server(child, Some(log_path));
    let start = Instant::now();
    loop {
        if let Some(status) = server.0.try_wait()? {
            bail!("Unprivileged SSH fixture exited: {status}; no system configuration was changed");
        }
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        ensure!(
            start.elapsed() < Duration::from_secs(5),
            "SSH fixture did not start"
        );
        thread::sleep(Duration::from_millis(25));
    }
    Ok(server)
}
#[test]
#[ignore = "Starts an isolated localhost sshd; run scripts/test-ssh.sh explicitly"]
fn key_auth_trust_worker_apply_and_changed_host_refusal() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir()?;
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))?;
    let host = dir.path().join("host");
    let client = dir.path().join("client");
    generate_key(&host)?;
    generate_key(&client)?;
    fs::copy(
        client.with_extension("pub"),
        dir.path().join("authorized_keys"),
    )?;
    fs::set_permissions(
        dir.path().join("authorized_keys"),
        fs::Permissions::from_mode(0o600),
    )?;
    let node_dir = PathBuf::from(
        std::env::var_os("PI_SYNC_TEST_NODE_DIR")
            .context("Use scripts/test-ssh.sh to provide the test Node directory")?,
    );
    ensure!(
        node_dir.join("node").is_file(),
        "Fixture Node executable missing"
    );
    let socket = TcpListener::bind(("127.0.0.1", 0))?;
    let port = socket.local_addr()?.port();
    drop(socket);
    let server = start_server(dir.path(), &host, port, &node_dir)?;
    let username = String::from_utf8(Command::new("id").arg("-un").output()?.stdout)?
        .trim()
        .to_owned();
    let config = ConnectionConfig {
        host: "127.0.0.1".into(),
        username,
        port,
        key_path: Some(client.clone()),
        use_config_user: false,
        use_config_port: false,
    };
    let known = dir.path().join("known_hosts");
    // -F prevents loading the user's config, and none prevents implicit personal
    // identities/agents from being tried. Every authentication input is synthetic.
    fs::write(
        dir.path().join("ssh_config"),
        format!(
            "Host fixture-alias\n  HostName 127.0.0.1\n  User {}\n  Port {}\n  IdentityFile \"{}\"\nHost *\n  IdentityAgent none\n  IdentityFile none\n  GlobalKnownHostsFile /dev/null\n  UserKnownHostsFile \"{}\"\n",
            config.username,
            port,
            client.display(),
            known.display()
        ),
    )?;
    let denied = fixture_connect(&config, &known, |_| AuthAnswer::Cancel);
    assert!(denied.is_err());
    assert!(
        fs::read_to_string(&known)
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "Rejected host must not create a trust record"
    );
    let public = fs::read_to_string(host.with_extension("pub"))?;
    let host_bytes = STANDARD.decode(
        public
            .split_whitespace()
            .nth(1)
            .context("Missing fixture host key")?,
    )?;
    let expected_fingerprint = format!(
        "SHA256:{}",
        STANDARD_NO_PAD.encode(Sha256::digest(host_bytes))
    );
    let mut prompts = 0;
    let mut transport = fixture_connect(&config, &known, |prompt| match prompt {
        AuthPrompt::TrustHost {
            host,
            port: actual_port,
            fingerprint,
        } => {
            assert!(
                host.contains(&format!("[127.0.0.1]:{port}")),
                "Trust prompt must identify the resolved fixture endpoint: {host}"
            );
            assert_eq!(actual_port, port);
            assert_eq!(fingerprint, expected_fingerprint);
            prompts += 1;
            AuthAnswer::Trust(true)
        }
        _ => AuthAnswer::Cancel,
    })?;
    assert_eq!(prompts, 1);
    assert!(known.is_file());
    let agent_dir = dir.path().join("pi-agent");
    let worker = include_str!("../../../backend/worker.mjs");
    // Preview snapshot is read-only, including a missing target directory.
    let snapshot = transport.run_worker(worker, &json!({"op":"snapshot","agentDir":agent_dir}))?;
    assert_eq!(snapshot["ok"], true);
    assert!(!agent_dir.exists());
    let apply=transport.run_worker(worker,&json!({"op":"apply","agentDir":agent_dir,"expected":{"settings":snapshot["settings"]["hash"],"state":snapshot["state"]["hash"]},"settings":{"theme":"dark","device":"fixture"}}))?;
    assert_eq!(apply["ok"], true);
    assert_eq!(apply["applied"][0]["verified"], true);
    let after = transport.run_worker(worker, &json!({"op":"snapshot","agentDir":agent_dir}))?;
    assert_eq!(
        after["settings"]["value"],
        json!({"theme":"dark","device":"fixture"})
    );
    let stale=transport.run_worker(worker,&json!({"op":"apply","agentDir":agent_dir,"expected":{"settings":null,"state":null},"settings":{"theme":"bad"}}))?;
    assert_eq!(stale["ok"], false);
    assert_eq!(stale["error"]["code"], "STALE_SNAPSHOT");
    // Exercise custom directory sync through the real private SSH protocol. The
    // 7 MiB raw fixture expands past the old 8 MiB response cap after base64.
    // These are generated bytes under the disposable fixture, never live config.
    let extensions = agent_dir.join("extensions");
    fs::create_dir(&extensions)?;
    let first = vec![b'A'; 3 * 1024 * 1024 + 512 * 1024];
    let second = vec![b'B'; 3 * 1024 * 1024 + 512 * 1024];
    fs::write(extensions.join("first.bin"), &first)?;
    fs::write(extensions.join("second.bin"), &second)?;
    let custom = transport.run_worker(
        worker,
        &json!({
            "op":"snapshot", "agentDir":agent_dir, "documents":[],
            "paths":["extensions"], "requirePaths":true
        }),
    )?;
    assert_eq!(custom["ok"], true);
    assert!(serde_json::to_vec(&custom)?.len() > 8 * 1024 * 1024);
    assert_eq!(custom["customFiles"].as_object().unwrap().len(), 2);
    assert_eq!(
        STANDARD.decode(
            custom["customFiles"]["extensions/first.bin"]["base64"]
                .as_str()
                .unwrap()
        )?,
        first
    );

    let custom_target = dir.path().join("custom-target");
    fs::create_dir_all(custom_target.join("extensions"))?;
    fs::write(custom_target.join("extensions/first.bin"), b"old fixture")?;
    fs::write(custom_target.join("extensions/local.txt"), b"target only")?;
    let before_custom = transport.run_worker(
        worker,
        &json!({
            "op":"snapshot", "agentDir":custom_target, "documents":[], "paths":["extensions"]
        }),
    )?;
    assert_eq!(before_custom["ok"], true);
    let copied = transport.run_worker(
        worker,
        &json!({
            "op":"apply", "agentDir":custom_target, "expected":{}, "paths":["extensions"],
            "expectedFiles":{
                "extensions/first.bin":before_custom["customFiles"]["extensions/first.bin"]["hash"],
                "extensions/second.bin":null,
                "extensions/local.txt":before_custom["customFiles"]["extensions/local.txt"]["hash"]
            },
            "fileWrites":{
                "extensions/first.bin":custom["customFiles"]["extensions/first.bin"]["base64"],
                "extensions/second.bin":custom["customFiles"]["extensions/second.bin"]["base64"]
            },
            "retainOneBackup":true
        }),
    )?;
    assert_eq!(
        copied["ok"], true,
        "Custom apply failed: {}",
        copied["error"]
    );
    let applied = copied["applied"].as_array().unwrap();
    assert_eq!(applied.len(), 2);
    assert!(applied.iter().all(|entry| entry["verified"] == true));
    assert_eq!(fs::read(custom_target.join("extensions/first.bin"))?, first);
    assert_eq!(
        fs::read(custom_target.join("extensions/second.bin"))?,
        second
    );
    assert_eq!(
        fs::read(custom_target.join("extensions/local.txt"))?,
        b"target only"
    );
    let saved_first = applied
        .iter()
        .find(|entry| entry["file"] == "path:extensions/first.bin")
        .unwrap();
    assert_eq!(
        fs::read(saved_first["backupPath"].as_str().unwrap())?,
        b"old fixture"
    );
    let custom_after = transport.run_worker(
        worker,
        &json!({
            "op":"snapshot", "agentDir":custom_target, "documents":[], "paths":["extensions"]
        }),
    )?;
    assert_eq!(custom_after["ok"], true);
    for file in ["extensions/first.bin", "extensions/second.bin"] {
        assert_eq!(
            custom_after["customFiles"][file]["hash"],
            custom["customFiles"][file]["hash"]
        );
    }
    drop(transport);
    let alias = ConnectionConfig {
        host: "fixture-alias".into(),
        username: String::new(),
        port: 22,
        key_path: None,
        use_config_user: true,
        use_config_port: true,
    };
    let mut alias_transport = fixture_connect(&alias, &known, |_| {
        panic!("Alias should use fixture config and existing trust")
    })?;
    let alias_snapshot =
        alias_transport.run_worker(worker, &json!({"op":"snapshot","agentDir":agent_dir}))?;
    assert_eq!(
        alias_snapshot["settings"]["value"],
        after["settings"]["value"]
    );
    drop(alias_transport);
    // A dedicated local agent offers the fixture key. Only its public half is
    // named in config, so this succeeds only through agent signing. Do not mutate
    // SSH_AUTH_SOCK in the test process or consult the user's actual agent.
    if std::env::var_os("PI_SYNC_TEST_SKIP_AGENT").as_deref() == Some(std::ffi::OsStr::new("1")) {
        eprintln!("SKIPPED: dedicated ssh-agent integration (PI_SYNC_TEST_SKIP_AGENT=1)");
    } else {
        let agent_socket = dir.path().join("agent.sock");
        let mut agent = Server(
            Command::new("ssh-agent")
                .arg("-D")
                .arg("-a")
                .arg(&agent_socket)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
            None,
        );
        let started = Instant::now();
        while !agent_socket.exists() {
            ensure!(agent.0.try_wait()?.is_none(), "Fixture ssh-agent exited");
            ensure!(
                started.elapsed() < Duration::from_secs(5),
                "Fixture agent startup timed out"
            );
            thread::sleep(Duration::from_millis(20));
        }
        ensure!(
            Command::new("ssh-add")
                .arg(&client)
                .env("SSH_AUTH_SOCK", &agent_socket)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
                .success(),
            "Cannot load synthetic identity into fixture ssh-agent"
        );
        let agent_config_path = dir.path().join("agent_config");
        fs::write(
            &agent_config_path,
            format!(
                "Host fixture-agent\n  HostName 127.0.0.1\n  User {}\n  Port {}\n  IdentityAgent \"{}\"\n  IdentityFile \"{}\"\n  IdentitiesOnly yes\n  GlobalKnownHostsFile /dev/null\n  UserKnownHostsFile \"{}\"\n",
                config.username,
                port,
                agent_socket.display(),
                client.with_extension("pub").display(),
                known.display()
            ),
        )?;
        let mut agent_options = fixture_options(&known)?;
        agent_options.config_file = Some(agent_config_path);
        let agent_connection = ConnectionConfig {
            host: "fixture-agent".into(),
            ..alias
        };
        let mut agent_transport = SshTransport::connect_with_options(
            &agent_connection,
            &known,
            |_| panic!("Agent-only connection must not ask for a secret"),
            &agent_options,
        )?;
        assert_eq!(
            agent_transport.run_worker(worker, &json!({"op":"snapshot","agentDir":agent_dir}))?["ok"],
            true
        );
        drop(agent_transport);
        drop(agent);
    }
    // Previously trusted host reconnects without an authentication or trust prompt.
    drop(fixture_connect(&config, &known, |_| {
        panic!("Unexpected prompt for trusted key-auth host")
    })?);
    // Disposable encrypted identity: these are synthetic fixture bytes, never a
    // user's password. Both cancellation and an incorrect answer must fail closed.
    const FIXTURE_PASSPHRASE: &str = "pi-sync-disposable-fixture-passphrase";
    let encrypted = dir.path().join("encrypted_client");
    generate_key_with_passphrase(&encrypted, FIXTURE_PASSPHRASE)?;
    let mut authorized = fs::read_to_string(dir.path().join("authorized_keys"))?;
    authorized.push_str(&fs::read_to_string(encrypted.with_extension("pub"))?);
    fs::write(dir.path().join("authorized_keys"), authorized)?;
    let encrypted_config = ConnectionConfig {
        key_path: Some(encrypted.clone()),
        ..config.clone()
    };
    for cancel in [true, false] {
        let mut asked = 0;
        let failed = fixture_connect(&encrypted_config, &known, |prompt| match prompt {
            AuthPrompt::KeyPassphrase { key_path } => {
                assert_eq!(key_path, encrypted);
                asked += 1;
                if cancel {
                    AuthAnswer::Cancel
                } else {
                    AuthAnswer::Secret(zeroize::Zeroizing::new("incorrect-fixture-answer".into()))
                }
            }
            _ => panic!("Unexpected prompt for encrypted-key fixture"),
        });
        assert!(
            (1..=3).contains(&asked),
            "Passphrase attempts must be bounded"
        );
        if cancel {
            assert_eq!(asked, 1);
        }
        let message = failed
            .err()
            .context("Incorrect or cancelled passphrase unexpectedly authenticated")?
            .to_string();
        assert!(!message.is_empty());
        assert!(!message.contains(FIXTURE_PASSPHRASE));
        assert!(!message.contains("incorrect-fixture-answer"));
    }
    let mut asked = 0;
    let mut encrypted_transport =
        fixture_connect(&encrypted_config, &known, |prompt| match prompt {
            AuthPrompt::KeyPassphrase { key_path } => {
                assert_eq!(key_path, encrypted);
                asked += 1;
                AuthAnswer::Secret(zeroize::Zeroizing::new(FIXTURE_PASSPHRASE.into()))
            }
            _ => panic!("Unexpected prompt for encrypted-key fixture"),
        })?;
    assert_eq!(asked, 1);
    let encrypted_snapshot =
        encrypted_transport.run_worker(worker, &json!({"op":"snapshot","agentDir":agent_dir}))?;
    assert_eq!(encrypted_snapshot["ok"], true);
    assert_eq!(
        encrypted_snapshot["settings"]["value"],
        after["settings"]["value"]
    );
    drop(encrypted_transport);
    exercise_session_lifecycle(&config, &known, dir.path())?;
    drop(server);
    let changed = dir.path().join("changed_host");
    generate_key(&changed)?;
    let _changed_server = start_server(dir.path(), &changed, port, &node_dir)?;
    let refused = fixture_connect(&config, &known, |_| {
        panic!("Changed host identity must not be offered a trust override")
    });
    assert!(refused.is_err());
    assert!(
        refused
            .err()
            .unwrap()
            .to_string()
            .contains("Host key changed")
    );
    Ok(())
}

// These deliberately broken workers run only against disposable local paths.
// A write followed by disconnect/timeout/cancellation has an uncertain result;
// neither the failed request nor a later call may silently reconnect and replay.
fn exercise_session_lifecycle(config: &ConnectionConfig, known: &Path, dir: &Path) -> Result<()> {
    const WORKER: &str = r#"
import { appendFile } from 'node:fs/promises';
let count = 0;
export async function handle(request) {
  count++;
  if (request.marker) await appendFile(request.marker, 'once\n');
  if (request.op === 'crash') process.exit(17);
  if (request.op === 'wait') await new Promise(() => {});
  if (request.op === 'oversize') return { ok: true, text: 'x'.repeat(49 * 1024 * 1024) };
  if (request.op === 'stderr') process.stderr.write('x'.repeat(256 * 1024));
  return { ok: true, count, text: request.text };
}
"#;
    let mut transport = fixture_connect(config, known, |_| {
        panic!("Unexpected fixture authentication")
    })?;
    for count in 1..=3 {
        let value = transport.run_worker(
            WORKER,
            &json!({"op":"count", "text":"quotes \" \n Unicode 雪"}),
        )?;
        assert_eq!(
            value["count"], count,
            "Requests must share one worker process"
        );
        assert_eq!(value["text"], "quotes \" \n Unicode 雪");
    }
    assert_eq!(
        transport.run_worker(WORKER, &json!({"op":"stderr"}))?["ok"],
        true
    );
    drop(transport);
    let oversize_marker = dir.join("oversize-marker");
    let mut transport = fixture_connect(config, known, |_| {
        panic!("Unexpected fixture authentication")
    })?;
    assert!(
        transport
            .run_worker(
                WORKER,
                &json!({
                    "marker": oversize_marker, "text": "x".repeat(49 * 1024 * 1024)
                })
            )
            .is_err(),
        "Oversized request must be rejected before transmission"
    );
    assert!(!oversize_marker.exists());
    drop(transport);
    let mut transport = fixture_connect(config, known, |_| {
        panic!("Unexpected fixture authentication")
    })?;
    assert!(
        transport
            .run_worker(WORKER, &json!({"op":"oversize"}))
            .is_err(),
        "Oversized responses must fail closed"
    );
    assert!(
        transport
            .run_worker(WORKER, &json!({"op":"count"}))
            .is_err()
    );
    drop(transport);
    for mode in ["crash", "timeout", "cancel"] {
        let marker = dir.join(format!("{mode}-marker"));
        let mut options = fixture_options(known)?;
        if mode == "timeout" {
            options.request_timeout = Duration::from_millis(300);
        }
        let mut transport = SshTransport::connect_with_options(
            config,
            known,
            |_| panic!("Unexpected fixture authentication"),
            &options,
        )?;
        let request =
            json!({"op": if mode == "crash" { "crash" } else { "wait" }, "marker":marker});
        let started = Instant::now();
        let failed = if mode == "cancel" {
            transport.run_worker_cancellable(WORKER, &request, || marker.exists())
        } else {
            transport.run_worker(WORKER, &request)
        };
        assert!(failed.is_err(), "{mode} should fail closed");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{mode} was not bounded"
        );
        assert_eq!(
            fs::read_to_string(&marker)?,
            "once\n",
            "{mode} must never replay a write"
        );
        assert!(
            transport.run_worker(WORKER, &request).is_err(),
            "Failed session must require explicit reconnect"
        );
        assert_eq!(fs::read_to_string(&marker)?, "once\n");
    }
    Ok(())
}

#[test]
fn fixture_diagnostics_are_bounded_to_the_tail() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sshd.log");
    let mut contents = vec![b'a'; 32 * 1024];
    contents.extend_from_slice(b"fixture rejection reason\n");
    fs::write(&path, &contents)?;
    let tail = fixture_log_tail(&path)?;
    assert_eq!(tail.len(), 16 * 1024);
    assert!(tail.ends_with("fixture rejection reason\n"));
    fs::write(&path, b"short log")?;
    assert_eq!(fixture_log_tail(&path)?, "short log");
    Ok(())
}
