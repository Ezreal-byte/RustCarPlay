use crate::diagnostics::DiagnosticIssue;
use serde::Serialize;
use std::net::IpAddr;
#[cfg(any(target_os = "windows", target_os = "linux"))]
use std::net::{Ipv4Addr, Ipv6Addr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressScope {
    Unspecified,
    Loopback,
    LinkLocal,
    Private,
    Multicast,
    Global,
}

pub fn address_scope(address: IpAddr) -> AddressScope {
    if address.is_unspecified() {
        return AddressScope::Unspecified;
    }
    if address.is_loopback() {
        return AddressScope::Loopback;
    }
    if address.is_multicast() {
        return AddressScope::Multicast;
    }
    match address {
        IpAddr::V4(value) if value.is_link_local() => AddressScope::LinkLocal,
        IpAddr::V6(value) if value.is_unicast_link_local() => AddressScope::LinkLocal,
        IpAddr::V4(value) if value.is_private() => AddressScope::Private,
        IpAddr::V6(value) if value.is_unique_local() => AddressScope::Private,
        _ => AddressScope::Global,
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InterfaceAddress {
    pub interface_index: u32,
    pub address: IpAddr,
    pub prefix_length: Option<u8>,
    pub scope: AddressScope,
    /// Required for IPv6 link-local socket addresses; zero for IPv4.
    pub scope_id: u32,
    pub is_up: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct NetworkDiagnostic {
    pub addresses: Vec<InterfaceAddress>,
    pub issue: Option<DiagnosticIssue>,
}

/// Interface identifiers are numeric; adapter names and MAC addresses are not exported.
pub fn diagnose() -> NetworkDiagnostic {
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    let result = native_addresses();
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    let result: std::io::Result<Vec<InterfaceAddress>> =
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported));
    match result {
        Ok(mut addresses) => {
            addresses.sort_by_key(|a| (a.interface_index, a.address, a.scope_id));
            addresses.dedup();
            NetworkDiagnostic {
                addresses,
                issue: None,
            }
        }
        Err(error) => NetworkDiagnostic {
            addresses: Vec::new(),
            issue: Some(DiagnosticIssue::io("network_interfaces", &error)),
        },
    }
}

#[cfg(target_os = "windows")]
fn native_addresses() -> std::io::Result<Vec<InterfaceAddress>> {
    use std::{
        mem::{self, MaybeUninit},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, NO_ERROR},
        NetworkManagement::IpHelper::{
            GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
            GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
        },
        Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6},
    };
    let mut bytes = 16 * 1024u32;
    for _ in 0..4 {
        // Vec of the native type guarantees alignment unlike a Vec<u8> cast.
        let count = (bytes as usize).div_ceil(mem::size_of::<IP_ADAPTER_ADDRESSES_LH>());
        let mut buffer = vec![MaybeUninit::<IP_ADAPTER_ADDRESSES_LH>::uninit(); count];
        let head = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        // SAFETY: aligned allocation has at least bytes capacity; Windows fills
        // the linked structures and pointers within it on success.
        let status = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC as u32,
                GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
                ptr::null(),
                head,
                &mut bytes,
            )
        };
        if status == ERROR_BUFFER_OVERFLOW {
            continue;
        }
        if status == ERROR_NO_DATA {
            return Ok(Vec::new());
        }
        if status != NO_ERROR {
            return Err(std::io::Error::from_raw_os_error(status as i32));
        }
        let mut result = Vec::new();
        let mut current = head;
        // SAFETY: GetAdaptersAddresses returned success and the backing buffer
        // remains alive for the complete traversal; sockaddr lengths are checked.
        unsafe {
            while !current.is_null() {
                let adapter = &*current;
                let mut unicast = adapter.FirstUnicastAddress;
                while !unicast.is_null() {
                    let item = &*unicast;
                    let socket = item.Address.lpSockaddr;
                    if !socket.is_null() {
                        let family = (*socket).sa_family;
                        let parsed = if family == AF_INET
                            && item.Address.iSockaddrLength as usize
                                >= mem::size_of::<SOCKADDR_IN>()
                        {
                            let addr = &*socket.cast::<SOCKADDR_IN>();
                            Some((
                                IpAddr::V4(Ipv4Addr::from(addr.sin_addr.S_un.S_addr.to_ne_bytes())),
                                0,
                                adapter.Anonymous1.Anonymous.IfIndex,
                            ))
                        } else if family == AF_INET6
                            && item.Address.iSockaddrLength as usize
                                >= mem::size_of::<SOCKADDR_IN6>()
                        {
                            let addr = &*socket.cast::<SOCKADDR_IN6>();
                            Some((
                                IpAddr::V6(Ipv6Addr::from(addr.sin6_addr.u.Byte)),
                                addr.Anonymous.sin6_scope_id,
                                adapter.Ipv6IfIndex,
                            ))
                        } else {
                            None
                        };
                        if let Some((address, scope_id, interface_index)) = parsed {
                            result.push(InterfaceAddress {
                                interface_index,
                                address,
                                prefix_length: Some(item.OnLinkPrefixLength),
                                scope: address_scope(address),
                                scope_id,
                                is_up: adapter.OperStatus == 1,
                            });
                        }
                    }
                    unicast = item.Next;
                }
                current = adapter.Next;
            }
        }
        return Ok(result);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "network interface list changed repeatedly",
    ))
}

#[cfg(target_os = "linux")]
fn native_addresses() -> std::io::Result<Vec<InterfaceAddress>> {
    use std::ptr;
    struct AddressList(*mut libc::ifaddrs);
    impl Drop for AddressList {
        fn drop(&mut self) {
            unsafe {
                libc::freeifaddrs(self.0);
            }
        }
    }
    let mut head = ptr::null_mut();
    // SAFETY: getifaddrs initializes head; RAII frees its entire list on return.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let list = AddressList(head);
    let mut current = list.0;
    let mut result = Vec::new();
    // SAFETY: list and sockaddr family layouts are provided by getifaddrs and
    // remain alive until AddressList drops after this traversal.
    unsafe {
        while !current.is_null() {
            let item = &*current;
            if !item.ifa_addr.is_null() {
                let index = libc::if_nametoindex(item.ifa_name);
                let (address, scope_id, netmask) = match (*item.ifa_addr).sa_family as i32 {
                    libc::AF_INET => {
                        let addr = &*item.ifa_addr.cast::<libc::sockaddr_in>();
                        let mask = (!item.ifa_netmask.is_null()).then(|| {
                            (*item.ifa_netmask.cast::<libc::sockaddr_in>())
                                .sin_addr
                                .s_addr
                                .to_ne_bytes()
                                .to_vec()
                        });
                        (
                            Some(IpAddr::V4(Ipv4Addr::from(
                                addr.sin_addr.s_addr.to_ne_bytes(),
                            ))),
                            0,
                            mask,
                        )
                    }
                    libc::AF_INET6 => {
                        let addr = &*item.ifa_addr.cast::<libc::sockaddr_in6>();
                        let mask = (!item.ifa_netmask.is_null()).then(|| {
                            (*item.ifa_netmask.cast::<libc::sockaddr_in6>())
                                .sin6_addr
                                .s6_addr
                                .to_vec()
                        });
                        (
                            Some(IpAddr::V6(Ipv6Addr::from(addr.sin6_addr.s6_addr))),
                            addr.sin6_scope_id,
                            mask,
                        )
                    }
                    _ => (None, 0, None),
                };
                if let Some(address) = address {
                    result.push(InterfaceAddress {
                        interface_index: index,
                        address,
                        scope: address_scope(address),
                        scope_id,
                        prefix_length: netmask.as_deref().and_then(prefix_length),
                        is_up: item.ifa_flags & libc::IFF_UP as u32 != 0,
                    });
                }
            }
            current = item.ifa_next;
        }
    }
    Ok(result)
}

#[cfg(any(target_os = "linux", test))]
fn prefix_length(mask: &[u8]) -> Option<u8> {
    let mut count = 0;
    let mut saw_zero = false;
    for byte in mask {
        for bit in (0..8).rev() {
            if byte & (1 << bit) != 0 {
                if saw_zero {
                    return None;
                }
                count += 1;
            } else {
                saw_zero = true;
            }
        }
    }
    Some(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scope_and_netmask_boundaries() {
        for (ip, expected) in [
            ("fe80::1", AddressScope::LinkLocal),
            ("febf::1", AddressScope::LinkLocal),
            ("fec0::1", AddressScope::Global),
            ("fd01::1", AddressScope::Private),
            ("::1", AddressScope::Loopback),
            ("169.254.1.1", AddressScope::LinkLocal),
            ("192.168.1.1", AddressScope::Private),
            ("224.0.0.251", AddressScope::Multicast),
        ] {
            assert_eq!(address_scope(ip.parse().unwrap()), expected);
        }
        assert_eq!(prefix_length(&[255, 255, 128, 0]), Some(17));
        assert_eq!(prefix_length(&[255, 127, 0, 0]), None);
        assert_eq!(prefix_length(&[255; 16]), Some(128));
    }
}
