//! Read-only current Wi-Fi identity and explicitly requested connection credentials.
//! Identity enumeration never accesses keys. Credentials are obtained only for the
//! selected active profile and remain in zeroizing memory, never diagnostics.
use crate::PlatformError;
use std::{fmt, io, net::IpAddr};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiSecurity {
    Open,
    Wep,
    WpaPersonal,
    Wpa2Personal,
    /// Negotiated SAE; advertise that supported security rather than inferring
    /// an unobserved WPA2 transition capability from an empty password.
    Wpa3Personal,
    Enterprise,
    EnhancedOpen,
    Unknown,
}

#[derive(Clone)]
pub struct CurrentWifi {
    pub ssid: String,
    pub security: WifiSecurity,
    /// Correlates with network::InterfaceAddress for choosing the real Wi-Fi IP.
    pub interface_index: Option<u32>,
}
impl fmt::Debug for CurrentWifi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CurrentWifi")
            .field("ssid", &"[redacted]")
            .field("security", &self.security)
            .field("interface_index", &self.interface_index)
            .finish()
    }
}

/// The current system profile's credentials, used only for iAP2 Wi-Fi sharing.
/// Do not serialize this type or copy its secret into configuration/UI state.
pub struct WifiCredentials {
    pub network: CurrentWifi,
    pub passphrase: Zeroizing<String>,
    /// Actual security: 0 open, 2 WPA/WPA2 Personal, 4 WPA3 SAE.
    pub security_type: u8,
}
impl fmt::Debug for WifiCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WifiCredentials")
            .field("network", &self.network)
            .field("passphrase", &"[redacted]")
            .field("security_type", &self.security_type)
            .finish()
    }
}

fn invalid(message: &'static str) -> PlatformError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

#[cfg(any(target_os = "windows", target_os = "linux", test))]
fn key_permission_error() -> PlatformError {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "系统未允许读取当前 Wi-Fi 的已保存密钥；Windows 可用管理员权限重新打开，Linux 请在系统网络设置中允许当前用户访问此连接",
    )
    .into()
}

#[cfg(any(target_os = "windows", target_os = "linux", test))]
fn iap_security(security: WifiSecurity) -> Result<u8, PlatformError> {
    match security {
        WifiSecurity::Open => Ok(0),
        WifiSecurity::WpaPersonal | WifiSecurity::Wpa2Personal => Ok(2),
        WifiSecurity::Wpa3Personal => Ok(4),
        _ => Err(PlatformError::Unsupported(
            "局域网 CarPlay 需要开放、WPA2-Personal 或 WPA3-Personal Wi-Fi；企业认证、WEP、Enhanced Open 暂不支持",
        )),
    }
}

#[cfg(any(target_os = "windows", target_os = "linux", test))]
fn credentials(
    network: CurrentWifi,
    passphrase: Zeroizing<String>,
) -> Result<WifiCredentials, PlatformError> {
    let security_type = iap_security(network.security)?;
    if (security_type == 0 && !passphrase.is_empty())
        || (security_type != 0 && passphrase.is_empty())
        || passphrase.contains('\0')
    {
        return Err(invalid("当前 Wi-Fi 密钥与实际网络安全类型不匹配"));
    }
    Ok(WifiCredentials {
        network,
        passphrase,
        security_type,
    })
}

/// Read the profile only when the selected address belongs to a connected Wi-Fi
/// interface. No fallback to another adapter, saved network, or open security.
pub fn credentials_for_address(address: IpAddr) -> Result<WifiCredentials, PlatformError> {
    let diagnostic = crate::network::diagnose();
    let mut indices: Vec<_> = diagnostic
        .addresses
        .iter()
        .filter(|entry| entry.is_up && entry.address == address)
        .map(|entry| entry.interface_index)
        .collect();
    indices.sort_unstable();
    indices.dedup();
    let [index] = indices.as_slice() else {
        return Err(invalid("请选择当前 Wi-Fi 的唯一有效本机地址"));
    };
    credentials_on(Some(*index))?.ok_or_else(|| {
        invalid("所选地址不属于已连接的 Wi-Fi；请先在系统设置中让电脑与 iPhone 连接同一 Wi-Fi")
    })
}

/// Selects only when exactly one Wi-Fi adapter is currently connected.
pub fn current_wifi_credentials() -> Result<Option<WifiCredentials>, PlatformError> {
    credentials_on(None)
}

fn credentials_on(index: Option<u32>) -> Result<Option<WifiCredentials>, PlatformError> {
    #[cfg(target_os = "windows")]
    {
        native::query_credentials(index)
    }
    #[cfg(target_os = "linux")]
    {
        linux::query_credentials(index)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = index;
        Err(PlatformError::Unsupported("system Wi-Fi credentials"))
    }
}

/// Returns the first connected Wi-Fi interface in Windows' enumeration order.
/// Multiple-adapter UIs can use current_wifi_on(Some(index)) instead. Access
/// denied remains an error (e.g. Windows location privacy), allowing manual input.
pub fn current_wifi() -> Result<Option<CurrentWifi>, PlatformError> {
    current_wifi_on(None)
}

pub fn current_wifi_on(interface_index: Option<u32>) -> Result<Option<CurrentWifi>, PlatformError> {
    #[cfg(target_os = "windows")]
    {
        native::query(interface_index)
    }
    #[cfg(target_os = "linux")]
    {
        linux::query(interface_index)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = interface_index;
        Err(PlatformError::Unsupported(
            "current Wi-Fi query is currently implemented on Windows and Linux only",
        ))
    }
}

#[cfg(any(target_os = "windows", test))]
fn decode_ssid(bytes: &[u8], length: u32) -> Result<String, PlatformError> {
    let bytes = bytes
        .get(..length as usize)
        .filter(|_| (1..=32).contains(&length))
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid Wi-Fi SSID length")
        })?;
    if bytes.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Wi-Fi SSID contains a zero byte unsupported by the endpoint text field",
        )
        .into());
    }
    // Lossy decoding would advertise the wrong SSID. Preserve bytes exactly or
    // report that this text-input application cannot represent that network.
    std::str::from_utf8(bytes).map(str::to_owned).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "Wi-Fi SSID is not UTF-8").into()
    })
}

#[cfg(any(target_os = "windows", test))]
fn profile_passphrase(
    xml: &str,
    network: &CurrentWifi,
) -> Result<Zeroizing<String>, PlatformError> {
    use quick_xml::{Reader, events::Event};
    let mut reader = Reader::from_str(xml);
    let mut path = Vec::new();
    let mut fields: [Option<Zeroizing<String>>; 5] = std::array::from_fn(|_| None);
    let names = [
        "WLANProfile/SSIDConfig/SSID/name",
        "WLANProfile/SSIDConfig/SSID/hex",
        "WLANProfile/MSM/security/sharedKey/protected",
        "WLANProfile/MSM/security/sharedKey/keyMaterial",
        "WLANProfile/MSM/security/sharedKey/keyType",
    ];
    loop {
        match reader
            .read_event()
            .map_err(|_| invalid("系统 Wi-Fi 配置格式无效"))?
        {
            Event::Start(tag) => {
                path.push(tag.local_name().as_ref().to_owned());
                if path.len() > 32 {
                    return Err(invalid("系统 Wi-Fi 配置嵌套过深"));
                }
                if let Some(index) = names.iter().position(|name| *name == path.join("/")) {
                    if fields[index].is_some() {
                        return Err(invalid("系统 Wi-Fi 配置包含重复字段"));
                    }
                    let raw = reader
                        .read_text(tag.name())
                        .map_err(|_| invalid("系统 Wi-Fi 配置字段无效"))?
                        .into_inner();
                    if raw.contains('<') {
                        return Err(invalid("系统 Wi-Fi 配置字段包含嵌套内容"));
                    }
                    fields[index] = Some(Zeroizing::new(
                        quick_xml::escape::unescape(&raw)
                            .map_err(|_| invalid("系统 Wi-Fi 配置字符转义无效"))?
                            .into_owned(),
                    ));
                    path.pop();
                }
            }
            Event::End(_) => {
                path.pop();
            }
            Event::DocType(_) => return Err(invalid("系统 Wi-Fi 配置不允许文档类型声明")),
            Event::Eof => break,
            _ => {}
        }
    }
    let [ssid, hex, protected, key, key_type] = fields;
    let expected_hex: String = network
        .ssid
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();
    if hex
        .as_ref()
        .is_some_and(|value| !value.eq_ignore_ascii_case(&expected_hex))
        || ssid
            .as_ref()
            .is_some_and(|value| value.as_str() != network.ssid)
        || (hex.is_none() && ssid.is_none())
    {
        return Err(invalid("当前 Wi-Fi 已变化，请刷新网络后重新连接"));
    }
    if network.security == WifiSecurity::Open {
        return Ok(Zeroizing::new(String::new()));
    }
    iap_security(network.security)?;
    // WlanGetProfile can succeed but return encrypted material when access is
    // denied. Never send that blob to the phone as though it were a password.
    if protected.as_ref().map(|value| value.trim()) != Some("false") {
        return Err(key_permission_error());
    }
    if !matches!(
        key_type.as_ref().map(|value| value.as_str()),
        Some("passPhrase" | "networkKey")
    ) {
        return Err(invalid("当前 Wi-Fi 配置没有可共享的个人网络密钥"));
    }
    key.filter(|value| !value.is_empty())
        .ok_or_else(key_permission_error)
}

#[cfg(target_os = "windows")]
mod native {
    use super::*;
    use std::{ffi::c_void, io, mem, ptr};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        NetworkManagement::{
            IpHelper::{ConvertInterfaceGuidToLuid, ConvertInterfaceLuidToIndex},
            Ndis::NET_LUID_LH,
            WiFi::*,
        },
    };
    use zeroize::Zeroize;

    struct Client(HANDLE);
    impl Drop for Client {
        fn drop(&mut self) {
            unsafe {
                WlanCloseHandle(self.0, ptr::null());
            }
        }
    }
    struct Allocation(*mut c_void);
    impl Drop for Allocation {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    WlanFreeMemory(self.0);
                }
            }
        }
    }
    fn status(code: u32) -> io::Result<()> {
        if code == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(code as i32))
        }
    }

    fn security(a: &WLAN_SECURITY_ATTRIBUTES) -> WifiSecurity {
        if a.bOneXEnabled != 0 {
            return WifiSecurity::Enterprise;
        }
        match a.dot11AuthAlgorithm {
            DOT11_AUTH_ALGO_OWE => WifiSecurity::EnhancedOpen,
            DOT11_AUTH_ALGO_80211_OPEN if a.bSecurityEnabled == 0 => WifiSecurity::Open,
            DOT11_AUTH_ALGO_80211_OPEN | DOT11_AUTH_ALGO_80211_SHARED_KEY => WifiSecurity::Wep,
            DOT11_AUTH_ALGO_WPA_PSK => WifiSecurity::WpaPersonal,
            DOT11_AUTH_ALGO_RSNA_PSK => WifiSecurity::Wpa2Personal,
            DOT11_AUTH_ALGO_WPA3_SAE => WifiSecurity::Wpa3Personal,
            DOT11_AUTH_ALGO_WPA
            | DOT11_AUTH_ALGO_RSNA
            | DOT11_AUTH_ALGO_WPA3_ENT_192
            | DOT11_AUTH_ALGO_WPA3_ENT => WifiSecurity::Enterprise,
            _ => WifiSecurity::Unknown,
        }
    }

    struct Selected {
        network: CurrentWifi,
        interface: windows_sys::core::GUID,
        profile: Vec<u16>,
    }

    fn select(
        client: &Client,
        wanted_index: Option<u32>,
        allow_first: bool,
    ) -> Result<Option<Selected>, PlatformError> {
        // SAFETY: handles/output buffers have the API-prescribed types. Both
        // WLAN allocations and the client handle have matching RAII release.
        unsafe {
            let mut list = ptr::null_mut();
            status(WlanEnumInterfaces(client.0, ptr::null(), &mut list))?;
            if list.is_null() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WLAN returned no interface list",
                )
                .into());
            }
            let _list_allocation = Allocation(list.cast());
            let count = (*list).dwNumberOfItems as usize;
            if count > 1024 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WLAN interface count exceeds limit",
                )
                .into());
            }
            let interfaces = ptr::addr_of!((*list).InterfaceInfo).cast::<WLAN_INTERFACE_INFO>();
            let mut selected = None;
            for offset in 0..count {
                let interface = &*interfaces.add(offset);
                if interface.isState != wlan_interface_state_connected {
                    continue;
                }
                let mut luid: NET_LUID_LH = mem::zeroed();
                let mut index = 0;
                let interface_index =
                    if ConvertInterfaceGuidToLuid(&interface.InterfaceGuid, &mut luid) == 0
                        && ConvertInterfaceLuidToIndex(&luid, &mut index) == 0
                    {
                        Some(index)
                    } else {
                        None
                    };
                if wanted_index.is_some() && wanted_index != interface_index {
                    continue;
                }
                let mut size = 0;
                let mut data = ptr::null_mut();
                status(WlanQueryInterface(
                    client.0,
                    &interface.InterfaceGuid,
                    wlan_intf_opcode_current_connection,
                    ptr::null(),
                    &mut size,
                    &mut data,
                    ptr::null_mut(),
                ))?;
                let _connection_allocation = Allocation(data);
                if data.is_null() || (size as usize) < mem::size_of::<WLAN_CONNECTION_ATTRIBUTES>()
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "WLAN connection result is truncated",
                    )
                    .into());
                }
                let attributes = &*data.cast::<WLAN_CONNECTION_ATTRIBUTES>();
                if attributes.isState != wlan_interface_state_connected {
                    continue;
                }
                let ssid = &attributes.wlanAssociationAttributes.dot11Ssid;
                let profile_length = attributes
                    .strProfileName
                    .iter()
                    .position(|value| *value == 0)
                    .ok_or_else(|| invalid("系统 Wi-Fi 配置名称未终止"))?;
                let candidate = Selected {
                    network: CurrentWifi {
                        ssid: decode_ssid(&ssid.ucSSID, ssid.uSSIDLength)?,
                        security: security(&attributes.wlanSecurityAttributes),
                        interface_index,
                    },
                    interface: interface.InterfaceGuid,
                    profile: attributes.strProfileName[..=profile_length].to_vec(),
                };
                if allow_first {
                    return Ok(Some(candidate));
                }
                if selected.is_some() {
                    return Err(invalid("有多个 Wi-Fi 连接，请选择明确的本机 Wi-Fi 地址"));
                }
                selected = Some(candidate);
            }
            Ok(selected)
        }
    }

    fn open() -> Result<Client, PlatformError> {
        let mut version = 0;
        let mut handle = ptr::null_mut();
        unsafe {
            status(WlanOpenHandle(2, ptr::null(), &mut version, &mut handle))?;
        }
        Ok(Client(handle))
    }

    pub(super) fn query(wanted_index: Option<u32>) -> Result<Option<CurrentWifi>, PlatformError> {
        Ok(select(&open()?, wanted_index, true)?.map(|selected| selected.network))
    }

    struct SecretAllocation {
        pointer: *mut u16,
        length: usize,
    }
    impl Drop for SecretAllocation {
        fn drop(&mut self) {
            if !self.pointer.is_null() {
                // The prefix length was counted inside this terminated allocation.
                unsafe {
                    std::slice::from_raw_parts_mut(self.pointer, self.length).zeroize();
                    WlanFreeMemory(self.pointer.cast());
                }
            }
        }
    }

    pub(super) fn query_credentials(
        wanted_index: Option<u32>,
    ) -> Result<Option<WifiCredentials>, PlatformError> {
        let client = open()?;
        let Some(selected) = select(&client, wanted_index, false)? else {
            return Ok(None);
        };
        let security_type = iap_security(selected.network.security)?;
        if security_type == 0 {
            return credentials(selected.network, Zeroizing::new(String::new())).map(Some);
        }
        let mut allocation = SecretAllocation {
            pointer: ptr::null_mut(),
            length: 0,
        };
        let mut flags = WLAN_PROFILE_GET_PLAINTEXT_KEY;
        // Only the selected adapter's currently connected profile is requested.
        let result = unsafe {
            WlanGetProfile(
                client.0,
                &selected.interface,
                selected.profile.as_ptr(),
                ptr::null(),
                &mut allocation.pointer,
                &mut flags,
                ptr::null_mut(),
            )
        };
        if result == 5 {
            return Err(key_permission_error());
        }
        status(result)?;
        if allocation.pointer.is_null() {
            return Err(invalid("系统未返回 Wi-Fi 配置"));
        }
        // API guarantees a terminated UTF-16 allocation. Cap parsing/memory use.
        while allocation.length < 256 * 1024 {
            if unsafe { *allocation.pointer.add(allocation.length) } == 0 {
                break;
            }
            allocation.length += 1;
        }
        if allocation.length == 256 * 1024 {
            return Err(invalid("系统 Wi-Fi 配置过大"));
        }
        let xml = Zeroizing::new(
            String::from_utf16(unsafe {
                std::slice::from_raw_parts(allocation.pointer, allocation.length)
            })
            .map_err(|_| invalid("系统 Wi-Fi 配置不是有效 UTF-16"))?,
        );
        let passphrase = profile_passphrase(&xml, &selected.network)?;
        let current = select(&client, wanted_index, false)?
            .ok_or_else(|| invalid("读取配置时 Wi-Fi 已断开，请重新连接"))?;
        if current.network.ssid != selected.network.ssid
            || current.network.security != selected.network.security
            || current.network.interface_index != selected.network.interface_index
            || current.profile != selected.profile
        {
            return Err(invalid("读取配置时 Wi-Fi 已变化，请重新连接"));
        }
        credentials(selected.network, passphrase).map(Some)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn distinguishes_sae_enterprise_and_enhanced_open() {
            for (algorithm, expected) in [
                (DOT11_AUTH_ALGO_RSNA_PSK, WifiSecurity::Wpa2Personal),
                (DOT11_AUTH_ALGO_WPA3_SAE, WifiSecurity::Wpa3Personal),
                (DOT11_AUTH_ALGO_WPA3_ENT_192, WifiSecurity::Enterprise),
                (DOT11_AUTH_ALGO_OWE, WifiSecurity::EnhancedOpen),
            ] {
                let attributes = WLAN_SECURITY_ATTRIBUTES {
                    bSecurityEnabled: 1,
                    dot11AuthAlgorithm: algorithm,
                    ..Default::default()
                };
                assert_eq!(security(&attributes), expected);
            }
            assert_eq!(
                security(&WLAN_SECURITY_ATTRIBUTES {
                    dot11AuthAlgorithm: DOT11_AUTH_ALGO_80211_OPEN,
                    ..Default::default()
                }),
                WifiSecurity::Open
            );
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{
        ffi::{CStr, CString},
        io::Read,
        process::{Command, Stdio},
        sync::mpsc,
        time::{Duration, Instant},
    };

    // All nmcli invocations use argument arrays and an exact active connection
    // UUID. Secret output is captured into zeroizing memory, never a terminal.
    fn nmcli(args: &[&str]) -> Result<Zeroizing<String>, PlatformError> {
        let mut child = Command::new("nmcli")
            .args(["--wait", "5", "--terse", "--escape", "no"])
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| {
                PlatformError::Unsupported("局域网模式需要 NetworkManager 的 nmcli 命令")
            })?;
        let stdout = child.stdout.take().expect("piped stdout");
        let (send, receive) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let mut output = Zeroizing::new(Vec::new());
            let result = stdout.take(65_537).read_to_end(&mut output);
            let _ = send.send((result, output));
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = worker.join();
                    return Err(
                        io::Error::new(io::ErrorKind::TimedOut, "系统网络配置查询超时").into(),
                    );
                }
            }
        };
        let _ = worker.join();
        let (read, output) = receive
            .recv()
            .map_err(|_| invalid("系统网络配置查询失败"))?;
        read.map_err(|_| invalid("系统网络配置读取失败"))?;
        if !status.success() {
            return Err(key_permission_error());
        }
        if output.len() > 65_536 {
            return Err(invalid("系统网络配置输出过大"));
        }
        let text =
            std::str::from_utf8(&output).map_err(|_| invalid("系统网络配置不是有效 UTF-8"))?;
        Ok(Zeroizing::new(
            text.strip_suffix('\n').unwrap_or(text).to_owned(),
        ))
    }

    fn interface(index: u32) -> Result<String, PlatformError> {
        let mut buffer = [0 as libc::c_char; libc::IF_NAMESIZE];
        if unsafe { libc::if_indextoname(index, buffer.as_mut_ptr()) }.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_str()
            .map(str::to_owned)
            .map_err(|_| invalid("网络接口名称不是有效 UTF-8"))
    }

    fn select(index: Option<u32>) -> Result<Option<(String, u32)>, PlatformError> {
        if let Some(index) = index {
            let name = interface(index)?;
            if nmcli(&["--get-values", "GENERAL.TYPE", "device", "show", &name])?.as_str() != "wifi"
            {
                return Ok(None);
            }
            return Ok(Some((name, index)));
        }
        let devices = nmcli(&["--fields", "DEVICE,TYPE,STATE", "device", "status"])?;
        let mut selected = None;
        for line in devices.lines() {
            let values: Vec<_> = line.splitn(3, ':').collect();
            if values.len() != 3 || values[1] != "wifi" || !values[2].starts_with("connected") {
                continue;
            }
            let name = CString::new(values[0]).map_err(|_| invalid("网络接口名称无效"))?;
            let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
            if index == 0 {
                continue;
            }
            if selected.is_some() {
                return Err(invalid("有多个 Wi-Fi 连接，请选择明确的本机 Wi-Fi 地址"));
            }
            selected = Some((values[0].to_owned(), index));
        }
        Ok(selected)
    }

    fn snapshot(
        name: &str,
        index: u32,
    ) -> Result<Option<(CurrentWifi, Zeroizing<String>)>, PlatformError> {
        let uuid = nmcli(&["--get-values", "GENERAL.CON-UUID", "device", "show", name])?;
        if uuid.is_empty() || uuid.as_str() == "--" {
            return Ok(None);
        }
        if uuid::Uuid::parse_str(&uuid).is_err() {
            return Err(invalid("当前系统网络标识无效"));
        }
        let mode = nmcli(&[
            "--get-values",
            "802-11-wireless.mode",
            "connection",
            "show",
            "uuid",
            &uuid,
        ])?;
        if !matches!(mode.as_str(), "infrastructure" | "" | "--") {
            return Err(PlatformError::Unsupported(
                "局域网模式需要已连接路由器的 Wi-Fi；请为电脑创建的接入点选择热点模式",
            ));
        }
        let ssid = nmcli(&[
            "--get-values",
            "802-11-wireless.ssid",
            "connection",
            "show",
            "uuid",
            &uuid,
        ])?;
        if ssid.is_empty() || ssid.len() > 32 || ssid.contains(['\0', '\r', '\n']) {
            return Err(invalid("当前 Wi-Fi 名称无法用于 CarPlay"));
        }
        let key_mgmt = nmcli(&[
            "--get-values",
            "802-11-wireless-security.key-mgmt",
            "connection",
            "show",
            "uuid",
            &uuid,
        ])?;
        let security = match key_mgmt.as_str() {
            "" | "--" => WifiSecurity::Open,
            "wpa-psk" => WifiSecurity::Wpa2Personal,
            "sae" => WifiSecurity::Wpa3Personal,
            "none" => WifiSecurity::Wep,
            "wpa-eap" | "wpa-eap-suite-b-192" | "ieee8021x" => WifiSecurity::Enterprise,
            "owe" => WifiSecurity::EnhancedOpen,
            _ => WifiSecurity::Unknown,
        };
        Ok(Some((
            CurrentWifi {
                ssid: ssid.to_string(),
                security,
                interface_index: Some(index),
            },
            uuid,
        )))
    }

    pub(super) fn query(index: Option<u32>) -> Result<Option<CurrentWifi>, PlatformError> {
        let Some((name, index)) = select(index)? else {
            return Ok(None);
        };
        Ok(snapshot(&name, index)?.map(|(network, _)| network))
    }

    pub(super) fn query_credentials(
        index: Option<u32>,
    ) -> Result<Option<WifiCredentials>, PlatformError> {
        let Some((name, index)) = select(index)? else {
            return Ok(None);
        };
        let Some((network, uuid)) = snapshot(&name, index)? else {
            return Ok(None);
        };
        let secret = if iap_security(network.security)? == 0 {
            Zeroizing::new(String::new())
        } else {
            let secret = nmcli(&[
                "--show-secrets",
                "--get-values",
                "802-11-wireless-security.psk",
                "connection",
                "show",
                "uuid",
                &uuid,
            ])?;
            if secret.is_empty() || secret.as_str() == "--" {
                return Err(key_permission_error());
            }
            secret
        };
        let Some((current, current_uuid)) = snapshot(&name, index)? else {
            return Err(invalid("读取配置时 Wi-Fi 已断开，请重新连接"));
        };
        if current_uuid != uuid
            || current.ssid != network.ssid
            || current.security != network.security
        {
            return Err(invalid("读取配置时 Wi-Fi 已变化，请重新连接"));
        }
        credentials(network, secret).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn network() -> CurrentWifi {
        CurrentWifi {
            ssid: "Home & LAN".into(),
            security: WifiSecurity::Wpa2Personal,
            interface_index: Some(7),
        }
    }
    fn profile(protected: &str, key: &str) -> String {
        format!(
            "<WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\"><SSIDConfig><SSID><name>Home &amp; LAN</name><hex>486F6D652026204C414E</hex></SSID></SSIDConfig><MSM><security><sharedKey><keyType>passPhrase</keyType><protected>{protected}</protected><keyMaterial>{key}</keyMaterial></sharedKey></security></MSM></WLANProfile>"
        )
    }
    #[test]
    fn current_profile_key_preserves_xml_escapes_and_whitespace_without_debug_leak() {
        let secret = profile_passphrase(
            &profile("false", "  secret&amp;&lt;&gt;&#x4E2D;  "),
            &network(),
        )
        .unwrap();
        assert_eq!(secret.as_str(), "  secret&<>中  ");
        let value = credentials(network(), secret).unwrap();
        assert_eq!(value.security_type, 2);
        let debug = format!("{value:?}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("Home & LAN"));
    }
    #[test]
    fn encrypted_or_different_profile_never_becomes_a_wifi_password() {
        assert!(profile_passphrase(&profile("true", "010203040506"), &network()).is_err());
        assert!(
            profile_passphrase(
                &profile("false", "password").replace("Home &amp; LAN", "Other"),
                &network()
            )
            .is_err()
        );
        assert!(
            profile_passphrase(
                &profile("false", "password").replace("486F6D65", "01234567"),
                &network()
            )
            .is_err()
        );
        assert!(
            profile_passphrase(&profile("false", "<nested>password</nested>"), &network()).is_err()
        );
        assert!(
            profile_passphrase(
                &profile("false", "password").replace(
                    "</sharedKey>",
                    "<keyMaterial>other</keyMaterial></sharedKey>"
                ),
                &network()
            )
            .is_err()
        );
    }
    #[test]
    fn actual_security_controls_open_and_sae_credentials() {
        let mut network = network();
        assert!(credentials(network.clone(), Zeroizing::new(String::new())).is_err());
        network.security = WifiSecurity::Open;
        assert_eq!(
            credentials(network.clone(), Zeroizing::new(String::new()))
                .unwrap()
                .security_type,
            0
        );
        assert!(credentials(network.clone(), Zeroizing::new("password".into())).is_err());
        network.security = WifiSecurity::Wpa3Personal;
        assert_eq!(
            credentials(network.clone(), Zeroizing::new("password".into()))
                .unwrap()
                .security_type,
            4
        );
        network.security = WifiSecurity::Enterprise;
        assert!(credentials(network, Zeroizing::new("password".into())).is_err());
    }
    #[test]
    fn ssid_keeps_length_utf8_and_debug_privacy() {
        assert_eq!(decode_ssid("中文WiFi".as_bytes(), 10).unwrap(), "中文WiFi");
        assert_eq!(decode_ssid(b"namepadding", 4).unwrap(), "name");
        assert!(decode_ssid(&[255], 1).is_err());
        assert!(decode_ssid(b"a\0b", 3).is_err());
        assert!(decode_ssid(&[0; 32], 33).is_err());
        assert!(decode_ssid(&[], 0).is_err());
        let wifi = CurrentWifi {
            ssid: "PrivateSSID".into(),
            security: WifiSecurity::Wpa2Personal,
            interface_index: Some(23),
        };
        assert!(!format!("{wifi:?}").contains("PrivateSSID"));
    }
}
