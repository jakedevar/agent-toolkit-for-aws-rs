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

impl ListenerIo for TcpListener {
    type Stream = TcpStream;
    fn accept_stream(&self) -> io::Result<TcpStream> {
        let (stream, _) = self.accept()?;
        stream.set_nonblocking(true)?;
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
                Err(e) => return Err(e),
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
                                    let page = response(if matches!(terminal, Outcome::Code(_)) {
                                        200
                                    } else {
                                        400
                                    });
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
mod scripted_tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

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
