//! Where an entry is unpacked to: a file, written in large chunks, counted and hashed
//! while it goes, with progress for the UI.

use super::*;

/// Unpacks into a file while counting and hashing what comes out.
pub(crate) enum Sink {
    Stored(Out),
    Deflate(Box<flate2::write::DeflateDecoder<Out>>),
}

impl Sink {
    pub(crate) fn new(method: u16, file: std::fs::File, limit: u64) -> Self {
        let out = Out {
            file: std::io::BufWriter::with_capacity(WRITE_BUFFER, file),
            crc: crc32fast::Hasher::new(),
            written: 0,
            limit,
        };
        if method == 8 {
            Sink::Deflate(Box::new(flate2::write::DeflateDecoder::new(out)))
        } else {
            Sink::Stored(out)
        }
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match self {
            Sink::Stored(o) => o.write_all(bytes),
            Sink::Deflate(d) => d.write_all(bytes),
        }
    }

    /// Bytes written and their CRC-32.
    pub(crate) fn finish(self) -> std::io::Result<(u64, u32)> {
        let mut out = match self {
            Sink::Stored(o) => o,
            Sink::Deflate(d) => d.finish()?,
        };
        out.file.flush()?;
        Ok((out.written, out.crc.finalize()))
    }
}

pub(crate) struct Out {
    pub(crate) file: std::io::BufWriter<std::fs::File>,
    pub(crate) crc: crc32fast::Hasher,
    pub(crate) written: u64,
    /// The size the archive gives: more output is damaged data (or a decompression bomb).
    pub(crate) limit: u64,
}

impl Write for Out {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written + buf.len() as u64 > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "more data than the archive says",
            ));
        }
        let n = self.file.write(buf)?;
        self.crc.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Progress over all chosen entries.
pub(crate) struct Meter<'a> {
    pub(crate) total: u64,
    pub(crate) received: u64,
    pub(crate) progress: Option<&'a watch::Sender<Progress>>,
    pub(crate) sample: (Instant, u64),
    pub(crate) speed: f64,
}

impl<'a> Meter<'a> {
    pub(crate) fn new(total: u64, progress: Option<&'a watch::Sender<Progress>>) -> Self {
        let m = Self {
            total,
            received: 0,
            progress,
            sample: (Instant::now(), 0),
            speed: 0.0,
        };
        m.send(Phase::Downloading);
        m
    }

    pub(crate) fn add(&mut self, n: u64) {
        self.received += n;
        let elapsed = self.sample.0.elapsed();
        if elapsed >= Duration::from_millis(500) {
            self.speed = self.received.saturating_sub(self.sample.1) as f64 / elapsed.as_secs_f64();
            self.sample = (Instant::now(), self.received);
        }
        self.send(Phase::Downloading);
    }

    pub(crate) fn finish(&mut self) {
        self.speed = 0.0;
        self.send(Phase::Done);
    }

    pub(crate) fn send(&self, phase: Phase) {
        if let Some(p) = self.progress {
            p.send_modify(|p| {
                p.phase = phase;
                p.size = Some(self.total);
                p.received = self.received;
                p.speed = self.speed;
                p.connections = usize::from(phase == Phase::Downloading);
            });
        }
    }
}
