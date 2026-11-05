//! Headless askpass executable for localhost integration tests.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
fn main() {
    std::process::exit(sync_core::askpass::run_helper_if_requested().unwrap_or(1));
}
