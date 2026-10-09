// SPDX-License-Identifier: GPL-3.0-only
//! Windows SDK-free driver verification for the explicit USB preparation flow.
//! Verification does not install a driver. Only --add-catalog changes system state.
//!
//! Uses DRIVER_ACTION_VERIFY, not the ordinary Authenticode application policy.
//! This verifies WHQL/WHCP policy; legacy cross-signed embedded drivers can be
//! rejected even when SDK signtool /kp accepts them. The preparation script
//! retains an explicit SDK compatibility check for its pinned legacy driver.
//! API contracts: https://learn.microsoft.com/windows/win32/api/wintrust/nf-wintrust-winverifytrust
//! https://learn.microsoft.com/windows/win32/api/mscat/nf-mscat-cryptcatadminaddcatalog

#[cfg(not(windows))]
fn main() {
    eprintln!("USB driver verification requires Windows");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows::run() {
        eprintln!("USB driver verification: {error}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod windows {
    use std::{
        ffi::OsStr,
        fs::{File, OpenOptions},
        mem::size_of,
        os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
        path::{Path, PathBuf},
        ptr::{null, null_mut},
    };
    use windows_sys::Win32::{
        Foundation::{HANDLE, INVALID_HANDLE_VALUE},
        Security::{Cryptography::Catalog::*, WinTrust::*},
        UI::Shell::IsUserAnAdmin,
    };

    type Result<T> = std::result::Result<T, String>;

    fn wide(value: &OsStr) -> Result<Vec<u16>> {
        let mut result: Vec<_> = value.encode_wide().collect();
        if result.contains(&0) {
            return Err("path contains a NUL character".into());
        }
        result.push(0);
        Ok(result)
    }

    struct Input {
        path: PathBuf,
        wide: Vec<u16>,
        file: File,
    }

    impl Input {
        fn open(path: &OsStr) -> Result<Self> {
            let path = Path::new(path)
                .canonicalize()
                .map_err(|error| format!("cannot resolve input: {error}"))?;
            // Keep the verified bytes stable until the operation is complete.
            // FILE_SHARE_READ permits other readers but denies writes/deletion.
            let file = OpenOptions::new()
                .read(true)
                .share_mode(1)
                .open(&path)
                .map_err(|error| format!("cannot open input read-only: {error}"))?;
            if !file
                .metadata()
                .map_err(|error| error.to_string())?
                .is_file()
            {
                return Err("input must be a regular file".into());
            }
            Ok(Self {
                wide: wide(path.as_os_str())?,
                path,
                file,
            })
        }

        fn handle(&self) -> HANDLE {
            self.file.as_raw_handle().cast()
        }
    }

    struct CatalogAdmin(isize);

    impl CatalogAdmin {
        fn new(algorithm: &str) -> Result<Self> {
            let algorithm = wide(OsStr::new(algorithm))?;
            let mut handle = 0;
            // SAFETY: pointers refer to initialized values for the duration of
            // the call; the returned context is released by Drop.
            if unsafe {
                CryptCATAdminAcquireContext2(
                    &mut handle,
                    &DRIVER_ACTION_VERIFY,
                    algorithm.as_ptr(),
                    null(),
                    0,
                )
            } == 0
            {
                return Err(format!(
                    "catalog context: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self(handle))
        }

        fn hash(&self, input: &Input) -> Result<Vec<u8>> {
            let mut length = 0;
            // The first call reports the required size, including on
            // ERROR_INSUFFICIENT_BUFFER; the second must succeed explicitly.
            unsafe {
                CryptCATAdminCalcHashFromFileHandle2(
                    self.0,
                    input.handle(),
                    &mut length,
                    null_mut(),
                    0,
                );
            }
            if !(1..=128).contains(&length) {
                return Err("invalid catalog member hash size".into());
            }
            let mut hash = vec![0; length as usize];
            let capacity = length;
            if unsafe {
                CryptCATAdminCalcHashFromFileHandle2(
                    self.0,
                    input.handle(),
                    &mut length,
                    hash.as_mut_ptr(),
                    0,
                )
            } == 0
                || length != capacity
            {
                return Err(format!(
                    "catalog member hash: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(hash)
        }
    }

    impl Drop for CatalogAdmin {
        fn drop(&mut self) {
            unsafe {
                CryptCATAdminReleaseContext(self.0, 0);
            }
        }
    }

    fn verify(mut data: WINTRUST_DATA) -> Result<()> {
        data.cbStruct = size_of::<WINTRUST_DATA>() as u32;
        data.dwUIChoice = WTD_UI_NONE;
        data.dwStateAction = WTD_STATEACTION_VERIFY;
        // Do not disable policy checks or revocation, override trust roots, or
        // accept a warning status. Windows applies the driver trust policy.
        data.dwProvFlags = WTD_DISABLE_MD2_MD4;
        let mut policy = DRIVER_ACTION_VERIFY;
        let status = unsafe {
            WinVerifyTrust(
                INVALID_HANDLE_VALUE,
                &mut policy,
                (&mut data as *mut WINTRUST_DATA).cast(),
            )
        };
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        unsafe {
            WinVerifyTrust(
                INVALID_HANDLE_VALUE,
                &mut policy,
                (&mut data as *mut WINTRUST_DATA).cast(),
            );
        }
        if status == 0 {
            Ok(())
        } else {
            Err(format!(
                "Windows driver trust policy rejected input (0x{:08X})",
                status as u32
            ))
        }
    }

    fn verify_embedded(input: &Input) -> Result<()> {
        let mut info = WINTRUST_FILE_INFO {
            cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
            pcwszFilePath: input.wide.as_ptr(),
            hFile: input.handle(),
            ..Default::default()
        };
        verify(WINTRUST_DATA {
            dwUnionChoice: WTD_CHOICE_FILE,
            Anonymous: WINTRUST_DATA_0 { pFile: &mut info },
            ..Default::default()
        })
    }

    fn verify_member(catalog: &Input, member: &Input) -> Result<()> {
        let mut failures = Vec::new();
        // Windows catalogs may index either SHA-256 or legacy SHA-1 member
        // hashes. Neither branch relaxes the catalog signature/driver policy.
        for algorithm in ["SHA256", "SHA1"] {
            let admin = CatalogAdmin::new(algorithm)?;
            let mut hash = admin.hash(member)?;
            let tag: String = hash.iter().map(|byte| format!("{byte:02X}")).collect();
            let tag = wide(OsStr::new(&tag))?;
            let mut info = WINTRUST_CATALOG_INFO {
                cbStruct: size_of::<WINTRUST_CATALOG_INFO>() as u32,
                pcwszCatalogFilePath: catalog.wide.as_ptr(),
                pcwszMemberTag: tag.as_ptr(),
                pcwszMemberFilePath: member.wide.as_ptr(),
                hMemberFile: member.handle(),
                pbCalculatedFileHash: hash.as_mut_ptr(),
                cbCalculatedFileHash: hash.len() as u32,
                hCatAdmin: admin.0,
                ..Default::default()
            };
            match verify(WINTRUST_DATA {
                dwUnionChoice: WTD_CHOICE_CATALOG,
                Anonymous: WINTRUST_DATA_0 {
                    pCatalog: &mut info,
                },
                ..Default::default()
            }) {
                Ok(()) => return Ok(()),
                Err(error) => failures.push(format!("{algorithm}: {error}")),
            }
        }
        Err(failures.join("; "))
    }

    fn valid_catalog_basename(name: &str) -> bool {
        name.strip_prefix("rustcarplay-libusb0-")
            .and_then(|value| value.strip_suffix(".cat"))
            .is_some_and(|value| {
                value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
    }

    fn add_catalog(catalog: &Input) -> Result<String> {
        let name = catalog
            .path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or("invalid catalog name")?;
        if !valid_catalog_basename(name) {
            return Err(
                "catalog registration requires rustcarplay-libusb0-<32 hex digits>.cat".into(),
            );
        }
        if unsafe { IsUserAnAdmin() } == 0 {
            return Err("catalog registration requires an elevated administrator process".into());
        }
        verify_embedded(catalog)?;
        let admin = CatalogAdmin::new("SHA256")?;
        // A NULL selected basename asks Windows to generate a unique name.
        // Unlike supplying a chosen name, this cannot replace another catalog.
        let handle = unsafe { CryptCATAdminAddCatalog(admin.0, catalog.wide.as_ptr(), null(), 0) };
        if handle == 0 {
            return Err(format!(
                "catalog registration: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut info = CATALOG_INFO {
            cbStruct: size_of::<CATALOG_INFO>() as u32,
            ..Default::default()
        };
        let obtained = unsafe { CryptCATCatalogInfoFromContext(handle, &mut info, 0) };
        unsafe {
            CryptCATAdminReleaseCatalogContext(admin.0, handle, 0);
        }
        if obtained == 0 {
            return Err("catalog registered, but Windows did not return its unique name".into());
        }
        let length = info
            .wszCatalogFile
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(info.wszCatalogFile.len());
        let path = PathBuf::from(
            String::from_utf16(&info.wszCatalogFile[..length])
                .map_err(|_| "invalid registered catalog path")?,
        );
        path.file_name()
            .and_then(OsStr::to_str)
            .map(str::to_owned)
            .ok_or_else(|| "invalid registered catalog name".into())
    }

    pub fn run() -> Result<()> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        match args.as_slice() {
            [mode, catalog, member] if mode == "--catalog" => {
                verify_member(&Input::open(catalog)?, &Input::open(member)?)?;
                println!("{{\"driver_catalog_member_verified\":true}}");
            }
            [mode, member] if mode == "--embedded" => {
                verify_embedded(&Input::open(member)?)?;
                println!("{{\"embedded_driver_signature_verified\":true}}");
            }
            [mode, catalog] if mode == "--add-catalog" => {
                let name = add_catalog(&Input::open(catalog)?)?;
                println!(
                    "{}",
                    serde_json::json!({"catalog_registered":true,"catalog_name":name})
                );
            }
            _ => return Err(
                "usage: usb_driver_verify --catalog CAT SYS | --embedded SYS | --add-catalog CAT"
                    .into(),
            ),
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn catalog_registration_requires_our_unique_basename() {
            assert!(valid_catalog_basename(
                "rustcarplay-libusb0-0123456789abcdef0123456789abcdef.cat"
            ));
            for name in [
                "libusb0.cat",
                "rustcarplay-libusb0-.cat",
                "rustcarplay-libusb0-0123456789abcdef0123456789abcdeg.cat",
                "../rustcarplay-libusb0-0123456789abcdef0123456789abcdef.cat",
            ] {
                assert!(!valid_catalog_basename(name));
            }
        }

        #[test]
        fn nul_in_native_path_is_rejected() {
            assert!(wide(OsStr::new("safe\0unsafe")).is_err());
        }
    }
}
