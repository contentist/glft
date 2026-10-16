//! The device's key pair. Its public half is the device's identity.

use std::{
    fs,
    io::{ErrorKind as IoKind, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

use iroh::SecretKey;

use crate::error::{Error, ErrorKind};

const KEY_FILE: &str = "secret.key";

/// Loads the key pair from `dir`, creating it on first run.
///
/// The file is readable by its owner only. Returns the key and whether it was just created.
pub fn load_or_create(dir: &Path) -> Result<(SecretKey, bool), Error> {
    let path = dir.join(KEY_FILE);
    match fs::read(&path) {
        Ok(bytes) => {
            let bytes: [u8; 32] = bytes
                .try_into()
                .map_err(|_| Error::new(ErrorKind::KeyFileDamaged, path.display().to_string()))?;
            Ok((SecretKey::from_bytes(&bytes), false))
        }
        Err(e) if e.kind() == IoKind::NotFound => {
            fs::create_dir_all(dir).map_err(|e| Error::io(ErrorKind::DataDirUnwritable, dir, e))?;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                .map_err(|e| Error::io(ErrorKind::DataDirUnwritable, dir, e))?;
            let key = SecretKey::generate();
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .map_err(|e| Error::io(ErrorKind::DataDirUnwritable, &path, e))?;
            file.write_all(&key.to_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|e| Error::io(ErrorKind::DataDirUnwritable, &path, e))?;
            Ok((key, true))
        }
        Err(e) => Err(Error::io(ErrorKind::DataDirUnwritable, &path, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_once_then_loads_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let (a, created) = load_or_create(dir.path()).unwrap();
        assert!(created);
        let (b, created) = load_or_create(dir.path()).unwrap();
        assert!(!created);
        assert_eq!(a.public(), b.public());
        let mode = fs::metadata(dir.path().join(KEY_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
