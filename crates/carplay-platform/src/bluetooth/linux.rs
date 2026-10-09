use super::{BluetoothAddress, BluetoothDiagnostic, ConnectOptions, ServiceUuid};
use crate::{PlatformError, diagnostics::DiagnosticIssue};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::{ffi::c_void, io, mem, ptr};

// Linux UAPI bluetooth/rfcomm.h: sa_family_t, little-endian bdaddr_t, channel.
#[repr(C)]
struct SockAddrRc {
    family: libc::sa_family_t,
    address: [u8; 6],
    channel: u8,
    padding: u8,
}

fn socket() -> io::Result<Socket> {
    Socket::new(
        Domain::from(libc::AF_BLUETOOTH),
        Type::STREAM,
        Some(Protocol::from(3)),
    )
}
fn address(peer: BluetoothAddress, channel: u8) -> SockAddr {
    let mut bytes = peer.octets();
    bytes.reverse();
    let raw = SockAddrRc {
        family: libc::AF_BLUETOOTH as _,
        address: bytes,
        channel,
        padding: 0,
    };
    // SAFETY: sockaddr_rc is an AF_BLUETOOTH socket address and fits storage.
    unsafe {
        let mut storage = socket2::SockAddrStorage::zeroed();
        ptr::copy_nonoverlapping(
            ptr::from_ref(&raw).cast::<u8>(),
            ptr::from_mut(&mut storage).cast::<u8>(),
            mem::size_of::<SockAddrRc>(),
        );
        SockAddr::new(storage, mem::size_of::<SockAddrRc>() as _)
    }
}
pub(super) fn connect(options: ConnectOptions) -> Result<Socket, PlatformError> {
    let channel = match options.channel {
        Some(channel) => channel,
        None => resolve_channel(options.peer, options.service)?,
    };
    let socket = socket()?;
    socket.connect_timeout(&address(options.peer, channel), options.timeout)?;
    Ok(socket)
}
pub(super) fn listen(local: BluetoothAddress, channel: u8) -> Result<Socket, PlatformError> {
    let socket = socket()?;
    socket.bind(&address(local, channel))?;
    socket.listen(8)?;
    Ok(socket)
}
pub(super) fn diagnose() -> BluetoothDiagnostic {
    let result = socket();
    let local_radio_count = std::fs::read_dir("/sys/class/bluetooth")
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("hci"))
                .count()
        });
    BluetoothDiagnostic {
        rfcomm_socket_supported: result.is_ok(),
        local_radio_count,
        uuid_lookup_backend: "bluez_libbluetooth_sdp",
        sdp_advertising_implemented: false,
        issue: result
            .err()
            .map(|error| DiagnosticIssue::io("rfcomm_socket", &error)),
    }
}

#[repr(C)]
struct SdpList {
    next: *mut SdpList,
    data: *mut c_void,
}
// uuid_t union's alignment is four, not sixteen (BlueZ lib/bluetooth/sdp.h).
#[repr(C)]
struct SdpUuid {
    kind: u8,
    padding: [u8; 3],
    value: [u32; 4],
}

/// Read-only SDP lookup using the BlueZ shared library. No pairing or adapter
/// settings are changed. libbluetooth has its own synchronous SDP timeout.
fn resolve_channel(peer: BluetoothAddress, uuid: ServiceUuid) -> Result<u8, PlatformError> {
    // SAFETY: signatures and layouts mirror BlueZ's public sdp_lib.h/sdp.h.
    // Library stays loaded until all sessions, lists, and records are released.
    unsafe {
        let library = libloading::Library::new("libbluetooth.so.3").map_err(|_| {
            PlatformError::ServiceDiscovery(
                "libbluetooth.so.3 unavailable; provide a known channel",
            )
        })?;
        macro_rules! symbol {
            ($name:literal, $ty:ty) => {
                *library
                    .get::<$ty>(concat!($name, "\0").as_bytes())
                    .map_err(|_| {
                        PlatformError::ServiceDiscovery("incompatible BlueZ SDP library")
                    })?
            };
        }
        let connect = symbol!(
            "sdp_connect",
            unsafe extern "C" fn(*const [u8; 6], *const [u8; 6], u32) -> *mut c_void
        );
        let close = symbol!("sdp_close", unsafe extern "C" fn(*mut c_void) -> i32);
        let create_uuid = symbol!(
            "sdp_uuid128_create",
            unsafe extern "C" fn(*mut SdpUuid, *const c_void) -> *mut SdpUuid
        );
        let search = symbol!(
            "sdp_service_search_attr_req",
            unsafe extern "C" fn(
                *mut c_void,
                *const SdpList,
                i32,
                *const SdpList,
                *mut *mut SdpList,
            ) -> i32
        );
        let access = symbol!(
            "sdp_get_access_protos",
            unsafe extern "C" fn(*const c_void, *mut *mut SdpList) -> i32
        );
        let port = symbol!(
            "sdp_get_proto_port",
            unsafe extern "C" fn(*const SdpList, i32) -> i32
        );
        let free_list = symbol!(
            "sdp_list_free",
            unsafe extern "C" fn(*mut SdpList, Option<unsafe extern "C" fn(*mut c_void)>)
        );
        let free_record = symbol!("sdp_record_free", unsafe extern "C" fn(*mut c_void));
        let mut remote = peer.octets();
        remote.reverse();
        let session = connect(&[0; 6], &remote, 0);
        if session.is_null() {
            return Err(PlatformError::ServiceDiscovery(
                "could not connect to paired peer's SDP service",
            ));
        }
        let mut native_uuid: SdpUuid = mem::zeroed();
        create_uuid(&mut native_uuid, uuid.as_bytes().as_ptr().cast());
        let search_list = SdpList {
            next: ptr::null_mut(),
            data: ptr::from_mut(&mut native_uuid).cast(),
        };
        let mut range = 0x0000_ffffu32;
        let attrs = SdpList {
            next: ptr::null_mut(),
            data: ptr::from_mut(&mut range).cast(),
        };
        let mut records = ptr::null_mut();
        let status = search(session, &search_list, 2, &attrs, &mut records);
        let mut found = None;
        let mut record = records;
        while !record.is_null() {
            let mut protocols = ptr::null_mut();
            if status == 0 && access((*record).data, &mut protocols) == 0 {
                let candidate = port(protocols, 3);
                if (1..=30).contains(&candidate) {
                    found.get_or_insert(candidate as u8);
                }
                let mut protocol = protocols;
                while !protocol.is_null() {
                    free_list((*protocol).data.cast(), None);
                    protocol = (*protocol).next;
                }
                free_list(protocols, None);
            }
            free_record((*record).data);
            record = (*record).next;
        }
        free_list(records, None);
        close(session);
        found.ok_or(PlatformError::ServiceDiscovery(if status == 0 {
            "service UUID has no RFCOMM channel"
        } else {
            "SDP query failed"
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_address_is_little_endian_bluez_layout() {
        assert_eq!(mem::size_of::<SockAddrRc>(), 10);
        assert_eq!(mem::size_of::<SdpUuid>(), 20);
        let address = address("01:23:45:67:89:AB".parse().unwrap(), 12);
        // SAFETY: constructed immediately above as sockaddr_rc.
        let raw = unsafe { &*address.as_ptr().cast::<SockAddrRc>() };
        assert_eq!(raw.address, [171, 137, 103, 69, 35, 1]);
        assert_eq!(raw.channel, 12);
    }
}
