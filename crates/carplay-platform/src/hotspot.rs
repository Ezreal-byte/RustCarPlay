//! Computer-hosted Wi-Fi for wireless CarPlay. Call blocking methods on a worker.
//!
//! Existing system hotspots remain system-owned. Only sessions started by this
//! module are stopped, and credentials are never serialized or included in Debug.
use std::{
    fmt,
    net::Ipv4Addr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct HotspotConfig {
    pub ssid: String,
    pub passphrase: Zeroizing<String>,
}
impl fmt::Debug for HotspotConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HotspotConfig")
            .field("credentials", &"[redacted]")
            .finish()
    }
}
impl HotspotConfig {
    pub fn validate(&self) -> Result<(), HotspotError> {
        if self.ssid.trim().is_empty()
            || self.ssid.len() > 32
            || self.ssid.chars().any(char::is_control)
            || !(8..=63).contains(&self.passphrase.len())
            || !self.passphrase.is_ascii()
            || self.passphrase.chars().any(char::is_control)
        {
            return Err(HotspotError::InvalidConfiguration);
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct HotspotEndpoint {
    pub ssid: String,
    pub passphrase: Zeroizing<String>,
    pub address: Ipv4Addr,
    pub interface_index: u32,
    /// iAP2: 2 WPA2, 3 WPA2/WPA3 transition, 4 WPA3 only.
    pub security_type: u8,
}
impl fmt::Debug for HotspotEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HotspotEndpoint")
            .field("credentials", &"[redacted]")
            .field("interface_index", &self.interface_index)
            .field("security_type", &self.security_type)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HotspotError {
    #[error("热点名称须为 1–32 字节，WPA2 密码须为 8–63 个 ASCII 字符")]
    InvalidConfiguration,
    #[error("已取消启动热点")]
    Cancelled,
    #[error("等待系统热点操作超时；请检查系统移动热点设置")]
    Timeout,
    #[error("系统不支持热点控制：{0}")]
    Unsupported(&'static str),
    #[error("Windows 未找到可共享的网络连接；请先连接 Wi-Fi 或以太网，再检查系统移动热点设置")]
    NoUpstream,
    #[error("系统禁止热点控制：{0}。请在系统移动热点设置中检查权限、适配器和共享网络")]
    Unavailable(&'static str),
    #[error("系统热点正被其他操作更改，请稍后重试")]
    Busy,
    #[error("已有不同配置的热点正在运行；请使用系统热点配置，或先在系统设置中关闭它")]
    AlreadyRunning,
    #[error("尚未找到唯一的热点 IPv4 接口；请检查系统热点已启用，且没有多个同时运行的热点")]
    EndpointUnavailable,
    #[error("Windows 热点操作失败（HRESULT 0x{0:08X}）；请检查系统移动热点设置")]
    Windows(u32),
    #[error("热点操作失败：{0}")]
    Operation(&'static str),
    #[error("NetworkManager 命令不可用或无法执行（{0:?}）")]
    Process(std::io::ErrorKind),
    #[error("{operation}；清理热点失败：{cleanup}。请在系统设置中检查热点是否仍开启")]
    Cleanup {
        operation: Box<HotspotError>,
        cleanup: Box<HotspotError>,
    },
}

/// Read-only capability probe. It neither requests privileges nor changes Wi-Fi.
pub fn capability() -> Result<(), HotspotError> {
    native::capability()
}

/// Read an already-running computer hotspot. Does not connect to an external AP.
pub fn current() -> Result<Option<HotspotEndpoint>, HotspotError> {
    native::current()
}

pub struct HotspotSession {
    endpoint: HotspotEndpoint,
    owned: bool,
    control: mpsc::Sender<mpsc::SyncSender<Result<(), HotspotError>>>,
    worker: Option<JoinHandle<()>>,
}

// Never transferred between threads. In particular, the Windows manager and
// its non-agile COM interfaces live entirely on the hotspot worker's apartment.
struct PreparedSession<S = native::Session> {
    endpoint: HotspotEndpoint,
    native: S,
}
trait SessionControl {
    fn started_by_us(&self) -> bool;
    fn stop(&mut self) -> Result<(), HotspotError>;
}
impl SessionControl for native::Session {
    fn started_by_us(&self) -> bool {
        self.started_by_us()
    }
    fn stop(&mut self) -> Result<(), HotspotError> {
        self.stop()
    }
}
#[cfg(any(target_os = "windows", target_os = "linux", test))]
fn finish_start<S: SessionControl>(
    result: Result<HotspotEndpoint, HotspotError>,
    mut native: S,
) -> Result<PreparedSession<S>, HotspotError> {
    match result {
        Ok(endpoint) => Ok(PreparedSession { endpoint, native }),
        Err(operation) => match native.stop() {
            Ok(()) => Err(operation),
            Err(cleanup) => Err(HotspotError::Cleanup {
                operation: Box::new(operation),
                cleanup: Box::new(cleanup),
            }),
        },
    }
}
impl fmt::Debug for HotspotSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HotspotSession")
            .field("endpoint", &self.endpoint)
            .field("started_by_us", &self.started_by_us())
            .finish()
    }
}
impl HotspotSession {
    /// Windows uses the existing system hotspot configuration. Linux reuses a
    /// running WPA2 hotspot or creates RustCarPlay with a fresh random password.
    pub fn start_default(cancel: &AtomicBool) -> Result<Self, HotspotError> {
        check_cancel(cancel)?;
        Self::start_worker(None, cancel)
    }
    /// A running hotspot with another configuration is never replaced.
    pub fn start(config: &HotspotConfig, cancel: &AtomicBool) -> Result<Self, HotspotError> {
        config.validate()?;
        check_cancel(cancel)?;
        Self::start_worker(Some(config.clone()), cancel)
    }
    pub fn endpoint(&self) -> &HotspotEndpoint {
        &self.endpoint
    }
    pub fn started_by_us(&self) -> bool {
        self.owned
    }
    /// Blocking cleanup. Call on the connection worker, never the UI thread.
    pub fn stop(&mut self) -> Result<(), HotspotError> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        let (sender, result) = mpsc::sync_channel(1);
        let sent = self.control.send(sender).is_ok();
        let outcome = if sent {
            result
                .recv()
                .unwrap_or(Err(HotspotError::Operation("热点后台线程提前退出")))
        } else {
            Err(HotspotError::Operation("热点后台线程已退出"))
        };
        let joined = worker.join();
        self.owned = false;
        joined.map_err(|_| HotspotError::Operation("热点后台线程异常退出"))?;
        outcome
    }

    fn start_worker(
        config: Option<HotspotConfig>,
        cancel: &AtomicBool,
    ) -> Result<Self, HotspotError> {
        Self::start_worker_with(cancel, move |cancel| native::start(config.as_ref(), cancel))
    }

    // S intentionally has no Send bound: native objects are constructed and
    // destroyed on the actor, and only the endpoint/result crosses the channel.
    fn start_worker_with<S: SessionControl + 'static, F>(
        cancel: &AtomicBool,
        start: F,
    ) -> Result<Self, HotspotError>
    where
        F: FnOnce(&AtomicBool) -> Result<PreparedSession<S>, HotspotError> + Send + 'static,
    {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (ready_sender, ready) = mpsc::sync_channel(1);
        let (control, commands) = mpsc::channel::<mpsc::SyncSender<Result<(), HotspotError>>>();
        let worker = thread::Builder::new()
            .name("carplay-hotspot".into())
            .spawn(move || {
                // The apartment is initialized before creating any Windows object
                // and released only after the session and manager have been dropped.
                #[cfg(target_os = "windows")]
                let _apartment = match native::Apartment::new() {
                    Ok(apartment) => apartment,
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                        return;
                    }
                };
                let mut session = match start(&worker_cancelled) {
                    Ok(session) => session,
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                        return;
                    }
                };
                if ready_sender
                    .send(Ok((
                        session.endpoint.clone(),
                        session.native.started_by_us(),
                    )))
                    .is_err()
                {
                    return;
                }
                // Channel closure also drops the native session on this same thread.
                if let Ok(reply) = commands.recv() {
                    let result = session.native.stop();
                    let _ = reply.send(result);
                }
            })
            .map_err(|_| HotspotError::Operation("无法启动热点后台线程"))?;
        loop {
            if cancel.load(Ordering::Acquire) {
                cancelled.store(true, Ordering::Release);
            }
            match ready.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok((endpoint, owned))) => {
                    let mut session = Self {
                        endpoint,
                        owned,
                        control,
                        worker: Some(worker),
                    };
                    if cancel.load(Ordering::Acquire) {
                        session.stop()?;
                        return Err(HotspotError::Cancelled);
                    }
                    return Ok(session);
                }
                Ok(Err(error)) => {
                    let _ = worker.join();
                    return Err(error);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = worker.join();
                    return Err(HotspotError::Operation("热点后台线程提前退出"));
                }
            }
        }
    }
}
impl Drop for HotspotSession {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
fn check_cancel(cancel: &AtomicBool) -> Result<(), HotspotError> {
    if cancel.load(Ordering::Acquire) {
        Err(HotspotError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod native {
    use super::*;
    use std::{
        mem::MaybeUninit,
        net::IpAddr,
        ptr, thread,
        time::{Duration, Instant},
    };
    use windows::{
        Foundation::{AsyncStatus, IAsyncAction, IAsyncInfo, IAsyncOperation},
        Networking::{Connectivity::NetworkInformation, NetworkOperators::*},
        Win32::System::{
            Com::CoIncrementMTAUsage,
            WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
        },
        core::Interface,
    };

    pub(super) struct Apartment(bool);
    impl Apartment {
        pub(super) fn new() -> Result<Self, HotspotError> {
            // windows-rs caches agile activation factories for process life.
            // Keep the MTA host alive equally long, including gaps between
            // workers/read-only probes. Otherwise releasing the final apartment
            // can leave a cached factory pointing at an unloaded implementation.
            // This is the same process-lifetime MTA strategy used by windows-core
            // when activating a class from a previously uninitialized thread.
            static MTA: std::sync::OnceLock<Result<(), u32>> = std::sync::OnceLock::new();
            let initialized = MTA.get_or_init(|| {
                // SAFETY: one process-lifetime MTA usage reference. The cookie
                // is intentionally retained until process exit, like the caches.
                unsafe { CoIncrementMTAUsage() }
                    .map(|_| ())
                    .map_err(|e| e.code().0 as u32)
            });
            if let Err(code) = initialized {
                return Err(HotspotError::Windows(*code));
            }
            // SAFETY: balanced on this thread; an existing STA remains owned by
            // its caller and is not uninitialized by this guard.
            match unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
                Ok(()) => Ok(Self(true)),
                Err(e) if e.code().0 as u32 == 0x80010106 => Ok(Self(false)),
                Err(e) => Err(e.into()),
            }
        }
    }
    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.0 {
                unsafe { RoUninitialize() };
            }
        }
    }
    impl From<windows::core::Error> for HotspotError {
        fn from(value: windows::core::Error) -> Self {
            // Do not expose OS messages: they can echo configuration data.
            Self::Windows(value.code().0 as u32)
        }
    }

    fn manager() -> Result<NetworkOperatorTetheringManager, HotspotError> {
        let profile = NetworkInformation::GetInternetConnectionProfile()
            .map_err(|_| HotspotError::NoUpstream)?;
        let capability =
            NetworkOperatorTetheringManager::GetTetheringCapabilityFromConnectionProfile(&profile)?;
        if capability != TetheringCapability::Enabled {
            return Err(HotspotError::Unavailable(match capability {
                TetheringCapability::DisabledByGroupPolicy => "组策略禁用",
                TetheringCapability::DisabledByHardwareLimitation => "Wi-Fi 驱动或网卡不支持",
                TetheringCapability::DisabledBySystemCapability => {
                    "缺少系统 Wi-Fi 控制能力（wiFiControl）"
                }
                TetheringCapability::DisabledByOperator => "网络运营商禁用",
                TetheringCapability::DisabledBySku => "Windows 版本不支持",
                _ => "系统报告热点不可用",
            }));
        }
        Ok(NetworkOperatorTetheringManager::CreateFromConnectionProfile(&profile)?)
    }
    pub(super) fn capability() -> Result<(), HotspotError> {
        let _apartment = Apartment::new()?;
        manager().map(|_| ())
    }

    fn wait(info: &IAsyncInfo, cancel: &AtomicBool) -> Result<(), HotspotError> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
                let _ = info.Cancel();
                return if cancel.load(Ordering::Acquire) {
                    Err(HotspotError::Cancelled)
                } else {
                    Err(HotspotError::Timeout)
                };
            }
            match info.Status()? {
                AsyncStatus::Started => thread::sleep(Duration::from_millis(50)),
                AsyncStatus::Completed => return Ok(()),
                AsyncStatus::Canceled => return Err(HotspotError::Cancelled),
                _ => return Err(HotspotError::Windows(info.ErrorCode()?.0 as u32)),
            }
        }
    }
    fn action(op: IAsyncAction, cancel: &AtomicBool) -> Result<(), HotspotError> {
        wait(&op.cast()?, cancel)?;
        Ok(op.GetResults()?)
    }
    fn operation(
        op: IAsyncOperation<NetworkOperatorTetheringOperationResult>,
        cancel: &AtomicBool,
    ) -> Result<TetheringOperationStatus, HotspotError> {
        wait(&op.cast()?, cancel)?;
        Ok(op.GetResults()?.Status()?)
    }
    fn success(status: TetheringOperationStatus) -> Result<(), HotspotError> {
        if status == TetheringOperationStatus::Success {
            return Ok(());
        }
        Err(HotspotError::Operation(match status {
            TetheringOperationStatus::WiFiDeviceOff => "请先打开电脑 Wi-Fi",
            TetheringOperationStatus::OperationInProgress => "系统正在处理另一个热点操作",
            TetheringOperationStatus::NetworkLimitedConnectivity => "共享网络不可用或连接受限",
            TetheringOperationStatus::BandInterference => {
                "当前 Wi-Fi 频段冲突，请在系统设置中更改热点频段"
            }
            TetheringOperationStatus::RadioRestriction => "无线电或飞行模式限制",
            TetheringOperationStatus::AlreadyOn => "热点已被其他程序启动",
            _ => "Windows 未能完成热点操作",
        }))
    }
    fn config_of(
        value: &NetworkOperatorTetheringAccessPointConfiguration,
    ) -> Result<HotspotConfig, HotspotError> {
        let config = HotspotConfig {
            ssid: value.Ssid()?.to_string(),
            passphrase: Zeroizing::new(value.Passphrase()?.to_string()),
        };
        config.validate()?;
        Ok(config)
    }
    fn security_of(
        value: &NetworkOperatorTetheringAccessPointConfiguration,
    ) -> Result<u8, HotspotError> {
        match value.AuthenticationKind() {
            Ok(TetheringWiFiAuthenticationKind::Wpa2) => Ok(2),
            Ok(TetheringWiFiAuthenticationKind::Wpa3TransitionMode) => Ok(3),
            Ok(TetheringWiFiAuthenticationKind::Wpa3) => Ok(4),
            // Windows before this interface only supported WPA2.
            Err(e) if e.code().0 as u32 == 0x80004002 => Ok(2),
            Err(e) => Err(e.into()),
            _ => Err(HotspotError::Unavailable("未知热点认证方式")),
        }
    }

    /// Identify the virtual Wi-Fi Direct adapter, not the internet uplink or a
    /// guessed 192.168.137.1. Ambiguous/missing adapters fail closed.
    fn endpoint(
        config: &NetworkOperatorTetheringAccessPointConfiguration,
    ) -> Result<HotspotEndpoint, HotspotError> {
        use windows_sys::Win32::{
            Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR},
            NetworkManagement::IpHelper::{
                GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
                GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
            },
            Networking::WinSock::AF_INET,
        };
        let mut size = 16_384;
        let mut indices = Vec::new();
        for _ in 0..4 {
            let mut storage = vec![
                MaybeUninit::<IP_ADAPTER_ADDRESSES_LH>::uninit();
                (size as usize)
                    .div_ceil(std::mem::size_of::<IP_ADAPTER_ADDRESSES_LH>())
            ];
            let head = storage.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            // SAFETY: aligned backing allocation remains alive for traversal;
            // pointers and strings are produced by GetAdaptersAddresses.
            unsafe {
                let status = GetAdaptersAddresses(
                    AF_INET as u32,
                    GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_DNS_SERVER | GAA_FLAG_SKIP_MULTICAST,
                    ptr::null(),
                    head,
                    &mut size,
                );
                if status == ERROR_BUFFER_OVERFLOW {
                    continue;
                }
                if status != NO_ERROR {
                    return Err(HotspotError::EndpointUnavailable);
                }
                let mut cursor = head;
                while !cursor.is_null() {
                    let item = &*cursor;
                    if item.OperStatus == 1 && item.IfType == 71 && !item.Description.is_null() {
                        let mut len = 0;
                        while len < 1024 && *item.Description.add(len) != 0 {
                            len += 1;
                        }
                        let description = String::from_utf16_lossy(std::slice::from_raw_parts(
                            item.Description,
                            len,
                        ))
                        .to_ascii_lowercase();
                        if description.contains("wi-fi direct")
                            || description.contains("wifi direct")
                        {
                            indices.push(item.Anonymous1.Anonymous.IfIndex);
                        }
                    }
                    cursor = item.Next;
                }
            }
            break;
        }
        let candidates: Vec<_> = crate::network::diagnose()
            .addresses
            .into_iter()
            .filter(|item| item.is_up && indices.contains(&item.interface_index))
            .filter_map(|item| match item.address {
                IpAddr::V4(address) if address.is_private() => {
                    Some((item.interface_index, address))
                }
                _ => None,
            })
            .collect();
        let [(interface_index, address)] = candidates.as_slice() else {
            return Err(HotspotError::EndpointUnavailable);
        };
        let values = config_of(config)?;
        Ok(HotspotEndpoint {
            ssid: values.ssid,
            passphrase: values.passphrase,
            address: *address,
            interface_index: *interface_index,
            security_type: security_of(config)?,
        })
    }
    pub(super) fn current() -> Result<Option<HotspotEndpoint>, HotspotError> {
        let _apartment = Apartment::new()?;
        let manager = manager()?;
        if manager.TetheringOperationalState()? != TetheringOperationalState::On {
            return Ok(None);
        }
        Ok(Some(endpoint(
            &manager.GetCurrentAccessPointConfiguration()?,
        )?))
    }

    pub(super) struct Session {
        manager: NetworkOperatorTetheringManager,
        owned: bool,
        previous: Option<NetworkOperatorTetheringAccessPointConfiguration>,
        expected: Option<HotspotConfig>,
    }
    impl Session {
        pub(super) fn started_by_us(&self) -> bool {
            self.owned
        }
        pub(super) fn stop(&mut self) -> Result<(), HotspotError> {
            if !self.owned && self.previous.is_none() {
                return Ok(());
            }
            let _apartment = Apartment::new()?;
            let cancel = AtomicBool::new(false);
            if let Some(expected) = &self.expected {
                let current = config_of(&self.manager.GetCurrentAccessPointConfiguration()?)?;
                if current.ssid != expected.ssid || current.passphrase != expected.passphrase {
                    // Another program/user replaced our configuration. It owns
                    // this newer hotspot; do not stop it or restore over it.
                    self.owned = false;
                    self.previous = None;
                    return Ok(());
                }
            }
            if self.owned {
                if self.manager.TetheringOperationalState()? != TetheringOperationalState::Off {
                    success(operation(self.manager.StopTetheringAsync()?, &cancel)?)?;
                }
                self.owned = false;
            }
            if let Some(previous) = &self.previous {
                action(self.manager.ConfigureAccessPointAsync(previous)?, &cancel)?;
                self.previous = None;
            }
            Ok(())
        }
    }
    impl Drop for Session {
        fn drop(&mut self) {
            let _ = self.stop();
        }
    }
    pub(super) fn start(
        requested: Option<&HotspotConfig>,
        cancel: &AtomicBool,
    ) -> Result<PreparedSession, HotspotError> {
        let _apartment = Apartment::new()?;
        let manager = manager()?;
        let current = manager.GetCurrentAccessPointConfiguration()?;
        let state = manager.TetheringOperationalState()?;
        if state == TetheringOperationalState::InTransition
            || state == TetheringOperationalState::Unknown
        {
            return Err(HotspotError::Busy);
        }
        if state == TetheringOperationalState::On {
            let current_values = config_of(&current)?;
            if requested.is_some_and(|r| {
                r.ssid != current_values.ssid || r.passphrase != current_values.passphrase
            }) {
                return Err(HotspotError::AlreadyRunning);
            }
            return Ok(PreparedSession {
                endpoint: endpoint(&current)?,
                native: Session {
                    manager,
                    owned: false,
                    previous: None,
                    expected: None,
                },
            });
        }
        let mut native = Session {
            manager,
            owned: false,
            previous: None,
            expected: None,
        };
        let result = (|| {
            if let Some(requested) = requested {
                let config = NetworkOperatorTetheringAccessPointConfiguration::new()?;
                config.SetSsid(&requested.ssid.as_str().into())?;
                config.SetPassphrase(&requested.passphrase.as_str().into())?;
                native.previous = Some(current);
                action(native.manager.ConfigureAccessPointAsync(&config)?, cancel)?;
                native.expected = Some(requested.clone());
            } else {
                native.expected = Some(config_of(&current)?);
            }
            check_cancel(cancel)?;
            // Mark ownership before awaiting so cancellation/error cleanup also
            // handles a hotspot that became active while the async call completed.
            let operation_handle = native.manager.StartTetheringAsync()?;
            native.owned = true;
            let result = operation(operation_handle, cancel)?;
            if result == TetheringOperationStatus::AlreadyOn {
                native.owned = false;
                native.previous = None;
            } else {
                success(result)?;
            }
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                check_cancel(cancel)?;
                let config = native.manager.GetCurrentAccessPointConfiguration()?;
                match endpoint(&config) {
                    Ok(endpoint) => return Ok(endpoint),
                    Err(HotspotError::EndpointUnavailable) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(100))
                    }
                    Err(error) => return Err(error),
                }
            }
        })();
        finish_start(result, native)
    }
}

#[cfg(target_os = "linux")]
#[path = "hotspot_linux.rs"]
mod native;

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
mod native {
    use super::*;
    pub(super) struct Session;
    impl Session {
        pub(super) fn started_by_us(&self) -> bool {
            false
        }
        pub(super) fn stop(&mut self) -> Result<(), HotspotError> {
            Ok(())
        }
    }
    pub(super) fn capability() -> Result<(), HotspotError> {
        Err(HotspotError::Unsupported(
            "仅支持 Windows Mobile Hotspot 和 Linux NetworkManager",
        ))
    }
    pub(super) fn current() -> Result<Option<HotspotEndpoint>, HotspotError> {
        capability()?;
        Ok(None)
    }
    pub(super) fn start(
        _: Option<&HotspotConfig>,
        _: &AtomicBool,
    ) -> Result<PreparedSession, HotspotError> {
        capability()?;
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc, sync::Mutex};

    type Trace = Arc<Mutex<Vec<(&'static str, thread::ThreadId)>>>;
    // Rc makes this mock deliberately !Send, just like the Windows COM manager.
    struct LocalSession {
        trace: Trace,
        stop_calls: Rc<Cell<u32>>,
        fail_stop: bool,
    }
    impl SessionControl for LocalSession {
        fn started_by_us(&self) -> bool {
            true
        }
        fn stop(&mut self) -> Result<(), HotspotError> {
            self.stop_calls.set(self.stop_calls.get() + 1);
            self.trace
                .lock()
                .unwrap()
                .push(("stop", thread::current().id()));
            if self.fail_stop {
                Err(HotspotError::Timeout)
            } else {
                Ok(())
            }
        }
    }
    impl Drop for LocalSession {
        fn drop(&mut self) {
            self.trace
                .lock()
                .unwrap()
                .push(("drop", thread::current().id()));
        }
    }
    fn local_session(trace: Trace, fail_stop: bool) -> PreparedSession<LocalSession> {
        trace
            .lock()
            .unwrap()
            .push(("create", thread::current().id()));
        PreparedSession {
            endpoint: HotspotEndpoint {
                ssid: "Test".into(),
                passphrase: Zeroizing::new("test-password".into()),
                address: Ipv4Addr::new(192, 168, 71, 1),
                interface_index: 71,
                security_type: 2,
            },
            native: LocalSession {
                trace,
                stop_calls: Rc::new(Cell::new(0)),
                fail_stop,
            },
        }
    }

    #[test]
    fn actor_owns_non_send_resources_and_stops_once_even_after_public_handle_moves() {
        let trace = Trace::default();
        let record = Arc::clone(&trace);
        let mut session = HotspotSession::start_worker_with(&AtomicBool::new(false), move |_| {
            Ok(local_session(record, false))
        })
        .unwrap();
        thread::spawn(move || {
            session.stop().unwrap();
            session.stop().unwrap();
        })
        .join()
        .unwrap();
        let events = trace.lock().unwrap();
        assert_eq!(
            events.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            ["create", "stop", "drop"]
        );
        let actor = events[0].1;
        assert_ne!(actor, thread::current().id());
        assert!(events.iter().all(|(_, thread)| *thread == actor));
    }

    #[test]
    fn actor_stop_failure_is_reported_and_still_joins_cleanup_thread() {
        let trace = Trace::default();
        let record = Arc::clone(&trace);
        let mut session = HotspotSession::start_worker_with(&AtomicBool::new(false), move |_| {
            Ok(local_session(record, true))
        })
        .unwrap();
        assert!(matches!(session.stop(), Err(HotspotError::Timeout)));
        assert_eq!(trace.lock().unwrap().last().unwrap().0, "drop");
    }

    #[test]
    fn cancellation_racing_start_cleans_up_before_returning_and_preserves_cleanup_errors() {
        for fail_stop in [false, true] {
            let cancelled = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&cancelled);
            let trace = Trace::default();
            let record = Arc::clone(&trace);
            let result = HotspotSession::start_worker_with(&cancelled, move |_| {
                let session = local_session(record, fail_stop);
                flag.store(true, Ordering::Release);
                Ok(session)
            });
            if fail_stop {
                assert!(matches!(result, Err(HotspotError::Timeout)));
            } else {
                assert!(matches!(result, Err(HotspotError::Cancelled)));
            }
            assert_eq!(trace.lock().unwrap().last().unwrap().0, "drop");
        }
        let session = local_session(Trace::default(), true);
        let result = finish_start(Err(HotspotError::Cancelled), session.native);
        assert!(matches!(result, Err(HotspotError::Cleanup { .. })));
    }
    #[test]
    fn public_session_can_move_between_connection_workers() {
        fn assert_send<T: Send>() {}
        assert_send::<HotspotSession>();
    }
    #[test]
    fn config_validation_and_debug_never_expose_credentials() {
        let mut config = HotspotConfig {
            ssid: "私人热点".into(),
            passphrase: Zeroizing::new("PrivateSecret123".into()),
        };
        assert!(config.validate().is_ok());
        assert!(!format!("{config:?}").contains("PrivateSecret"));
        assert!(!format!("{config:?}").contains("私人"));
        config.ssid = "中".repeat(11);
        assert!(config.validate().is_err());
        config.ssid = "RustCarPlay".into();
        config.passphrase = Zeroizing::new("short".into());
        assert!(config.validate().is_err());
        config.passphrase = Zeroizing::new("abcd\nefgh".into());
        assert!(config.validate().is_err());
        let endpoint = HotspotEndpoint {
            ssid: "PrivateSSID".into(),
            passphrase: Zeroizing::new("PrivateSecret123".into()),
            address: Ipv4Addr::new(192, 168, 77, 1),
            interface_index: 77,
            security_type: 2,
        };
        assert!(!format!("{endpoint:?}").contains("Private"));
        assert!(!format!("{endpoint:?}").contains("192.168"));
    }
    #[test]
    fn pre_cancelled_start_never_reaches_system() {
        assert!(matches!(
            HotspotSession::start_default(&AtomicBool::new(true)),
            Err(HotspotError::Cancelled)
        ));
    }
}
