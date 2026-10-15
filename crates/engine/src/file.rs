//! The output file: one file, preallocated, written concurrently at offsets.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::Arc;

/// Shared handle to the partial download. Writes are positional, so every connection
/// writes straight to its own region without seeking or coordinating with the others.
#[derive(Clone)]
pub struct OutputFile {
    file: Arc<File>,
}

impl OutputFile {
    /// Opens (or creates) the partial file. When `size` is known the file is made sparse
    /// and extended to full length, so writes far into the file do not force the OS to
    /// zero-fill everything before them first (NTFS does that for non-sparse files).
    pub fn open(path: &Path, size: Option<u64>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        if let Some(size) = size
            && file.metadata()?.len() != size
        {
            make_sparse(&file);
            file.set_len(size)?;
        }
        Ok(Self {
            file: Arc::new(file),
        })
    }

    /// Writes all of `buf` at `offset` on a blocking thread.
    pub async fn write_at(&self, buf: Vec<u8>, offset: u64) -> io::Result<Vec<u8>> {
        let file = Arc::clone(&self.file);
        tokio::task::spawn_blocking(move || write_all_at(&file, &buf, offset).map(|()| buf))
            .await
            .map_err(io::Error::other)?
    }

    /// Flushes written data to disk.
    pub async fn sync(&self) -> io::Result<()> {
        let file = Arc::clone(&self.file);
        tokio::task::spawn_blocking(move || file.sync_data())
            .await
            .map_err(io::Error::other)?
    }

    /// Sets the final length (used when the size was not known in advance).
    pub fn set_len(&self, len: u64) -> io::Result<()> {
        self.file.set_len(len)
    }
}

#[cfg(windows)]
fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = file.seek_write(buf, offset)?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buf, offset)
}

/// Marks the file sparse. Best effort: filesystems without sparse support (FAT, exFAT)
/// just fall back to a normal file.
#[cfg(windows)]
#[allow(unsafe_code)]
fn make_sparse(file: &File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;

    let mut returned = 0u32;
    // SAFETY: the handle is owned by `file` and valid for the duration of the call;
    // FSCTL_SET_SPARSE takes no input or output buffer, and the call is synchronous.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        tracing::debug!("could not mark file sparse: {}", io::Error::last_os_error());
    }
}

#[cfg(not(windows))]
fn make_sparse(_file: &File) {
    // `set_len` already creates sparse files on common Unix filesystems.
}
