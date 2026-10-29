//! #5082 allocation accounting for [`CappedBuf`]: injected small,
//! non-power-of-two caps, chunks crossing every growth boundary, refusal.

use super::*;

/// Appends `chunk`-byte pieces until refused; checks the invariants after
/// every call and returns the buffer.
fn fill_in_chunks(limit: usize, chunk: usize) -> CappedBuf {
    let mut buf = CappedBuf::new(limit);
    let piece = vec![0xA5_u8; chunk];
    loop {
        let before_len = buf.len();
        let before_cap = buf.capacity();
        match buf.extend(&piece) {
            Ok(()) => assert_eq!(buf.len(), before_len + chunk),
            Err(BufError::Limit { attempted }) => {
                assert_eq!(attempted, before_len + chunk);
                assert!(attempted > limit);
                assert_eq!(buf.len(), before_len, "a refused append copies nothing");
                assert_eq!(
                    buf.capacity(),
                    before_cap,
                    "a refused append reserves nothing"
                );
                return buf;
            }
            Err(BufError::Alloc) => panic!("allocation refused in a small test"),
        }
        assert!(
            buf.capacity() <= limit,
            "capacity {} > limit {limit}",
            buf.capacity()
        );
        assert!(buf.peak() < 2 * limit, "peak {} >= 2 * {limit}", buf.peak());
    }
}

#[test]
fn capacity_never_exceeds_a_small_non_power_of_two_limit() {
    for limit in [1_usize, 7, 4095, 4097, 10_007, 65_537, 100_003] {
        for chunk in [1_usize, 3, 1000, 4096, 8191] {
            let buf = fill_in_chunks(limit, chunk);
            assert!(buf.len() <= limit);
            assert!(
                buf.len() + chunk > limit,
                "stopped only when the next chunk overflows"
            );
            assert!(buf.capacity() <= limit);
        }
    }
}

#[test]
fn growth_steps_double_and_clamp_to_the_limit() {
    let limit = 100_003;
    let mut buf = CappedBuf::new(limit);
    assert_eq!(buf.capacity(), 0, "an empty buffer allocates nothing");
    buf.extend(&[1]).expect("first byte");
    assert_eq!(buf.capacity(), MIN_GROWTH);
    let mut capacities = vec![buf.capacity()];
    while buf.len() < limit {
        let room = limit - buf.len();
        buf.extend(&vec![2_u8; room.min(1500)])
            .expect("within limit");
        if capacities.last() != Some(&buf.capacity()) {
            capacities.push(buf.capacity());
        }
    }
    assert_eq!(buf.len(), limit, "the exact limit is accepted");
    assert_eq!(
        capacities,
        vec![4096, 8192, 16_384, 32_768, 65_536, 100_003]
    );
    assert_eq!(buf.growths(), capacities.len());
    // The largest transient: 65 536 old + 100 003 new.
    assert_eq!(buf.peak(), 65_536 + 100_003);
    assert!(matches!(
        buf.extend(&[0]),
        Err(BufError::Limit { attempted: 100_004 })
    ));
}

#[test]
fn one_large_append_reserves_exactly_what_it_needs() {
    let mut buf = CappedBuf::new(1_000_000);
    buf.extend(&vec![0_u8; 300_001]).expect("fits");
    assert_eq!(buf.capacity(), 300_001);
    assert_eq!(buf.peak(), 300_001, "no old buffer to move");
    buf.extend(&[0]).expect("fits");
    assert_eq!(buf.capacity(), 600_002, "doubling");
    assert!(buf.capacity() <= buf.limit());
}

#[test]
fn zero_limit_refuses_everything_and_allocates_nothing() {
    let mut buf = CappedBuf::new(0);
    assert!(buf.extend(&[]).is_ok());
    assert_eq!(buf.extend(&[1]), Err(BufError::Limit { attempted: 1 }));
    assert_eq!(buf.capacity(), 0);
    assert!(buf.is_empty());
}

#[test]
fn patch_rewrites_in_place_without_growth() {
    let mut buf = CappedBuf::new(64);
    buf.extend(b"abcdef").expect("fits");
    let cap = buf.capacity();
    buf.patch(2, b"XY");
    assert_eq!(buf.as_slice(), b"abXYef");
    assert_eq!(buf.capacity(), cap);
    assert_eq!(buf.into_vec(), b"abXYef".to_vec());
}
