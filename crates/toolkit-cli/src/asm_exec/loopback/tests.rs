//! Offline tests for the loopback-only HTTP transport. The only sockets are
//! listeners on 127.0.0.1 inside this process.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::*;
use toolkit_aws::{Headers, HttpRequest, Method, Timeouts, Transport};

#[test]
fn only_loopback_http_endpoints_are_accepted() {
    for (url, base) in [
        ("http://localhost:2773", "http://localhost:2773"),
        ("http://LOCALHOST:2773/", "http://LOCALHOST:2773"),
        ("http://127.0.0.1:2773", "http://127.0.0.1:2773"),
        ("http://127.5.5.5", "http://127.5.5.5:80"),
        ("http://[::1]:9", "http://[::1]:9"),
    ] {
        let endpoint =
            LoopbackEndpoint::parse(url).unwrap_or_else(|reason| panic!("{url}: {reason}"));
        assert_eq!(endpoint.base_url(), base);
    }
    for url in [
        "",
        "http://",
        "https://localhost:2773",
        "ftp://localhost",
        "http://example.com:2773",
        "http://169.254.169.254",
        "http://10.0.0.1",
        "http://0.0.0.0",
        "http://localhost.evil.com",
        "http://127.0.0.1.evil.com",
        "http://[::ffff:127.0.0.1]",
        "http://[::2]",
        "http://user@localhost:2773",
        "http://localhost:2773/path",
        "http://localhost:2773?x=1",
        "http://localhost:0",
        "http://localhost:99999",
        "http://localhost:abc",
        "http://local host",
        "http://[::1",
    ] {
        assert!(LoopbackEndpoint::parse(url).is_err(), "{url}");
    }
}

/// Serves one connection: reads the request head, writes `response`, returns
/// the request text.
fn serve(response: Vec<u8>) -> (u16, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut chunk).unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..count]);
        }
        stream.write_all(&response).unwrap();
        String::from_utf8_lossy(&request).into_owned()
    });
    (port, handle)
}

fn get(port: u16, path: &str) -> HttpRequest {
    let mut headers = Headers::new();
    headers.insert("X-Test", "yes").unwrap();
    HttpRequest {
        method: Method::Get,
        url: format!("http://127.0.0.1:{port}{path}"),
        headers,
        body: Vec::new(),
        timeouts: Timeouts::new(Duration::from_secs(5), Duration::from_secs(5)).unwrap(),
    }
}

#[test]
fn a_request_is_sent_in_origin_form_with_its_headers() {
    let (port, handle) = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec());
    let response = LoopbackHttp.send(&get(port, "/ping?a=1")).unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"ok");
    let request = handle.join().unwrap().to_ascii_lowercase();
    assert!(
        request.starts_with("get /ping?a=1 http/1.1\r\n"),
        "{request}"
    );
    assert!(request.contains(&format!("host: 127.0.0.1:{port}\r\n")));
    assert!(request.contains("x-test: yes\r\n"));
    assert!(request.contains("connection: close\r\n"));
}

#[test]
fn bodies_are_read_by_length_by_chunks_and_to_end_of_stream() {
    let cases: [(&str, &str); 3] = [
        ("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhelloEXTRA", "hello"),
        (
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;ext=1\r\nde\r\n0\r\n\r\n",
            "abcde",
        ),
        ("HTTP/1.0 200 OK\r\n\r\nuntil close", "until close"),
    ];
    for (raw, expected) in cases {
        let (port, handle) = serve(raw.as_bytes().to_vec());
        let response = LoopbackHttp.send(&get(port, "/")).unwrap();
        handle.join().unwrap();
        assert_eq!(response.body, expected.as_bytes());
    }
}

#[test]
fn error_statuses_and_headers_are_returned_not_followed() {
    let (port, handle) = serve(
        b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
    );
    let response = LoopbackHttp.send(&get(port, "/ping")).unwrap();
    handle.join().unwrap();
    assert_eq!(response.status, 302);
    assert_eq!(
        response.headers.get("location"),
        Some("http://169.254.169.254/")
    );
}

#[test]
fn malformed_responses_are_refused() {
    for raw in [
        "garbage",
        "HTTP/2 200\r\n\r\n",
        "HTTP/1.1 abc\r\n\r\n",
        "HTTP/1.1 700 X\r\n\r\n",
        "HTTP/1.1 200 OK\r\nno colon\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort",
        "HTTP/1.1 200 OK\r\nContent-Length: x\r\n\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab\r\n0\r\n\r\n",
    ] {
        let (port, handle) = serve(raw.as_bytes().to_vec());
        let result = LoopbackHttp.send(&get(port, "/"));
        handle.join().unwrap();
        assert!(result.is_err(), "{raw}");
    }
}

#[test]
fn urls_that_are_not_loopback_are_refused_before_connecting() {
    for url in [
        "http://10.0.0.1:2773/ping",
        "https://127.0.0.1:2773/ping",
        "http://example.com/ping",
        "http://127.0.0.1:2773@evil.example/ping",
    ] {
        let mut request = get(1, "/");
        request.url = url.to_string();
        let error = LoopbackHttp.send(&request).unwrap_err();
        assert_eq!(error.code(), "aws_connect", "{url}");
    }
}

#[test]
fn a_closed_port_is_a_connect_error() {
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let error = LoopbackHttp.send(&get(port, "/")).unwrap_err();
    assert_eq!(error.code(), "aws_connect");
}

#[test]
fn a_silent_server_hits_the_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_millis(2500));
        drop(stream);
    });
    let mut request = get(port, "/");
    request.timeouts = Timeouts::new(Duration::from_secs(1), Duration::from_secs(1)).unwrap();
    let error = LoopbackHttp.send(&request).unwrap_err();
    handle.join().unwrap();
    assert_eq!(error.code(), "aws_timeout");
}
