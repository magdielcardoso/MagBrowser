//! Tab manager for the browser shell.

use std::cell::RefCell;
use std::rc::Rc;

use browser_core::{Browser, TabId};

/// Manages the tab strip on top of [`browser_core::TabSet`].
pub struct TabManager {
    browser: Rc<RefCell<Browser>>,
}

impl TabManager {
    /// Creates a manager over a shared browser.
    pub fn new(browser: Rc<RefCell<Browser>>) -> Self {
        Self { browser }
    }

    /// Opens a tab, optionally navigating it.
    pub fn open(&self, url: Option<&str>) -> Result<TabId, String> {
        let mut browser = self.browser.borrow_mut();
        let id = browser.new_tab();
        if let Some(url) = url {
            browser.navigate(url).map_err(|err| err.to_string())?;
        }
        Ok(id)
    }

    /// Selects a tab by index.
    pub fn select(&self, index: usize) -> Result<(), String> {
        let id = {
            let browser = self.browser.borrow();
            browser
                .tabs()
                .ids()
                .get(index)
                .copied()
                .ok_or_else(|| format!("no tab at index {index}"))?
        };
        self.browser
            .borrow_mut()
            .select_tab(id)
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    /// Closes the active tab.
    pub fn close_active(&self) -> Result<(), String> {
        let mut browser = self.browser.borrow_mut();
        let id = browser.tab().id;
        if browser.tabs_mut().close(id) {
            Ok(())
        } else {
            Err("cannot close the last tab".to_string())
        }
    }

    /// Titles and the active index.
    pub fn titles(&self) -> (Vec<String>, usize) {
        self.browser.borrow().tabs().titles()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_core::Settings;

    #[test]
    fn opens_and_selects_tabs() {
        let browser = Rc::new(RefCell::new(Browser::new(Settings::headless())));
        let tabs = TabManager::new(browser.clone());
        let first = browser.borrow().tab().id;
        tabs.open(None).unwrap();
        assert_ne!(browser.borrow().tab().id, first);
        assert!(tabs.select(0).is_ok());
        assert_eq!(browser.borrow().tab().id, first);
        let (titles, _) = tabs.titles();
        assert!(!titles.is_empty());
        assert!(tabs.close_active().is_ok());
    }
}
