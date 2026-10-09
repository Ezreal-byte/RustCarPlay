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
        if !launcher_executable.is_absolute() {
            return Err("The launcher executable path must be absolute.".into());
        }
        let root = launcher_executable
            .parent()
            .ok_or("Cannot locate the portable application directory.")?
            .to_path_buf();
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
        let registry_directory = root.join(".local/gstreamer");
        let scanner_name = format!("gst-plugin-scanner{}", platform.suffix());
        let scanner_candidates = [
            runtime.join("libexec/gstreamer-1.0").join(&scanner_name),
            runtime.join("bin").join(&scanner_name),
        ];
        let mut environment = BTreeMap::new();
        prepend_paths(&mut environment, inherited, "PATH", &[runtime.join("bin")])?;
        match platform {
            Platform::Windows => {
                environment.insert(
                    "RUSTCARPLAY_LIBIMOBILEDEVICE_DIR".into(),
                    root.join("runtime/usb/bin").into_os_string(),
                );
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
            log_directory: root.join(".local/logs"),
            root,
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
            .current_dir(&self.root)
            .envs(&self.environment);
        command
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
        assert!(!plan.environment.contains_key(OsStr::new("LD_LIBRARY_PATH")));
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
}
