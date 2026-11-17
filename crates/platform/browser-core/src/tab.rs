//! Tabs: multiple pages sharing a window.

use browser_layout::Size;
use browser_navigation::{NavigationAction, SessionHistory};

use crate::page::Page;

/// Identifier of a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(pub u64);

/// A single tab: a page plus its session history.
pub struct Tab {
    /// Stable identifier.
    pub id: TabId,
    /// The rendered page.
    pub page: Page,
    /// Session history for this tab.
    pub history: SessionHistory,
}

impl Tab {
    /// Creates an empty tab.
    pub fn new(id: TabId, viewport: Size) -> Self {
        Self {
            id,
            page: Page::new(viewport),
            history: SessionHistory::new(),
        }
    }

    /// Title for the tab strip.
    pub fn title(&self) -> String {
        let title = self.page.title().trim().to_string();
        if title.is_empty() {
            self.page.url.as_ref().map_or_else(
                || "Novo separador".to_string(),
                |u| u.host().unwrap_or("page").to_string(),
            )
        } else {
            title
        }
    }

    /// Records a navigation in the tab history.
    pub fn record_navigation(&mut self, action: NavigationAction, url: Option<browser_url::Url>) {
        let title = self.page.title().to_string();
        let _ = self.history.navigate(action, url, &title);
    }
}

struct Slot {
    tab: Tab,
    closed: bool,
}

/// An ordered set of tabs with one active tab.
pub struct TabSet {
    slots: Vec<Slot>,
    active: usize,
    next_id: u64,
    viewport: Size,
}

impl TabSet {
    /// Creates a set with one empty tab.
    pub fn new(viewport: Size) -> Self {
        let mut set = Self {
            slots: Vec::new(),
            active: 0,
            next_id: 1,
            viewport,
        };
        set.open_empty();
        set
    }

    /// Opens a new empty tab and activates it.
    pub fn open_empty(&mut self) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        self.slots.push(Slot {
            tab: Tab::new(id, self.viewport),
            closed: false,
        });
        self.active = self.slots.len() - 1;
        id
    }

    /// Opens a new tab and loads HTML into it.
    pub fn open_html(&mut self, html: &str, url: Option<browser_url::Url>) -> TabId {
        let id = self.open_empty();
        if let Some(tab) = self.slots.last_mut().map(|s| &mut s.tab) {
            tab.page.load_html(html, url);
        }
        id
    }

    /// Closes a tab, keeping at least one open.
    pub fn close(&mut self, id: TabId) -> bool {
        if self.slots.iter().filter(|s| !s.closed).count() <= 1 {
            return false;
        }
        let Some(index) = self.slots.iter().position(|s| s.tab.id == id && !s.closed) else {
            return false;
        };
        self.slots[index].closed = true;
        if self.active >= index {
            self.active = self.active.saturating_sub(1);
        }
        if self.active >= self.slots.len() {
            self.active = self.slots.len() - 1;
        }
        true
    }

    /// Activates a tab.
    pub fn select(&mut self, id: TabId) -> bool {
        match self.slots.iter().position(|s| s.tab.id == id && !s.closed) {
            Some(index) => {
                self.active = index;
                true
            }
            None => false,
        }
    }

    /// The active tab.
    pub fn active(&self) -> &Tab {
        &self.slots[self.active.min(self.slots.len().saturating_sub(1))].tab
    }

    /// The active tab, mutably.
    pub fn active_mut(&mut self) -> &mut Tab {
        let index = self.active.min(self.slots.len().saturating_sub(1));
        &mut self.slots[index].tab
    }

    /// Looks up a tab by id.
    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.slots
            .iter()
            .find(|s| s.tab.id == id && !s.closed)
            .map(|s| &s.tab)
    }

    /// Looks up a tab by id, mutably.
    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.slots
            .iter_mut()
            .find(|s| s.tab.id == id && !s.closed)
            .map(|s| &mut s.tab)
    }

    /// Identifiers of the open tabs, in strip order.
    pub fn ids(&self) -> Vec<TabId> {
        self.slots
            .iter()
            .filter(|slot| !slot.closed)
            .map(|slot| slot.tab.id)
            .collect()
    }

    /// Number of open tabs.
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| !s.closed).count()
    }

    /// True when no tab is open (never after construction).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Titles of all open tabs, in order, with the active index.
    pub fn titles(&self) -> (Vec<String>, usize) {
        let mut titles = Vec::new();
        let mut active = 0;
        for slot in &self.slots {
            if slot.closed {
                continue;
            }
            if std::ptr::eq(slot, &self.slots[self.active]) {
                active = titles.len();
            }
            titles.push(slot.tab.title());
        }
        (titles, active)
    }

    /// Resizes every tab.
    pub fn resize(&mut self, viewport: Size) {
        self.viewport = viewport;
        for slot in &mut self.slots {
            if !slot.closed {
                slot.tab.page.resize(viewport);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> Size {
        Size {
            width: 640.0,
            height: 480.0,
        }
    }

    #[test]
    fn opens_and_closes_tabs() {
        let mut tabs = TabSet::new(viewport());
        assert_eq!(tabs.len(), 1);
        let second = tabs.open_html(
            "<html><head><title>Two</title></head><body></body></html>",
            None,
        );
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.active().id, second);
        assert!(tabs.close(second));
        assert_eq!(tabs.len(), 1);
        assert!(!tabs.close(tabs.active().id));
    }

    #[test]
    fn tab_titles_fall_back_to_host() {
        let mut tabs = TabSet::new(viewport());
        tabs.active_mut().page.load_html(
            "<html><body></body></html>",
            Some(browser_url::Url::parse("https://example.com/x").unwrap()),
        );
        assert_eq!(tabs.active().title(), "example.com");
    }

    #[test]
    fn selecting_tab_changes_active() {
        let mut tabs = TabSet::new(viewport());
        let first = tabs.active().id;
        let second = tabs.open_empty();
        assert!(tabs.select(first));
        assert_eq!(tabs.active().id, first);
        assert!(tabs.select(second));
        assert_eq!(tabs.active().id, second);
        assert!(!tabs.select(TabId(999)));
    }
}
