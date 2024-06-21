use std::time::{Duration, Instant};

use wepoll2::Poller;

#[test]
fn twice() {
    let poller = Poller::new().unwrap();
    let mut events = Vec::with_capacity(1);
    let dur = Duration::from_secs(1);
    let margin = Duration::from_millis(10);

    for _ in 0..2 {
        let start = Instant::now();
        poller
            .wait(events.spare_capacity_mut(), Some(dur), false)
            .unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed >= dur - margin,
            "{:?} < {:?}",
            elapsed,
            dur - margin
        );
    }
}

#[test]
fn non_blocking() {
    let poller = Poller::new().unwrap();
    let mut events = Vec::with_capacity(1);

    for _ in 0..100 {
        poller
            .wait(
                events.spare_capacity_mut(),
                Some(Duration::from_secs(0)),
                false,
            )
            .unwrap();
    }
}
