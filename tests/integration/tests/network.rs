//! Network integration tests against a local HTTP server.
//!
//! The server is a real `TcpListener` on 127.0.0.1; nothing leaves the machine.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use browser_core::{Browser, Settings};

struct Server {
    address: String,
    handle: Option<thread::JoinHandle<Vec<String>>>,
}

impl Server {
    /// Starts a server that answers `responses` in order, one connection each.
    fn start(responses: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut buffer = [0_u8; 8192];
                let read = stream.read(&mut buffer).unwrap_or(0);
                requests.push(String::from_utf8_lossy(&buffer[..read]).into_owned());
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
            requests
        });
        Self {
            address,
            handle: Some(handle),
        }
    }

    fn join(mut self) -> Vec<String> {
        self.handle
            .take()
            .map(|h| h.join().unwrap())
            .unwrap_or_default()
    }
}

fn settings() -> Settings {
    let mut settings = Settings::headless();
    settings.enable_network = true;
    settings
}

#[test]
fn follows_redirects_and_sends_cookies() {
    let redirect = "HTTP/1.1 302 Found\r\n\
                    Location: /page\r\n\
                    Set-Cookie: session=abc; Path=/; HttpOnly\r\n\
                    Content-Length: 0\r\n\
                    Connection: close\r\n\r\n";
    let page = "<html><head><title>Redirected</title></head>\
                <body><p>arrived</p></body></html>";
    let body = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{}",
        page.len(),
        page
    );
    let server = Server::start(vec![redirect.to_string(), body]);

    let mut browser = Browser::new(settings());
    let snapshot = browser.navigate(&server.address).unwrap();
    assert_eq!(snapshot.title, "Redirected");
    assert!(snapshot.text.contains("arrived"));

    let requests = server.join();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET / HTTP/1.1"));
    assert!(requests[1].starts_with("GET /page HTTP/1.1"));
    assert!(
        requests[1]
            .to_ascii_lowercase()
            .contains("cookie: session=abc"),
        "cookie missing from second request:\n{}",
        requests[1]
    );
    assert_eq!(browser.client_mut().cookies_mut().len(), 1);
}

#[test]
fn http_errors_render_an_error_page() {
    let response = "HTTP/1.1 404 Not Found\r\n\
                    Content-Length: 0\r\n\
                    Connection: close\r\n\r\n";
    let server = Server::start(vec![response.to_string()]);
    let mut browser = Browser::new(settings());
    let snapshot = browser.navigate(&server.address).unwrap();
    assert_eq!(snapshot.title, "404");
    assert!(snapshot.text.contains("404"));
    let _ = server.join();
}

#[test]
fn chunked_responses_are_decoded() {
    let html = "<html><head><title>Chunked</title></head><body>ok</body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html\r\n\
         Transfer-Encoding: chunked\r\n\
         Connection: close\r\n\r\n\
         {:x}\r\n{}\r\n0\r\n\r\n",
        html.len(),
        html
    );
    let server = Server::start(vec![response]);
    let mut browser = Browser::new(settings());
    let snapshot = browser.navigate(&server.address).unwrap();
    assert_eq!(snapshot.title, "Chunked");
    let _ = server.join();
}

#[test]
fn network_can_be_disabled() {
    let mut browser = Browser::new(Settings::headless());
    let err = browser.navigate("http://127.0.0.1:9/").unwrap_err();
    assert!(matches!(err, browser_core::BrowserError::NetworkDisabled));
}
