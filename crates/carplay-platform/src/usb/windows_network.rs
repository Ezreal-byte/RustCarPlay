// SPDX-License-Identifier: GPL-3.0-only
//! Read-only association of one physical USB phone with its UsbNcm adapter.
//! Friendly names, adapter order, and the appearance of a new IP are never
//! identity evidence. SetupAPI ancestry and the descriptor's MI number are.

use crate::network;
use std::ffi::OsStr;
use std::io;
use std::net::{IpAddr, SocketAddrV6, TcpListener};
use std::ptr;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_IDW, CM_Get_Parent, CR_SUCCESS, DICS_FLAG_GLOBAL, DIGCF_PRESENT, DIREG_DRV,
    HDEVINFO, SP_DEVINFO_DATA, SPDRP_SERVICE, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
    SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW, SetupDiOpenDevRegKey,
};
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, GetLastError};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    ConvertInterfaceGuidToLuid, GetIfEntry2, MIB_IF_ROW2,
};
use windows_sys::Win32::System::Registry::{
    KEY_QUERY_VALUE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegGetValueW,
};
use windows_sys::core::GUID;

const NET_CLASS: GUID = GUID::from_u128(0x4d36e972_e325_11ce_bfc1_08002be10318);

struct DeviceSet(HDEVINFO);
impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: this handle was returned by SetupDiGetClassDevsW and is
        // released exactly once after enumeration and all property reads.
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

pub(super) struct NcmEndpoint {
    pub bind: SocketAddrV6,
    pub mac: [u8; 6],
}

fn instance_id(devinst: u32) -> Option<String> {
    // Windows MAX_DEVICE_ID_LEN is 200 including NUL.
    let mut value = [0_u16; 200];
    // SAFETY: fixed writable UTF-16 output capacity and an enumerated devinst.
    let status = unsafe { CM_Get_Device_IDW(devinst, value.as_mut_ptr(), value.len() as u32, 0) };
    (status == CR_SUCCESS).then(|| nul_string(&value)).flatten()
}

fn nul_string(value: &[u16]) -> Option<String> {
    let length = value.iter().position(|c| *c == 0)?;
    String::from_utf16(&value[..length]).ok()
}

fn ancestors(mut devinst: u32) -> Vec<String> {
    let mut result = Vec::new();
    for _ in 0..32 {
        let Some(id) = instance_id(devinst) else {
            break;
        };
        result.push(id);
        let mut parent = 0;
        // SAFETY: writable output; reading the parent never changes PnP state.
        if unsafe { CM_Get_Parent(&mut parent, devinst, 0) } != CR_SUCCESS || parent == devinst {
            break;
        }
        devinst = parent;
    }
    result
}

fn belongs_to_phone(chain: &[String], phone: &str, control: u8) -> bool {
    let Some(root) = chain.iter().position(|s| s.eq_ignore_ascii_case(phone)) else {
        return false;
    };
    let expected = format!("MI_{control:02X}");
    chain[..root].iter().any(|s| {
        let s = s.to_ascii_uppercase();
        let mut parts = s.split('\\');
        parts.next() == Some("USB")
            && parts.next().is_some_and(|hardware| {
                hardware.split('&').any(|part| part == "VID_05AC")
                    && hardware.split('&').any(|part| part == expected)
            })
    })
}

fn driver_service(set: HDEVINFO, info: &SP_DEVINFO_DATA) -> Option<String> {
    let mut text = [0_u16; 256];
    let mut kind = 0;
    let mut bytes = 0;
    // SAFETY: info belongs to the still-live set; buffer is aligned UTF-16 and
    // its byte capacity is correctly supplied to SetupAPI.
    let status = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set,
            info,
            SPDRP_SERVICE,
            &mut kind,
            text.as_mut_ptr().cast(),
            std::mem::size_of_val(&text) as u32,
            &mut bytes,
        )
    };
    (status != 0 && kind == REG_SZ && bytes as usize <= std::mem::size_of_val(&text))
        .then(|| nul_string(&text))
        .flatten()
}

fn interface_guid(set: HDEVINFO, info: &SP_DEVINFO_DATA) -> Option<GUID> {
    // SAFETY: opens only the enumerated adapter's existing driver key, with
    // read-only access. No guessed registry path or write capability is used.
    let key =
        unsafe { SetupDiOpenDevRegKey(set, info, DICS_FLAG_GLOBAL, 0, DIREG_DRV, KEY_QUERY_VALUE) };
    if key.is_null() || key as isize == -1 {
        return None;
    }
    let name: Vec<_> = "NetCfgInstanceId\0".encode_utf16().collect();
    let mut text = [0_u16; 80];
    let mut bytes = std::mem::size_of_val(&text) as u32;
    // SAFETY: live read-only key and NUL-terminated name; the native call is
    // bounded by the UTF-16 buffer's byte capacity and restricted to REG_SZ.
    let status = unsafe {
        RegGetValueW(
            key,
            ptr::null(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            text.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    // SAFETY: key was returned by SetupDiOpenDevRegKey and is released once.
    unsafe { RegCloseKey(key) };
    if status != 0 || bytes as usize > std::mem::size_of_val(&text) {
        return None;
    }
    let text = nul_string(&text)?;
    let id = uuid::Uuid::parse_str(&text).ok()?;
    Some(GUID::from_u128(id.as_u128()))
}

fn valid_mac(address: &[u8]) -> Option<[u8; 6]> {
    let mac: [u8; 6] = address.try_into().ok()?;
    (mac != [0; 6] && mac[0] & 1 == 0).then_some(mac)
}

/// Returns only an enabled UsbNcm adapter descended from `phone`, whose USB
/// interface matches the NCM control-interface descriptor. A carrier-down
/// adapter may be used: iPhone can withhold NCM traffic until StartSession.
pub(super) fn find_ncm_endpoint(phone: &OsStr, control: u8) -> io::Result<Option<NcmEndpoint>> {
    let Some(phone) = phone.to_str() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid USB instance ID",
        ));
    };
    // SAFETY: valid class GUID, no enumerator/window, present adapters only.
    let raw =
        unsafe { SetupDiGetClassDevsW(&NET_CLASS, ptr::null(), ptr::null_mut(), DIGCF_PRESENT) };
    if raw == -1 {
        return Err(io::Error::last_os_error());
    }
    let set = DeviceSet(raw);
    let mut rows = Vec::new();
    for index in 0..4096 {
        let mut info = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: writable properly initialized structure and a live set.
        if unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut info) } == 0 {
            // SAFETY: reads error from the immediately preceding SetupAPI call.
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        if !belongs_to_phone(&ancestors(info.DevInst), phone, control)
            || !driver_service(set.0, &info).is_some_and(|s| s.eq_ignore_ascii_case("UsbNcm"))
        {
            continue;
        }
        let Some(guid) = interface_guid(set.0, &info) else {
            continue;
        };
        let mut row = MIB_IF_ROW2::default();
        // SAFETY: initialized output row and a parsed network interface GUID.
        if unsafe { ConvertInterfaceGuidToLuid(&guid, &mut row.InterfaceLuid) } != 0
            || unsafe { GetIfEntry2(&mut row) } != 0
        {
            continue;
        }
        rows.push(row);
    }
    if rows.len() > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "multiple NCM adapters match the selected physical USB interface",
        ));
    }
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    if row.AdminStatus != 1 || row.InterfaceIndex == 0 || row.PhysicalAddressLength != 6 {
        return Ok(None);
    }
    let Some(mac) = valid_mac(&row.PhysicalAddress[..6]) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid NCM MAC address",
        ));
    };
    // OperStatus is intentionally not a condition here. Only local ownership
    // and bindability are needed before the iAP2 StartSession tells the phone
    // to start media; waiting for carrier could deadlock this bootstrap.
    for address in network::diagnose().addresses {
        if address.interface_index != row.InterfaceIndex {
            continue;
        }
        if let IpAddr::V6(ip) = address.address
            && ip.is_unicast_link_local()
        {
            let bind = SocketAddrV6::new(ip, 0, 0, row.InterfaceIndex);
            if let Ok(listener) = TcpListener::bind(bind) {
                drop(listener);
                return Ok(Some(NcmEndpoint {
                    bind: SocketAddrV6::new(ip, 7000, 0, row.InterfaceIndex),
                    mac,
                }));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ncm_match_requires_exact_physical_ancestor_and_interface() {
        let phone = r"USB\VID_05AC&PID_12A8\PHONE_A";
        let chain = vec![
            r"USB\VID_05AC&PID_12A8&MI_02\PORT".into(),
            phone.into(),
            r"USB\ROOT_HUB30\HUB".into(),
        ];
        assert!(belongs_to_phone(&chain, &phone.to_lowercase(), 2));
        assert!(!belongs_to_phone(&chain, phone, 1));
        assert!(!belongs_to_phone(
            &chain,
            r"USB\VID_05AC&PID_12A8\PHONE_B",
            2
        ));
        assert!(!belongs_to_phone(&chain, "PHONE_A", 2));
    }

    #[test]
    fn similar_interface_tokens_or_unrelated_adapters_do_not_match() {
        let phone = r"USB\VID_05AC&PID_12A8\PHONE";
        for child in [
            r"USB\VID_05AC&PID_12A8&MI_020\PORT",
            r"PCI\VID_05AC&MI_02\PORT",
            r"USB\VID_05AB&MI_02\PORT",
        ] {
            assert!(!belongs_to_phone(&[child.into(), phone.into()], phone, 2));
        }
    }

    #[test]
    fn ethernet_identity_rejects_zero_multicast_and_wrong_length() {
        assert_eq!(valid_mac(&[2, 1, 2, 3, 4, 5]), Some([2, 1, 2, 3, 4, 5]));
        assert_eq!(valid_mac(&[0; 6]), None);
        assert_eq!(valid_mac(&[1; 6]), None);
        assert_eq!(valid_mac(&[2; 8]), None);
    }
}
