//! `location` bindings: reading the URL and requesting navigation.

use std::cell::RefCell;
use std::rc::Rc;

use browser_js::{JsError, Runtime, Value};
use browser_url::Url;

use crate::HostState;

/// Installs the `location` object (also reachable as `window.location`).
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    let location = runtime.new_object();

    runtime.define_accessor(
        location.clone(),
        "href",
        {
            let state = state.clone();
            Box::new(move |_runtime, _this, _args| {
                let url = state.borrow().url.clone();
                Ok(match url {
                    Some(url) => Value::String(url),
                    None => Value::String(String::new()),
                })
            })
        },
        {
            let state = state.clone();
            Box::new(move |_runtime, _this, args| {
                let value = args.first().map(Value::to_string_value).unwrap_or_default();
                request_navigation(&state, &value)?;
                Ok(Value::Undefined)
            })
        },
    );

    for (name, extract) in [
        ("pathname", pathname as fn(&Url) -> String),
        ("host", host),
        ("hostname", hostname),
        ("protocol", protocol),
        ("origin", origin),
        ("search", search),
        ("hash", hash),
    ] {
        let state = state.clone();
        runtime.define_accessor(
            location.clone(),
            name,
            {
                let state = state.clone();
                Box::new(move |_runtime, _this, _args| {
                    let url = state.borrow().url.clone();
                    let value = match url.as_deref().and_then(|u| Url::parse(u).ok()) {
                        Some(url) => extract(&url),
                        None => String::new(),
                    };
                    Ok(Value::String(value))
                })
            },
            {
                Box::new(|_runtime, _this, _args| {
                    Err(JsError::type_error("read-only location component"))
                })
            },
        );
        let _ = extract;
    }

    runtime.define_host_method(location.clone(), "assign", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let value = args.first().map(Value::to_string_value).unwrap_or_default();
            request_navigation(&state, &value)?;
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(location.clone(), "replace", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let value = args.first().map(Value::to_string_value).unwrap_or_default();
            request_navigation(&state, &value)?;
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(location.clone(), "reload", {
        let state = state.clone();
        Box::new(move |_runtime, _this, _args| {
            let current = state.borrow().url.clone();
            if let Some(current) = current {
                state.borrow_mut().navigation = Some(current);
            }
            Ok(Value::Undefined)
        })
    });

    runtime.set_global("location", location.clone());
    let global = runtime.global();
    runtime.set_property(global, "location", location);
}

fn request_navigation(state: &Rc<RefCell<HostState>>, target: &str) -> Result<(), JsError> {
    let resolved = {
        let state = state.borrow();
        match state.url.as_deref().and_then(|base| Url::parse(base).ok()) {
            Some(base) => base.join(target).map(|url| url.as_str()).ok(),
            None => Url::parse(target).map(|url| url.as_str()).ok(),
        }
    };
    match resolved {
        Some(url) => {
            state.borrow_mut().navigation = Some(url);
            Ok(())
        }
        None => Err(JsError::new(format!("invalid navigation target: {target}"))),
    }
}

fn pathname(url: &Url) -> String {
    url.path()
}

fn host(url: &Url) -> String {
    let mut host = url.host().unwrap_or("").to_string();
    if let Some(port) = url.port() {
        host.push(':');
        host.push_str(&port.to_string());
    }
    host
}

fn hostname(url: &Url) -> String {
    url.host().unwrap_or("").to_string()
}

fn protocol(url: &Url) -> String {
    format!("{}:", url.scheme())
}

fn origin(url: &Url) -> String {
    url.origin().serialize()
}

fn search(url: &Url) -> String {
    url.query().map(|q| format!("?{q}")).unwrap_or_default()
}

fn hash(url: &Url) -> String {
    url.fragment().map(|f| format!("#{f}")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::Document;

    fn setup(url: Option<&str>) -> (Runtime, Rc<RefCell<HostState>>) {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(Document::new()));
        let state = crate::install(&mut runtime, document, url.map(ToString::to_string));
        (runtime, state)
    }

    #[test]
    fn reads_url_components() {
        let (mut runtime, _state) = setup(Some("https://example.com/a/b?x=1#frag"));
        assert_eq!(
            runtime.eval("location.pathname").unwrap(),
            Value::String("/a/b".to_string())
        );
        assert_eq!(
            runtime.eval("location.search").unwrap(),
            Value::String("?x=1".to_string())
        );
        assert_eq!(
            runtime.eval("location.host").unwrap(),
            Value::String("example.com".to_string())
        );
        assert_eq!(
            runtime.eval("location.origin").unwrap(),
            Value::String("https://example.com:443".to_string())
        );
    }

    #[test]
    fn assignment_requests_navigation() {
        let (mut runtime, state) = setup(Some("https://example.com/dir/page"));
        runtime.eval("location.href = 'next'").unwrap();
        assert_eq!(
            state.borrow().navigation.as_deref(),
            Some("https://example.com/dir/next")
        );
    }

    #[test]
    fn reload_navigates_to_the_current_url() {
        let (mut runtime, state) = setup(Some("https://example.com/dir/page"));
        runtime.eval("location.reload()").unwrap();
        assert_eq!(
            state.borrow().navigation.as_deref(),
            Some("https://example.com/dir/page")
        );
    }
}
