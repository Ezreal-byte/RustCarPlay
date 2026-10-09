// SPDX-License-Identifier: GPL-3.0-only
//! Persistent pairing identity: DPAPI on Windows; private atomic files on Unix.
use anyhow::{Context, ensure};
use carplay_auth::{AirPlayIdentity, AuthError, PairingStore};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
struct State {
    version: u32,
    pairing_id: String,
    seed: [u8; 32],
    peers: BTreeMap<String, [u8; 32]>,
}
pub struct FilePairingStore {
    path: PathBuf,
    state: RwLock<State>,
}

impl FilePairingStore {
    pub fn open(directory: &Path) -> anyhow::Result<(Arc<AirPlayIdentity>, Arc<Self>)> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder.create(directory)?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(directory)?;
        let path = directory.join("pairings.dat");
        let state = if path.exists() {
            let meta = fs::symlink_metadata(&path)?;
            ensure!(
                !meta.file_type().is_symlink() && meta.is_file() && meta.len() <= 1024 * 1024,
                "invalid pairing store file"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                ensure!(
                    meta.permissions().mode() & 0o077 == 0,
                    "pairing file must be private (mode 0600)"
                );
            }
            let bytes = unprotect(&fs::read(&path)?)?;
            let state: State =
                serde_json::from_slice(&bytes).context("invalid pairing store; file preserved")?;
            ensure!(state.version == 1, "unsupported pairing store version");
            state
        } else {
            let identity = AirPlayIdentity::generate()?;
            let state = State {
                version: 1,
                pairing_id: identity.pairing_id.clone(),
                seed: *identity.export_seed(),
                peers: BTreeMap::new(),
            };
            persist(&path, &state)?;
            state
        };
        let identity = Arc::new(AirPlayIdentity::from_seed(
            state.pairing_id.clone(),
            state.seed,
        )?);
        Ok((
            identity,
            Arc::new(Self {
                path,
                state: RwLock::new(state),
            }),
        ))
    }
}

impl PairingStore for FilePairingStore {
    fn get(&self, id: &str) -> carplay_auth::Result<Option<[u8; 32]>> {
        Ok(self
            .state
            .read()
            .map_err(|_| AuthError::Store("lock poisoned".into()))?
            .peers
            .get(id)
            .copied())
    }
    fn save(&self, id: &str, key: [u8; 32]) -> carplay_auth::Result<()> {
        if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
            return Err(AuthError::InvalidInput("pairing identifier"));
        }
        let mut guard = self
            .state
            .write()
            .map_err(|_| AuthError::Store("lock poisoned".into()))?;
        if guard.peers.len() >= 64 && !guard.peers.contains_key(id) {
            return Err(AuthError::Store("pairing limit reached".into()));
        }
        let mut next = guard.clone();
        next.peers.insert(id.into(), key);
        persist(&self.path, &next)
            .map_err(|_| AuthError::Store("could not atomically persist pairing".into()))?;
        *guard = next;
        Ok(())
    }
}

fn persist(path: &Path, state: &State) -> anyhow::Result<()> {
    let plain = Zeroizing::new(serde_json::to_vec(state)?);
    let bytes = protect(&plain)?;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().context("store parent")?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temp.write_all(&bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    #[cfg(unix)]
    fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

#[cfg(not(windows))]
fn protect(data: &[u8]) -> anyhow::Result<Zeroizing<Vec<u8>>> {
    Ok(Zeroizing::new(data.to_vec()))
}
#[cfg(not(windows))]
fn unprotect(data: &[u8]) -> anyhow::Result<Zeroizing<Vec<u8>>> {
    Ok(Zeroizing::new(data.to_vec()))
}
#[cfg(windows)]
fn protect(data: &[u8]) -> anyhow::Result<Zeroizing<Vec<u8>>> {
    dpapi(data, true)
}
#[cfg(windows)]
fn unprotect(data: &[u8]) -> anyhow::Result<Zeroizing<Vec<u8>>> {
    dpapi(data, false)
}
#[cfg(windows)]
fn dpapi(data: &[u8], encrypt: bool) -> anyhow::Result<Zeroizing<Vec<u8>>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len().try_into()?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: input is valid throughout the call; Windows allocates output and LocalFree owns it.
    let ok = unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful DPAPI returned cbData bytes, copied before allocation release.
    let result = unsafe {
        let copy =
            Zeroizing::new(std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec());
        std::ptr::write_bytes(out.pbData, 0, out.cbData as usize);
        LocalFree(out.pbData.cast());
        copy
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_and_pairing_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (id, store) = FilePairingStore::open(dir.path()).unwrap();
        store.save("test-controller", [42; 32]).unwrap();
        let (again, reopened) = FilePairingStore::open(dir.path()).unwrap();
        assert_eq!(id.public_key(), again.public_key());
        assert_eq!(reopened.get("test-controller").unwrap(), Some([42; 32]));
        #[cfg(windows)]
        assert!(
            !String::from_utf8_lossy(&fs::read(dir.path().join("pairings.dat")).unwrap())
                .contains("seed")
        );
    }
    #[test]
    fn corrupt_store_is_preserved_not_reset() {
        let dir = tempfile::tempdir().unwrap();
        FilePairingStore::open(dir.path()).unwrap();
        let file = dir.path().join("pairings.dat");
        fs::write(&file, b"corrupt").unwrap();
        assert!(FilePairingStore::open(dir.path()).is_err());
        assert_eq!(fs::read(file).unwrap(), b"corrupt");
    }
}
