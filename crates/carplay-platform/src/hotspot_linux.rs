//! NetworkManager backend. No password is placed in argv or saved in a profile.
use super::*;
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Write},
    net::IpAddr,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// stdout may contain a password: keep it bounded, zeroizing and out of errors.
fn nmcli(
    args: &[&str],
    input: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Zeroizing<String>, HotspotError> {
    check_cancel(cancel)?;
    let mut child = Command::new("nmcli")
        .args(["--wait", "20", "--terse", "--escape", "no"])
        .args(args)
        .env("LC_ALL", "C")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| HotspotError::Process(e.kind()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or(HotspotError::Operation("无法读取 NetworkManager 响应"))?;
    let reader = thread::spawn(move || {
        let mut bytes = Zeroizing::new(Vec::new());
        stdout.take(65_537).read_to_end(&mut bytes).map(|_| bytes)
    });
    if let Some(input) = input {
        let write_result = child
            .stdin
            .take()
            .ok_or(HotspotError::Operation(
                "无法向 NetworkManager 提供临时凭据",
            ))
            .and_then(|mut stdin| {
                stdin
                    .write_all(input.as_bytes())
                    .map_err(|e| HotspotError::Process(e.kind()))
            });
        if let Err(error) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(error);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(25);
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err(HotspotError::Operation(
                        "NetworkManager 拒绝热点操作；请检查 Wi-Fi 网卡、权限与系统网络设置",
                    ))
                };
            }
            Ok(None) => {}
            Err(error) => break Err(HotspotError::Process(error.kind())),
        }
        if cancel.load(Ordering::Acquire) {
            break Err(HotspotError::Cancelled);
        }
        if Instant::now() >= deadline {
            break Err(HotspotError::Timeout);
        }
        thread::sleep(Duration::from_millis(50));
    };
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let bytes = reader
        .join()
        .map_err(|_| HotspotError::Operation("NetworkManager 输出线程失败"))?
        .map_err(|e| HotspotError::Process(e.kind()))?;
    result?;
    if bytes.len() > 65_536 {
        return Err(HotspotError::Operation("NetworkManager 响应超过限制"));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| HotspotError::Operation("NetworkManager 返回非 UTF-8 配置"))?;
    Ok(Zeroizing::new(
        text.trim_end_matches(['\r', '\n']).to_owned(),
    ))
}

fn wifi_interfaces(cancel: &AtomicBool) -> Result<Vec<String>, HotspotError> {
    Ok(nmcli(
        &["--fields", "DEVICE,TYPE", "device", "status"],
        None,
        cancel,
    )?
    .lines()
    .filter_map(|line| {
        line.rsplit_once(':')
            .filter(|(_, kind)| *kind == "wifi")
            .map(|(name, _)| name.to_owned())
    })
    .collect())
}
fn property(
    uuid: &str,
    name: &str,
    secret: bool,
    cancel: &AtomicBool,
) -> Result<Zeroizing<String>, HotspotError> {
    let mut args = vec!["--get-values", name, "connection", "show", "uuid", uuid];
    if secret {
        args.insert(0, "--show-secrets");
    }
    nmcli(&args, None, cancel)
}
fn interface_index(name: &str) -> Result<u32, HotspotError> {
    let name = CString::new(name).map_err(|_| HotspotError::EndpointUnavailable)?;
    // SAFETY: the interface name is NUL terminated and lives across this call.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    if index == 0 {
        Err(HotspotError::EndpointUnavailable)
    } else {
        Ok(index)
    }
}
fn address(interface: &str) -> Result<(u32, Ipv4Addr), HotspotError> {
    let index = interface_index(interface)?;
    let candidates: Vec<_> = crate::network::diagnose()
        .addresses
        .into_iter()
        .filter(|item| item.interface_index == index && item.is_up)
        .filter_map(|item| match item.address {
            IpAddr::V4(address) if address.is_private() => Some(address),
            _ => None,
        })
        .collect();
    let [address] = candidates.as_slice() else {
        return Err(HotspotError::EndpointUnavailable);
    };
    Ok((index, *address))
}

fn running(cancel: &AtomicBool) -> Result<Option<(HotspotEndpoint, String)>, HotspotError> {
    let mut result = None;
    for interface in wifi_interfaces(cancel)? {
        let uuid = nmcli(
            &[
                "--get-values",
                "GENERAL.CON-UUID",
                "device",
                "show",
                &interface,
            ],
            None,
            cancel,
        )?;
        if uuid::Uuid::parse_str(&uuid).is_err() {
            continue;
        }
        if property(&uuid, "802-11-wireless.mode", false, cancel)?.as_str() != "ap" {
            continue;
        }
        if result.is_some() {
            return Err(HotspotError::EndpointUnavailable);
        }
        let security = property(&uuid, "802-11-wireless-security.key-mgmt", false, cancel)?;
        let security_type = match security.as_str() {
            "wpa-psk" => 2,
            "sae" => 4,
            _ => {
                return Err(HotspotError::Unavailable(
                    "系统热点必须使用 WPA2 或 WPA3 个人认证",
                ));
            }
        };
        let values = HotspotConfig {
            ssid: property(&uuid, "802-11-wireless.ssid", false, cancel)?.to_string(),
            passphrase: property(&uuid, "802-11-wireless-security.psk", true, cancel)?,
        };
        values.validate().map_err(|_| {
            HotspotError::Unavailable("无法读取系统热点凭据，请在系统网络设置中配置热点")
        })?;
        let (interface_index, address) = address(&interface)?;
        result = Some((
            HotspotEndpoint {
                ssid: values.ssid,
                passphrase: values.passphrase,
                address,
                interface_index,
                security_type,
            },
            uuid.to_string(),
        ));
    }
    Ok(result)
}
pub(super) fn current() -> Result<Option<HotspotEndpoint>, HotspotError> {
    running(&AtomicBool::new(false)).map(|value| value.map(|(endpoint, _)| endpoint))
}
fn capable_interface(cancel: &AtomicBool) -> Result<String, HotspotError> {
    let mut busy = false;
    for interface in wifi_interfaces(cancel)? {
        let supported = nmcli(
            &[
                "--get-values",
                "WIFI-PROPERTIES.AP",
                "device",
                "show",
                &interface,
            ],
            None,
            cancel,
        )?;
        if supported.as_str() == "yes" {
            let active = nmcli(
                &[
                    "--get-values",
                    "GENERAL.CON-UUID",
                    "device",
                    "show",
                    &interface,
                ],
                None,
                cancel,
            )?;
            if uuid::Uuid::parse_str(&active).is_ok() {
                busy = true;
                continue;
            }
            return Ok(interface);
        }
    }
    if busy {
        return Err(HotspotError::Unavailable(
            "Wi-Fi 网卡正在使用；请使用另一块网卡，或先在系统设置中切换为热点",
        ));
    }
    Err(HotspotError::Unavailable(
        "NetworkManager 未找到支持 AP 模式的 Wi-Fi 网卡",
    ))
}
pub(super) fn capability() -> Result<(), HotspotError> {
    capable_interface(&AtomicBool::new(false)).map(|_| ())
}

pub(super) struct Session {
    /// Only set for the fresh UUID generated by this process; never a user's AP.
    owned_uuid: Option<String>,
}
impl Session {
    pub(super) fn started_by_us(&self) -> bool {
        self.owned_uuid.is_some()
    }
    pub(super) fn stop(&mut self) -> Result<(), HotspotError> {
        if let Some(uuid) = &self.owned_uuid {
            nmcli(
                &["connection", "delete", "uuid", uuid],
                None,
                &AtomicBool::new(false),
            )?;
            self.owned_uuid = None;
        }
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn random_values() -> Result<(String, HotspotConfig), HotspotError> {
    let mut random = Zeroizing::new([0u8; 36]);
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut *random))
        .map_err(|e| HotspotError::Process(e.kind()))?;
    let mut id: [u8; 16] = random[..16].try_into().expect("fixed size");
    id[6] = (id[6] & 0x0f) | 0x40;
    id[8] = (id[8] & 0x3f) | 0x80;
    let uuid = uuid::Uuid::from_bytes(id).to_string();
    let mut password = Zeroizing::new(String::with_capacity(40));
    for byte in &random[16..] {
        use std::fmt::Write;
        write!(&mut *password, "{byte:02x}").expect("String formatting");
    }
    Ok((
        uuid,
        HotspotConfig {
            ssid: "RustCarPlay".into(),
            passphrase: password,
        },
    ))
}

pub(super) fn start(
    requested: Option<&HotspotConfig>,
    cancel: &AtomicBool,
) -> Result<PreparedSession, HotspotError> {
    if let Some((endpoint, _)) = running(cancel)? {
        if requested.is_some_and(|r| r.ssid != endpoint.ssid || r.passphrase != endpoint.passphrase)
        {
            return Err(HotspotError::AlreadyRunning);
        }
        return Ok(PreparedSession {
            endpoint,
            native: Session { owned_uuid: None },
        });
    }
    let interface = capable_interface(cancel)?;
    // Avoid silently disconnecting the uplink on single-radio laptops. A user
    // may explicitly switch that interface to a hotspot in NetworkManager first.
    let active = nmcli(
        &[
            "--get-values",
            "GENERAL.CON-UUID",
            "device",
            "show",
            &interface,
        ],
        None,
        cancel,
    )?;
    if uuid::Uuid::parse_str(&active).is_ok() {
        return Err(HotspotError::Unavailable(
            "Wi-Fi 网卡正在使用；请使用另一块网卡，或先在系统设置中切换为热点",
        ));
    }
    let (uuid, defaults) = random_values()?;
    let config = requested.unwrap_or(&defaults);
    config.validate()?;
    let native = Session {
        owned_uuid: Some(uuid.clone()),
    };
    let result = (|| {
        // NOT_SAVED (2) keeps the PSK out of NetworkManager's persistent profile.
        nmcli(
            &[
                "connection",
                "add",
                "type",
                "wifi",
                "ifname",
                &interface,
                "con-name",
                &format!("RustCarPlay-{}", &uuid[..8]),
                "connection.uuid",
                &uuid,
                "connection.autoconnect",
                "no",
                "802-11-wireless.ssid",
                &config.ssid,
                "802-11-wireless.mode",
                "ap",
                "802-11-wireless-security.key-mgmt",
                "wpa-psk",
                "802-11-wireless-security.psk-flags",
                "2",
                "ipv4.method",
                "shared",
                "ipv6.method",
                "disabled",
            ],
            None,
            cancel,
        )?;
        let secret = Zeroizing::new(format!(
            "802-11-wireless-security.psk:{}\n",
            config.passphrase.as_str()
        ));
        nmcli(
            &[
                "connection",
                "up",
                "uuid",
                &uuid,
                "passwd-file",
                "/dev/stdin",
            ],
            Some(&secret),
            cancel,
        )?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            check_cancel(cancel)?;
            match address(&interface) {
                Ok((interface_index, address)) => {
                    return Ok(HotspotEndpoint {
                        ssid: config.ssid.clone(),
                        passphrase: config.passphrase.clone(),
                        address,
                        interface_index,
                        security_type: 2,
                    });
                }
                Err(HotspotError::EndpointUnavailable) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(100))
                }
                Err(error) => return Err(error),
            }
        }
    })();
    finish_start(result, native)
}
