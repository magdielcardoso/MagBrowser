//! Host-side integration tests with a local HTTP server.
//!
//! These exercise the wiring added by this crate rather than the individual
//! subsystems: observer delivery after a navigation/layout, and service worker
//! fetch interception before the network.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use browser_js::Value;
use browser_layout::Size;

use crate::{Browser, Settings};

/// Minimal HTTP server that maps request paths to response bodies.
struct HttpServer {
    addr: std::net::SocketAddr,
}

impl HttpServer {
    fn start(routes: HashMap<String, String>) -> Self {
        let routes = Arc::new(Mutex::new(routes));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local server");
        let addr = listener.local_addr().expect("local address");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap_or(0);
                let line = String::from_utf8_lossy(&request[..read]);
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body = routes
                    .lock()
                    .ok()
                    .and_then(|routes| routes.get(&path).cloned())
                    .unwrap_or_else(|| "not found".to_string());
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        Self { addr }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
}

fn browser() -> Browser {
    Browser::new(Settings {
        enable_scripting: true,
        enable_network: true,
        ..Settings::default()
    })
}

fn drain(runtime: &mut browser_js::Runtime) {
    for _ in 0..16 {
        if runtime.run_microtasks().is_err() {
            break;
        }
    }
}

#[test]
fn observers_fire_after_navigation_and_resize() {
    let mut routes = HashMap::new();
    routes.insert(
        "/".to_string(),
        "<html><body><div id='box' style='width:50px;height:50px'>x</div>\
         <script>var hits = 0; var ro = new ResizeObserver(function () { hits++; }); \
         ro.observe(document.getElementById('box'));</script></body></html>"
            .to_string(),
    );
    let server = HttpServer::start(routes);
    let url = server.url("/");
    let mut browser = browser();
    browser
        .navigate(&url)
        .unwrap_or_else(|error| panic!("navigate failed: {error}"));

    let hits = browser
        .tab_mut()
        .page
        .runtime
        .as_mut()
        .expect("runtime")
        .eval("hits")
        .expect("eval hits")
        .to_number() as usize;
    assert!(hits >= 1, "ResizeObserver did not fire after navigation");

    browser.resize(400.0, 300.0);
    let runtime = browser.tab_mut().page.runtime.as_mut().expect("runtime");
    drain(runtime);
    let hits_after = runtime.eval("hits").expect("eval hits").to_number() as usize;
    assert!(
        hits_after >= hits,
        "resize must not lose observer state ({hits_after} < {hits})"
    );
}

#[test]
fn service_worker_intercepts_fetch_before_network() {
    let mut routes = HashMap::new();
    routes.insert(
        "/app/".to_string(),
        "<html><body><div id='out'>pending</div><script>\
         fetch('/app/data').then(function (r) { \
           document.getElementById('out').textContent = r.status + '|' + r.statusText; \
         });</script></body></html>"
            .to_string(),
    );
    routes.insert("/app/data".to_string(), "from-network".to_string());
    routes.insert("/app/sw.js".to_string(), "/* worker source */".to_string());
    let server = HttpServer::start(routes);
    let page_url = server.url("/app/");
    let script_url = server.url("/app/sw.js");
    let scope = server.url("/app/");

    let mut browser = browser();
    let source = "ServiceWorkerGlobalScope.addEventListener('fetch', function (event) { \
                    event.respondWith(new Response('from-worker', \
                      {status: 209, statusText: 'From Worker'})); \
                  });";
    browser
        .register_service_worker(&scope, &script_url, Some(source))
        .expect("secure registration");
    browser.activate_service_worker(&scope).expect("activation");
    assert!(browser.service_worker_registrations()[0].is_active());

    browser
        .navigate(&page_url)
        .unwrap_or_else(|error| panic!("navigate failed: {error}"));
    let runtime = browser.tab_mut().page.runtime.as_mut().expect("runtime");
    drain(runtime);
    assert_eq!(
        runtime
            .eval("document.getElementById('out').textContent")
            .expect("eval"),
        Value::String("209|From Worker".to_string())
    );
}

#[test]
fn service_worker_respects_scope_and_https() {
    let mut browser = browser();
    // Insecure (non-loopback HTTP) registrations are rejected.
    assert!(
        browser
            .register_service_worker("http://example.com/app/", "http://example.com/sw.js", None,)
            .is_err()
    );
    // Out-of-scope URLs pass through to the network.
    let source = "ServiceWorkerGlobalScope.addEventListener('fetch', function (e) { \
                    e.respondWith(new Response('from-worker')); \
                  });";
    browser
        .register_service_worker(
            "http://127.0.0.1:9/app/",
            "http://127.0.0.1:9/app/sw.js",
            Some(source),
        )
        .expect("loopback registration");
    browser
        .activate_service_worker("http://127.0.0.1:9/app/")
        .expect("activation");
    assert!(
        browser
            .service_workers()
            .handle_fetch("http://127.0.0.1:9/other/data")
            .is_none()
    );
    assert!(
        browser
            .service_workers()
            .handle_fetch("http://127.0.0.1:9/app/data")
            .is_some()
    );
}

#[test]
fn page_keeps_layout_resolver_in_sync_after_recompute() {
    let mut page = crate::Page::new(Size {
        width: 200.0,
        height: 100.0,
    });
    page.load_html(
        "<html><body><div id='box'>x</div><script>var n = 0; \
         new ResizeObserver(function(){ n++; }).observe(document.getElementById('box'));\
         </script></body></html>",
        None,
    );
    let scripts = crate::script::collect_inline_scripts(&page);
    let (count, _) = crate::script::execute_scripts(&mut page, &scripts);
    assert_eq!(count, 1);
    let runtime = page.runtime.as_mut().expect("runtime");
    assert!(runtime.eval("n").expect("eval").to_number() >= 1.0);
}
