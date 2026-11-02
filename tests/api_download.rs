use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};

use pipetrain::config::ApiConfig;
use pipetrain::github::{ApiError, Client, Download};

const FAKE: &str = "ghp_FAKE0123456789";

struct Canned {
    status: &'static str,
    headers: Vec<(&'static str, String)>,
    body: String,
}

fn canned(status: &'static str, body: &str) -> Canned {
    Canned {
        status,
        headers: Vec::new(),
        body: body.to_string(),
    }
}

fn serve(make: impl FnOnce(&str) -> Vec<Canned>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let replies = make(&base);
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            seen.push(read_request(&mut stream));
            stream.write_all(render(&reply).as_bytes()).unwrap();
        }
        seen
    });
    (base, handle)
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).unwrap();
        data.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&data).to_string();
        if read == 0 || text.contains("\r\n\r\n") {
            return text;
        }
    }
}

fn render(reply: &Canned) -> String {
    let mut text = format!("HTTP/1.1 {}\r\n", reply.status);
    for (name, value) in &reply.headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    text.push_str("Connection: close\r\n\r\n");
    text.push_str(&reply.body);
    text
}

fn client(base: &str, token: &str) -> Client {
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    Client::with_base_url(token.to_string(), &cfg, false, base)
}

fn redirect(to: &str) -> Canned {
    let mut reply = canned("302 Found", "");
    reply.headers = vec![("location", to.to_string())];
    reply
}

#[test]
fn follows_a_redirect_on_the_same_origin() {
    let (base, server) =
        serve(|base| vec![redirect(&format!("{base}/blob")), canned("200 OK", "BYTES")]);
    let bytes = client(&base, FAKE)
        .download(&format!("{base}/asset"), 1024)
        .unwrap();
    let seen = server.join().unwrap();
    assert_eq!(bytes, b"BYTES");
    assert!(seen[0].starts_with("GET /asset HTTP/1.1"), "{}", seen[0]);
    assert!(seen[1].starts_with("GET /blob HTTP/1.1"), "{}", seen[1]);
    assert!(seen[1].to_lowercase().contains("authorization: bearer"));
}

#[test]
fn an_anonymous_download_sends_no_authorization() {
    let (base, server) = serve(|_| vec![canned("200 OK", "BYTES")]);
    client(&base, "")
        .download(&format!("{base}/asset"), 1024)
        .unwrap();
    let seen = server.join().unwrap();
    assert!(!seen[0].to_lowercase().contains("authorization"));
}

#[test]
fn a_body_over_the_limit_is_too_large() {
    let (base, server) = serve(|_| vec![canned("200 OK", &"x".repeat(100))]);
    let error = client(&base, FAKE)
        .download(&format!("{base}/asset"), 10)
        .unwrap_err();
    server.join().unwrap();
    assert_eq!(error, Download::TooLarge);
}

#[test]
fn a_missing_file_is_not_found() {
    let (base, server) = serve(|_| vec![canned("404 Not Found", "{}")]);
    let url = format!("{base}/asset");
    let error = client(&base, FAKE).download(&url, 1024).unwrap_err();
    server.join().unwrap();
    assert_eq!(error, Download::Api(ApiError::NotFound { what: url }));
}

#[test]
fn a_redirect_to_plain_http_elsewhere_is_refused() {
    let (base, server) = serve(|_| vec![redirect("http://example.invalid/x")]);
    let error = client(&base, FAKE)
        .download(&format!("{base}/asset"), 1024)
        .unwrap_err();
    server.join().unwrap();
    assert!(
        matches!(error, Download::Api(ApiError::Other { .. })),
        "{error:?}"
    );
}

#[test]
fn a_redirect_loop_stops() {
    let (base, server) = serve(|base| (0..6).map(|_| redirect(&format!("{base}/again"))).collect());
    let error = client(&base, FAKE)
        .download(&format!("{base}/asset"), 1024)
        .unwrap_err();
    server.join().unwrap();
    assert!(
        matches!(error, Download::Api(ApiError::Other { .. })),
        "{error:?}"
    );
}
