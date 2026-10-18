//! When a failed download is tried again. A failure that may pass by itself (a busy server, a
//! dropped connection) gets a few attempts, with growing waits. A machine with no route to the
//! internet is not a failing server: its downloads wait, checking again every few seconds, and
//! use no attempt, so they carry on as soon as the connection is back.

use std::time::Duration;

use idma_engine::Error as E;

/// Longest wait between automatic retries.
pub(crate) const MAX_DELAY: Duration = Duration::from_secs(300);

/// How long a download waits, while the machine has no route to the internet, before it
/// checks again.
pub(crate) const OFFLINE_DELAY: Duration = Duration::from_secs(15);

/// Whether the machine has a route to the internet. Connecting a UDP socket sends nothing;
/// it only asks the system which network to use, and that fails when no network is up.
pub(crate) fn has_route() -> bool {
    ["1.1.1.1:443", "[2606:4700:4700::1111]:443"]
        .iter()
        .any(|addr| connects(addr))
}

fn connects(addr: &str) -> bool {
    let local = if addr.starts_with('[') {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    std::net::UdpSocket::bind(local)
        .and_then(|socket| socket.connect(addr))
        .is_ok()
}

/// A failure of the network itself (no answer, a dropped connection), not of the server.
fn network_failure(e: &E) -> bool {
    matches!(
        e,
        E::Http(_) | E::TooManyFailures { .. } | E::Truncated | E::Stalled(_)
    )
}

/// Whether `e` is a network failure while the machine has no route to the internet.
pub(crate) fn offline(e: &E) -> bool {
    network_failure(e) && !has_route()
}

/// Where a failed download goes next, given the attempts it has used: the attempts after this
/// failure and how long to wait, or `None` when it stops. An offline failure keeps the attempts
/// it has and waits [`OFFLINE_DELAY`].
pub(crate) fn plan(
    previous: u32,
    max: u32,
    base: Duration,
    retryable: bool,
    offline: bool,
) -> Option<(u32, Duration)> {
    if !retryable {
        None
    } else if offline {
        Some((previous, OFFLINE_DELAY))
    } else if previous >= max {
        None
    } else {
        let attempts = previous + 1;
        let delay = base
            .saturating_mul(1 << (attempts - 1).min(16))
            .min(MAX_DELAY);
        Some((attempts, delay))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_server_gets_five_attempts_with_doubling_waits() {
        let base = Duration::from_secs(5);
        let mut previous = 0;
        let mut waits = Vec::new();
        while let Some((attempts, wait)) = plan(previous, 5, base, true, false) {
            previous = attempts;
            waits.push(wait.as_secs());
        }
        assert_eq!(waits, [5, 10, 20, 40, 80]);
        assert_eq!(previous, 5);
    }

    #[test]
    fn waits_stop_growing_at_the_cap() {
        let (_, wait) = plan(9, 20, Duration::from_secs(5), true, false).expect("an attempt");
        assert_eq!(wait, MAX_DELAY);
    }

    #[test]
    fn an_offline_failure_waits_without_using_an_attempt() {
        let base = Duration::from_secs(5);
        assert_eq!(plan(0, 5, base, true, true), Some((0, OFFLINE_DELAY)));
        // Still waits once the attempts are used up.
        assert_eq!(plan(5, 5, base, true, true), Some((5, OFFLINE_DELAY)));
    }

    #[test]
    fn other_failures_stop_at_once() {
        let base = Duration::from_secs(5);
        assert_eq!(plan(0, 5, base, false, false), None);
        assert_eq!(plan(0, 5, base, false, true), None);
    }

    #[test]
    fn network_failures_are_the_ones_without_an_answer() {
        assert!(network_failure(&E::Truncated));
        assert!(network_failure(&E::Stalled(Duration::from_secs(1))));
        assert!(network_failure(&E::TooManyFailures {
            attempts: 3,
            last: "connection refused".to_owned(),
        }));
        assert!(!network_failure(&E::Status(404)));
        assert!(!network_failure(&E::DiskFull {
            need: None,
            free: None,
        }));
    }
}
