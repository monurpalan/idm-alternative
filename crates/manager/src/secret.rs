//! A download's request headers can carry cookies and passwords (`Cookie`, `Authorization`),
//! so the list keeps them encrypted. On Windows with DPAPI: only this Windows user, on this
//! machine, can read them back. On Linux with a key of the app's kept in the Secret Service
//! (GNOME Keyring, KWallet), the values encrypted with it (AES-256-GCM); without a Secret
//! Service (a server, a bare window manager) they are stored as they are, as before, and the
//! log says so once. The app keeps other secrets (the proxy's password) the same way, with
//! [`seal_setting`] and [`open_setting`].

/// Marks a value encrypted with DPAPI; anything without a mark is plain JSON from before
/// encryption.
const PREFIX: &str = "dpapi:";

/// Marks a value encrypted with the key in the Secret Service: base64 of nonce, then
/// ciphertext with its tag.
const KEYED: &str = "sskey:";

/// Ties the data to this use, so another program running as the same user cannot decrypt
/// it with DPAPI alone. The text is part of the key: changing it makes the saved headers
/// unreadable, so it keeps the name the app had when they were first saved. (With the
/// Secret Service key it is the associated data, for the same reason.)
const ENTROPY: &[u8] = b"idm-alternative request headers";

/// The same for secrets in the app's settings.
const SETTINGS: &[u8] = b"shardload settings";

/// The stored form of `plain`.
pub(crate) fn seal(plain: &str) -> String {
    seal_with(plain, ENTROPY)
}

/// The plain text of a stored value; `None` when it cannot be decrypted (the list was copied
/// from another user or machine).
pub(crate) fn open(stored: &str) -> Option<String> {
    open_with(stored, ENTROPY)
}

/// The stored form of a secret in the settings, e.g. a proxy address with its password.
pub fn seal_setting(plain: &str) -> String {
    seal_with(plain, SETTINGS)
}

/// The plain text of a secret saved with [`seal_setting`]; `None` when it cannot be decrypted.
/// Text that was never encrypted comes back as it is.
pub fn open_setting(stored: &str) -> Option<String> {
    open_with(stored, SETTINGS)
}

fn seal_with(plain: &str, entropy: &[u8]) -> String {
    #[cfg(windows)]
    match dpapi::protect(plain.as_bytes(), entropy) {
        Ok(sealed) => {
            use base64::Engine as _;
            return format!(
                "{PREFIX}{}",
                base64::engine::general_purpose::STANDARD.encode(sealed)
            );
        }
        Err(e) => tracing::warn!("could not encrypt a secret: {e}"),
    }
    #[cfg(target_os = "linux")]
    if let Some(key) = keyring_key::get() {
        return keyed::seal(plain, entropy, key);
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    let _ = entropy;
    plain.to_owned()
}

fn open_with(stored: &str, entropy: &[u8]) -> Option<String> {
    if stored.starts_with(KEYED) {
        #[cfg(target_os = "linux")]
        return keyed::open(stored, entropy, keyring_key::get()?);
        #[cfg(not(target_os = "linux"))]
        return None;
    }
    let Some(encoded) = stored.strip_prefix(PREFIX) else {
        return Some(stored.to_owned());
    };
    #[cfg(windows)]
    {
        use base64::Engine as _;
        let sealed = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let plain = dpapi::unprotect(&sealed, entropy).ok()?;
        String::from_utf8(plain).ok()
    }
    #[cfg(not(windows))]
    {
        let _ = (encoded, entropy);
        None
    }
}

/// Whether a stored value is already encrypted.
pub(crate) fn is_sealed(stored: &str) -> bool {
    stored.starts_with(PREFIX) || stored.starts_with(KEYED)
}

/// AES-256-GCM with a key of the app's (see [`keyring_key`]); `entropy` is the associated
/// data, so headers and settings cannot open each other's values.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod keyed {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes256Gcm, Nonce};
    use base64::Engine as _;

    pub fn seal(plain: &str, entropy: &[u8], key: &[u8; 32]) -> String {
        let mut nonce = [0u8; 12];
        if getrandom::fill(&mut nonce).is_err() {
            tracing::warn!("no random numbers for a secret's nonce; kept as it is");
            return plain.to_owned();
        }
        let cipher = Aes256Gcm::new(key.into());
        let Ok(sealed) = cipher.encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plain.as_bytes(),
                aad: entropy,
            },
        ) else {
            return plain.to_owned();
        };
        let mut out = nonce.to_vec();
        out.extend_from_slice(&sealed);
        format!(
            "{}{}",
            super::KEYED,
            base64::engine::general_purpose::STANDARD.encode(out)
        )
    }

    pub fn open(stored: &str, entropy: &[u8], key: &[u8; 32]) -> Option<String> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(stored.strip_prefix(super::KEYED)?)
            .ok()?;
        if raw.len() < 12 {
            return None;
        }
        let (nonce, sealed) = raw.split_at(12);
        let plain = Aes256Gcm::new(key.into())
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: sealed,
                    aad: entropy,
                },
            )
            .ok()?;
        String::from_utf8(plain).ok()
    }
}

/// The app's key in the Secret Service: read once, made on first use, `None` (and said once)
/// when there is no Secret Service to keep it.
#[cfg(target_os = "linux")]
mod keyring_key {
    use std::sync::OnceLock;

    const SERVICE: &str = "io.github.monurpalan.shardload";
    const ACCOUNT: &str = "secrets-key";

    pub fn get() -> Option<&'static [u8; 32]> {
        static KEY: OnceLock<Option<[u8; 32]>> = OnceLock::new();
        KEY.get_or_init(|| match load() {
            Ok(key) => Some(key),
            Err(e) => {
                tracing::info!(
                    "no Secret Service ({e}): cookies and passwords are stored unencrypted"
                );
                None
            }
        })
        .as_ref()
    }

    fn load() -> Result<[u8; 32], keyring::Error> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT)?;
        match entry.get_secret() {
            Ok(bytes) if bytes.len() == 32 => {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                Ok(key)
            }
            // None yet (or not one of ours): a new one.
            Ok(_) | Err(keyring::Error::NoEntry) => {
                let mut key = [0u8; 32];
                getrandom::fill(&mut key)
                    .map_err(|e| keyring::Error::PlatformFailure(Box::new(e)))?;
                entry.set_secret(&key)?;
                Ok(key)
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr().cast_mut(),
        }
    }

    /// Copies DPAPI's output and frees it.
    ///
    /// # Safety
    /// `out` must hold a buffer DPAPI allocated with `LocalAlloc`.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: DPAPI filled `out` with `cbData` readable bytes, freed right after copying.
        unsafe {
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(out.pbData.cast());
            v
        }
    }

    pub fn protect(data: &[u8], entropy: &[u8]) -> std::io::Result<Vec<u8>> {
        let input = blob(data);
        let extra = blob(entropy);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: the input blobs point at live slices DPAPI only reads; `out` receives a
        // buffer that `take` frees.
        unsafe {
            if CryptProtectData(
                &input,
                std::ptr::null(),
                &extra,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(take(out))
        }
    }

    pub fn unprotect(data: &[u8], entropy: &[u8]) -> std::io::Result<Vec<u8>> {
        let input = blob(data);
        let extra = blob(entropy);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: as in `protect`.
        unsafe {
            if CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &extra,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(take(out))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let plain = r#"[["Cookie","SID=abc"],["Authorization","Basic dTpw"]]"#;
        let stored = seal(plain);
        #[cfg(windows)]
        {
            assert!(is_sealed(&stored));
            assert!(!stored.contains("SID=abc"));
        }
        assert_eq!(open(&stored).as_deref(), Some(plain));
        // Rows from before encryption read as they are.
        assert_eq!(open("[]").as_deref(), Some("[]"));
        // Damaged or foreign data does not decrypt.
        assert_eq!(open("dpapi:AAAA"), None);
    }

    #[test]
    fn settings_use_their_own_key() {
        let proxy = "http://user:pa55@127.0.0.1:8080";
        let stored = seal_setting(proxy);
        assert_eq!(open_setting(&stored).as_deref(), Some(proxy));
        #[cfg(windows)]
        {
            assert!(!stored.contains("pa55"));
            // Headers and settings cannot open each other's secrets.
            assert_eq!(open(&stored), None);
            assert_eq!(open_setting(&seal(proxy)), None);
        }
    }

    #[test]
    fn keyed_values() {
        let key = [7u8; 32];
        let plain = r#"[["Cookie","SID=abc"]]"#;
        let stored = keyed::seal(plain, ENTROPY, &key);
        assert!(stored.starts_with(KEYED) && is_sealed(&stored));
        assert!(!stored.contains("SID"));
        assert_eq!(keyed::open(&stored, ENTROPY, &key).as_deref(), Some(plain));
        // A fresh nonce each time.
        assert_ne!(stored, keyed::seal(plain, ENTROPY, &key));
        // Another key, another use (settings), or damage: nothing.
        assert_eq!(keyed::open(&stored, ENTROPY, &[8u8; 32]), None);
        assert_eq!(keyed::open(&stored, SETTINGS, &key), None);
        let mut damaged = stored.clone();
        damaged.pop();
        damaged.push(if stored.ends_with('A') { 'B' } else { 'A' });
        assert_eq!(keyed::open(&damaged, ENTROPY, &key), None);
        assert_eq!(keyed::open("sskey:AAAA", ENTROPY, &key), None);
    }

    /// Without a Secret Service (a CI machine, a server) secrets stay as they are, and a value
    /// encrypted on another machine's keyring cannot be read.
    #[cfg(target_os = "linux")]
    #[test]
    fn without_a_secret_service_values_stay_plain() {
        if keyring_key::get().is_some() {
            return; // this machine has one: `round_trip` covers it
        }
        assert_eq!(seal("a=1"), "a=1");
        assert_eq!(open("a=1").as_deref(), Some("a=1"));
        let elsewhere = keyed::seal("a=1", ENTROPY, &[1u8; 32]);
        assert_eq!(open(&elsewhere), None);
    }
}
