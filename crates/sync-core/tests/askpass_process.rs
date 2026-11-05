//! Real child-process coverage; run with `node scripts/test-askpass.mjs`.
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use sync_core::{
    askpass::Askpass,
    connection::ConnectionConfig,
    transport::{AuthAnswer, AuthPrompt},
};
use zeroize::Zeroizing;

// Reap even on an assertion failure, so a failed test never leaves a helper
// waiting for its ten-minute interactive deadline.
struct Helper(Option<Child>);
impl Helper {
    fn spawn(command: &mut Command) -> Self {
        Self(Some(
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn askpass fixture"),
        ))
    }
    fn finish(mut self, mut poll: impl FnMut()) -> Output {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            poll();
            if self.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                return self.0.take().unwrap().wait_with_output().unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "askpass helper did not exit within ten seconds"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Helper {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn bridge(executable: &Path) -> Askpass {
    Askpass::new(
        &ConnectionConfig::parse("ssh pi@example.com").unwrap(),
        executable,
    )
    .unwrap()
}
fn command(executable: &Path, prompt: &str, bridge: &Askpass) -> Command {
    let mut command = Command::new(executable);
    command.arg(prompt);
    bridge.configure(&mut command);
    command
}

#[test]
#[ignore = "requires the compiled fixture; run node scripts/test-askpass.mjs"]
fn native_helper_process_protocol() {
    let executable = PathBuf::from(
        std::env::var_os("PI_SYNC_TEST_ASKPASS")
            .expect("PI_SYNC_TEST_ASKPASS must name the compiled fixture"),
    );
    assert!(executable.is_absolute() && executable.is_file());
    let secret = "synthetic-process-secret-42";
    let fingerprint = format!("SHA256:{}", STANDARD_NO_PAD.encode([7; 32]));
    let trust = format!(
        "The authenticity of host 'example.com (127.0.0.1)' can't be established.\nED25519 key fingerprint is {fingerprint}.\nThis key is not known by any other names.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? "
    );
    for (raw, expected, cancel) in [
        ("pi@example.com's password: ", Some(secret), false),
        (
            "Enter passphrase for key '/synthetic/id_ed25519': ",
            Some(secret),
            false,
        ),
        (trust.as_str(), Some("yes"), false),
        ("pi@example.com's password: ", None, true),
        ("Enter OTP:", None, true),
    ] {
        let mut bridge = bridge(&executable);
        let helper = Helper::spawn(&mut command(&executable, raw, &bridge));
        let mut callbacks = 0;
        let mut cancelled = false;
        let output = helper.finish(|| {
            cancelled |= bridge
                .poll(&mut |prompt| {
                    callbacks += 1;
                    assert_eq!(callbacks, 1);
                    match prompt {
                        AuthPrompt::Password { username } => assert_eq!(username, "pi"),
                        AuthPrompt::KeyPassphrase { key_path } => {
                            assert_eq!(key_path, Path::new("/synthetic/id_ed25519"))
                        }
                        AuthPrompt::TrustHost {
                            host,
                            port,
                            fingerprint: actual,
                        } => {
                            assert_eq!(host, "example.com (127.0.0.1)");
                            assert_eq!(port, 22);
                            assert_eq!(actual, fingerprint);
                        }
                    }
                    match expected {
                        Some("yes") => AuthAnswer::Trust(true),
                        Some(value) => AuthAnswer::Secret(Zeroizing::new(value.to_owned())),
                        None => AuthAnswer::Cancel,
                    }
                })
                .unwrap();
        });
        assert_eq!(cancelled, cancel);
        assert_eq!(callbacks, usize::from(raw != "Enter OTP:"));
        assert_eq!(output.status.success(), expected.is_some());
        assert!(
            output.stderr.is_empty(),
            "helper must never emit diagnostic data"
        );
        assert_eq!(
            output.stdout,
            expected
                .map(|value| format!("{value}\n").into_bytes())
                .unwrap_or_default()
        );
    }

    // A parent disappearing cannot strand the helper or yield a secret.
    let bridge = bridge(&executable);
    let helper = Helper::spawn(&mut command(
        &executable,
        "pi@example.com's password:",
        &bridge,
    ));
    drop(bridge);
    let output = helper.finish(|| {});
    assert!(!output.status.success());
    assert!(output.stdout.is_empty() && output.stderr.is_empty());

    // Without helper mode the fixture exits, rather than starting a UI/runtime.
    let output = Helper::spawn(
        Command::new(&executable)
            .env_remove("PI_SYNC_ASKPASS_ENDPOINT")
            .env_remove("PI_SYNC_ASKPASS_CAPABILITY"),
    )
    .finish(|| {});
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
}
