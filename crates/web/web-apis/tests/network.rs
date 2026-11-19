//! End-to-end coverage for the network scripting APIs.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use browser_dom::Document;
use browser_js::{Runtime, Value};
use browser_url::Url;
use browser_web_apis::HostState;
use browser_web_apis::fetch::{FetchError, FetchHandler, FetchRequest, FetchResponse};

struct TestServer {
    addr: SocketAddr,
}

impl TestServer {
    fn start(status: &'static str, reason: &'static str, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local server");
        let addr = listener.local_addr().expect("local address");
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nX-Server: mag-test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request);
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

/// HTTP fetch performed by the host handler with plain `std::net` (web-apis
/// deliberately has no HTTP client dependency).
fn tcp_fetch(request: FetchRequest) -> Result<FetchResponse, FetchError> {
    let url = Url::parse(&request.url).map_err(|error| FetchError::new(error.to_string()))?;
    let host = url
        .host()
        .ok_or_else(|| FetchError::new("request URL has no host"))?;
    let port = url.port_or_default().unwrap_or(80);
    let mut stream =
        TcpStream::connect((host, port)).map_err(|error| FetchError::new(error.to_string()))?;
    let mut wire = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        request.method,
        url.request_target(),
        host
    );
    for (name, value) in &request.headers {
        wire.push_str(&format!("{name}: {value}\r\n"));
    }
    wire.push_str("\r\n");
    stream
        .write_all(wire.as_bytes())
        .map_err(|error| FetchError::new(error.to_string()))?;
    stream
        .write_all(&request.body)
        .map_err(|error| FetchError::new(error.to_string()))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|error| FetchError::new(error.to_string()))?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| FetchError::new("malformed HTTP response"))?;
    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.splitn(3, ' ');
    let _version = parts.next();
    let status: u16 = parts
        .next()
        .unwrap_or("0")
        .parse()
        .map_err(|_| FetchError::new("malformed status line"))?;
    let reason = parts.next().unwrap_or("").to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect();
    Ok(FetchResponse::new(
        status,
        reason,
        headers,
        body.as_bytes().to_vec(),
    ))
}

fn setup(handler: FetchHandler, url: &str) -> (Runtime, Rc<RefCell<HostState>>) {
    let mut runtime = Runtime::new();
    let document = Rc::new(RefCell::new(Document::new()));
    let host = browser_web_apis::install(&mut runtime, document, Some(url.to_owned()));
    host.borrow_mut().set_fetch_handler(handler);
    (runtime, host)
}

fn settle(runtime: &mut Runtime) {
    for _ in 0..8 {
        runtime.run_microtasks().expect("microtasks");
    }
}

fn value_of(runtime: &mut Runtime, expression: &str) -> Value {
    runtime.eval(expression).unwrap_or_else(|error| {
        panic!("eval of {expression:?} failed: {error}");
    })
}

#[test]
fn fetch_exposes_response_members_and_body_readers() {
    let server = TestServer::start("200", "OK", r#"{"ok":true,"n":7}"#);
    let url = server.url("/data");
    let handler: FetchHandler = Arc::new(tcp_fetch);
    let (mut runtime, _host) = setup(handler, "http://example.test/page");

    let script = format!(
        "var result; fetch('{}').then(function(r) {{ result = r; }});",
        url
    );
    runtime.eval(&script).expect("fetch call");
    settle(&mut runtime);

    assert_eq!(value_of(&mut runtime, "result.ok"), Value::Bool(true));
    assert_eq!(
        value_of(&mut runtime, "result.status"),
        Value::Number(200.0)
    );
    assert_eq!(
        value_of(&mut runtime, "result.statusText"),
        Value::String("OK".to_owned())
    );
    assert_eq!(
        value_of(&mut runtime, "result.headers.get('content-type')"),
        Value::String("application/json".to_owned())
    );
    assert_eq!(
        value_of(&mut runtime, "result.headers.get('x-server')"),
        Value::String("mag-test".to_owned())
    );

    runtime
        .eval(
            "var text, json, buffer; \
             result.text().then(function (v) { text = v; }); \
             result.json().then(function (v) { json = v; }); \
             result.arrayBuffer().then(function (v) { buffer = v; });",
        )
        .expect("body readers");
    settle(&mut runtime);

    assert_eq!(
        value_of(&mut runtime, "text"),
        Value::String(r#"{"ok":true,"n":7}"#.to_owned())
    );
    assert_eq!(value_of(&mut runtime, "json.n"), Value::Number(7.0));
    assert_eq!(
        value_of(&mut runtime, "buffer.byteLength"),
        Value::Number(17.0)
    );
}

#[test]
fn fetch_passes_method_headers_and_body_to_the_host() {
    let captured: Arc<Mutex<Vec<FetchRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let handler: FetchHandler = Arc::new(move |request| {
        sink.lock().expect("lock").push(request);
        Ok(FetchResponse::new(
            201,
            "Created",
            vec![("X-Reply".to_owned(), "yes".to_owned())],
            b"done".to_vec(),
        ))
    });
    let (mut runtime, _host) = setup(handler, "http://example.test/page");
    runtime
        .eval(
            "var status; \
             fetch('/submit', { \
                 method: 'post', \
                 headers: { 'X-Token': 'abc' }, \
                 body: 'hello=world' \
             }).then(function (r) { status = r.status; });",
        )
        .expect("fetch init");
    settle(&mut runtime);

    assert_eq!(value_of(&mut runtime, "status"), Value::Number(201.0));
    let requests = captured.lock().expect("lock");
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.url, "http://example.test/submit");
    assert_eq!(request.method, "POST");
    assert_eq!(request.body, b"hello=world");
    assert!(
        request
            .headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("X-Token") && value == "abc")
    );
    assert!(
        request
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("Content-Type"))
    );
}

#[test]
fn fetch_rejects_when_the_host_fails() {
    let handler: FetchHandler = Arc::new(|_| Err(FetchError::new("network unreachable")));
    let (mut runtime, _host) = setup(handler, "http://example.test/page");
    runtime
        .eval("var error; fetch('/x').catch(function (e) { error = e.message; });")
        .expect("fetch rejection");
    settle(&mut runtime);
    assert_eq!(
        value_of(&mut runtime, "error"),
        Value::String("network unreachable".to_owned())
    );
}

#[test]
fn url_search_params_supports_get_set_append_and_tostring() {
    let handler: FetchHandler =
        Arc::new(|request| Ok(FetchResponse::new(200, "OK", Vec::new(), request.body)));
    let (mut runtime, _host) = setup(handler, "http://example.test/page");

    runtime
        .eval(
            "var p = new URLSearchParams('a=1&b=2&a=3'); \
             var first = p.get('a'); \
             p.append('b', '4'); \
             var appended = p.toString(); \
             p.set('a', '9'); \
             var updated = p.toString(); \
             var encoded = new URLSearchParams({ q: 'hello world' }).toString(); \
             var fromParams = new URLSearchParams('x=1'); \
             fromParams.append('y', '2'); \
             var chained = fromParams.toString();",
        )
        .expect("URLSearchParams");

    assert_eq!(value_of(&mut runtime, "first"), Value::String("1".into()));
    assert_eq!(
        value_of(&mut runtime, "appended"),
        Value::String("a=1&b=2&a=3&b=4".into())
    );
    assert_eq!(
        value_of(&mut runtime, "updated"),
        Value::String("a=9&b=2&b=4".into())
    );
    assert_eq!(
        value_of(&mut runtime, "encoded"),
        Value::String("q=hello+world".into())
    );
    assert_eq!(
        value_of(&mut runtime, "chained"),
        Value::String("x=1&y=2".into())
    );
}

#[test]
fn abort_controller_propagates_to_fetch() {
    let handler: FetchHandler =
        Arc::new(|_| Ok(FetchResponse::new(200, "OK", Vec::new(), b"ok".to_vec())));
    let (mut runtime, _host) = setup(handler, "http://example.test/page");
    runtime
        .eval(
            "var controller = new AbortController(); \
             var fired = 0; \
             controller.signal.addEventListener('abort', function () { fired++; }); \
             controller.abort(); \
             var aborted = controller.signal.aborted; \
             var errorName; \
             fetch('/x', { signal: controller.signal }) \
                 .catch(function (e) { errorName = e.name; });",
        )
        .expect("abort controller");
    settle(&mut runtime);

    assert_eq!(value_of(&mut runtime, "aborted"), Value::Bool(true));
    assert_eq!(value_of(&mut runtime, "fired"), Value::Number(1.0));
    assert_eq!(
        value_of(&mut runtime, "errorName"),
        Value::String("AbortError".into())
    );
}

#[test]
fn xml_http_request_supports_synchronous_mode() {
    let server = TestServer::start("200", "OK", "sync-body");
    let url = server.url("/sync");
    let seen_headers: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let captured = seen_headers.clone();
    let handler: FetchHandler = Arc::new(move |request| {
        *captured.lock().expect("lock") = request.headers.clone();
        tcp_fetch(request)
    });
    let (mut runtime, _host) = setup(handler, "http://example.test/page");

    runtime
        .eval(&format!(
            "var xhr = new XMLHttpRequest(); \
             var states = []; \
             var loaded; \
             xhr.onreadystatechange = function () {{ states.push(xhr.readyState); }}; \
             xhr.onload = function () {{ loaded = xhr.responseText + '|' + xhr.status; }}; \
             xhr.open('GET', '{url}', false); \
             xhr.setRequestHeader('X-From-XHR', 'yes'); \
             xhr.send();"
        ))
        .expect("sync XHR");

    assert!(
        seen_headers
            .lock()
            .expect("lock")
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("X-From-XHR") && value == "yes")
    );

    assert_eq!(
        value_of(&mut runtime, "states.join(',')"),
        Value::String("1,4".into())
    );
    assert_eq!(
        value_of(&mut runtime, "loaded"),
        Value::String("sync-body|200".into())
    );
    assert_eq!(
        value_of(&mut runtime, "xhr.statusText"),
        Value::String("OK".into())
    );
}

#[test]
fn xml_http_request_defers_async_callbacks_to_microtasks() {
    let server = TestServer::start("200", "OK", "async-body");
    let url = server.url("/async");
    let handler: FetchHandler = Arc::new(tcp_fetch);
    let (mut runtime, _host) = setup(handler, "http://example.test/page");

    runtime
        .eval(&format!(
            "var xhr = new XMLHttpRequest(); \
             var loaded; \
             xhr.onload = function () {{ loaded = xhr.responseText; }}; \
             xhr.open('GET', '{url}'); \
             xhr.send();"
        ))
        .expect("async XHR");
    assert_eq!(
        value_of(&mut runtime, "typeof loaded"),
        Value::String("undefined".into())
    );
    settle(&mut runtime);
    assert_eq!(
        value_of(&mut runtime, "loaded"),
        Value::String("async-body".into())
    );
    assert_eq!(value_of(&mut runtime, "xhr.readyState"), Value::Number(4.0));
}

#[test]
fn existing_apis_keep_working() {
    let handler: FetchHandler =
        Arc::new(|_| Ok(FetchResponse::new(200, "OK", Vec::new(), Vec::new())));
    let (mut runtime, host) = setup(handler, "http://example.test/page");
    runtime
        .eval(
            "document.title = 'still here'; \
             var fired = false; \
             setTimeout(function () { fired = true; }, 0);",
        )
        .expect("existing APIs");
    assert_eq!(
        value_of(&mut runtime, "document.title"),
        Value::String("still here".into())
    );
    browser_web_apis::drain_timers_now(&mut runtime, &host);
    settle(&mut runtime);
    assert_eq!(value_of(&mut runtime, "fired"), Value::Bool(true));
}
