#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
// The public portable entry point is intentionally named RustCarPlay.exe.
#![allow(non_snake_case)]

use carplay_launcher::{LaunchPlan, Platform};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::process::Stdio;

fn main() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let cli = arguments
        .first()
        .is_some_and(|argument| argument == "--cli");
    if cli {
        attach_cli_console();
    }
    match run(arguments) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("RustCarPlay: {error}");
            if !cli {
                show_error(&error);
            }
            std::process::exit(1);
        }
    }
}

fn run(arguments: Vec<std::ffi::OsString>) -> Result<i32, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("Cannot locate the launcher executable: {error}"))?;
    let inherited = std::env::vars_os().collect::<BTreeMap<_, _>>();
    let mut plan = LaunchPlan::new(&executable, arguments, Platform::current(), &inherited)?;
    plan.validate_bundle()?;
    for directory in [&plan.registry_directory, &plan.log_directory] {
        fs::create_dir_all(directory).map_err(|error| {
            format!(
                "Cannot create {}: {error}. Extract this portable package into a writable directory.",
                directory.display()
            )
        })?;
    }
    let log_path = plan.log_directory.join("launcher.log");
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| {
            format!(
                "Cannot write {}: {error}. Use a writable extraction directory.",
                log_path.display()
            )
        })?;
    // Do not record command arguments or environment values: either can contain
    // credentials supplied by the operator.
    writeln!(
        log,
        "RustCarPlay {} launcher starting (cli={})",
        env!("CARGO_PKG_VERSION"),
        plan.cli
    )
    .map_err(|error| format!("Cannot write the launcher log: {error}"))?;
    let mut command = plan.command();
    if !plan.cli {
        command.stdout(Stdio::from(
            log.try_clone().map_err(|error| error.to_string())?,
        ));
        command.stderr(Stdio::from(
            log.try_clone().map_err(|error| error.to_string())?,
        ));
    }
    let status = command.status().map_err(|error| {
        format!(
            "Cannot start {}: {error}. Check that the complete runtime was extracted. Log: {}",
            plan.application.display(),
            log_path.display()
        )
    })?;
    let _ = writeln!(log, "Application exited: {status}");
    if !status.success() && !plan.cli {
        return Err(format!(
            "The application exited with {status}. Details: {}",
            log_path.display()
        ));
    }
    Ok(status.code().unwrap_or(1))
}

#[cfg(target_os = "windows")]
fn attach_cli_console() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    // It is normal for this to fail if output is redirected or a console is
    // already attached. Inherited standard handles remain available.
    unsafe { AttachConsole(u32::MAX) };
}

#[cfg(not(target_os = "windows"))]
fn attach_cli_console() {}

#[cfg(target_os = "windows")]
fn show_error(error: &str) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            window: *mut std::ffi::c_void,
            text: *const u16,
            title: *const u16,
            flags: u32,
        ) -> i32;
    }
    let message = format!("RustCarPlay 启动失败 / Unable to start\n\n{error}")
        .replace('\0', "�")
        .encode_utf16()
        .chain([0])
        .collect::<Vec<_>>();
    let title = "RustCarPlay".encode_utf16().chain([0]).collect::<Vec<_>>();
    unsafe { MessageBoxW(std::ptr::null_mut(), message.as_ptr(), title.as_ptr(), 0x10) };
}

#[cfg(not(target_os = "windows"))]
fn show_error(_error: &str) {
    // stderr remains available when launched from a terminal. No GUI/media
    // toolkit is linked into this bootstrap executable.
}
