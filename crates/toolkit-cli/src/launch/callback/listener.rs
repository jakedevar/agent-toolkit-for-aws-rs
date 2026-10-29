//! Bounded loopback callback listener.
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use super::{parse_request, response, Outcome, MAX_HEAD};

const SLOT_COUNT: usize = 16;
const ADMISSION: usize = 8;
const SLOT_TIMEOUT: Duration = Duration::from_secs(2);

struct Slot {
    stream: TcpStream,
    head: Vec<u8>,
    output: Option<(Vec<u8>, usize)>,
    deadline: Instant,
}

impl Slot {
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
    let mut slots: Vec<Option<Slot>> = (0..SLOT_COUNT).map(|_| None).collect();
    let mut first = 0;
    loop {
        let start = Instant::now();
        if start >= flow_end {
            return Ok(None);
        }
        let mut busy = false;
        for _ in 0..ADMISSION {
            let now = Instant::now();
            expire(&mut slots, now);
            if now >= flow_end {
                return Ok(None);
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    busy = true;
                    if let Some(empty) = slots.iter_mut().find(|slot| slot.is_none()) {
                        stream.set_nonblocking(true)?;
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
            let now = Instant::now();
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
                            || e.kind() == io::ErrorKind::Interrupted =>
                    {
                        ()
                    }
                    Err(_) => drop_slot = true,
                }
            } else {
                let mut buf = [0_u8; MAX_HEAD];
                let room = MAX_HEAD - slot.head.len();
                match slot.stream.read(&mut buf[..room]) {
                    Ok(0) => drop_slot = true,
                    Ok(n) => {
                        busy = true;
                        slot.head.extend_from_slice(&buf[..n]);
                        if let Some(end) = slot.head.windows(4).position(|w| w == b"\r\n\r\n") {
                            if Instant::now() >= flow_end {
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
                                    while written < page.len() && Instant::now() < deadline {
                                        match stream.write(&page[written..]) {
                                            Ok(0) => break,
                                            Ok(n) => written += n,
                                            Err(e)
                                                if e.kind() == io::ErrorKind::WouldBlock
                                                    || e.kind() == io::ErrorKind::Interrupted =>
                                            {
                                                std::thread::sleep(Duration::from_millis(2));
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
                            || e.kind() == io::ErrorKind::Interrupted =>
                    {
                        ()
                    }
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
            let now = Instant::now();
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
            std::thread::sleep(
                next_turn
                    .min(next_deadline)
                    .min(flow_end)
                    .saturating_duration_since(now),
            );
        }
    }
}

fn expire(slots: &mut [Option<Slot>], now: Instant) {
    for slot in slots {
        if slot.as_ref().is_some_and(|s| now >= s.deadline) {
            *slot = None;
        }
    }
}
