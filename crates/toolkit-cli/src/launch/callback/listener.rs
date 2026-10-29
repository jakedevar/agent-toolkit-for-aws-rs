//! Bounded loopback callback listener.
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use super::{parse_request, response, Outcome, MAX_HEAD};

const SLOT_COUNT: usize = 16;
const ADMISSION: usize = 8;
const SLOT_TIMEOUT: Duration = Duration::from_secs(2);

struct Slot<S> {
    stream: S,
    head: Vec<u8>,
    output: Option<(Vec<u8>, usize)>,
    deadline: Instant,
}

impl<S> Slot<S> {
    fn queue(&mut self, status: u16, now: Instant, flow_end: Instant) {
        self.output = Some((response(status), 0));
        self.deadline = (now + SLOT_TIMEOUT).min(flow_end + SLOT_TIMEOUT);
    }
}

/// Bind the sole supported callback address and ask the OS for an ephemeral port.
///
/// # Errors
/// Returns the socket failure if binding or configuring the listener fails.
pub fn bind() -> io::Result<TcpListener> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    match listener.local_addr()? {
        SocketAddr::V4(addr) if *addr.ip() == Ipv4Addr::LOCALHOST && addr.port() != 0 => {
            Ok(listener)
        }
        _ => Err(io::Error::other("invalid callback listener address")),
    }
}

/// Serve callbacks until a state-matching request or the flow deadline.
///
/// The caller owns exchange and persistence. The listener is dropped before
/// this function returns the matching request. No traffic count ends the flow.
///
/// # Errors
/// Returns an unrecoverable listener error.
#[allow(clippy::too_many_lines)]
pub fn serve(listener: TcpListener, state: &str, flow_end: Instant) -> io::Result<Option<Outcome>> {
    let port = listener.local_addr()?.port();
    serve_with(listener, port, state, flow_end, &SystemClock)
}

trait ListenerIo {
    type Stream: Read + Write;
    fn accept_stream(&self) -> io::Result<Self::Stream>;
}

fn connection_accept_error(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted | io::ErrorKind::ConnectionReset
    ) {
        return true;
    }
    #[cfg(unix)]
    {
        use rustix::io::Errno;
        return [
            Errno::PROTO,
            Errno::NETDOWN,
            Errno::NETUNREACH,
            Errno::HOSTDOWN,
            Errno::HOSTUNREACH,
            Errno::NOPROTOOPT,
            Errno::OPNOTSUPP,
        ]
        .into_iter()
        .any(|errno| error.raw_os_error() == io::Error::from(errno).raw_os_error());
    }
    #[cfg(not(unix))]
    false
}

impl ListenerIo for TcpListener {
    type Stream = TcpStream;
    fn accept_stream(&self) -> io::Result<TcpStream> {
        let (stream, _) = self.accept()?;
        // A failure belongs to this accepted connection, not the listener.
        stream.set_nonblocking(true).map_err(|_| {
            io::Error::new(io::ErrorKind::ConnectionAborted, "connection setup failed")
        })?;
        Ok(stream)
    }
}

trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[allow(clippy::too_many_lines)]
fn serve_with<L: ListenerIo>(
    listener: L,
    port: u16,
    state: &str,
    flow_end: Instant,
    clock: &impl Clock,
) -> io::Result<Option<Outcome>> {
    let mut slots: Vec<Option<Slot<L::Stream>>> = (0..SLOT_COUNT).map(|_| None).collect();
    let mut first = 0;
    loop {
        let start = clock.now();
        if start >= flow_end {
            return Ok(None);
        }
        let mut busy = false;
        for _ in 0..ADMISSION {
            let now = clock.now();
            expire(&mut slots, now);
            if now >= flow_end {
                return Ok(None);
            }
            match listener.accept_stream() {
                Ok(stream) => {
                    busy = true;
                    if let Some(empty) = slots.iter_mut().find(|slot| slot.is_none()) {
                        *empty = Some(Slot {
                            stream,
                            head: Vec::with_capacity(MAX_HEAD),
                            output: None,
                            deadline: (now + SLOT_TIMEOUT).min(flow_end),
                        });
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if connection_accept_error(&e) => {}
                // Listener errors are retried with idle pacing.
                Err(_) => break,
            }
        }
        for offset in 0..SLOT_COUNT {
            let now = clock.now();
            expire(&mut slots, now);
            if now >= flow_end {
                return Ok(None);
            }
            let index = (first + offset) % SLOT_COUNT;
            let Some(slot) = slots[index].as_mut() else {
                continue;
            };
            let mut drop_slot = false;
            if let Some((bytes, sent)) = &mut slot.output {
                match slot.stream.write(&bytes[*sent..]) {
                    Ok(0) => drop_slot = true,
                    Ok(n) => {
                        *sent += n;
                        busy = true;
                        drop_slot = *sent == bytes.len();
                    }
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => drop_slot = true,
                }
            } else {
                let mut buf = [0_u8; MAX_HEAD];
                let room = MAX_HEAD - slot.head.len();
                match slot.stream.read(&mut buf[..room]) {
                    Ok(0) => drop_slot = true,
                    Ok(n) => {
                        busy = true;
                        if clock.now() >= slot.deadline {
                            slots[index] = None;
                            continue;
                        }
                        slot.head.extend_from_slice(&buf[..n]);
                        if let Some(end) = slot.head.windows(4).position(|w| w == b"\r\n\r\n") {
                            if clock.now() >= flow_end {
                                return Ok(None);
                            }
                            let outcome = parse_request(&slot.head[..end + 4], port, state);
                            match outcome {
                                Outcome::Ignore(status) => slot.queue(status, now, flow_end),
                                terminal => {
                                    let page = if matches!(terminal, Outcome::Code(_)) {
                                        response(200)
                                    } else {
                                        super::authorization_failed_response()
                                    };
                                    let mut stream =
                                        slots[index].take().expect("occupied slot").stream;
                                    drop(slots);
                                    drop(listener);
                                    let deadline =
                                        (now + SLOT_TIMEOUT).min(flow_end + SLOT_TIMEOUT);
                                    let mut written = 0;
                                    while written < page.len() && clock.now() < deadline {
                                        match stream.write(&page[written..]) {
                                            Ok(0) => break,
                                            Ok(n) => written += n,
                                            Err(e)
                                                if e.kind() == io::ErrorKind::WouldBlock
                                                    || e.kind() == io::ErrorKind::Interrupted =>
                                            {
                                                clock.sleep(Duration::from_millis(2));
                                            }
                                            Err(_) => break,
                                        }
                                    }
                                    return Ok(Some(terminal));
                                }
                            }
                        } else if slot.head.len() == MAX_HEAD {
                            slot.queue(400, now, flow_end);
                        }
                    }
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => drop_slot = true,
                }
            }
            if drop_slot {
                slots[index] = None;
            }
        }
        first = (first + 1) % SLOT_COUNT;
        let next_turn = start + Duration::from_millis(if busy { 2 } else { 10 });
        loop {
            let now = clock.now();
            expire(&mut slots, now);
            if now >= flow_end {
                return Ok(None);
            }
            if now >= next_turn {
                break;
            }
            let next_deadline = slots
                .iter()
                .flatten()
                .map(|s| s.deadline)
                .min()
                .unwrap_or(flow_end);
            clock.sleep(
                next_turn
                    .min(next_deadline)
                    .min(flow_end)
                    .saturating_duration_since(now),
            );
        }
    }
}

fn expire<S>(slots: &mut [Option<Slot<S>>], now: Instant) {
    for slot in slots {
        if slot.as_ref().is_some_and(|s| now >= s.deadline) {
            *slot = None;
        }
    }
}

#[cfg(test)]
pub(super) fn serve_scripted_flood(
    flow_end: Instant,
    accepts: &std::cell::Cell<usize>,
) -> io::Result<Option<Outcome>> {
    struct Flood<'a>(&'a std::cell::Cell<usize>);
    impl ListenerIo for Flood<'_> {
        type Stream = std::io::Cursor<Vec<u8>>;
        fn accept_stream(&self) -> io::Result<Self::Stream> {
            self.0.set(self.0.get() + 1);
            Ok(std::io::Cursor::new(b"invalid\r\n\r\n".to_vec()))
        }
    }
    struct ClockCell(std::cell::Cell<Instant>);
    impl Clock for ClockCell {
        fn now(&self) -> Instant {
            self.0.get()
        }
        fn sleep(&self, duration: Duration) {
            self.0.set(self.0.get() + duration);
        }
    }
    let clock = ClockCell(std::cell::Cell::new(flow_end - Duration::from_millis(50)));
    serve_with(Flood(accepts), 1234, "s", flow_end, &clock)
}

#[cfg(test)]
mod scripted_tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    struct TimedClock(Rc<Cell<Instant>>);
    impl Clock for TimedClock {
        fn now(&self) -> Instant {
            self.0.get()
        }
        fn sleep(&self, duration: Duration) {
            self.0.set(self.0.get() + duration);
        }
    }
    struct TimedListener {
        stream: std::cell::RefCell<Option<TimedStream>>,
    }
    struct TimedStream {
        now: Rc<Cell<Instant>>,
        start: Instant,
        chunks: std::collections::VecDeque<(Duration, Vec<u8>)>,
        write_blocked: bool,
    }
    impl ListenerIo for TimedListener {
        type Stream = TimedStream;
        fn accept_stream(&self) -> io::Result<TimedStream> {
            self.stream
                .borrow_mut()
                .take()
                .ok_or_else(|| io::ErrorKind::WouldBlock.into())
        }
    }
    impl Read for TimedStream {
        fn read(&mut self, dest: &mut [u8]) -> io::Result<usize> {
            let Some((at, _)) = self.chunks.front() else {
                return Err(io::ErrorKind::WouldBlock.into());
            };
            if self.now.get().duration_since(self.start) < *at {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let (_, chunk) = self.chunks.pop_front().unwrap();
            dest[..chunk.len()].copy_from_slice(&chunk);
            Ok(chunk.len())
        }
    }
    impl Write for TimedStream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.write_blocked {
                Err(io::ErrorKind::WouldBlock.into())
            } else {
                Ok(bytes.len())
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn timed_callback(
        chunks: Vec<(Duration, Vec<u8>)>,
        flow: Duration,
        write_blocked: bool,
    ) -> (Option<Outcome>, Duration) {
        let start = Instant::now();
        let now = Rc::new(Cell::new(start));
        let listener = TimedListener {
            stream: std::cell::RefCell::new(Some(TimedStream {
                now: now.clone(),
                start,
                chunks: chunks.into(),
                write_blocked,
            })),
        };
        let result =
            serve_with(listener, 1234, "s", start + flow, &TimedClock(now.clone())).unwrap();
        (result, now.get().duration_since(start))
    }
    fn valid_head() -> Vec<u8> {
        b"GET /?state=s&code=ok HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n".to_vec()
    }

    #[test]
    fn real_socket_bind_is_loopback_only_and_fake_clock_expires() {
        use std::net::{Ipv6Addr, SocketAddrV6};
        let listener = bind().unwrap();
        let addr = listener.local_addr().unwrap();
        assert_eq!(addr.ip(), Ipv4Addr::LOCALHOST);
        assert_ne!(addr.port(), 0);
        assert!(TcpStream::connect((Ipv4Addr::new(127, 0, 0, 2), addr.port())).is_err());
        if std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).is_ok() {
            assert!(
                TcpStream::connect(SocketAddrV6::new(Ipv6Addr::LOCALHOST, addr.port(), 0, 0))
                    .is_err()
            );
        }
        let start = Instant::now();
        let clock = FakeClock(Cell::new(start));
        assert_eq!(
            serve_with(
                listener,
                addr.port(),
                "s",
                start + Duration::from_millis(25),
                &clock
            )
            .unwrap(),
            None
        );
        assert_eq!(clock.now(), start + Duration::from_millis(25));
    }

    #[test]
    fn denied_and_invalid_code_use_authorization_failed_page() {
        struct Once(std::cell::RefCell<Option<Stream>>);
        struct Stream {
            head: std::io::Cursor<Vec<u8>>,
            output: Rc<std::cell::RefCell<Vec<u8>>>,
        }
        impl ListenerIo for Once {
            type Stream = Stream;
            fn accept_stream(&self) -> io::Result<Stream> {
                self.0
                    .borrow_mut()
                    .take()
                    .ok_or_else(|| io::ErrorKind::WouldBlock.into())
            }
        }
        impl Read for Stream {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
                self.head.read(b)
            }
        }
        impl Write for Stream {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.output.borrow_mut().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        for query in ["state=s&error=access_denied", "state=s&code="] {
            let start = Instant::now();
            let output = Rc::new(std::cell::RefCell::new(Vec::new()));
            let head = format!("GET /?{query} HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n");
            let listener = Once(std::cell::RefCell::new(Some(Stream {
                head: std::io::Cursor::new(head.into_bytes()),
                output: output.clone(),
            })));
            let result = serve_with(
                listener,
                1234,
                "s",
                start + Duration::from_secs(5),
                &FakeClock(Cell::new(start)),
            )
            .unwrap();
            assert!(matches!(
                result,
                Some(Outcome::Denied(_) | Outcome::InvalidCode)
            ));
            assert!(String::from_utf8(output.borrow().clone())
                .unwrap()
                .contains("Authorization failed"));
        }
    }

    #[test]
    fn connection_error_does_not_delay_next_admission() {
        struct AbortedThenCallback(Cell<usize>, Rc<Cell<Instant>>, Instant);
        impl ListenerIo for AbortedThenCallback {
            type Stream = TimedStream;
            fn accept_stream(&self) -> io::Result<TimedStream> {
                let n = self.0.get();
                self.0.set(n + 1);
                match n {
                    0 => Err(io::ErrorKind::ConnectionAborted.into()),
                    1 => {
                        assert_eq!(
                            self.1.get(),
                            self.2,
                            "callback must be admitted in the same turn"
                        );
                        Ok(TimedStream {
                            now: self.1.clone(),
                            start: self.2,
                            chunks: vec![(Duration::ZERO, valid_head())].into(),
                            write_blocked: false,
                        })
                    }
                    _ => Err(io::ErrorKind::WouldBlock.into()),
                }
            }
        }
        let start = Instant::now();
        let now = Rc::new(Cell::new(start));
        assert_eq!(
            serve_with(
                AbortedThenCallback(Cell::new(0), now.clone(), start),
                1234,
                "s",
                start + Duration::from_secs(5),
                &TimedClock(now)
            )
            .unwrap(),
            Some(Outcome::Code("ok".into()))
        );
    }

    #[test]
    fn accept_errors_do_not_end_flow() {
        struct ErrorsThenCallback(Cell<usize>, Rc<Cell<Instant>>, Instant);
        impl ListenerIo for ErrorsThenCallback {
            type Stream = TimedStream;
            fn accept_stream(&self) -> io::Result<TimedStream> {
                let n = self.0.get();
                self.0.set(n + 1);
                match n {
                    0 => Err(io::ErrorKind::ConnectionAborted.into()),
                    1 => Err(io::Error::from_raw_os_error(24)),
                    2 => Ok(TimedStream {
                        now: self.1.clone(),
                        start: self.2,
                        chunks: vec![(Duration::ZERO, valid_head())].into(),
                        write_blocked: false,
                    }),
                    _ => Err(io::ErrorKind::WouldBlock.into()),
                }
            }
        }
        let start = Instant::now();
        let now = Rc::new(Cell::new(start));
        let result = serve_with(
            ErrorsThenCallback(Cell::new(0), now.clone(), start),
            1234,
            "s",
            start + Duration::from_secs(5),
            &TimedClock(now),
        );
        assert_eq!(result.unwrap(), Some(Outcome::Code("ok".into())));
    }

    #[test]
    fn trickling_head_does_not_extend_two_second_deadline() {
        let head = valid_head();
        let chunks = vec![
            (Duration::ZERO, head[..1].to_vec()),
            (Duration::from_millis(1900), head[1..2].to_vec()),
            (Duration::from_millis(2100), head[2..].to_vec()),
        ];
        let (result, elapsed) = timed_callback(chunks, Duration::from_secs(3), false);
        assert_eq!(result, None);
        assert_eq!(elapsed, Duration::from_secs(3));
    }

    #[test]
    fn callback_head_before_flow_deadline_is_eligible_at_deadline_is_not() {
        let flow = Duration::from_secs(1);
        assert_eq!(
            timed_callback(
                vec![(Duration::from_millis(990), valid_head())],
                flow,
                false
            )
            .0,
            Some(Outcome::Code("ok".into()))
        );
        assert_eq!(
            timed_callback(vec![(flow, valid_head())], flow, false).0,
            None
        );
    }

    #[test]
    fn blocked_response_write_does_not_prevent_exchange() {
        let (result, elapsed) = timed_callback(
            vec![(Duration::ZERO, valid_head())],
            Duration::from_secs(5),
            true,
        );
        assert_eq!(result, Some(Outcome::Code("ok".into())));
        assert_eq!(elapsed, Duration::from_secs(2));
    }

    struct FakeClock(Cell<Instant>);
    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.0.get()
        }
        fn sleep(&self, duration: Duration) {
            self.0.set(self.0.get() + duration);
        }
    }

    struct FakeListener {
        accepts: Rc<Cell<usize>>,
        reads: Rc<Cell<usize>>,
        callback_at: Option<usize>,
    }
    struct FakeStream {
        callback: bool,
        part: usize,
        reads: Rc<Cell<usize>>,
    }
    impl ListenerIo for FakeListener {
        type Stream = FakeStream;
        fn accept_stream(&self) -> io::Result<FakeStream> {
            let count = self.accepts.get();
            self.accepts.set(count + 1);
            Ok(FakeStream {
                callback: self.callback_at == Some(count),
                part: 0,
                reads: self.reads.clone(),
            })
        }
    }
    impl Read for FakeStream {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if !self.callback {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            self.reads.set(self.reads.get() + 1);
            let parts: [&[u8]; 3] = [
                b"GET /?state=s",
                b"&code=ok HTTP/1.1\r\n",
                b"Host: 127.0.0.1:1234\r\n\r\n",
            ];
            let part = parts[self.part];
            self.part += 1;
            bytes[..part.len()].copy_from_slice(part);
            Ok(part.len())
        }
    }
    impl Write for FakeStream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn flood_does_not_starve_an_admitted_callback() {
        let now = Instant::now();
        let clock = FakeClock(Cell::new(now));
        let accepts = Rc::new(Cell::new(0));
        let reads = Rc::new(Cell::new(0));
        let listener = FakeListener {
            accepts: accepts.clone(),
            reads: reads.clone(),
            callback_at: Some(5),
        };
        assert_eq!(
            serve_with(listener, 1234, "s", now + Duration::from_secs(1), &clock).unwrap(),
            Some(Outcome::Code("ok".into()))
        );
        assert_eq!(reads.get(), 3);
        assert_eq!(accepts.get(), 24);
        assert_eq!(clock.now().duration_since(now), Duration::from_millis(4));
    }

    #[test]
    fn always_ready_flood_obeys_two_millisecond_turn_pacing() {
        let start = Instant::now();
        let clock = FakeClock(Cell::new(start));
        let accepts = Rc::new(Cell::new(0));
        let listener = FakeListener {
            accepts: accepts.clone(),
            reads: Rc::new(Cell::new(0)),
            callback_at: None,
        };
        assert_eq!(
            serve_with(listener, 1234, "s", start + Duration::from_secs(1), &clock).unwrap(),
            None
        );
        assert_eq!(accepts.get(), 4000);
        assert_eq!(clock.now().duration_since(start), Duration::from_secs(1));
    }

    #[test]
    fn idle_listener_obeys_ten_millisecond_turn_pacing() {
        struct Idle(Rc<Cell<usize>>);
        impl ListenerIo for Idle {
            type Stream = FakeStream;
            fn accept_stream(&self) -> io::Result<FakeStream> {
                self.0.set(self.0.get() + 1);
                Err(io::ErrorKind::WouldBlock.into())
            }
        }
        let start = Instant::now();
        let clock = FakeClock(Cell::new(start));
        let accepts = Rc::new(Cell::new(0));
        assert_eq!(
            serve_with(
                Idle(accepts.clone()),
                1234,
                "s",
                start + Duration::from_secs(1),
                &clock
            )
            .unwrap(),
            None
        );
        assert_eq!(accepts.get(), 100);
    }

    #[test]
    fn slot_service_rotates_without_starving_admitted_connections() {
        use std::cell::RefCell;
        struct Listener {
            count: Cell<usize>,
            serviced: Rc<RefCell<Vec<usize>>>,
        }
        struct Stream {
            id: usize,
            serviced: Rc<RefCell<Vec<usize>>>,
        }
        impl ListenerIo for Listener {
            type Stream = Stream;
            fn accept_stream(&self) -> io::Result<Stream> {
                let id = self.count.get();
                self.count.set(id + 1);
                if id >= 16 {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                Ok(Stream {
                    id,
                    serviced: self.serviced.clone(),
                })
            }
        }
        impl Read for Stream {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                self.serviced.borrow_mut().push(self.id);
                Err(io::ErrorKind::WouldBlock.into())
            }
        }
        impl Write for Stream {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::WouldBlock.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let start = Instant::now();
        let clock = FakeClock(Cell::new(start));
        let serviced = Rc::new(RefCell::new(Vec::new()));
        let listener = Listener {
            count: Cell::new(0),
            serviced: serviced.clone(),
        };
        assert_eq!(
            serve_with(
                listener,
                1234,
                "s",
                start + Duration::from_millis(14),
                &clock
            )
            .unwrap(),
            None
        );
        let calls = serviced.borrow();
        assert_eq!(&calls[..8], &(0..8).collect::<Vec<_>>());
        assert_eq!(
            &calls[8..24],
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0]
        );
        assert_eq!(
            &calls[24..40],
            &[2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0, 1]
        );
        assert_eq!(calls.len(), 40);
    }

    #[test]
    fn flow_deadline_inside_admission_quantum_stops_within_one_operation() {
        struct AdvancingListener {
            now: Rc<Cell<Instant>>,
            accepts: Rc<Cell<usize>>,
        }
        impl ListenerIo for AdvancingListener {
            type Stream = FakeStream;
            fn accept_stream(&self) -> io::Result<FakeStream> {
                self.now.set(self.now.get() + Duration::from_millis(1));
                self.accepts.set(self.accepts.get() + 1);
                Ok(FakeStream {
                    callback: false,
                    part: 0,
                    reads: Rc::new(Cell::new(0)),
                })
            }
        }
        let start = Instant::now();
        let now = Rc::new(Cell::new(start));
        let accepts = Rc::new(Cell::new(0));
        let listener = AdvancingListener {
            now: now.clone(),
            accepts: accepts.clone(),
        };
        assert_eq!(
            serve_with(
                listener,
                1234,
                "s",
                start + Duration::from_micros(3500),
                &TimedClock(now.clone())
            )
            .unwrap(),
            None
        );
        assert_eq!(accepts.get(), 4);
        assert_eq!(now.get().duration_since(start), Duration::from_millis(4));
    }

    #[test]
    fn admission_expires_slot_before_it_can_be_serviced() {
        struct AdvancingListener {
            now: Rc<Cell<Instant>>,
            accepts: Cell<usize>,
            reads: Rc<Cell<usize>>,
        }
        struct CountStream(Rc<Cell<usize>>);
        impl ListenerIo for AdvancingListener {
            type Stream = CountStream;
            fn accept_stream(&self) -> io::Result<CountStream> {
                let n = self.accepts.get();
                self.accepts.set(n + 1);
                if n == 0 {
                    return Ok(CountStream(self.reads.clone()));
                }
                if n == 1 {
                    self.now.set(self.now.get() + SLOT_TIMEOUT);
                }
                Err(io::ErrorKind::WouldBlock.into())
            }
        }
        impl Read for CountStream {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                self.0.set(self.0.get() + 1);
                Err(io::ErrorKind::WouldBlock.into())
            }
        }
        impl Write for CountStream {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::WouldBlock.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let start = Instant::now();
        let now = Rc::new(Cell::new(start));
        let reads = Rc::new(Cell::new(0));
        let listener = AdvancingListener {
            now: now.clone(),
            accepts: Cell::new(0),
            reads: reads.clone(),
        };
        assert_eq!(
            serve_with(
                listener,
                1234,
                "s",
                start + Duration::from_millis(2010),
                &TimedClock(now)
            )
            .unwrap(),
            None
        );
        assert_eq!(reads.get(), 0);
    }

    #[test]
    fn staggered_trickle_expires_during_always_ready_admission() {
        struct Flood {
            now: Rc<Cell<Instant>>,
            next: Cell<usize>,
            dropped: Rc<Cell<Option<Instant>>>,
            reads: Rc<Cell<usize>>,
        }
        struct Stream {
            now: Rc<Cell<Instant>>,
            dropped: Rc<Cell<Option<Instant>>>,
            reads: Rc<Cell<usize>>,
            special: bool,
            next_byte: Instant,
        }
        impl ListenerIo for Flood {
            type Stream = Stream;
            fn accept_stream(&self) -> io::Result<Stream> {
                let special = self.next.get() == 0;
                self.next.set(self.next.get() + 1);
                self.now.set(self.now.get() + Duration::from_millis(1));
                Ok(Stream {
                    now: self.now.clone(),
                    dropped: self.dropped.clone(),
                    reads: self.reads.clone(),
                    special,
                    next_byte: self.now.get(),
                })
            }
        }
        impl Drop for Stream {
            fn drop(&mut self) {
                if self.special {
                    self.dropped.set(Some(self.now.get()));
                }
            }
        }
        impl Read for Stream {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if !self.special || self.now.get() < self.next_byte {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                self.reads.set(self.reads.get() + 1);
                self.next_byte = self.now.get() + Duration::from_millis(500);
                bytes[0] = b'x';
                Ok(1)
            }
        }
        impl Write for Stream {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::WouldBlock.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let start = Instant::now();
        let now = Rc::new(Cell::new(start));
        let dropped = Rc::new(Cell::new(None));
        let reads = Rc::new(Cell::new(0));
        let listener = Flood {
            now: now.clone(),
            next: Cell::new(0),
            dropped: dropped.clone(),
            reads: reads.clone(),
        };
        assert_eq!(
            serve_with(
                listener,
                1234,
                "s",
                start + Duration::from_millis(2100),
                &TimedClock(now)
            )
            .unwrap(),
            None
        );
        assert!(reads.get() >= 3);
        let expiry = start + SLOT_TIMEOUT;
        let at = dropped.get().unwrap();
        assert!(at >= expiry && at <= expiry + Duration::from_millis(1));
    }

    #[test]
    fn malformed_and_wrong_state_flood_never_ends_flow() {
        struct Requests {
            next: Cell<usize>,
            ready: Cell<bool>,
            served: Rc<Cell<usize>>,
        }
        struct RequestStream {
            id: usize,
            served: Rc<Cell<usize>>,
        }
        impl ListenerIo for Requests {
            type Stream = RequestStream;
            fn accept_stream(&self) -> io::Result<RequestStream> {
                if !self.ready.replace(!self.ready.get()) {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                let id = self.next.get();
                if id > 600 {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                self.next.set(id + 1);
                Ok(RequestStream {
                    id,
                    served: self.served.clone(),
                })
            }
        }
        impl Read for RequestStream {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.served.set(self.served.get() + 1);
                let head: &[u8] = if self.id < 300 {
                    b"INVALID\r\n\r\n"
                } else if self.id < 600 && self.id.is_multiple_of(2) {
                    b"GET /?state=wrong&error=denied HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n"
                } else if self.id < 600 {
                    b"GET /?state=wrong&code=bad HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n"
                } else {
                    b"GET /?state=s&code=ok HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n"
                };
                bytes[..head.len()].copy_from_slice(head);
                Ok(head.len())
            }
        }
        impl Write for RequestStream {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let start = Instant::now();
        let served = Rc::new(Cell::new(0));
        let listener = Requests {
            next: Cell::new(0),
            ready: Cell::new(true),
            served: served.clone(),
        };
        let clock = FakeClock(Cell::new(start));
        assert_eq!(
            serve_with(listener, 1234, "s", start + Duration::from_secs(10), &clock).unwrap(),
            Some(Outcome::Code("ok".into()))
        );
        assert_eq!(served.get(), 601);
    }

    #[test]
    fn full_slots_close_excess_unread_then_serve_after_slot_frees() {
        use std::cell::RefCell;
        struct Listener {
            count: Cell<usize>,
            now: Rc<Cell<Instant>>,
            start: Instant,
            reads: Rc<RefCell<Vec<usize>>>,
            drops: Rc<RefCell<Vec<usize>>>,
        }
        struct Stream {
            id: usize,
            now: Rc<Cell<Instant>>,
            start: Instant,
            reads: Rc<RefCell<Vec<usize>>>,
            drops: Rc<RefCell<Vec<usize>>>,
        }
        impl ListenerIo for Listener {
            type Stream = Stream;
            fn accept_stream(&self) -> io::Result<Stream> {
                let id = self.count.get();
                let elapsed = self.now.get().duration_since(self.start);
                if id >= 18 || (id == 17 && elapsed < Duration::from_millis(6)) {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                self.count.set(id + 1);
                Ok(Stream {
                    id,
                    now: self.now.clone(),
                    start: self.start,
                    reads: self.reads.clone(),
                    drops: self.drops.clone(),
                })
            }
        }
        impl Read for Stream {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.reads.borrow_mut().push(self.id);
                if self.id == 0
                    && self.now.get().duration_since(self.start) >= Duration::from_millis(4)
                {
                    return Ok(0);
                }
                if self.id == 17 {
                    let head = valid_head();
                    bytes[..head.len()].copy_from_slice(&head);
                    return Ok(head.len());
                }
                Err(io::ErrorKind::WouldBlock.into())
            }
        }
        impl Write for Stream {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl Drop for Stream {
            fn drop(&mut self) {
                self.drops.borrow_mut().push(self.id);
            }
        }
        let start = Instant::now();
        let now = Rc::new(Cell::new(start));
        let reads = Rc::new(RefCell::new(Vec::new()));
        let drops = Rc::new(RefCell::new(Vec::new()));
        let listener = Listener {
            count: Cell::new(0),
            now: now.clone(),
            start,
            reads: reads.clone(),
            drops: drops.clone(),
        };
        assert_eq!(
            serve_with(
                listener,
                1234,
                "s",
                start + Duration::from_secs(1),
                &TimedClock(now)
            )
            .unwrap(),
            Some(Outcome::Code("ok".into()))
        );
        assert!(!reads.borrow().contains(&16));
        assert!(drops.borrow().contains(&16));
        assert!(reads.borrow().contains(&17));
    }

    #[test]
    fn progressing_flood_stays_within_work_rate_bounds() {
        struct Listener {
            now: Rc<Cell<Instant>>,
            accepts: Rc<Cell<usize>>,
            reads: Rc<Cell<usize>>,
        }
        struct Stream {
            now: Rc<Cell<Instant>>,
            reads: Rc<Cell<usize>>,
        }
        impl ListenerIo for Listener {
            type Stream = Stream;
            fn accept_stream(&self) -> io::Result<Stream> {
                self.now.set(self.now.get() + Duration::from_micros(1));
                self.accepts.set(self.accepts.get() + 1);
                Ok(Stream {
                    now: self.now.clone(),
                    reads: self.reads.clone(),
                })
            }
        }
        impl Read for Stream {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.now.set(self.now.get() + Duration::from_micros(1));
                self.reads.set(self.reads.get() + 1);
                bytes[0] = b'a';
                Ok(1)
            }
        }
        impl Write for Stream {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let start = Instant::now();
        let now = Rc::new(Cell::new(start));
        let accepts = Rc::new(Cell::new(0));
        let reads = Rc::new(Cell::new(0));
        let listener = Listener {
            now: now.clone(),
            accepts: accepts.clone(),
            reads: reads.clone(),
        };
        assert_eq!(
            serve_with(
                listener,
                1234,
                "s",
                start + Duration::from_secs(1),
                &TimedClock(now)
            )
            .unwrap(),
            None
        );
        assert!(accepts.get() <= 4000 && accepts.get() > 3900);
        assert!(reads.get() <= 8000 && reads.get() > 7800);
    }

    #[test]
    fn each_turn_bounds_admissions_slots_and_head_buffers() {
        #[derive(Default)]
        struct Stats {
            accepts: usize,
            reads: usize,
            max_read: usize,
            max_live: usize,
        }
        struct TrackedClock {
            now: Cell<Instant>,
            turns: Rc<std::cell::RefCell<Vec<Stats>>>,
        }
        impl Clock for TrackedClock {
            fn now(&self) -> Instant {
                self.now.get()
            }
            fn sleep(&self, d: Duration) {
                self.now.set(self.now.get() + d);
                self.turns.borrow_mut().push(Stats::default());
            }
        }
        struct Listener(Rc<std::cell::RefCell<Vec<Stats>>>, Rc<Cell<usize>>);
        struct Stream(Rc<std::cell::RefCell<Vec<Stats>>>, Rc<Cell<usize>>);
        impl ListenerIo for Listener {
            type Stream = Stream;
            fn accept_stream(&self) -> io::Result<Stream> {
                let live = self.1.get() + 1;
                self.1.set(live);
                let mut turns = self.0.borrow_mut();
                let s = turns.last_mut().unwrap();
                s.accepts += 1;
                s.max_live = s.max_live.max(live);
                Ok(Stream(self.0.clone(), self.1.clone()))
            }
        }
        impl Drop for Stream {
            fn drop(&mut self) {
                self.1.set(self.1.get() - 1);
            }
        }
        impl Read for Stream {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                let mut turns = self.0.borrow_mut();
                let s = turns.last_mut().unwrap();
                s.reads += 1;
                s.max_read = s.max_read.max(bytes.len());
                bytes[0] = b'x';
                Ok(1)
            }
        }
        impl Write for Stream {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let start = Instant::now();
        let turns = Rc::new(std::cell::RefCell::new(vec![Stats::default()]));
        let clock = TrackedClock {
            now: Cell::new(start),
            turns: turns.clone(),
        };
        assert_eq!(
            serve_with(
                Listener(turns.clone(), Rc::new(Cell::new(0))),
                1234,
                "s",
                start + Duration::from_millis(20),
                &clock
            )
            .unwrap(),
            None
        );
        let turns = turns.borrow();
        assert!(turns.iter().any(|s| s.reads == 16));
        for turn in turns.iter() {
            assert!(turn.accepts <= 8 && turn.reads <= 16 && turn.max_read <= MAX_HEAD);
            // One excess connection can exist momentarily before it is dropped unread.
            assert!(turn.max_live <= 17);
        }
    }

    #[test]
    fn head_finishing_at_slot_deadline_is_not_accepted() {
        struct SlowListener {
            now: Rc<Cell<Instant>>,
            accepted: Cell<bool>,
        }
        struct SlowStream {
            now: Rc<Cell<Instant>>,
        }
        impl ListenerIo for SlowListener {
            type Stream = SlowStream;
            fn accept_stream(&self) -> io::Result<SlowStream> {
                if self.accepted.replace(true) {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                Ok(SlowStream {
                    now: self.now.clone(),
                })
            }
        }
        impl Read for SlowStream {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.now.set(self.now.get() + SLOT_TIMEOUT);
                let request = b"GET /?state=s&code=ok HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n";
                bytes[..request.len()].copy_from_slice(request);
                Ok(request.len())
            }
        }
        impl Write for SlowStream {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        struct SharedClock(Rc<Cell<Instant>>);
        impl Clock for SharedClock {
            fn now(&self) -> Instant {
                self.0.get()
            }
            fn sleep(&self, duration: Duration) {
                self.0.set(self.0.get() + duration);
            }
        }
        let start = Instant::now();
        let now = Rc::new(Cell::new(start));
        let listener = SlowListener {
            now: now.clone(),
            accepted: Cell::new(false),
        };
        assert_eq!(
            serve_with(
                listener,
                1234,
                "s",
                start + Duration::from_secs(3),
                &SharedClock(now)
            )
            .unwrap(),
            None
        );
    }
}
