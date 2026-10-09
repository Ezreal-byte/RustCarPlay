//! A launch plan keeps native media libraries out of the launcher itself.
//! Environment changes are applied to the child Command, never to this process.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    Windows,
    Linux,
    MacOs,
}

impl Platform {
    pub fn current() -> Self {
        #[cfg(target_os = "windows")]
        return Self::Windows;
        #[cfg(target_os = "linux")]
        return Self::Linux;
        #[cfg(target_os = "macos")]
        return Self::MacOs;
        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        compile_error!("The portable launcher supports Windows, Linux and macOS.");
    }

    fn suffix(self) -> &'static str {
        if self == Self::Windows { ".exe" } else { "" }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::MacOs => "macos",
        }
    }
}

#[derive(Debug)]
pub struct LaunchPlan {
    pub root: PathBuf,
    /// Existing application settings use relative .local paths. Installed
    /// applications run from the user's data directory to preserve that layout.
    pub working_directory: PathBuf,
    pub application: PathBuf,
    pub arguments: Vec<OsString>,
    pub cli: bool,
    pub environment: BTreeMap<OsString, OsString>,
    pub registry_directory: PathBuf,
    pub log_directory: PathBuf,
    pub plugin_directory: PathBuf,
    pub scanner_candidates: [PathBuf; 2],
}

impl LaunchPlan {
    pub fn new(
        launcher_executable: &Path,
        arguments: Vec<OsString>,
        platform: Platform,
        inherited: &BTreeMap<OsString, OsString>,
    ) -> Result<Self, String> {
        let root = package_root(launcher_executable, platform)?;
        let installed = installed_mode(&root)?;
        if platform == Platform::MacOs
            && launcher_executable.parent() != Some(root.as_path())
            && !installed
        {
            return Err(
                "The macOS application is missing Resources/payload/INSTALLATION.json.".into(),
            );
        }
        Self::build(
            launcher_executable,
            arguments,
            platform,
            inherited,
            installed,
        )
    }

    fn build(
        launcher_executable: &Path,
        arguments: Vec<OsString>,
        platform: Platform,
        inherited: &BTreeMap<OsString, OsString>,
        installed: bool,
    ) -> Result<Self, String> {
        // Windows environment keys are case insensitive; vars_os commonly
        // reports "Path", while Unix legitimately distinguishes it from PATH.
        let windows_environment;
        let inherited = if platform == Platform::Windows {
            windows_environment = inherited
                .iter()
                .map(|(key, value)| (key.to_ascii_uppercase(), value.clone()))
                .collect();
            &windows_environment
        } else {
            inherited
        };
        let root = package_root(launcher_executable, platform)?;
        let working_directory = if installed {
            user_data_directory(platform, inherited)?
        } else {
            root.clone()
        };
        let cli = arguments
            .first()
            .is_some_and(|argument| argument == "--cli");
        let arguments = if cli {
            arguments.into_iter().skip(1).collect()
        } else {
            arguments
        };
        let application_name = if cli {
            "rustcarplay"
        } else {
            "carplay-desktop"
        };
        let application = root
            .join("app")
            .join(format!("{application_name}{}", platform.suffix()));
        let runtime = root.join("runtime/gstreamer");
        let plugin_directory = runtime.join("lib/gstreamer-1.0");
        let registry_directory = working_directory.join(".local/gstreamer");
        let scanner_name = format!("gst-plugin-scanner{}", platform.suffix());
        let scanner_candidates = [
            runtime.join("libexec/gstreamer-1.0").join(&scanner_name),
            runtime.join("bin").join(&scanner_name),
        ];
        let mut environment = BTreeMap::new();
        prepend_paths(&mut environment, inherited, "PATH", &[runtime.join("bin")])?;
        match platform {
            Platform::Windows => {
                for (variable, relative) in [
                    ("RUSTCARPLAY_LIBIMOBILEDEVICE_DIR", "runtime/usb/bin"),
                    ("RUSTCARPLAY_USB_FILTER_DIR", "runtime/usb-filter"),
                ] {
                    environment.insert(
                        variable.into(),
                        inherited
                            .get(OsStr::new(variable))
                            .cloned()
                            .unwrap_or_else(|| root.join(relative).into_os_string()),
                    );
                }
            }
            Platform::Linux => {
                prepend_paths(
                    &mut environment,
                    inherited,
                    "LD_LIBRARY_PATH",
                    &[runtime.join("lib")],
                )?;
                environment.insert(
                    "ALSA_CONFIG_DIR".into(),
                    runtime.join("share/alsa").into_os_string(),
                );
                environment.insert(
                    "ALSA_PLUGIN_DIR".into(),
                    runtime.join("lib/alsa-lib").into_os_string(),
                );
            }
            Platform::MacOs => {
                for name in ["DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"] {
                    prepend_paths(&mut environment, inherited, name, &[runtime.join("lib")])?;
                }
            }
        }
        for name in ["GST_PLUGIN_SYSTEM_PATH_1_0", "GST_PLUGIN_PATH_1_0"] {
            environment.insert(name.into(), plugin_directory.clone().into_os_string());
        }
        // Prevent inherited system settings from pointing the child at a scanner
        // from another GStreamer build or CPU architecture.
        for name in ["GST_PLUGIN_SCANNER", "GST_PLUGIN_SCANNER_1_0"] {
            environment.insert(name.into(), scanner_candidates[0].clone().into_os_string());
        }
        environment.insert(
            "GST_REGISTRY_1_0".into(),
            registry_directory
                .join(format!(
                    "registry-{}-{}.bin",
                    platform.name(),
                    std::env::consts::ARCH
                ))
                .into_os_string(),
        );
        environment.insert(
            "RUSTCARPLAY_AUTH_DIR".into(),
            inherited
                .get(OsStr::new("RUSTCARPLAY_AUTH_DIR"))
                .cloned()
                .unwrap_or_else(|| root.join("resources/auth").into_os_string()),
        );
        Ok(Self {
            log_directory: working_directory.join(".local/logs"),
            root,
            working_directory,
            application,
            arguments,
            cli,
            environment,
            registry_directory,
            plugin_directory,
            scanner_candidates,
        })
    }

    pub fn validate_bundle(&mut self) -> Result<(), String> {
        if !self.application.is_file() {
            return Err(format!(
                "Application not found: {}. Extract the entire portable archive before launching.",
                self.application.display()
            ));
        }
        if !self.plugin_directory.is_dir() {
            return Err(format!(
                "Bundled media plugins are missing: {}. Extract the entire portable archive.",
                self.plugin_directory.display()
            ));
        }
        let scanner = self
            .scanner_candidates
            .iter()
            .find(|candidate| candidate.is_file())
            .ok_or_else(|| {
                format!(
                    "Bundled media scanner is missing: {}.",
                    self.scanner_candidates[0].display()
                )
            })?;
        for name in ["GST_PLUGIN_SCANNER", "GST_PLUGIN_SCANNER_1_0"] {
            self.environment
                .insert(name.into(), scanner.as_os_str().to_owned());
        }
        Ok(())
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.application);
        command
            .args(&self.arguments)
            .current_dir(&self.working_directory)
            .envs(&self.environment);
        command
    }
}

fn package_root(launcher_executable: &Path, platform: Platform) -> Result<PathBuf, String> {
    if !launcher_executable.is_absolute() {
        return Err("The launcher executable path must be absolute.".into());
    }
    let parent = launcher_executable
        .parent()
        .ok_or("Cannot locate the application directory.")?;
    if platform == Platform::MacOs
        && parent.file_name() == Some(OsStr::new("MacOS"))
        && let Some(contents) = parent.parent()
        && contents.file_name() == Some(OsStr::new("Contents"))
        && contents.parent().and_then(Path::extension) == Some(OsStr::new("app"))
    {
        // Keep only the signed launcher in the app's executable directory.
        // The installer preserves the portable payload layout under Resources.
        return Ok(contents.join("Resources/payload"));
    }
    Ok(parent.to_path_buf())
}

fn installed_mode(root: &Path) -> Result<bool, String> {
    let marker = root.join("INSTALLATION.json");
    let metadata = match std::fs::metadata(&marker) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Cannot read {}: {error}", marker.display())),
    };
    if !metadata.is_file() || metadata.len() > 4096 {
        return Err("The INSTALLATION.json marker is invalid or too large.".into());
    }
    let bytes = std::fs::read(&marker)
        .map_err(|error| format!("Cannot read {}: {error}", marker.display()))?;
    validate_installation_marker(&bytes)?;
    Ok(true)
}

fn validate_installation_marker(bytes: &[u8]) -> Result<(), String> {
    let marker: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| "The INSTALLATION.json marker is not valid JSON.")?;
    if marker.get("schema").and_then(serde_json::Value::as_u64) != Some(1)
        || marker.get("mode").and_then(serde_json::Value::as_str) != Some("installed")
        || marker.get("product").and_then(serde_json::Value::as_str) != Some("RustCarPlay")
    {
        return Err(
            "The INSTALLATION.json marker has an unsupported schema, mode or product.".into(),
        );
    }
    Ok(())
}

fn user_data_directory(
    platform: Platform,
    inherited: &BTreeMap<OsString, OsString>,
) -> Result<PathBuf, String> {
    let absolute_environment = |name: &str| {
        inherited
            .get(OsStr::new(name))
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    match platform {
        Platform::Windows => absolute_environment("LOCALAPPDATA")
            .map(|path| path.join("RustCarPlay"))
            .ok_or_else(|| "Installed mode requires an absolute LOCALAPPDATA directory.".into()),
        Platform::Linux => absolute_environment("XDG_DATA_HOME")
            .or_else(|| absolute_environment("HOME").map(|home| home.join(".local/share")))
            .map(|path| path.join("rustcarplay"))
            .ok_or_else(|| {
                "Installed mode requires an absolute XDG_DATA_HOME or HOME directory.".into()
            }),
        Platform::MacOs => absolute_environment("HOME")
            .map(|home| home.join("Library/Application Support/RustCarPlay"))
            .ok_or_else(|| "Installed mode requires an absolute HOME directory.".into()),
    }
}

fn prepend_paths(
    output: &mut BTreeMap<OsString, OsString>,
    inherited: &BTreeMap<OsString, OsString>,
    name: &str,
    bundled: &[PathBuf],
) -> Result<(), String> {
    let mut paths = bundled.to_vec();
    if let Some(value) = inherited.get(OsStr::new(name)) {
        // Empty entries mean the current working directory on Unix. Do not
        // introduce it as an additional native-library search location.
        paths.extend(std::env::split_paths(value).filter(|path| !path.as_os_str().is_empty()));
    }
    let value = std::env::join_paths(paths)
        .map_err(|error| format!("Cannot prepare the {name} search path: {error}"))?;
    output.insert(name.into(), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable() -> PathBuf {
        std::env::temp_dir().join("CarPlay 中文 folder/RustCarPlay.exe")
    }

    fn plan(platform: Platform, arguments: &[&str]) -> LaunchPlan {
        LaunchPlan::new(
            &executable(),
            arguments.iter().map(OsString::from).collect(),
            platform,
            &BTreeMap::new(),
        )
        .unwrap()
    }

    #[test]
    fn gui_arguments_are_forwarded_without_shell_parsing() {
        let plan = plan(Platform::Windows, &["--smoke-test", "a b", "$(untouched)"]);
        assert!(!plan.cli);
        assert_eq!(plan.arguments, ["--smoke-test", "a b", "$(untouched)"]);
        assert_eq!(plan.application, plan.root.join("app/carplay-desktop.exe"));
        let command = plan.command();
        assert_eq!(command.get_current_dir(), Some(plan.root.as_path()));
        assert_eq!(command.get_args().collect::<Vec<_>>(), plan.arguments);
    }

    #[test]
    fn cli_selector_is_removed_but_all_following_arguments_survive() {
        let plan = plan(Platform::Linux, &["--cli", "--", "--cli", "中文路径"]);
        assert!(plan.cli);
        assert_eq!(plan.arguments, ["--", "--cli", "中文路径"]);
        assert_eq!(plan.application, plan.root.join("app/rustcarplay"));
    }

    #[test]
    fn explicit_auth_directory_is_preserved_without_modifying_input() {
        let inherited = BTreeMap::from([(
            OsString::from("RUSTCARPLAY_AUTH_DIR"),
            OsString::from("operator-provided-auth"),
        )]);
        let original = inherited.clone();
        let plan = LaunchPlan::new(&executable(), vec![], Platform::Windows, &inherited).unwrap();
        assert_eq!(
            plan.environment[OsStr::new("RUSTCARPLAY_AUTH_DIR")],
            "operator-provided-auth"
        );
        assert_eq!(inherited, original);
    }

    #[test]
    fn default_auth_and_windows_usb_use_bundle_directories() {
        let plan = plan(Platform::Windows, &[]);
        assert_eq!(
            plan.environment[OsStr::new("RUSTCARPLAY_AUTH_DIR")],
            plan.root.join("resources/auth")
        );
        assert_eq!(
            plan.environment[OsStr::new("RUSTCARPLAY_LIBIMOBILEDEVICE_DIR")],
            plan.root.join("runtime/usb/bin")
        );
        assert_eq!(
            plan.environment[OsStr::new("RUSTCARPLAY_USB_FILTER_DIR")],
            plan.root.join("runtime/usb-filter")
        );
        assert!(!plan.environment.contains_key(OsStr::new("LD_LIBRARY_PATH")));
    }

    #[test]
    fn installed_windows_preserves_explicit_auth_and_usb_overrides() {
        let user = std::env::temp_dir().join("carplay user overrides");
        let inherited = BTreeMap::from([
            (OsString::from("LocalAppData"), user.into_os_string()),
            (
                OsString::from("rustcarplay_auth_dir"),
                OsString::from("custom-auth"),
            ),
            (
                OsString::from("rustcarplay_libimobiledevice_dir"),
                OsString::from("custom-usbmux"),
            ),
            (
                OsString::from("rustcarplay_usb_filter_dir"),
                OsString::from("custom-filter"),
            ),
        ]);
        let original = inherited.clone();
        let plan =
            LaunchPlan::build(&executable(), vec![], Platform::Windows, &inherited, true).unwrap();
        for (key, value) in [
            ("RUSTCARPLAY_AUTH_DIR", "custom-auth"),
            ("RUSTCARPLAY_LIBIMOBILEDEVICE_DIR", "custom-usbmux"),
            ("RUSTCARPLAY_USB_FILTER_DIR", "custom-filter"),
        ] {
            assert_eq!(plan.environment[OsStr::new(key)], value);
        }
        assert_eq!(inherited, original);
        assert_eq!(plan.root, executable().parent().unwrap());
    }

    #[test]
    fn library_search_paths_prepend_private_runtime_and_preserve_existing_paths() {
        for (platform, variable) in [
            (Platform::Linux, "LD_LIBRARY_PATH"),
            (Platform::MacOs, "DYLD_LIBRARY_PATH"),
            (Platform::MacOs, "DYLD_FALLBACK_LIBRARY_PATH"),
        ] {
            let existing = std::env::temp_dir().join("existing libraries");
            let inherited = BTreeMap::from([(variable.into(), existing.clone().into_os_string())]);
            let plan = LaunchPlan::new(&executable(), vec![], platform, &inherited).unwrap();
            let paths =
                std::env::split_paths(&plan.environment[OsStr::new(variable)]).collect::<Vec<_>>();
            assert_eq!(
                paths.first(),
                Some(&plan.root.join("runtime/gstreamer/lib"))
            );
            assert_eq!(paths.last(), Some(&existing));
        }
    }

    #[test]
    fn windows_path_and_auth_environment_names_are_case_insensitive() {
        let existing = std::env::temp_dir().join("existing path");
        let inherited = BTreeMap::from([
            (OsString::from("Path"), existing.clone().into_os_string()),
            (
                OsString::from("rustcarplay_auth_dir"),
                OsString::from("custom-auth"),
            ),
        ]);
        let plan = LaunchPlan::new(&executable(), vec![], Platform::Windows, &inherited).unwrap();
        let paths =
            std::env::split_paths(&plan.environment[OsStr::new("PATH")]).collect::<Vec<_>>();
        assert_eq!(paths.last(), Some(&existing));
        assert_eq!(
            plan.environment[OsStr::new("RUSTCARPLAY_AUTH_DIR")],
            "custom-auth"
        );
    }

    #[test]
    fn linux_audio_configuration_uses_the_bundled_alsa_files() {
        let inherited = BTreeMap::from([
            (
                OsString::from("ALSA_CONFIG_DIR"),
                OsString::from("host-configuration"),
            ),
            (
                OsString::from("ALSA_PLUGIN_DIR"),
                OsString::from("host-plugins"),
            ),
        ]);
        let plan = LaunchPlan::new(&executable(), vec![], Platform::Linux, &inherited).unwrap();
        assert_eq!(
            plan.environment[OsStr::new("ALSA_CONFIG_DIR")],
            plan.root.join("runtime/gstreamer/share/alsa")
        );
        assert_eq!(
            plan.environment[OsStr::new("ALSA_PLUGIN_DIR")],
            plan.root.join("runtime/gstreamer/lib/alsa-lib")
        );
        for platform in [Platform::Windows, Platform::MacOs] {
            let plan = LaunchPlan::new(&executable(), vec![], platform, &BTreeMap::new()).unwrap();
            assert!(!plan.environment.contains_key(OsStr::new("ALSA_CONFIG_DIR")));
            assert!(!plan.environment.contains_key(OsStr::new("ALSA_PLUGIN_DIR")));
        }
    }

    #[test]
    fn scanner_and_registry_are_private_and_architecture_specific() {
        for platform in [Platform::Windows, Platform::Linux, Platform::MacOs] {
            let plan = plan(platform, &[]);
            assert_eq!(
                plan.environment[OsStr::new("GST_PLUGIN_SCANNER")],
                plan.scanner_candidates[0]
            );
            let registry = Path::new(&plan.environment[OsStr::new("GST_REGISTRY_1_0")]);
            assert_eq!(registry.parent(), Some(plan.registry_directory.as_path()));
            assert!(
                registry
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains(std::env::consts::ARCH)
            );
        }
    }

    #[test]
    fn relative_launcher_path_is_rejected() {
        assert!(
            LaunchPlan::new(
                Path::new("RustCarPlay.exe"),
                vec![],
                Platform::Windows,
                &BTreeMap::new()
            )
            .is_err()
        );
    }

    #[test]
    fn incomplete_bundle_reports_the_missing_application() {
        let mut plan = plan(Platform::Windows, &[]);
        assert!(
            plan.validate_bundle()
                .unwrap_err()
                .contains("Application not found")
        );
    }

    #[test]
    fn installed_mode_keeps_resources_in_bundle_and_state_in_user_data() {
        let home = std::env::temp_dir().join("launcher user data 中文");
        let inherited = BTreeMap::from([
            (OsString::from("HOME"), home.clone().into_os_string()),
            (
                OsString::from("LocalAppData"),
                home.clone().into_os_string(),
            ),
            (
                OsString::from("XDG_DATA_HOME"),
                home.join("xdg").into_os_string(),
            ),
        ]);
        for (platform, expected) in [
            (Platform::Windows, home.join("RustCarPlay")),
            (Platform::Linux, home.join("xdg/rustcarplay")),
            (
                Platform::MacOs,
                home.join("Library/Application Support/RustCarPlay"),
            ),
        ] {
            let plan =
                LaunchPlan::build(&executable(), vec![], platform, &inherited, true).unwrap();
            assert_eq!(plan.working_directory, expected);
            assert_eq!(plan.command().get_current_dir(), Some(expected.as_path()));
            assert_eq!(plan.log_directory, expected.join(".local/logs"));
            assert_eq!(plan.registry_directory, expected.join(".local/gstreamer"));
            assert_eq!(
                plan.environment[OsStr::new("RUSTCARPLAY_AUTH_DIR")],
                plan.root.join("resources/auth")
            );
            assert!(plan.plugin_directory.starts_with(&plan.root));
            assert!(plan.application.starts_with(&plan.root));
        }
    }

    #[test]
    fn installed_linux_ignores_relative_xdg_path_and_uses_home() {
        let home = std::env::temp_dir().join("launcher user");
        let inherited = BTreeMap::from([
            (OsString::from("HOME"), home.clone().into_os_string()),
            (
                OsString::from("XDG_DATA_HOME"),
                OsString::from("relative-ignored"),
            ),
        ]);
        let plan =
            LaunchPlan::build(&executable(), vec![], Platform::Linux, &inherited, true).unwrap();
        assert_eq!(
            plan.working_directory,
            home.join(".local/share/rustcarplay")
        );
    }

    #[test]
    fn missing_user_data_path_never_falls_back_to_installation_directory() {
        for platform in [Platform::Windows, Platform::Linux, Platform::MacOs] {
            assert!(
                LaunchPlan::build(&executable(), vec![], platform, &BTreeMap::new(), true).is_err()
            );
            assert_eq!(
                plan(platform, &[]).working_directory,
                executable().parent().unwrap()
            );
        }
    }

    #[test]
    fn installation_marker_is_validated_instead_of_silently_using_portable_mode() {
        assert!(
            validate_installation_marker(
                br#"{"schema":1,"mode":"installed","product":"RustCarPlay","version":"0.1.1"}"#
            )
            .is_ok()
        );
        for invalid in [
            br#"{"schema":true,"mode":"installed","product":"RustCarPlay"}"#.as_slice(),
            br#"{"schema":2,"mode":"installed","product":"RustCarPlay"}"#,
            br#"{"schema":1,"mode":"portable","product":"RustCarPlay"}"#,
            br#"{"schema":1,"mode":"installed","product":"other"}"#,
            b"not json",
        ] {
            assert!(validate_installation_marker(invalid).is_err());
        }
    }

    #[test]
    fn macos_application_reads_marker_and_resources_from_payload() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temporary =
            std::env::temp_dir().join(format!("carplay launcher {} {unique}", std::process::id()));
        std::fs::create_dir(&temporary).unwrap();
        let bundle = temporary.join("Renamed 中文 application.app");
        let contents = bundle.join("Contents");
        let resources = contents.join("Resources");
        let payload = resources.join("payload");
        let launcher = contents.join("MacOS/RustCarPlay");
        std::fs::create_dir_all(&payload).unwrap();
        let marker = payload.join("INSTALLATION.json");
        std::fs::write(
            &marker,
            br#"{"schema":1,"mode":"installed","product":"RustCarPlay"}"#,
        )
        .unwrap();
        let user = temporary.join("user home");
        let inherited = BTreeMap::from([(OsString::from("HOME"), user.clone().into_os_string())]);
        let result = LaunchPlan::new(
            &launcher,
            vec!["--cli".into(), "--version".into()],
            Platform::MacOs,
            &inherited,
        );
        // Remove only the files/directories this test created, without recursively
        // deleting any caller-controlled location.
        std::fs::remove_file(marker).unwrap();
        for directory in [&payload, &resources, &contents, &bundle, &temporary] {
            std::fs::remove_dir(directory).unwrap();
        }
        let plan = result.unwrap();
        assert_eq!(plan.root, payload);
        assert_eq!(plan.application, payload.join("app/rustcarplay"));
        assert_eq!(plan.arguments, ["--version"]);
        assert_eq!(
            plan.environment[OsStr::new("RUSTCARPLAY_AUTH_DIR")],
            payload.join("resources/auth")
        );
        assert_eq!(
            plan.plugin_directory,
            payload.join("runtime/gstreamer/lib/gstreamer-1.0")
        );
        assert_eq!(
            plan.scanner_candidates[0],
            payload.join("runtime/gstreamer/libexec/gstreamer-1.0/gst-plugin-scanner")
        );
        assert_eq!(
            plan.working_directory,
            user.join("Library/Application Support/RustCarPlay")
        );
        assert_eq!(
            plan.registry_directory,
            plan.working_directory.join(".local/gstreamer")
        );
    }

    #[test]
    fn only_standard_macos_app_layout_redirects_the_package_root() {
        let temporary = std::env::temp_dir().join("carplay package location tests");
        for relative in [
            "portable/RustCarPlay",
            "portable/Contents/MacOS/RustCarPlay",
            "Named.app/RustCarPlay",
            "Named.app/Other/MacOS/RustCarPlay",
        ] {
            let launcher = temporary.join(relative);
            assert_eq!(
                package_root(&launcher, Platform::MacOs).unwrap(),
                launcher.parent().unwrap()
            );
        }
        let launcher = temporary.join("Named.app/Contents/MacOS/RustCarPlay");
        for platform in [Platform::Windows, Platform::Linux] {
            assert_eq!(
                package_root(&launcher, platform).unwrap(),
                launcher.parent().unwrap()
            );
        }
        assert!(
            LaunchPlan::new(&launcher, vec![], Platform::MacOs, &BTreeMap::new())
                .unwrap_err()
                .contains("INSTALLATION.json")
        );
    }
}
