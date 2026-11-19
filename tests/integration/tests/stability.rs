//! Stability tests for heavy pages.
//!
//! A page that loops forever in JavaScript, nests thousands of elements,
//! carries a huge stylesheet or answers slowly must not make navigation hang.
//! Every navigation runs on a worker thread and the test waits with
//! `recv_timeout`, so a regression fails with a clear message instead of
//! hanging CI. After the heavy page, the same `Browser` is navigated again to
//! prove it is still usable.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use browser_core::{Browser, Settings};

/// Wall-clock budget for a heavy navigation *and* the follow-up one.
const NAVIGATION_BUDGET: Duration = Duration::from_secs(30);

/// Titles produced by the two navigations, each possibly failing.
type NavigationTitles = (Result<String, String>, Result<String, String>);

/// One canned HTTP response.
struct Reply {
    body: String,
    delay: Duration,
}

impl Reply {
    fn html(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    fn slow(body: impl Into<String>, delay: Duration) -> Self {
        Self {
            body: body.into(),
            delay,
        }
    }
}

/// A tiny scripted server bound to an ephemeral local port. Each accepted
/// connection consumes the next reply, so a test can serve a heavy page and
/// then a small control page on the same address.
struct Server {
    address: String,
    handle: Option<thread::JoinHandle<Vec<String>>>,
}

impl Server {
    fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local server");
        let address = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for reply in replies {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut buffer = [0_u8; 8192];
                let read = stream.read(&mut buffer).unwrap_or(0);
                requests.push(String::from_utf8_lossy(&buffer[..read]).into_owned());
                if !reply.delay.is_zero() {
                    thread::sleep(reply.delay);
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n{}",
                    reply.body.len(),
                    reply.body
                );
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

    fn url(&self) -> &str {
        &self.address
    }

    fn join(mut self) -> Vec<String> {
        self.handle
            .take()
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default()
    }
}

fn settings() -> Settings {
    let mut settings = Settings::headless();
    settings.enable_network = true;
    settings
}

/// Control page served after the heavy one. A successful second navigation
/// proves the browser did not get stuck parsing or executing the first page.
fn control_page() -> String {
    "<!DOCTYPE html><html><head><title>Still alive</title></head>\
     <body><p>Usable</p></body></html>"
        .to_string()
}

/// Runs two navigations on a worker thread: `url` (heavy) and then a control
/// page. Returns both titles, or a descriptive timeout error.
fn navigate_then_check(url: &str) -> Result<NavigationTitles, String> {
    let (sender, receiver) = mpsc::channel();
    let url = url.to_string();
    let started = Instant::now();
    let worker = thread::spawn(move || {
        let mut browser = Browser::new(settings());
        let first = browser
            .navigate(&url)
            .map(|snapshot| snapshot.title)
            .map_err(|error| error.to_string());
        let second = browser
            .navigate(&format!("{url}/still-alive"))
            .map(|snapshot| snapshot.title)
            .map_err(|error| error.to_string());
        let _ = sender.send((first, second));
    });

    let outcome = receiver.recv_timeout(NAVIGATION_BUDGET);
    if outcome.is_ok() {
        let _ = worker.join();
    }
    let elapsed = started.elapsed();
    outcome.map_err(|error| {
        format!(
            "navigation did not finish within {NAVIGATION_BUDGET:?} \
             (elapsed {elapsed:?}): {error}"
        )
    })
}

/// Shared assertions: the heavy page loads and the browser is still usable.
fn assert_stable(server: Server, expected_title: &str) {
    let (first, second) = navigate_then_check(server.url()).expect("navigation hung");
    assert!(
        first.is_ok(),
        "heavy page failed to load: {:?}",
        first.as_ref().err()
    );
    assert_eq!(
        first.unwrap(),
        expected_title,
        "unexpected document after heavy page"
    );
    assert_eq!(
        second.ok().as_deref(),
        Some("Still alive"),
        "browser was not usable for the follow-up navigation"
    );
    let _ = server.join();
}

#[test]
fn infinite_loop_script_does_not_hang_navigation() {
    let heavy = "<!DOCTYPE html><html><head><title>Spinner</title></head>\
                 <body><p>spinning</p><script>while(true){}</script></body></html>";
    let server = Server::start(vec![Reply::html(heavy), Reply::html(control_page())]);
    assert_stable(server, "Spinner");
}

#[test]
fn deeply_nested_html_does_not_hang_navigation() {
    let depth = 1_500;
    let heavy = format!(
        "<!DOCTYPE html><html><head><title>Deep</title></head>\
         <body>{}x{}</body></html>",
        "<div>".repeat(depth),
        "</div>".repeat(depth)
    );
    let server = Server::start(vec![Reply::html(heavy), Reply::html(control_page())]);
    assert_stable(server, "Deep");
}

#[test]
fn huge_inline_css_does_not_hang_navigation() {
    let rules: String = (0..10_000)
        .map(|index| format!(".c{index} {{ color: rgb({index}, 0, 0); padding: 1px; }}"))
        .collect();
    let heavy = format!(
        "<!DOCTYPE html><html><head><title>Heavy CSS</title><style>{rules}</style>\
         </head><body><div class='c7'>styled</div></body></html>"
    );
    let server = Server::start(vec![Reply::html(heavy), Reply::html(control_page())]);
    assert_stable(server, "Heavy CSS");
}

#[test]
fn slow_response_does_not_hang_navigation() {
    let heavy = "<!DOCTYPE html><html><head><title>Slow</title></head>\
                 <body><p>arrived</p></body></html>";
    let server = Server::start(vec![
        Reply::slow(heavy, Duration::from_millis(800)),
        Reply::html(control_page()),
    ]);
    assert_stable(server, "Slow");
}
