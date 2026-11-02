//! What holds a running download back, as far as the download itself can tell: connections
//! that keep dropping, a disk that cannot keep up, a server that allows few connections.

use super::*;

/// How a running download is doing apart from its speed (see [`Progress::health`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Health {
    /// Connections that failed and were opened again during this download.
    pub reconnects: u64,
    /// Share of the connections' time spent writing to the disk lately, 0 to 1: near 1, the
    /// disk cannot take the data as fast as the network brings it.
    pub disk_wait: f64,
    /// The server refused connections beyond this many during this download.
    pub server_limit: Option<usize>,
}

/// Weight of the newest tick in [`Health::disk_wait`]: about two seconds of memory.
const DISK_WAIT_WEIGHT: f64 = 0.05;

/// Turns the connections writing at each tick into [`Health::disk_wait`].
#[derive(Default)]
pub(crate) struct DiskGauge {
    wait: f64,
}

impl DiskGauge {
    /// Samples the share of the `active` connections that are writing right now.
    pub(crate) fn tick(&mut self, ctx: &Ctx, active: usize) -> f64 {
        self.update(ctx.disk_writes.load(Ordering::Acquire), active)
    }

    fn update(&mut self, writing: usize, active: usize) -> f64 {
        if active > 0 {
            let now = (writing as f64 / active as f64).min(1.0);
            self.wait += (now - self.wait) * DISK_WAIT_WEIGHT;
        }
        self.wait
    }
}

/// Counts a write in progress for as long as it lives, also when its connection is stopped
/// in the middle of it.
struct Writing<'a>(&'a AtomicUsize);

impl Drop for Writing<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Ctx {
    /// Writes `buf` at `offset`, counted as writing meanwhile (see [`DiskGauge`]).
    pub(crate) async fn write_counted(&self, buf: Vec<u8>, offset: u64) -> Result<Vec<u8>> {
        self.disk_writes.fetch_add(1, Ordering::AcqRel);
        let _writing = Writing(&self.disk_writes);
        if let Some(delay) = self.slow_disk {
            tokio::time::sleep(delay).await;
        }
        Ok(self.file.write_at(buf, offset).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauge_follows_the_share_of_connections_writing() {
        let mut g = DiskGauge::default();
        let mut wait = 0.0;
        // Four connections, three of them writing at any time.
        for _ in 0..200 {
            wait = g.update(3, 4);
        }
        assert!((wait - 0.75).abs() < 0.01, "{wait}");
        // A fast disk again: the share falls back within seconds.
        for _ in 0..60 {
            wait = g.update(0, 4);
        }
        assert!(wait < 0.05, "{wait}");
        // No connection: nothing to say, the last share stays.
        assert!((g.update(0, 0) - wait).abs() < f64::EPSILON);
    }
}
