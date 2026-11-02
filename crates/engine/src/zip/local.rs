//! A ZIP archive on disk: a finished download unpacked into a folder next to it. Its index is
//! read the same way as a remote one's, from the end of the file.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

/// A ZIP archive on disk, with its entries read.
pub struct LocalZip {
    file: File,
    size: u64,
    entries: Vec<ZipEntry>,
    /// Where the list of files starts and ends, with its end record.
    central: (u64, u64),
}

impl LocalZip {
    /// Reads the archive's index. The file is untrusted: every record is checked against the
    /// file's size before it is read, and a list of files over [`MAX_CENTRAL`] is refused.
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let size = file.metadata()?.len();
        let tail_start = size.saturating_sub(TAIL);
        let tail = read_at(&mut file, size, tail_start, (size - tail_start) as usize)?;
        let mut end = parse_end(&tail, tail_start).map_err(Error::Archive)?;
        if let Some(record) = end.locator {
            let at_locator = end.at - LOCATOR_LEN as u64;
            let found = [record, at_locator.saturating_sub(EOCD64_LEN as u64)]
                .into_iter()
                .find_map(|at| {
                    let bytes = read_at(&mut file, size, at, EOCD64_LEN).ok()?;
                    parse_end64(&bytes).ok().map(|e| End { at, ..e })
                });
            end = found.ok_or_else(|| {
                Error::Archive("the ZIP64 end of central directory is missing".into())
            })?;
        }
        let shift = end
            .at
            .checked_sub(end.central_offset + end.central_size)
            .ok_or_else(|| Error::Archive("the central directory overlaps its end".into()))?;
        if end.central_size > MAX_CENTRAL {
            return Err(Error::Archive(format!(
                "the list of files is too large ({} MiB)",
                end.central_size >> 20
            )));
        }
        let central = read_at(
            &mut file,
            size,
            end.central_offset + shift,
            end.central_size as usize,
        )?;
        let entries = parse_central(&central, end.entries, shift).map_err(Error::Archive)?;
        Ok(Self {
            file,
            size,
            entries,
            central: (end.central_offset + shift, size),
        })
    }

    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// Where the list of files starts, to the end of the file.
    pub(crate) fn central(&self) -> (u64, u64) {
        self.central
    }

    /// Unpacks every entry into `out_dir`, keeping their folders. Entries that cannot be
    /// unpacked (unsupported, encrypted, unsafe name, damaged, a file already in the way) are
    /// reported and the rest go on; a disk failure, or `stop` set, ends the whole call.
    pub fn unpack(&mut self, out_dir: &Path, stop: &AtomicBool) -> Result<Vec<Extracted>> {
        let mut entries = self.entries.clone();
        entries.sort_by_key(|e| e.header_offset);
        let mut results = Vec::with_capacity(entries.len());
        for e in &entries {
            if stop.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let path = safe_path(&e.name).map(|p| out_dir.join(p));
            let mut result = Extracted {
                name: e.name.clone(),
                path: path.clone(),
                size: e.size,
                skipped: false,
                error: None,
            };
            result.error = match path {
                None => Some("unsafe name: it would be written outside the folder".into()),
                Some(path) if e.is_dir => {
                    std::fs::create_dir_all(&path)?;
                    None
                }
                Some(_) if e.unsupported().is_some() => e.unsupported(),
                Some(path) if path.exists() => Some("a file is already there".into()),
                Some(path) => self.unpack_entry(e, &path)?.err(),
            };
            results.push(result);
        }
        Ok(results)
    }

    /// Unpacks one entry into `path` (through `<path>.shardload-part`). The outer error ends
    /// the whole call; the inner one is this entry's problem only.
    fn unpack_entry(
        &mut self,
        e: &ZipEntry,
        path: &Path,
    ) -> Result<std::result::Result<(), String>> {
        let Some(data_at) = self.data_start(e)? else {
            return Ok(Err("its local header is missing".into()));
        };
        if data_at + e.compressed_size > self.size {
            return Ok(Err("its data runs past the end of the file".into()));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let part = crate::state::sibling(path, ".shardload-part");
        let mut sink = Sink::new(e.method, File::create(&part)?, e.size);
        self.file.seek(SeekFrom::Start(data_at))?;
        let mut data = (&mut self.file).take(e.compressed_size);
        let mut buf = vec![0u8; 256 << 10];
        loop {
            let n = data.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if let Err(err) = sink.write(&buf[..n]) {
                return damaged(err, &part);
            }
        }
        let (written, crc) = match sink.finish() {
            Ok(done) => done,
            Err(err) => return damaged(err, &part),
        };
        let wrong = if written != e.size {
            Some(format!("unpacked to {written} bytes instead of {}", e.size))
        } else if crc != e.crc32 {
            Some(format!(
                "CRC-32 mismatch: expected {:08x}, got {crc:08x}",
                e.crc32
            ))
        } else {
            None
        };
        if let Some(why) = wrong {
            std::fs::remove_file(&part).ok();
            return Ok(Err(why));
        }
        std::fs::rename(&part, path)?;
        Ok(Ok(()))
    }

    /// Unpacks every entry it can into nothing, checking each one's size and CRC-32. The byte
    /// ranges of the entries that fail (local header to the end of the data); encrypted
    /// entries and compression methods it cannot unpack are left out, not called damaged.
    /// `go_on` is asked before each entry; its error ends the check.
    pub(crate) fn damaged_entries(
        &mut self,
        go_on: &dyn Fn() -> Result<()>,
    ) -> Result<Vec<(u64, u64)>> {
        let mut entries = self.entries.clone();
        entries.sort_by_key(|e| e.header_offset);
        let mut damaged = Vec::new();
        for e in entries
            .iter()
            .filter(|e| !e.is_dir && e.unsupported().is_none())
        {
            go_on()?;
            let header_end = (e.header_offset + LOCAL_LEN as u64).min(self.size);
            let size = self.size;
            let range = |end: u64| (e.header_offset, end.clamp(header_end, size));
            let Some(data_at) = self.data_start(e)? else {
                damaged.push(range(header_end));
                continue;
            };
            let end = data_at.saturating_add(e.compressed_size);
            if end > self.size || !self.entry_is_sound(e, data_at)? {
                damaged.push(range(end));
            }
        }
        Ok(damaged)
    }

    /// Whether the entry's data unpacks to its size and CRC-32.
    fn entry_is_sound(&mut self, e: &ZipEntry, data_at: u64) -> Result<bool> {
        let mut sink = Sink::new(e.method, std::io::sink(), e.size);
        self.file.seek(SeekFrom::Start(data_at))?;
        let mut data = (&mut self.file).take(e.compressed_size);
        let mut buf = vec![0u8; 256 << 10];
        loop {
            let n = data.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if sink.write(&buf[..n]).is_err() {
                return Ok(false);
            }
        }
        Ok(matches!(sink.finish(), Ok((written, crc)) if written == e.size && crc == e.crc32))
    }

    /// Where an entry's data starts, after its local header; `None` without one.
    pub(crate) fn data_start(&mut self, e: &ZipEntry) -> Result<Option<u64>> {
        if e.header_offset + LOCAL_LEN as u64 > self.size {
            return Ok(None);
        }
        let header = read_at(&mut self.file, self.size, e.header_offset, LOCAL_LEN)?;
        if u32_at(&header, 0) != LOCAL_SIG {
            return Ok(None);
        }
        let extra = u64::from(u16_at(&header, 26)) + u64::from(u16_at(&header, 28));
        Ok(Some(e.header_offset + LOCAL_LEN as u64 + extra))
    }
}

/// `len` bytes at `at`, or [`Error::Archive`] when they are not all in the file.
fn read_at(file: &mut File, size: u64, at: u64, len: usize) -> Result<Vec<u8>> {
    if at.checked_add(len as u64).is_none_or(|end| end > size) {
        return Err(Error::Archive(
            "a record points past the end of the file".into(),
        ));
    }
    file.seek(SeekFrom::Start(at))?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shardload_testserver::zip::{DEFLATE, Entry, STORED, write};

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join("shardload-engine-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).expect("dir");
        d
    }

    #[test]
    fn unpacks_an_archive_on_disk() {
        let d = dir("local-zip");
        let entries = vec![
            Entry::new("a.txt", "hello", STORED),
            Entry::dir("sub/"),
            Entry::new("sub/b.txt", "hello hello ".repeat(500), DEFLATE),
            Entry {
                descriptor: true,
                ..Entry::new("sub/c.bin", vec![7u8; 5000], DEFLATE)
            },
            Entry::new("../../evil.txt", "x", STORED),
        ];
        for zip64 in [false, true] {
            let archive = d.join(format!("a{zip64}.zip"));
            std::fs::write(&archive, write(&entries, zip64, b"MZ program", b"")).expect("write");
            let out = d.join(format!("out{zip64}"));
            let mut zip = LocalZip::open(&archive).expect("open");
            assert_eq!(zip.entries().len(), 5);
            let results = zip.unpack(&out, &AtomicBool::new(false)).expect("unpack");
            let errors: Vec<_> = results.iter().filter_map(|r| r.error.as_deref()).collect();
            assert_eq!(
                errors,
                ["unsafe name: it would be written outside the folder"]
            );
            assert_eq!(std::fs::read(out.join("a.txt")).expect("a"), b"hello");
            let b = std::fs::read_to_string(out.join("sub/b.txt")).expect("b");
            assert_eq!(b, "hello hello ".repeat(500));
            assert_eq!(
                std::fs::read(out.join("sub/c.bin")).expect("c"),
                [7u8; 5000]
            );
            assert!(!d.join("evil.txt").exists());
            // Again: what is there already is left alone and said.
            let again = zip.unpack(&out, &AtomicBool::new(false)).expect("again");
            assert!(
                again
                    .iter()
                    .filter(|r| !r.name.ends_with('/'))
                    .all(|r| r.error.is_some())
            );
        }
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn damaged_archives_are_refused_or_reported() {
        let d = dir("local-zip-bad");
        let good = write(
            &[Entry::new("x.bin", vec![1u8; 4000], DEFLATE)],
            false,
            &[],
            &[],
        );
        // Not an archive; cut short; a flipped byte in the data.
        std::fs::write(d.join("text.zip"), b"just some text").expect("write");
        assert!(matches!(
            LocalZip::open(&d.join("text.zip")),
            Err(Error::Archive(_))
        ));
        std::fs::write(d.join("cut.zip"), &good[..good.len() - 10]).expect("write");
        assert!(LocalZip::open(&d.join("cut.zip")).is_err());
        let mut flipped = good.clone();
        flipped[40] ^= 0xFF;
        std::fs::write(d.join("flip.zip"), &flipped).expect("write");
        let mut zip = LocalZip::open(&d.join("flip.zip")).expect("index is intact");
        let r = zip
            .unpack(&d.join("out"), &AtomicBool::new(false))
            .expect("unpack");
        assert!(r[0].error.is_some(), "{r:?}");
        assert!(!d.join("out/x.bin").exists());
        assert!(!d.join("out/x.bin.shardload-part").exists());
        std::fs::remove_dir_all(&d).ok();
    }
}
