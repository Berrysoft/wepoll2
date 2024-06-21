use std::time::{Duration, Instant};

use wepoll2::Poller;

#[test]
fn below_ms() {
    let poller = Poller::new().unwrap();
    let mut events = Vec::with_capacity(1);

    let dur = Duration::from_micros(100);
    let margin = Duration::from_micros(500);
    let mut lowest = Duration::from_secs(1000);

    for _ in 0..1_000 {
        let now = Instant::now();
        let n = poller
            .wait(events.spare_capacity_mut(), Some(dur), false)
            .unwrap();
        let elapsed = now.elapsed();

        assert_eq!(n, 0);
        assert!(elapsed >= dur, "{:?} < {:?}", elapsed, dur);
        lowest = lowest.min(elapsed);
    }

    assert!(lowest < dur + margin, "{:?} >= {:?}", lowest, dur + margin);
}

#[test]
fn above_ms() {
    let poller = Poller::new().unwrap();
    let mut events = Vec::with_capacity(1);

    let dur = Duration::from_millis(3);
    let margin = Duration::from_micros(500);
    let mut lowest = Duration::from_secs(1000);

    for _ in 0..1_000 {
        let now = Instant::now();
        let n = poller
            .wait(events.spare_capacity_mut(), Some(dur), false)
            .unwrap();
        let elapsed = now.elapsed();

        assert_eq!(n, 0);
        assert!(
            elapsed >= dur - margin,
            "{:?} < {:?}",
            elapsed,
            dur - margin
        );
        lowest = lowest.min(elapsed);
    }

    assert!(lowest < dur + margin, "{:?} >= {:?}", lowest, dur + margin);
}
