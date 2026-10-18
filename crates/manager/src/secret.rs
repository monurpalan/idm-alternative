//! A download's request headers can carry cookies and passwords (`Cookie`, `Authorization`),
//! so the list keeps them encrypted with Windows DPAPI: only this Windows user, on this
//! machine, can read them back. Elsewhere they are stored as they are.

/// Marks an encrypted value; anything else is plain JSON from before encryption.
const PREFIX: &str = "dpapi:";

/// Ties the data to this use, so another program running as the same user cannot decrypt
/// it with DPAPI alone. The text is part of the key: changing it makes the saved headers
/// unreadable, so it keeps the name the app had when they were first saved.
#[cfg(windows)]
const ENTROPY: &[u8] = b"idm-alternative request headers";

/// The stored form of `plain`.
pub(crate) fn seal(plain: &str) -> String {
    #[cfg(windows)]
    match dpapi::protect(plain.as_bytes(), ENTROPY) {
        Ok(sealed) => {
            use base64::Engine as _;
            return format!(
                "{PREFIX}{}",
                base64::engine::general_purpose::STANDARD.encode(sealed)
            );
        }
        Err(e) => tracing::warn!("could not encrypt request headers: {e}"),
    }
    plain.to_owned()
}

/// The plain text of a stored value; `None` when it cannot be decrypted (the list was copied
/// from another user or machine).
pub(crate) fn open(stored: &str) -> Option<String> {
    let Some(encoded) = stored.strip_prefix(PREFIX) else {
        return Some(stored.to_owned());
    };
    #[cfg(windows)]
    {
        use base64::Engine as _;
        let sealed = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let plain = dpapi::unprotect(&sealed, ENTROPY).ok()?;
        String::from_utf8(plain).ok()
    }
    #[cfg(not(windows))]
    {
        let _ = encoded;
        None
    }
}

/// Whether a stored value is already encrypted.
pub(crate) fn is_sealed(stored: &str) -> bool {
    stored.starts_with(PREFIX)
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
}
