use super::{BluetoothAddress, BluetoothDiagnostic, ConnectOptions};
use crate::{PlatformError, diagnostics::DiagnosticIssue};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::{io, mem, ptr, sync::atomic::AtomicBool};
use windows_sys::Win32::{Devices::Bluetooth::*, Foundation::CloseHandle};

fn decoded_name(name: &[u16]) -> String {
    let end = name
        .iter()
        .position(|&value| value == 0)
        .unwrap_or(name.len());
    String::from_utf16_lossy(&name[..end])
}

fn cached_address(value: u64) -> BluetoothAddress {
    let bytes = value.to_be_bytes();
    BluetoothAddress([bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]])
}

pub(super) fn paired_devices() -> Result<Vec<super::PairedDevice>, PlatformError> {
    // SAFETY: sized Win32 structs, no active inquiry, handles closed exactly once.
    unsafe {
        let search = BLUETOOTH_DEVICE_SEARCH_PARAMS {
            dwSize: mem::size_of::<BLUETOOTH_DEVICE_SEARCH_PARAMS>() as u32,
            fReturnAuthenticated: 1,
            fReturnRemembered: 1,
            fReturnUnknown: 0,
            fReturnConnected: 0,
            fIssueInquiry: 0,
            ..Default::default()
        };
        let mut info = BLUETOOTH_DEVICE_INFO {
            dwSize: mem::size_of::<BLUETOOTH_DEVICE_INFO>() as u32,
            ..Default::default()
        };
        let enumeration = BluetoothFindFirstDevice(&search, &mut info);
        if enumeration.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(259) {
                Ok(Vec::new())
            } else {
                Err(error.into())
            };
        }
        let result = (|| {
            let mut devices = Vec::new();
            loop {
                devices.push(super::PairedDevice {
                    name: decoded_name(&info.szName),
                    address: cached_address(info.Address.Anonymous.ullLong),
                    authenticated: info.fAuthenticated != 0,
                    remembered: info.fRemembered != 0,
                    connected: info.fConnected != 0,
                });
                if devices.len() > 4096 {
                    return Err(io::Error::other("Bluetooth cache exceeds device limit").into());
                }
                info = BLUETOOTH_DEVICE_INFO {
                    dwSize: mem::size_of::<BLUETOOTH_DEVICE_INFO>() as u32,
                    ..Default::default()
                };
                if BluetoothFindNextDevice(enumeration, &mut info) == 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(259) {
                        return Err(error.into());
                    }
                    break;
                }
            }
            devices.sort_by_key(|device| device.address.octets());
            devices.dedup_by_key(|device| device.address.octets());
            Ok(devices)
        })();
        BluetoothFindDeviceClose(enumeration);
        result
    }
}

pub(super) fn local_adapters() -> Result<Vec<super::LocalAdapter>, PlatformError> {
    // SAFETY: all structs are initialized with native sizes; radio handles and
    // enumeration handles are released even when GetRadioInfo fails.
    unsafe {
        let params = BLUETOOTH_FIND_RADIO_PARAMS {
            dwSize: mem::size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32,
        };
        let mut radio = ptr::null_mut();
        let enumeration = BluetoothFindFirstRadio(&params, &mut radio);
        if enumeration.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(259) {
                Ok(Vec::new())
            } else {
                Err(error.into())
            };
        }
        let result = (|| {
            let mut adapters = Vec::new();
            loop {
                let mut info = BLUETOOTH_RADIO_INFO {
                    dwSize: mem::size_of::<BLUETOOTH_RADIO_INFO>() as u32,
                    ..Default::default()
                };
                let status = BluetoothGetRadioInfo(radio, &mut info);
                CloseHandle(radio);
                radio = ptr::null_mut();
                if status != 0 {
                    return Err(io::Error::from_raw_os_error(status as i32).into());
                }
                adapters.push(super::LocalAdapter {
                    name: decoded_name(&info.szName),
                    address: cached_address(info.address.Anonymous.ullLong),
                });
                if BluetoothFindNextRadio(enumeration, &mut radio) == 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(259) {
                        return Err(error.into());
                    }
                    break;
                }
            }
            Ok(adapters)
        })();
        BluetoothFindRadioClose(enumeration);
        result
    }
}

fn socket() -> io::Result<Socket> {
    Socket::new(
        Domain::from(AF_BTH as i32),
        Type::STREAM,
        Some(Protocol::from(3)),
    )
}

fn address(peer: BluetoothAddress, channel: u32, uuid: Option<super::ServiceUuid>) -> SockAddr {
    let mut raw = SOCKADDR_BTH {
        addressFamily: AF_BTH,
        btAddr: peer.as_integer(),
        port: channel,
        ..Default::default()
    };
    if let Some(uuid) = uuid {
        let b = uuid.as_bytes();
        raw.serviceClassId = windows_sys::core::GUID {
            data1: u32::from_be_bytes(b[0..4].try_into().unwrap()),
            data2: u16::from_be_bytes(b[4..6].try_into().unwrap()),
            data3: u16::from_be_bytes(b[6..8].try_into().unwrap()),
            data4: b[8..16].try_into().unwrap(),
        };
    }
    // SAFETY: SOCKADDR_BTH is a WinSock address and fits in SOCKADDR_STORAGE.
    // Copy bytes rather than taking potentially unaligned references to packed fields.
    unsafe {
        let mut storage = socket2::SockAddrStorage::zeroed();
        ptr::copy_nonoverlapping(
            ptr::from_ref(&raw).cast::<u8>(),
            ptr::from_mut(&mut storage).cast::<u8>(),
            mem::size_of::<SOCKADDR_BTH>(),
        );
        SockAddr::new(storage, mem::size_of::<SOCKADDR_BTH>() as _)
    }
}

pub(super) fn connect(
    options: ConnectOptions,
    cancelled: &AtomicBool,
) -> Result<Socket, PlatformError> {
    let socket = socket()?;
    let remote = address(
        options.peer,
        u32::from(options.channel.unwrap_or(0)),
        options.channel.is_none().then_some(options.service),
    );
    Ok(super::connecting::connect(
        socket,
        &remote,
        options.timeout,
        cancelled,
    )?)
}

pub(super) fn listen(local: BluetoothAddress, channel: u8) -> Result<Socket, PlatformError> {
    let socket = socket()?;
    socket.bind(&address(local, u32::from(channel), None))?;
    socket.listen(8)?;
    Ok(socket)
}

pub(super) fn diagnose() -> BluetoothDiagnostic {
    let result = socket();
    let mut count = 0;
    // SAFETY: correctly sized input and initialized output handle; each handle
    // returned by the enumeration is closed once with its matching API.
    unsafe {
        let params = BLUETOOTH_FIND_RADIO_PARAMS {
            dwSize: mem::size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32,
        };
        let mut radio = ptr::null_mut();
        let enumeration = BluetoothFindFirstRadio(&params, &mut radio);
        if !enumeration.is_null() {
            loop {
                count += 1;
                if !radio.is_null() {
                    CloseHandle(radio);
                }
                radio = ptr::null_mut();
                if BluetoothFindNextRadio(enumeration, &mut radio) == 0 {
                    break;
                }
            }
            BluetoothFindRadioClose(enumeration);
        }
    }
    BluetoothDiagnostic {
        rfcomm_socket_supported: result.is_ok(),
        local_radio_count: Some(count),
        uuid_lookup_backend: "winsock_sdp",
        sdp_advertising_implemented: false,
        issue: result
            .err()
            .map(|error| DiagnosticIssue::io("rfcomm_socket", &error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_address_has_documented_bluetooth_layout() {
        let addr = address("01:23:45:67:89:AB".parse().unwrap(), 7, None);
        assert_eq!(addr.family(), AF_BTH);
        assert_eq!(addr.len() as usize, mem::size_of::<SOCKADDR_BTH>());
        // SAFETY: address() constructed a SOCKADDR_BTH of the checked length.
        let raw = unsafe { ptr::read_unaligned(addr.as_ptr().cast::<SOCKADDR_BTH>()) };
        let port = raw.port;
        let peer = raw.btAddr;
        assert_eq!(port, 7);
        assert_eq!(peer, 0x0123_4567_89ab);
    }
    #[test]
    fn cache_addresses_and_utf16_names_have_correct_boundaries() {
        assert_eq!(
            cached_address(0x0123_4567_89ab).to_colon_string(),
            "01:23:45:67:89:AB"
        );
        assert_eq!(decoded_name(&[0x0041, 0x4e2d, 0, 0x0042]), "A中");
        assert_eq!(decoded_name(&[0xd800]), "\u{fffd}");
        assert_eq!(decoded_name(&[]), "");
    }
}
