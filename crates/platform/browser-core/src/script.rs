//! Page script execution: `<script>` collection and the JS runtime bridge.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use browser_http::{Headers, Method, Request};
use browser_js::Runtime;
use browser_navigation::NavigationDeadline;
use browser_resource_loader::ResourceLoader;
use browser_url::Url;
use browser_web_apis::HostState;
use browser_web_apis::fetch::{FetchError, FetchHandler, FetchRequest, FetchResponse};

use crate::page::Page;

/// Collected inline scripts of a page, in document order.
pub fn collect_inline_scripts(page: &Page) -> Vec<String> {
    let document = page.document.borrow();
    let mut out = Vec::new();
    for node in document.get_elements_by_tag_name("script") {
        if document.has_attr(node, "src") {
            continue; // external scripts are fetched by the browser layer
        }
        let kind = document
            .attr(node, "type")
            .unwrap_or("text/javascript")
            .to_ascii_lowercase();
        if kind.contains("javascript") || kind.is_empty() {
            out.push(document.text_content(node));
        }
    }
    out
}

/// Creates the page's runtime and installs the web platform bindings.
pub fn ensure_runtime(page: &mut Page) -> bool {
    if page.runtime.is_some() {
        return true;
    }
    let mut runtime = Runtime::new();
    let url = page.url.as_ref().map(|url| url.as_str());
    let host = browser_web_apis::install(&mut runtime, page.document.clone(), url);
    // Bridge `getComputedStyle` to the style engine. The computed tree is
    // cloned so the JS resolver can outlive mutable borrows of the page.
    let styles = page.styles.clone();
    host.borrow_mut()
        .set_computed_style_resolver(move |node, name| {
            let style = styles.get(node)?;
            computed_property_css(style, name)
        });
    // Default network bridge for this page. Embedders with a shared loader /
    // navigation budget can replace it by calling `install_fetch_bridge` again.
    let loader = ResourceLoader::default();
    let deadline = NavigationDeadline::new(loader.timeout());
    install_fetch_bridge(&host, loader, deadline);
    page.runtime = Some(runtime);
    page.host = Some(host);
    true
}

/// Installs the host bridge used by `fetch()` and `XMLHttpRequest`.
///
/// The bridge owns a [`ResourceLoader`] (for its HTTP client, cookies and
/// budget) and a [`NavigationDeadline`]. The request timeout already comes from
/// the loader's client config; the deadline additionally bounds the whole
/// script-networking window so one slow request cannot outlive the page load.
///
/// The returned callback is `Send + Sync`: the loader is guarded by a mutex and
/// all bridge types are plain owned data, so an embedder may move the handler
/// to a worker thread without changing the JavaScript-facing API.
pub fn install_fetch_bridge(
    host: &Rc<RefCell<HostState>>,
    loader: ResourceLoader,
    deadline: NavigationDeadline,
) {
    let loader = Arc::new(Mutex::new(loader));
    let handler: FetchHandler = Arc::new(
        move |request: FetchRequest| -> Result<FetchResponse, FetchError> {
            if deadline.is_expired() {
                return Err(FetchError::new("navigation deadline exceeded"));
            }
            let url = Url::parse(&request.url)
                .map_err(|error| FetchError::new(format!("invalid URL: {error}")))?;
            let mut headers = Headers::new();
            for (name, value) in &request.headers {
                headers.append(name, value);
            }
            let http_request = Request {
                method: Method::parse(&request.method),
                url,
                headers,
                body: request.body,
            };
            let mut loader = loader
                .lock()
                .map_err(|_| FetchError::new("fetch bridge lock poisoned"))?;
            let response = loader
                .client_mut()
                .fetch(&http_request)
                .map_err(|error| FetchError::new(error.to_string()))?;
            let response_headers = response
                .headers
                .iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect();
            Ok(FetchResponse::new(
                response.status,
                response.reason.clone(),
                response_headers,
                response.body,
            ))
        },
    );
    host.borrow_mut().set_fetch_handler(handler);
}

fn computed_property_css(style: &browser_style::ComputedStyle, name: &str) -> Option<String> {
    use browser_style::Dim;
    let dim_css = |dim: Dim| match dim {
        Dim::Auto => "auto".to_string(),
        Dim::Px(px) => format!("{px}px"),
        Dim::Percent(percent) => format!("{percent}%"),
        Dim::Calc { percent, px } => format!("calc({percent}% + {px}px)"),
    };
    Some(match name {
        "color" => style.color().to_css(),
        "background-color" => style.background_color().to_css(),
        "display" => format!("{:?}", style.display()).to_ascii_lowercase(),
        "width" => dim_css(style.width()),
        "height" => dim_css(style.height()),
        "font-size" => format!("{}px", style.font_size()),
        "opacity" => style.opacity().to_string(),
        _ => return None,
    })
}

/// Executes the page's inline scripts, returning how many ran and console output.
pub fn execute_in_page(page: &mut Page) -> (usize, Vec<String>) {
    let scripts = collect_inline_scripts(page);
    execute_scripts(page, &scripts)
}

/// Executes the given scripts in order, returning how many ran and console output.
pub fn execute_scripts(page: &mut Page, scripts: &[String]) -> (usize, Vec<String>) {
    if page.scripts_executed {
        return (0, Vec::new());
    }
    if scripts.is_empty() {
        return (0, Vec::new());
    }
    if !ensure_runtime(page) {
        return (0, Vec::new());
    }
    let Some(host) = page.host.clone() else {
        return (0, Vec::new());
    };
    let Some(mut runtime) = page.runtime.take() else {
        return (0, Vec::new());
    };

    page.scripts_executed = true;
    let mut executed = 0;
    for (index, script) in scripts.iter().enumerate() {
        let name = format!("inline-{index}");
        let _ = runtime.eval_named(script, &name);
        let _ = runtime.run_microtasks();
        executed += 1;
    }
    browser_web_apis::drain_timers_now(&mut runtime, &host);
    let _ = runtime.run_microtasks();

    page.runtime = Some(runtime);
    let output = host.borrow_mut().take_console();
    (executed, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_layout::Size;

    #[test]
    fn collects_inline_scripts_only() {
        let mut page = Page::new(Size::default());
        page.load_html(
            "<html><body><script>var a = 1;</script><script src='x.js'></script></body></html>",
            None,
        );
        let scripts = collect_inline_scripts(&page);
        assert_eq!(scripts.len(), 1);
        assert!(scripts[0].contains("var a = 1"));
    }

    #[test]
    fn scripts_mutate_the_dom() {
        let mut page = Page::new(Size::default());
        page.load_html(
            "<html><body><div id='out'>before</div><script>document.getElementById('out').textContent = 'after';</script></body></html>",
            None,
        );
        let (count, _console) = execute_in_page(&mut page);
        assert_eq!(count, 1);
        page.recompute();
        assert_eq!(page.text().trim(), "after");
    }

    struct TestServer {
        addr: std::net::SocketAddr,
    }

    impl TestServer {
        fn start(status: &'static str, reason: &'static str, body: &'static str) -> Self {
            use std::io::{Read, Write};
            use std::net::TcpListener;

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind local server");
            let addr = listener.local_addr().expect("local address");
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nX-Server: mag-test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

    fn drain(runtime: &mut browser_js::Runtime) {
        for _ in 0..8 {
            runtime.run_microtasks().expect("microtasks");
        }
    }

    #[test]
    fn fetch_bridge_uses_the_resource_loader_over_http() {
        let server = TestServer::start("200", "OK", "bridge-body");
        let url = server.url("/api");
        let mut page = Page::new(Size::default());
        let script = format!(
            "var out; fetch('{url}').then(function (r) {{ \
                 out = r.status + '|' + r.ok + '|' + r.headers.get('x-server'); \
             }});"
        );
        page.load_html(
            &format!("<html><body><script>{script}</script></body></html>"),
            Some(browser_url::Url::parse("http://example.test/").unwrap()),
        );
        let (count, _console) = execute_in_page(&mut page);
        assert_eq!(count, 1);
        let runtime = page.runtime.as_mut().expect("runtime");
        drain(runtime);
        assert_eq!(
            runtime.eval("out").expect("result"),
            browser_js::Value::String("200|true|mag-test".to_owned())
        );
    }

    #[test]
    fn fetch_bridge_surfaces_error_statuses() {
        let server = TestServer::start("404", "Not Found", "missing");
        let url = server.url("/missing");
        let mut page = Page::new(Size::default());
        let script = format!(
            "var out; fetch('{url}').then(function (r) {{ \
                 out = r.status + '|' + r.ok; \
             }});"
        );
        page.load_html(
            &format!("<html><body><script>{script}</script></body></html>"),
            Some(browser_url::Url::parse("http://example.test/").unwrap()),
        );
        execute_in_page(&mut page);
        let runtime = page.runtime.as_mut().expect("runtime");
        drain(runtime);
        assert_eq!(
            runtime.eval("out").expect("result"),
            browser_js::Value::String("404|false".to_owned())
        );
    }

    #[test]
    fn fetch_bridge_rejects_after_the_navigation_deadline() {
        use std::time::{Duration, Instant};

        let mut runtime = browser_js::Runtime::new();
        let document = Rc::new(RefCell::new(browser_dom::Document::new()));
        let host = browser_web_apis::install(&mut runtime, document, None);
        let expired = NavigationDeadline::from_parts(
            Instant::now() - Duration::from_secs(2),
            Duration::from_secs(1),
        );
        install_fetch_bridge(&host, ResourceLoader::default(), expired);

        let handler = host.borrow().fetch_handler.clone().expect("handler");
        let result = handler(FetchRequest {
            url: "http://example.test/".to_owned(),
            method: "GET".to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
            signal: None,
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("deadline"));
    }
}
