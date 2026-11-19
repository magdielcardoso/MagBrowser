//! Deterministic download queue used by the engine and headless tests.
//!
//! [`DownloadManager`] owns an ordered list of [`Download`]s. A download moves
//! through [`DownloadState`]s as it is enqueued, started by the manager,
//! advanced by [`DownloadManager::tick`] and finally completed. Time is never
//! read from the clock: [`DownloadManager::tick`] advances every in-progress
//! download by a fixed number of bytes, so simulations are fully reproducible.

use std::fmt;

/// Lifecycle state of a [`Download`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadState {
    /// Waiting for a free download slot.
    Queued,
    /// Currently transferring bytes.
    InProgress,
    /// Transfer paused by the user; holds no active slot.
    Paused,
    /// Transfer finished successfully.
    Done,
    /// Transfer failed; the string describes the reason.
    Failed(String),
}

impl DownloadState {
    /// Whether this is a terminal state (no further transfer will occur).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Failed(_))
    }
}

/// A single download and its transfer progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    /// Stable identifier assigned by the manager.
    pub id: u64,
    /// Source URL.
    pub url: String,
    /// Current lifecycle state.
    pub state: DownloadState,
    /// Number of bytes transferred so far.
    pub bytes_downloaded: u64,
    /// Total size in bytes; `0` means an empty download.
    pub total_bytes: u64,
}

impl Download {
    /// Fraction of the download completed, in `0.0..=1.0`.
    pub fn progress(&self) -> f64 {
        if self.total_bytes == 0 {
            return if self.state == DownloadState::Done {
                1.0
            } else {
                0.0
            };
        }
        self.bytes_downloaded as f64 / self.total_bytes as f64
    }

    /// Whether the transfer is currently active.
    pub fn is_active(&self) -> bool {
        self.state == DownloadState::InProgress
    }

    /// Whether the transfer has reached a terminal state.
    pub fn is_finished(&self) -> bool {
        self.state.is_terminal()
    }
}

/// Errors raised by [`DownloadManager`] control operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// No download with the given id exists.
    NotFound(u64),
    /// The requested state transition is not legal from the current state.
    InvalidTransition {
        id: u64,
        from: DownloadState,
        to: DownloadState,
    },
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "no download with id {id}"),
            Self::InvalidTransition { id, from, to } => {
                write!(f, "cannot move download {id} from {from:?} to {to:?}")
            }
        }
    }
}

impl std::error::Error for DownloadError {}

/// Default number of bytes advanced per [`DownloadManager::tick`].
pub const DEFAULT_BYTES_PER_TICK: u64 = 1;
/// Default number of downloads allowed to transfer at once.
pub const DEFAULT_MAX_CONCURRENT: usize = 1;

/// An ordered, deterministic queue of downloads.
///
/// At most [`Self::max_concurrent`] downloads are `InProgress` at once. When a
/// slot frees (a transfer finishes, is paused, cancelled or fails), the next
/// `Queued` download is promoted automatically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadManager {
    next_id: u64,
    bytes_per_tick: u64,
    max_concurrent: usize,
    downloads: Vec<Download>,
}

impl Default for DownloadManager {
    fn default() -> Self {
        Self {
            next_id: 0,
            bytes_per_tick: DEFAULT_BYTES_PER_TICK,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            downloads: Vec::new(),
        }
    }
}

impl DownloadManager {
    /// Creates an empty manager with the default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets how many bytes each [`Self::tick`] transfers per active download.
    pub fn with_bytes_per_tick(mut self, bytes: u64) -> Self {
        self.bytes_per_tick = bytes;
        self
    }

    /// Sets how many downloads may transfer at once (at least one).
    pub fn with_max_concurrent(mut self, max: usize) -> Self {
        self.max_concurrent = max.max(1);
        self
    }

    /// Bytes transferred per active download on each tick.
    pub fn bytes_per_tick(&self) -> u64 {
        self.bytes_per_tick
    }

    /// Maximum number of simultaneous transfers.
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Adds a download to the queue and returns its stable id.
    pub fn enqueue(&mut self, url: impl Into<String>, total_bytes: u64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.downloads.push(Download {
            id,
            url: url.into(),
            state: DownloadState::Queued,
            bytes_downloaded: 0,
            total_bytes,
        });
        self.promote_queued();
        id
    }

    /// Advances every in-progress download, returning how many completed.
    ///
    /// This is deterministic: no wall-clock time is read. Each active download
    /// gains [`Self::bytes_per_tick`] bytes (clamped to its total); reaching the
    /// total moves it to `Done` and frees its slot.
    pub fn tick(&mut self) -> usize {
        let mut completed = 0;
        for download in &mut self.downloads {
            if download.state == DownloadState::InProgress {
                download.bytes_downloaded = download
                    .bytes_downloaded
                    .saturating_add(self.bytes_per_tick)
                    .min(download.total_bytes);
                if download.bytes_downloaded >= download.total_bytes {
                    download.state = DownloadState::Done;
                    completed += 1;
                }
            }
        }
        if completed > 0 {
            self.promote_queued();
        }
        completed
    }

    /// Pauses an active download, freeing its slot for the next queued one.
    pub fn pause(&mut self, id: u64) -> Result<(), DownloadError> {
        {
            let download = self.find_mut(id).ok_or(DownloadError::NotFound(id))?;
            if download.state != DownloadState::InProgress {
                return Err(DownloadError::InvalidTransition {
                    id,
                    from: download.state.clone(),
                    to: DownloadState::Paused,
                });
            }
            download.state = DownloadState::Paused;
        }
        self.promote_queued();
        Ok(())
    }

    /// Resumes a paused download.
    ///
    /// If no slot is free the download is returned to `Queued` and will start
    /// automatically once a slot opens.
    pub fn resume(&mut self, id: u64) -> Result<(), DownloadError> {
        {
            let download = self.find_mut(id).ok_or(DownloadError::NotFound(id))?;
            if download.state != DownloadState::Paused {
                return Err(DownloadError::InvalidTransition {
                    id,
                    from: download.state.clone(),
                    to: DownloadState::InProgress,
                });
            }
        }
        let has_slot = self.active_count() < self.max_concurrent;
        let download = self.find_mut(id).expect("download was checked above");
        download.state = if has_slot {
            DownloadState::InProgress
        } else {
            DownloadState::Queued
        };
        Ok(())
    }

    /// Removes a download from the queue and returns it.
    ///
    /// Removing an active download frees its slot and promotes the next one.
    pub fn cancel(&mut self, id: u64) -> Result<Download, DownloadError> {
        let index = self
            .downloads
            .iter()
            .position(|download| download.id == id)
            .ok_or(DownloadError::NotFound(id))?;
        let removed = self.downloads.remove(index);
        self.promote_queued();
        Ok(removed)
    }

    /// Marks a non-terminal download as `Failed` with the given reason.
    pub fn fail(&mut self, id: u64, reason: impl Into<String>) -> Result<(), DownloadError> {
        let reason = reason.into();
        {
            let download = self.find_mut(id).ok_or(DownloadError::NotFound(id))?;
            if download.state.is_terminal() {
                return Err(DownloadError::InvalidTransition {
                    id,
                    from: download.state.clone(),
                    to: DownloadState::Failed(reason),
                });
            }
            download.state = DownloadState::Failed(reason);
        }
        self.promote_queued();
        Ok(())
    }

    /// Returns a download by id, if present.
    pub fn get(&self, id: u64) -> Option<&Download> {
        self.downloads.iter().find(|download| download.id == id)
    }

    /// All downloads, in insertion order.
    pub fn downloads(&self) -> &[Download] {
        &self.downloads
    }

    /// Number of downloads in the queue.
    pub fn len(&self) -> usize {
        self.downloads.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.downloads.is_empty()
    }

    /// Current state of a download, if present.
    pub fn state(&self, id: u64) -> Option<&DownloadState> {
        self.get(id).map(|download| &download.state)
    }

    /// Completion fraction of a download, if present.
    pub fn progress(&self, id: u64) -> Option<f64> {
        self.get(id).map(Download::progress)
    }

    fn find_mut(&mut self, id: u64) -> Option<&mut Download> {
        self.downloads.iter_mut().find(|download| download.id == id)
    }

    fn active_count(&self) -> usize {
        self.downloads
            .iter()
            .filter(|download| download.state == DownloadState::InProgress)
            .count()
    }

    fn promote_queued(&mut self) {
        loop {
            if self.active_count() >= self.max_concurrent {
                break;
            }
            let Some(id) = self
                .downloads
                .iter()
                .find(|download| download.state == DownloadState::Queued)
                .map(|download| download.id)
            else {
                break;
            };
            let download = self
                .downloads
                .iter_mut()
                .find(|download| download.id == id)
                .expect("id came from the same list");
            download.state = if download.total_bytes == 0 {
                DownloadState::Done
            } else {
                DownloadState::InProgress
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_starts_first_download() {
        let mut manager = DownloadManager::new();
        let id = manager.enqueue("https://example.com/a", 10);
        let download = manager.get(id).unwrap();
        assert_eq!(download.id, id);
        assert_eq!(download.url, "https://example.com/a");
        assert_eq!(download.state, DownloadState::InProgress);
        assert_eq!(download.bytes_downloaded, 0);
        assert_eq!(download.total_bytes, 10);
        assert_eq!(manager.len(), 1);
        assert!(!manager.is_empty());
    }

    #[test]
    fn queue_is_sequential_by_default() {
        let mut manager = DownloadManager::new();
        let first = manager.enqueue("a", 1);
        let second = manager.enqueue("b", 1);
        let third = manager.enqueue("c", 1);
        assert_eq!(manager.state(first), Some(&DownloadState::InProgress));
        assert_eq!(manager.state(second), Some(&DownloadState::Queued));
        assert_eq!(manager.state(third), Some(&DownloadState::Queued));

        assert_eq!(manager.tick(), 1);
        assert_eq!(manager.state(first), Some(&DownloadState::Done));
        assert_eq!(manager.state(second), Some(&DownloadState::InProgress));
        assert_eq!(manager.tick(), 1);
        assert_eq!(manager.state(second), Some(&DownloadState::Done));
        assert_eq!(manager.state(third), Some(&DownloadState::InProgress));
        assert_eq!(manager.tick(), 1);
        assert_eq!(manager.state(third), Some(&DownloadState::Done));
        assert_eq!(manager.tick(), 0);
    }

    #[test]
    fn tick_advances_by_configured_bytes_and_clamps() {
        let mut manager = DownloadManager::new().with_bytes_per_tick(3);
        let id = manager.enqueue("a", 7);
        assert_eq!(manager.bytes_per_tick(), 3);
        assert_eq!(manager.progress(id), Some(0.0));
        manager.tick();
        assert_eq!(manager.get(id).unwrap().bytes_downloaded, 3);
        manager.tick();
        assert_eq!(manager.get(id).unwrap().bytes_downloaded, 6);
        manager.tick();
        assert_eq!(manager.get(id).unwrap().bytes_downloaded, 7);
        assert_eq!(manager.get(id).unwrap().state, DownloadState::Done);
        assert!((manager.progress(id).unwrap() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn pause_and_resume_work() {
        let mut manager = DownloadManager::new();
        let id = manager.enqueue("a", 10);
        manager.pause(id).unwrap();
        assert_eq!(manager.state(id), Some(&DownloadState::Paused));
        assert_eq!(manager.tick(), 0);
        assert_eq!(manager.get(id).unwrap().bytes_downloaded, 0);
        manager.resume(id).unwrap();
        assert_eq!(manager.state(id), Some(&DownloadState::InProgress));
        manager.tick();
        assert_eq!(manager.get(id).unwrap().bytes_downloaded, 1);
        assert_eq!(manager.state(id), Some(&DownloadState::InProgress));
    }

    #[test]
    fn pause_promotes_next_queued_download() {
        let mut manager = DownloadManager::new();
        let first = manager.enqueue("a", 10);
        let second = manager.enqueue("b", 10);
        manager.pause(first).unwrap();
        assert_eq!(manager.state(first), Some(&DownloadState::Paused));
        assert_eq!(manager.state(second), Some(&DownloadState::InProgress));
    }

    #[test]
    fn invalid_transitions_are_reported() {
        let mut manager = DownloadManager::new();
        let id = manager.enqueue("a", 4);
        assert_eq!(manager.pause(999), Err(DownloadError::NotFound(999)));
        let second = manager.enqueue("b", 4);
        assert_eq!(
            manager.pause(second),
            Err(DownloadError::InvalidTransition {
                id: second,
                from: DownloadState::Queued,
                to: DownloadState::Paused,
            })
        );
        assert_eq!(
            manager.resume(id),
            Err(DownloadError::InvalidTransition {
                id,
                from: DownloadState::InProgress,
                to: DownloadState::InProgress,
            })
        );
    }

    #[test]
    fn cancel_removes_and_promotes() {
        let mut manager = DownloadManager::new();
        let first = manager.enqueue("a", 4);
        let second = manager.enqueue("b", 4);
        let removed = manager.cancel(first).unwrap();
        assert_eq!(removed.id, first);
        assert_eq!(manager.get(first), None);
        assert_eq!(manager.state(second), Some(&DownloadState::InProgress));
        assert_eq!(manager.cancel(first), Err(DownloadError::NotFound(first)));
    }

    #[test]
    fn fail_marks_state_and_frees_slot() {
        let mut manager = DownloadManager::new();
        let first = manager.enqueue("a", 10);
        let second = manager.enqueue("b", 10);
        manager.fail(first, "404").unwrap();
        assert_eq!(
            manager.state(first),
            Some(&DownloadState::Failed("404".to_string()))
        );
        assert!(manager.get(first).unwrap().is_finished());
        assert_eq!(manager.state(second), Some(&DownloadState::InProgress));
        assert_eq!(
            manager.fail(first, "again"),
            Err(DownloadError::InvalidTransition {
                id: first,
                from: DownloadState::Failed("404".to_string()),
                to: DownloadState::Failed("again".to_string()),
            })
        );
    }

    #[test]
    fn max_concurrent_allows_parallel_downloads() {
        let mut manager = DownloadManager::new()
            .with_bytes_per_tick(2)
            .with_max_concurrent(2);
        let a = manager.enqueue("a", 2);
        let b = manager.enqueue("b", 2);
        let c = manager.enqueue("c", 2);
        assert_eq!(manager.max_concurrent(), 2);
        assert_eq!(manager.state(a), Some(&DownloadState::InProgress));
        assert_eq!(manager.state(b), Some(&DownloadState::InProgress));
        assert_eq!(manager.state(c), Some(&DownloadState::Queued));
        assert_eq!(manager.tick(), 2);
        assert_eq!(manager.state(a), Some(&DownloadState::Done));
        assert_eq!(manager.state(b), Some(&DownloadState::Done));
        assert_eq!(manager.state(c), Some(&DownloadState::InProgress));
    }

    #[test]
    fn zero_byte_download_completes_immediately() {
        let mut manager = DownloadManager::new();
        let id = manager.enqueue("empty", 0);
        assert_eq!(manager.state(id), Some(&DownloadState::Done));
        assert_eq!(manager.progress(id), Some(1.0));
        assert_eq!(manager.tick(), 0);
    }

    #[test]
    fn resume_waits_when_no_slot_is_free() {
        let mut manager = DownloadManager::new();
        let a = manager.enqueue("a", 10);
        let b = manager.enqueue("b", 10);
        manager.pause(a).unwrap();
        manager.resume(a).unwrap();
        assert_eq!(manager.state(a), Some(&DownloadState::Queued));
        let c = manager.enqueue("c", 10);
        assert_eq!(manager.state(c), Some(&DownloadState::Queued));
        manager.cancel(b).unwrap();
        assert_eq!(manager.state(a), Some(&DownloadState::InProgress));
        assert_eq!(manager.state(c), Some(&DownloadState::Queued));
    }

    #[test]
    fn ids_are_stable_and_downloads_are_listed() {
        let mut manager = DownloadManager::new();
        let a = manager.enqueue("a", 1);
        let b = manager.enqueue("b", 1);
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        let ids: Vec<u64> = manager
            .downloads()
            .iter()
            .map(|download| download.id)
            .collect();
        assert_eq!(ids, vec![a, b]);
    }

    #[test]
    fn errors_display() {
        assert!(DownloadError::NotFound(7).to_string().contains('7'));
        let error = DownloadError::InvalidTransition {
            id: 1,
            from: DownloadState::Done,
            to: DownloadState::Paused,
        };
        assert!(error.to_string().contains("Done"));
    }
}
