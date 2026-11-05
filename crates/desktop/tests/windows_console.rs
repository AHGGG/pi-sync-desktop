//! Exercise the production debug executable's console behavior without a GPU.
//! A separate console confines the generated Ctrl+C to this disposable fixture.
#![cfg(all(windows, debug_assertions))]

use std::{
    io::Read,
    os::windows::process::CommandExt,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};
use sync_core::{askpass::Askpass, connection::ConnectionConfig, transport::AuthAnswer};

const PROBE: &str = "PI_SYNC_TEST_PRIVATE_CONSOLE";
const CREATE_NEW_CONSOLE: u32 = 0x00000010;
const CTRL_C_EVENT: u32 = 0;
const STATUS_CONTROL_C_EXIT: u32 = 0xc000013a;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
    fn GenerateConsoleCtrlEvent(event: u32, process_group: u32) -> i32;
    fn GetConsoleProcessList(processes: *mut u32, capacity: u32) -> u32;
}

// A handler function is not inherited by children. Setting the process's
// ignore-Ctrl+C flag instead would also make the desktop ignore the test event.
unsafe extern "system" fn protect_probe(event: u32) -> i32 {
    i32::from(event == CTRL_C_EVENT)
}

struct Guard(Child);
impl Guard {
    fn wait(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.0.try_wait().expect("poll fixture") {
                return status;
            }
            assert!(Instant::now() < deadline, "fixture did not exit in time");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn console_processes() -> Vec<u32> {
    let mut ids = [0; 16];
    // SAFETY: the buffer has capacity for the number of u32 entries supplied.
    let count = unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) };
    assert!(
        count > 0 && count as usize <= ids.len(),
        "query private console"
    );
    ids[..count as usize].to_vec()
}

fn collect(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut output = String::new();
        pipe.read_to_string(&mut output).unwrap();
        output
    })
}

#[test]
fn ctrl_c_stops_desktop_debug_process() {
    if std::env::var_os(PROBE).is_none() {
        // Never generate control events in Cargo's or the CI runner's console.
        // Run just this test in its own new console, not a new process group
        // within the existing console (CTRL_C_EVENT cannot target a group).
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ctrl_c_stops_desktop_debug_process",
                "--nocapture",
            ])
            .env(PROBE, "1")
            .creation_flags(CREATE_NEW_CONSOLE)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start isolated console probe");
        let mut child = Guard(child);
        // Drain while waiting so a failure backtrace cannot fill a Windows pipe.
        let stdout = collect(child.0.stdout.take().unwrap());
        let stderr = collect(child.0.stderr.take().unwrap());
        let status = child.wait(Duration::from_secs(30));
        let stdout = stdout.join().unwrap();
        let stderr = stderr.join().unwrap();
        assert!(status.success(), "console probe failed: {stdout}\n{stderr}");
        return;
    }

    // Check isolation before installing the handler or emitting any event.
    assert_eq!(console_processes(), [std::process::id()]);
    // SAFETY: clear any ignore flag inherited from the runner, in this private
    // probe only. Its child must receive normal console interrupt processing.
    assert_ne!(unsafe { SetConsoleCtrlHandler(None, 0) }, 0);
    // SAFETY: the static callback only inspects its argument and returns a BOOL.
    assert_ne!(unsafe { SetConsoleCtrlHandler(Some(protect_probe), 1) }, 0);

    let executable = Path::new(env!("CARGO_BIN_EXE_pi-sync-desktop"));
    let connection = ConnectionConfig::parse("ssh fixture@example.com").unwrap();
    let mut bridge = Askpass::new(&connection, executable).unwrap();
    let mut command = Command::new(executable);
    bridge.configure(&mut command);
    // Existing askpass mode keeps the actual binary alive on a synthetic local
    // socket. It never initializes GPUI, profiles, Node or an SSH connection.
    let mut desktop = Guard(
        command
            .arg("fixture@example.com's password:")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start production desktop executable"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut interrupted = false;
    while !interrupted {
        let result = bridge.poll(&mut |_| {
            assert!(
                console_processes().contains(&desktop.0.id()),
                "debug desktop must inherit the launching console"
            );
            // SAFETY: both processes are confined to the console verified above.
            assert_ne!(unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) }, 0);
            let status = desktop.wait(Duration::from_secs(10));
            assert_eq!(
                status.code().map(|code| code as u32),
                Some(STATUS_CONTROL_C_EXIT)
            );
            interrupted = true;
            AuthAnswer::Cancel
        });
        // After Ctrl+C the socket may already be closed; no reply is needed.
        if interrupted {
            break;
        }
        result.expect("poll synthetic askpass startup");
        assert!(
            desktop.0.try_wait().unwrap().is_none(),
            "desktop exited before Ctrl+C"
        );
        assert!(
            Instant::now() < deadline,
            "desktop did not enter askpass mode"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
