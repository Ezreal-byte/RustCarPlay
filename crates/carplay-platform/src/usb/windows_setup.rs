//! Explicit, user-initiated Windows USB preparation through the reviewed helper.
//! Call on a worker thread after a button click. Discovery never invokes this.

use serde::Deserialize;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparationOutcome {
    Ready,
    ReplugRequired {
        message: String,
        resume_after_replug: bool,
    },
    Failed(String),
}

#[derive(Deserialize)]
struct HelperResult {
    success: bool,
    #[serde(default)]
    requires_replug: bool,
    #[serde(default)]
    resume_after_replug: bool,
    error: Option<String>,
    cleanup_error: Option<String>,
    restart_error: Option<String>,
}

fn parse_result(bytes: &[u8], exit_code: u32) -> Result<PreparationOutcome, String> {
    if bytes.len() > 64 * 1024 {
        return Err("Windows USB preparation returned an oversized result.".into());
    }
    let result: HelperResult = serde_json::from_slice(bytes)
        .map_err(|_| "Windows USB preparation did not return a valid result.".to_owned())?;
    if result.success && exit_code == 0 {
        return Ok(PreparationOutcome::Ready);
    }
    let mut message = result
        .error
        .unwrap_or_else(|| format!("Windows USB preparation failed (exit {exit_code})."));
    for detail in [result.cleanup_error, result.restart_error]
        .into_iter()
        .flatten()
    {
        message.push('\n');
        message.push_str(&detail);
    }
    if result.requires_replug {
        Ok(PreparationOutcome::ReplugRequired {
            message,
            resume_after_replug: result.resume_after_replug,
        })
    } else {
        Ok(PreparationOutcome::Failed(message))
    }
}

fn absolute(path: PathBuf, purpose: &str) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("{purpose} must be an absolute directory."));
    }
    Ok(path)
}

fn package_directory() -> Result<(PathBuf, bool), String> {
    if let Some(root) = std::env::var_os("RUSTCARPLAY_PACKAGE_DIR") {
        return absolute(root.into(), "RUSTCARPLAY_PACKAGE_DIR").map(|root| {
            let development = root.join("Cargo.toml").is_file();
            (root, development)
        });
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let parent = executable
        .parent()
        .ok_or("Cannot locate application directory.")?;
    if parent.file_name() == Some(OsStr::new("app")) {
        return Ok((
            parent
                .parent()
                .ok_or("Cannot locate package directory.")?
                .into(),
            false,
        ));
    }
    let development = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("Cannot locate development directory.")?;
    if executable.starts_with(development.join("target"))
        && development.join("Cargo.toml").is_file()
    {
        return Ok((development.into(), true));
    }
    Err("Start the installed RustCarPlay launcher before preparing Windows USB.".into())
}

fn state_directory() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("RUSTCARPLAY_USB_STATE_DIR") {
        return absolute(path.into(), "RUSTCARPLAY_USB_STATE_DIR");
    }
    let user = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable.")?;
    absolute(
        PathBuf::from(user).join("RustCarPlay/.local/windows-usb"),
        "USB state path",
    )
}

/// A saved checkpoint changes the button offered to the user, never starts a
/// privileged operation. The helper still verifies the exact phone and mode.
pub fn pending_replug(phone: &super::UsbPhone) -> bool {
    if phone.readiness == super::Readiness::InterfacesAvailable {
        return false;
    }
    let Some(token) = phone.windows_device_token.as_deref() else {
        return false;
    };
    state_directory().is_ok_and(|state| pending_replug_at(&state, token))
}

fn bounded_read(path: &Path, maximum: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(maximum + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= maximum).then_some(bytes)
}

fn pending_replug_at(state: &Path, token: &str) -> bool {
    if !token.strip_prefix("winusb-").is_some_and(|suffix| {
        suffix.len() == 16
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
    }) {
        return false;
    }
    if !state.join(format!("{token}.json")).is_file()
        && !state.join(format!("{token}.filter.json")).is_file()
    {
        return false;
    }
    let checkpoint = state.join(format!("{token}.resume.json"));
    if checkpoint.exists() {
        return bounded_read(&checkpoint, 4096)
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|v| {
                v["resume_after_replug"] == true
                    && matches!(
                        v["stage"].as_str(),
                        Some(
                            "filter_install"
                                | "initial_restart"
                                | "initial_mode_after_replug"
                                | "descriptor_reset"
                        )
                    )
            });
    }
    // Recover an interrupted 0.1.3 preparation without inventing a target
    // configuration. Arm will independently validate this same fresh report.
    let report = state.join(format!("{token}.descriptors.json"));
    let fresh = fs::metadata(&report)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|time| time.elapsed().ok())
        .is_some_and(|age| age <= Duration::from_secs(30 * 60));
    fresh
        && bounded_read(&report, 64 * 1024)
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|v| {
                v.as_array().is_some_and(|a| {
                    a.len() == 1
                        && a[0]["windows_device_token"] == token
                        && a[0]["configuration"].is_object()
                })
            })
}

fn stage_message(stage: &str) -> Option<&'static str> {
    Some(match stage.trim() {
        "preflight" => "正在检查 USB 设备与运行环境…",
        "driver_verify" => "正在校验 USB 驱动签名与现有配置…",
        "filter_install" => "正在安装所选 iPhone 的 USB 驱动组件…",
        "configuration_prepare" => "正在保存恢复记录并准备 USB 配置…",
        "initial_restart" => "正在请求 Windows 重启 USB 设备；系统拒绝时会提示拔插…",
        "initial_mode_after_replug" => "正在检查拔插后的 iPhone USB 模式…",
        "descriptor_probe" => "正在读取 iPhone 的 CarPlay USB 配置…",
        "descriptor_reset" => "正在重置 USB 模式；可能需要再次拔插数据线…",
        "activate_configuration" => "正在激活 CarPlay USBMUX / NCM 配置…",
        "cleanup" => "正在恢复安全的下次插入配置…",
        _ => return None,
    })
}

fn supported_powershell(candidate: &Path) -> bool {
    let Ok(mut child) = Command::new(candidate)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$PSVersionTable.PSVersion.ToString()",
        ])
        .creation_flags(0x08000000)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child.wait_with_output().is_ok_and(|o| {
                    o.status.success()
                        && (o.stdout.starts_with(b"7.") || o.stdout.starts_with(b"5.1."))
                });
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn powershell() -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("CODEX_PWSH_PATH") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(directory) = std::env::var_os("ProgramFiles") {
        candidates.push(PathBuf::from(directory).join("PowerShell/7/pwsh.exe"));
    }
    if let Some(directory) = std::env::var_os("SystemRoot") {
        candidates
            .push(PathBuf::from(directory).join("System32/WindowsPowerShell/v1.0/powershell.exe"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(
            std::env::split_paths(&path)
                .filter(|p| p.is_absolute())
                .map(|p| p.join("pwsh.exe")),
        );
    }
    for candidate in candidates {
        if candidate.is_absolute() && candidate.is_file() && supported_powershell(&candidate) {
            return Ok(candidate);
        }
    }
    Err("Windows PowerShell 5.1 or PowerShell 7 is unavailable; restore the Windows PowerShell component and retry.".into())
}

fn arguments(
    root: &Path,
    state: &Path,
    result: &Path,
    device_id: &str,
    resume: bool,
    legacy: bool,
) -> Result<Vec<OsString>, String> {
    if !device_id.strip_prefix("usb-").is_some_and(|id| {
        id.len() == 16
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) {
        return Err("Select a discovered USB iPhone before preparation.".into());
    }
    let mut args: Vec<OsString> = [
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.extend([
        root.join("scripts/start-windows-usb.ps1").into_os_string(),
        "-Action".into(),
        "Prepare".into(),
        "-DeviceId".into(),
        device_id.into(),
        "-StateDirectory".into(),
        state.as_os_str().into(),
        "-ResultFile".into(),
        result.as_os_str().into(),
    ]);
    if resume {
        args.push("-ResumeAfterReplug".into());
    }
    if legacy {
        args.push("-LegacyStateDirectory".into());
        args.push(root.join(".local/windows-usb").into_os_string());
    }
    Ok(args)
}

/// Windows argv escaping for ShellExecuteExW; this is never PowerShell source.
fn command_line(args: &[OsString]) -> Result<Vec<u16>, String> {
    let mut output = Vec::new();
    for (index, argument) in args.iter().enumerate() {
        if index != 0 {
            output.push(b' ' as u16);
        }
        output.push(b'"' as u16);
        let mut slashes = 0;
        for ch in argument.encode_wide() {
            if ch == 0 {
                return Err("Invalid NUL in preparation argument.".into());
            }
            if ch == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            output.extend(std::iter::repeat_n(
                b'\\' as u16,
                if ch == b'"' as u16 {
                    slashes * 2 + 1
                } else {
                    slashes
                },
            ));
            slashes = 0;
            output.push(ch);
        }
        output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        output.push(b'"' as u16);
    }
    output.push(0);
    Ok(output)
}

struct Process(HANDLE);
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Explicitly requests elevation and waits for the helper's own cleanup.
/// It must not be called from discovery, startup, or the UI rendering thread.
pub fn prepare_usb(
    device_id: &str,
    resume_after_replug: bool,
    progress: impl Fn(String),
) -> Result<PreparationOutcome, String> {
    progress("正在检查 USB 准备环境…".into());
    let (root, development) = package_directory()?;
    let state = state_directory()?;
    for name in [
        "start-windows-usb.ps1",
        "windows-usb-config.ps1",
        "windows-usb-filter.ps1",
        "windows-usb-paths.ps1",
    ] {
        if !root.join("scripts").join(name).is_file() {
            return Err("The installed USB preparation scripts are missing; reinstall the complete package.".into());
        }
    }
    let shell = powershell()?;
    fs::create_dir_all(&state).map_err(|_| "Cannot create the USB state directory.".to_owned())?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let result_file = state.join(format!("preparation-{}-{nonce}.json", std::process::id()));
    let phone = super::native::probe(Some(device_id)).map_err(|e| e.to_string())?;
    let has_record = phone
        .windows_device_token
        .as_ref()
        .is_some_and(|token| state.join(format!("{token}.filter.json")).is_file());
    let args = arguments(
        &root,
        &state,
        &result_file,
        device_id,
        resume_after_replug,
        development && !has_record && root.join(".local/windows-usb").is_dir(),
    )?;
    let parameters = command_line(&args)?;
    let program: Vec<_> = shell.as_os_str().encode_wide().chain([0]).collect();
    let directory: Vec<_> = root.as_os_str().encode_wide().chain([0]).collect();
    let verb: Vec<_> = "runas".encode_utf16().chain([0]).collect();
    let mut execute = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb.as_ptr(),
        lpFile: program.as_ptr(),
        lpParameters: parameters.as_ptr(),
        lpDirectory: directory.as_ptr(),
        nShow: SW_HIDE,
        ..unsafe { std::mem::zeroed() }
    };
    progress("正在请求管理员权限；系统可能自动批准，不一定出现弹窗…".into());
    if unsafe { ShellExecuteExW(&mut execute) } == 0 {
        let error = std::io::Error::last_os_error();
        return Err(if error.raw_os_error() == Some(1223) {
            "Windows USB preparation was cancelled at the administrator prompt.".into()
        } else {
            format!("Cannot start Windows USB preparation: {error}")
        });
    }
    if execute.hProcess.is_null() {
        return Err("Windows did not return a preparation process handle.".into());
    }
    let process = Process(execute.hProcess);
    progress("USB 准备脚本已启动，正在检查设备…".into());
    // Never terminate this process halfway through a registry transaction. The
    // helper has bounded device waits and always disarms in its finally block.
    let progress_file = result_file.with_extension("json.progress");
    let mut last_stage = String::new();
    let mut last_update = Instant::now();
    loop {
        match unsafe { WaitForSingleObject(process.0, 250) } {
            WAIT_OBJECT_0 => break,
            WAIT_TIMEOUT => {},
            _ => return Err("Could not wait for Windows USB preparation; check its saved result before retrying.".into()),
        }
        if let Some(bytes) = bounded_read(&progress_file, 256)
            && let Ok(stage) = std::str::from_utf8(&bytes)
            && let Some(message) = stage_message(stage)
            && (stage != last_stage || last_update.elapsed() >= Duration::from_secs(30))
        {
            progress(if stage == last_stage {
                format!("{message}（仍在执行，请保持手机解锁）")
            } else {
                message.into()
            });
            last_stage = stage.into();
            last_update = Instant::now();
        }
    }
    let _ = fs::remove_file(&progress_file);
    let mut code = 0;
    if unsafe { GetExitCodeProcess(process.0, &mut code) } == 0 {
        return Err("Cannot read preparation status.".into());
    }
    let metadata = fs::metadata(&result_file)
        .map_err(|_| "Windows USB preparation did not save a result.".to_owned())?;
    if metadata.len() > 64 * 1024 {
        return Err("Windows USB preparation returned an oversized result.".into());
    }
    let bytes = fs::read(&result_file).map_err(|e| e.to_string())?;
    let outcome = parse_result(&bytes, code);
    let _ = fs::remove_file(&result_file);
    if outcome == Ok(PreparationOutcome::Ready) {
        let phone = super::native::probe(Some(device_id)).map_err(|e| e.to_string())?;
        if !phone
            .configuration
            .is_some_and(|c| phone.active_configuration == Some(c.configuration_value))
        {
            return Ok(PreparationOutcome::Failed("USB preparation finished but CarPlay configuration is not active. Refresh the device and retry.".into()));
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_survives_restart_but_needs_same_phone_records_and_valid_stage() {
        let state = std::env::temp_dir().join(format!(
            "rustcarplay-resume-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&state).unwrap();
        let token = "winusb-0123456789ABCDEF";
        let receipt = state.join(format!("{token}.filter.json"));
        let checkpoint = state.join(format!("{token}.resume.json"));
        fs::write(
            &checkpoint,
            br#"{"resume_after_replug":true,"stage":"descriptor_reset"}"#,
        )
        .unwrap();
        assert!(
            !pending_replug_at(&state, token),
            "checkpoint alone cannot skip preparation"
        );
        fs::write(&receipt, b"{}").unwrap();
        assert!(pending_replug_at(&state, token));
        assert!(!pending_replug_at(&state, "winusb-1111111111111111"));
        assert!(!pending_replug_at(&state, "../escape"));
        fs::write(
            &checkpoint,
            br#"{"resume_after_replug":true,"stage":"cleanup"}"#,
        )
        .unwrap();
        assert!(
            !pending_replug_at(&state, token),
            "failed cleanup cannot be resumed"
        );
        fs::remove_file(&checkpoint).unwrap();
        let report = state.join(format!("{token}.descriptors.json"));
        fs::write(&report, format!(r#"[{{"windows_device_token":"{token}","configuration":{{"configuration_index":5}}}}]"#)).unwrap();
        assert!(
            pending_replug_at(&state, token),
            "fresh 0.1.3 checkpoints can be recovered"
        );
        fs::File::options()
            .write(true)
            .open(&report)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(31 * 60)),
            )
            .unwrap();
        assert!(
            !pending_replug_at(&state, token),
            "stale legacy reports cannot skip discovery"
        );
        fs::write(&checkpoint, vec![b' '; 4097]).unwrap();
        assert!(!pending_replug_at(&state, token));
        fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn progress_only_exposes_known_stage_descriptions() {
        assert!(
            stage_message("activate_configuration")
                .unwrap()
                .contains("USBMUX")
        );
        assert!(stage_message("descriptor_reset").unwrap().contains("拔插"));
        assert!(stage_message("untrusted arbitrary helper text").is_none());
    }

    #[test]
    fn helper_arguments_keep_paths_literal_and_reject_untrusted_device_text() {
        let root = Path::new(r"C:\App 中文 $literal;data");
        let state = Path::new(r"C:\Users\User\state");
        let result = state.join("result.json");
        let args = arguments(root, state, &result, "usb-0123456789abcdef", true, false).unwrap();
        assert!(!args.iter().any(|a| a == "-Command"));
        assert!(args.contains(&root.join("scripts/start-windows-usb.ps1").into_os_string()));
        assert!(args.contains(&OsString::from("-ResumeAfterReplug")));
        assert!(!args.contains(&OsString::from("-LegacyStateDirectory")));
        for invalid in [
            "usb-0123456789abcdef;exit",
            "-File evil.ps1",
            "usb-0123456789ABCDEf",
        ] {
            assert!(arguments(root, state, &result, invalid, false, false).is_err());
        }
    }

    #[test]
    fn argv_quotes_spaces_quotes_and_terminal_backslashes_without_shell_source() {
        let encoded = command_line(&[
            r"C:\folder with space\".into(),
            "a\"b".into(),
            "$(literal)".into(),
        ])
        .unwrap();
        let text = String::from_utf16(&encoded[..encoded.len() - 1]).unwrap();
        assert_eq!(
            text,
            "\"C:\\folder with space\\\\\" \"a\\\"b\" \"$(literal)\""
        );
        assert!(command_line(&["bad\0argument".into()]).is_err());
    }

    #[test]
    fn helper_result_distinguishes_replug_from_failure_and_requires_zero_exit() {
        assert_eq!(
            parse_result(br#"{"success":true}"#, 0).unwrap(),
            PreparationOutcome::Ready
        );
        assert!(matches!(
            parse_result(br#"{"success":true}"#, 1).unwrap(),
            PreparationOutcome::Failed(_)
        ));
        assert_eq!(parse_result(br#"{"success":false,"requires_replug":true,"resume_after_replug":true,"error":"Reconnect cable"}"#, 1).unwrap(),
                   PreparationOutcome::ReplugRequired { message: "Reconnect cable".into(), resume_after_replug: true });
        assert!(parse_result(br#"{}"#, 0).is_err());
        assert!(parse_result(&vec![b' '; 65537], 0).is_err());
    }
}
