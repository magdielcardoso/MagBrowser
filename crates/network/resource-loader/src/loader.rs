//! Loading resources over HTTP(S) and from `file:` URLs.

use std::time::Duration;

use browser_http::{Client, ClientConfig, HttpError, Response};
use browser_url::{Url, UrlError};

use crate::fetch::Resource;
use crate::mime;

/// Errors raised while loading a resource.
#[derive(Debug)]
pub enum LoadError {
    /// Invalid or unsupported URL.
    Url(UrlError),
    /// Network failure.
    Http(HttpError),
    /// The scheme cannot be loaded.
    UnsupportedScheme(String),
    /// Local file error.
    Io(String),
    /// HTTP status is not successful.
    Status(u16),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(err) => write!(f, "invalid URL: {err}"),
            Self::Http(err) => write!(f, "network error: {err}"),
            Self::UnsupportedScheme(scheme) => write!(f, "unsupported scheme: {scheme}"),
            Self::Io(err) => write!(f, "file error: {err}"),
            Self::Status(status) => write!(f, "HTTP {status}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<UrlError> for LoadError {
    fn from(value: UrlError) -> Self {
        Self::Url(value)
    }
}

impl From<HttpError> for LoadError {
    fn from(value: HttpError) -> Self {
        Self::Http(value)
    }
}

/// Loads subresources (stylesheets, scripts, images) for a page.
pub struct ResourceLoader {
    client: Client,
    allowed_schemes: Vec<String>,
}

impl Default for ResourceLoader {
    fn default() -> Self {
        Self::new(ClientConfig::default())
    }
}

impl ResourceLoader {
    /// Creates a loader with the given HTTP configuration.
    pub fn new(config: ClientConfig) -> Self {
        Self {
            client: Client::new(config),
            allowed_schemes: vec!["http".into(), "https".into(), "file".into()],
        }
    }

    /// Restricts the schemes that may be loaded (for example `["https"]`).
    pub fn set_allowed_schemes(&mut self, schemes: &[&str]) {
        self.allowed_schemes = schemes.iter().map(|s| (*s).to_string()).collect();
    }

    /// The underlying HTTP client.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The underlying HTTP client, mutably.
    pub fn client_mut(&mut self) -> &mut Client {
        &mut self.client
    }

    /// Loads a URL into a [`Resource`].
    pub fn load(&mut self, url: &Url) -> Result<Resource, LoadError> {
        if !self
            .allowed_schemes
            .iter()
            .any(|scheme| scheme == url.scheme())
        {
            return Err(LoadError::UnsupportedScheme(url.scheme().to_string()));
        }
        match url.scheme() {
            "http" | "https" => {
                let response: Response = self.client.get(url.clone())?;
                if !response.is_success() {
                    return Err(LoadError::Status(response.status));
                }
                let mime = response
                    .media_type()
                    .unwrap_or_else(|| mime::from_path(&url.path()).to_string());
                let charset = response.charset();
                let status = response.status;
                let mut resource = Resource::new(url.clone(), status, &mime, response.body);
                resource.charset = charset;
                Ok(resource)
            }
            "file" => {
                let path = format!("/{}", url.path_segments().join("/"));
                let bytes =
                    std::fs::read(&path).map_err(|err| LoadError::Io(format!("{path}: {err}")))?;
                let mime = mime::sniff_image(&bytes)
                    .map_or_else(|| mime::from_path(&path).to_string(), str::to_string);
                Ok(Resource::new(url.clone(), 200, &mime, bytes))
            }
            other => Err(LoadError::UnsupportedScheme(other.to_string())),
        }
    }

    /// Loads a URL and decodes it as text.
    pub fn load_text(&mut self, url: &Url) -> Result<String, LoadError> {
        Ok(self.load(url)?.text())
    }

    /// Sets the connect/read timeout.
    pub fn set_timeout(&mut self, timeout: Duration) {
        let mut config = self.client.config().clone();
        config.timeout = timeout;
        self.client = Client::new(config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_local_files() {
        let dir = std::env::temp_dir().join(format!("mag-loader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("style.css");
        std::fs::write(&path, "body { color: red }").unwrap();
        let mut loader = ResourceLoader::default();
        let url = Url::parse(&format!("file://{}", path.display())).unwrap();
        let resource = loader.load(&url).unwrap();
        assert_eq!(resource.mime, "text/css");
        assert!(resource.is_css());
        assert_eq!(resource.text(), "body { color: red }");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_disallowed_schemes() {
        let mut loader = ResourceLoader::default();
        loader.set_allowed_schemes(&["https"]);
        let url = Url::parse("http://example.com/").unwrap();
        assert!(matches!(
            loader.load(&url),
            Err(LoadError::UnsupportedScheme(_))
        ));
    }
}
