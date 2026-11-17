//! Resolving command-line and address-bar input to a navigable URL.

/// Turns a path, bare domain or URL into something [`browser_core::Browser`]
/// understands.
///
/// Local files become `file://` URLs (canonicalised when possible). Anything
/// with a scheme is passed through, and bare hosts are left to the browser's
/// URL normalisation.
pub fn resolve_target(target: &str) -> String {
    let trimmed = target.trim();
    if trimmed.contains("://") || trimmed.starts_with("about:") {
        return trimmed.to_string();
    }
    let path = std::path::Path::new(trimmed);
    if path.exists()
        && let Ok(canonical) = std::fs::canonicalize(path)
    {
        return format!("file://{}", canonical.display());
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_pass_through_and_files_become_file_urls() {
        assert_eq!(resolve_target("https://a.test/x"), "https://a.test/x");
        assert_eq!(resolve_target("about:blank"), "about:blank");
        let dir = std::env::temp_dir().join(format!("mag-target-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.html");
        std::fs::write(&path, "<html></html>").unwrap();
        let resolved = resolve_target(&path.display().to_string());
        assert!(resolved.starts_with("file://"));
        assert!(resolved.ends_with("p.html"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
