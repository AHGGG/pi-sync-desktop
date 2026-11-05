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
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use sync_core::{
    connection::ConnectionConfig,
    transport::{AuthAnswer, AuthPrompt, SshTransport},
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
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
    let child = Command::new(sshd)
        .args(["-D", "-e", "-f"])
        .arg(config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut server = Server(child);
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
        key_path: Some(client),
    };
    let known = dir.path().join("known_hosts");
    let denied = SshTransport::connect(&config, &known, |_| AuthAnswer::Cancel);
    assert!(denied.is_err());
    assert!(!known.exists());
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
    let mut transport = SshTransport::connect(&config, &known, |prompt| match prompt {
        AuthPrompt::TrustHost {
            host,
            port: actual_port,
            fingerprint,
        } => {
            assert_eq!(host, "127.0.0.1");
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
    // Previously trusted host reconnects without an authentication or trust prompt.
    drop(SshTransport::connect(&config, &known, |_| {
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
        let failed = SshTransport::connect(&encrypted_config, &known, |prompt| match prompt {
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
        assert_eq!(asked, 1);
        let message = failed
            .err()
            .context("Incorrect or cancelled passphrase unexpectedly authenticated")?
            .to_string();
        assert_eq!(
            message,
            if cancel {
                "Authentication cancelled"
            } else {
                "SSH key authentication failed"
            }
        );
        assert!(!message.contains(FIXTURE_PASSPHRASE));
        assert!(!message.contains("incorrect-fixture-answer"));
    }
    let mut asked = 0;
    let mut encrypted_transport =
        SshTransport::connect(&encrypted_config, &known, |prompt| match prompt {
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
    drop(server);
    let changed = dir.path().join("changed_host");
    generate_key(&changed)?;
    let _changed_server = start_server(dir.path(), &changed, port, &node_dir)?;
    let refused = SshTransport::connect(&config, &known, |_| {
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
