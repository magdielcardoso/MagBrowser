//! Filesystem access, optionally restricted to a sandbox root.
//!
//! [`StdFileSystem`] is a thin wrapper over [`std::fs`]. [`SandboxedFileSystem`]
//! wraps any other filesystem and refuses every path that resolves outside its
//! root, including `..` traversal, absolute paths outside the root, and symlinks
//! that escape it. Escape attempts fail with [`PlatformError::PermissionDenied`].

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::PlatformError;

/// Filesystem contract used by the engine.
pub trait FileSystem {
    /// Reads a file into memory.
    fn read(&self, path: &Path) -> Result<Vec<u8>, PlatformError>;

    /// Writes a file, creating or truncating it.
    fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<(), PlatformError>;

    /// Whether `path` exists and is readable as a file or directory.
    fn exists(&self, path: &Path) -> bool;

    /// Fully resolves `path` (including symlinks), or `None` if it cannot be
    /// resolved because it does not exist.
    fn canonicalize(&self, path: &Path) -> Option<PathBuf>;
}

/// Direct filesystem access with no restrictions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StdFileSystem;

impl StdFileSystem {
    /// Creates a direct filesystem handle.
    pub fn new() -> Self {
        Self
    }
}

impl FileSystem for StdFileSystem {
    fn read(&self, path: &Path) -> Result<Vec<u8>, PlatformError> {
        Ok(fs::read(path)?)
    }

    fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<(), PlatformError> {
        fs::write(path, bytes)?;
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        fs::canonicalize(path).ok()
    }
}

/// A filesystem view restricted to a canonicalised root directory.
///
/// Paths passed to the [`FileSystem`] methods are interpreted relative to the
/// root unless they are absolute; in both cases the resolved path must stay
/// inside the root. The check is applied to the canonical path, so a symlink
/// inside the root that points outside is rejected too.
#[derive(Debug, Clone)]
pub struct SandboxedFileSystem<F: FileSystem> {
    inner: F,
    root: PathBuf,
}

impl<F: FileSystem> SandboxedFileSystem<F> {
    /// Wraps `inner`, restricting access to `root`.
    ///
    /// `root` must exist; it is canonicalised once here so later comparisons are
    /// symlink-safe.
    pub fn new(inner: F, root: impl AsRef<Path>) -> Result<Self, PlatformError> {
        let requested = root.as_ref();
        let canonical = inner
            .canonicalize(requested)
            .ok_or_else(|| PlatformError::PermissionDenied(requested.to_path_buf()))?;
        if !inner.exists(&canonical) {
            return Err(PlatformError::PermissionDenied(requested.to_path_buf()));
        }
        Ok(Self {
            inner,
            root: canonical,
        })
    }

    /// The canonical sandbox root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The wrapped filesystem.
    pub fn inner(&self) -> &F {
        &self.inner
    }

    /// Resolves `path` against the root and verifies it stays inside.
    fn resolve(&self, path: &Path) -> Result<PathBuf, PlatformError> {
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        let lexical = lexical_normalize(&joined)
            .ok_or_else(|| PlatformError::PermissionDenied(path.to_path_buf()))?;
        if !lexical.starts_with(&self.root) {
            return Err(PlatformError::PermissionDenied(path.to_path_buf()));
        }
        match canonicalize_allow_missing(&self.inner, &lexical) {
            Some(resolved) if resolved.starts_with(&self.root) => Ok(resolved),
            _ => Err(PlatformError::PermissionDenied(path.to_path_buf())),
        }
    }
}

impl<F: FileSystem> FileSystem for SandboxedFileSystem<F> {
    fn read(&self, path: &Path) -> Result<Vec<u8>, PlatformError> {
        let resolved = self.resolve(path)?;
        self.inner.read(&resolved)
    }

    fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<(), PlatformError> {
        let resolved = self.resolve(path)?;
        self.inner.write(&resolved, bytes)
    }

    fn exists(&self, path: &Path) -> bool {
        self.resolve(path)
            .is_ok_and(|resolved| self.inner.exists(&resolved))
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        self.resolve(path).ok()
    }
}

/// Lexically resolves `.` and `..` components without touching the filesystem.
///
/// Returns `None` if the path tries to go above the filesystem root.
fn lexical_normalize(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Some(normalized)
}

/// Canonicalises the deepest existing ancestor of `path` and re-appends the
/// missing tail. Returns `None` when even the root cannot be resolved.
fn canonicalize_allow_missing<F: FileSystem>(filesystem: &F, path: &Path) -> Option<PathBuf> {
    if let Some(resolved) = filesystem.canonicalize(path) {
        return Some(resolved);
    }
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        let name = current.file_name()?.to_os_string();
        missing.push(name);
        if !current.pop() {
            return None;
        }
        if let Some(resolved) = filesystem.canonicalize(&current) {
            let mut result = resolved;
            for part in missing.iter().rev() {
                result.push(part);
            }
            return Some(result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("mag-fs-{}-{unique}-{label}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            // Resolve the temp directory itself (macOS /tmp is a symlink).
            let path = fs::canonicalize(&path).unwrap();
            Self { path }
        }

        fn join(&self, part: &str) -> PathBuf {
            self.path.join(part)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn sandbox(label: &str) -> (TempDir, SandboxedFileSystem<StdFileSystem>) {
        let dir = TempDir::new(label);
        let sandbox = SandboxedFileSystem::new(StdFileSystem::new(), &dir.path).unwrap();
        (dir, sandbox)
    }

    #[test]
    fn std_filesystem_round_trips() {
        let dir = TempDir::new("std");
        let mut fs_handle = StdFileSystem::new();
        let path = dir.join("file.txt");
        assert!(!fs_handle.exists(&path));
        fs_handle.write(&path, b"hello").unwrap();
        assert!(fs_handle.exists(&path));
        assert_eq!(fs_handle.read(&path).unwrap(), b"hello");
        let canonical = fs_handle.canonicalize(&path).unwrap();
        assert_eq!(canonical, fs::canonicalize(&path).unwrap());
        assert!(fs_handle.canonicalize(&dir.join("missing")).is_none());
    }

    #[test]
    fn std_filesystem_reports_missing_files() {
        let dir = TempDir::new("missing");
        let fs_handle = StdFileSystem::new();
        let error = fs_handle.read(&dir.join("nope")).unwrap_err();
        assert!(matches!(error, PlatformError::Io(_)));
    }

    #[test]
    fn sandbox_allows_relative_and_absolute_paths_inside_root() {
        let (dir, mut sandbox) = sandbox("inside");
        sandbox.write(Path::new("a.txt"), b"relative").unwrap();
        assert_eq!(sandbox.read(Path::new("a.txt")).unwrap(), b"relative");
        let absolute = dir.join("b.txt");
        sandbox.write(&absolute, b"absolute").unwrap();
        assert_eq!(sandbox.read(&absolute).unwrap(), b"absolute");
        assert!(sandbox.exists(Path::new("a.txt")));
        assert_eq!(sandbox.root(), dir.path.as_path());
    }

    #[test]
    fn sandbox_allows_nested_and_dot_segments_inside_root() {
        let (dir, mut sandbox) = sandbox("dots");
        fs::create_dir_all(dir.join("sub")).unwrap();
        sandbox
            .write(Path::new("sub/../c.txt"), b"normalised")
            .unwrap();
        assert_eq!(
            sandbox.read(Path::new("./sub/../c.txt")).unwrap(),
            b"normalised"
        );
        assert!(dir.join("c.txt").exists());
    }

    #[test]
    fn sandbox_rejects_parent_directory_escape() {
        let (_dir, mut sandbox) = sandbox("dots-escape");
        let error = sandbox
            .write(Path::new("../escaped.txt"), b"no")
            .unwrap_err();
        assert!(matches!(error, PlatformError::PermissionDenied(_)));
        let error = sandbox
            .read(Path::new("sub/../../escaped.txt"))
            .unwrap_err();
        assert!(matches!(error, PlatformError::PermissionDenied(_)));
        assert!(!sandbox.exists(Path::new("../escaped.txt")));
    }

    #[test]
    fn sandbox_rejects_absolute_path_outside_root() {
        let outside = TempDir::new("outside");
        let target = outside.join("secret.txt");
        fs::write(&target, b"secret").unwrap();
        let (_dir, mut sandbox) = sandbox("abs-escape");
        let error = sandbox.read(&target).unwrap_err();
        assert!(matches!(error, PlatformError::PermissionDenied(_)));
        let error = sandbox.write(&target, b"overwrite").unwrap_err();
        assert_eq!(error, PlatformError::PermissionDenied(target.clone()));
        // The file outside is untouched.
        assert_eq!(fs::read(&target).unwrap(), b"secret");
        assert!(sandbox.canonicalize(&target).is_none());
    }

    #[test]
    fn sandbox_rejects_absolute_root_escape_with_dotdot() {
        let (_dir, sandbox) = sandbox("abs-dotdot");
        let escaped = sandbox.root().join("..").join("escape.txt");
        let error = sandbox.read(&escaped).unwrap_err();
        assert!(matches!(error, PlatformError::PermissionDenied(_)));
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let outside = TempDir::new("symlink-outside");
        let secret = outside.join("secret.txt");
        fs::write(&secret, b"secret").unwrap();
        let (dir, mut sandbox) = sandbox("symlink");
        symlink(&secret, dir.join("link.txt")).unwrap();
        symlink(&outside.path, dir.join("linkdir")).unwrap();

        let error = sandbox.read(Path::new("link.txt")).unwrap_err();
        assert!(matches!(error, PlatformError::PermissionDenied(_)));
        let error = sandbox.read(Path::new("linkdir/secret.txt")).unwrap_err();
        assert!(matches!(error, PlatformError::PermissionDenied(_)));
        let error = sandbox.write(Path::new("link.txt"), b"no").unwrap_err();
        assert!(matches!(error, PlatformError::PermissionDenied(_)));
        assert_eq!(fs::read(&secret).unwrap(), b"secret");
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_allows_symlink_inside_root() {
        use std::os::unix::fs::symlink;

        let (dir, mut sandbox) = sandbox("symlink-inside");
        fs::write(dir.join("real.txt"), b"real").unwrap();
        symlink(dir.join("real.txt"), dir.join("alias.txt")).unwrap();
        assert_eq!(sandbox.read(Path::new("alias.txt")).unwrap(), b"real");
        sandbox.write(Path::new("alias.txt"), b"via link").unwrap();
        assert_eq!(fs::read(dir.join("real.txt")).unwrap(), b"via link");
    }

    #[test]
    fn sandbox_canonicalizes_missing_files_inside_root() {
        let (dir, sandbox) = sandbox("new-file");
        let resolved = sandbox
            .canonicalize(Path::new("later/new.txt"))
            .expect("missing paths inside the root resolve");
        assert!(resolved.starts_with(dir.path.as_path()));
        assert_eq!(resolved, dir.path.join("later/new.txt"));
    }

    #[test]
    fn sandbox_construction_requires_existing_root() {
        let missing = std::env::temp_dir().join("mag-fs-does-not-exist-xyz");
        let error = SandboxedFileSystem::new(StdFileSystem::new(), missing).unwrap_err();
        assert!(matches!(error, PlatformError::PermissionDenied(_)));
    }

    #[test]
    fn lexical_normalize_handles_segments() {
        assert_eq!(
            lexical_normalize(Path::new("/a/b/../c/./d")).unwrap(),
            PathBuf::from("/a/c/d")
        );
        assert!(lexical_normalize(Path::new("/..")).is_none());
    }
}
