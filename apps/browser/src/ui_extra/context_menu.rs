//! Right-click context menu model and hit-testing with plain `f32` geometry.
//!
//! The window layer owns the actual drawing; this module only describes which
//! actions the menu offers, where each item sits, and which action a click
//! lands on. No winit or wgpu types are used.

// A framework-agnostic model: not every accessor is used by the window shell.
#![allow(dead_code)]

/// Actions offered by the page context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextAction {
    /// Open the link under the cursor in a new tab.
    OpenInNewTab,
    /// Copy the link under the cursor to the clipboard.
    CopyLink,
    /// Navigate back in history.
    Back,
    /// Reload the current page.
    Reload,
    /// Open developer tools for this page.
    Inspect,
}

impl ContextAction {
    /// Every action, in the order shown in the default menu.
    pub const ALL: [Self; 5] = [
        Self::OpenInNewTab,
        Self::CopyLink,
        Self::Back,
        Self::Reload,
        Self::Inspect,
    ];

    /// The label drawn for this action.
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenInNewTab => "Open in new tab",
            Self::CopyLink => "Copy link",
            Self::Back => "Back",
            Self::Reload => "Reload",
            Self::Inspect => "Inspect",
        }
    }
}

/// A 2D point in the same coordinate space the menu is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: f32,
    /// Vertical coordinate.
    pub y: f32,
}

impl Point {
    /// Creates a point.
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rectangle used for hit-testing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width along the x axis.
    pub width: f32,
    /// Height along the y axis.
    pub height: f32,
}

impl Rect {
    /// Creates a rectangle from its top-left corner and size.
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// True when `point` lies inside the rectangle (left/top inclusive,
    /// right/bottom exclusive).
    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x
            && point.x < self.x + self.width
            && point.y >= self.y
            && point.y < self.y + self.height
    }
}

/// One row of the context menu.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    /// Action performed when the item is activated.
    pub action: ContextAction,
    /// Text drawn for the item.
    pub label: String,
    /// Hit-test bounds of the row.
    pub bounds: Rect,
    /// Whether the row can be activated (disabled rows are skipped by hit tests).
    pub enabled: bool,
}

/// A context menu anchored at an origin point.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextMenu {
    /// Top-left anchor of the menu.
    pub origin: Point,
    /// Items in display order, top to bottom.
    pub items: Vec<MenuItem>,
}

impl ContextMenu {
    /// Default width of a menu row in logical pixels.
    pub const DEFAULT_ITEM_WIDTH: f32 = 180.0;
    /// Default height of a menu row in logical pixels.
    pub const DEFAULT_ITEM_HEIGHT: f32 = 28.0;

    /// Creates the default page menu with all five actions enabled.
    pub fn new(origin: Point) -> Self {
        Self::with_actions(origin, ContextAction::ALL)
    }

    /// Creates a menu containing only the given actions, each enabled.
    pub fn with_actions(origin: Point, actions: impl IntoIterator<Item = ContextAction>) -> Self {
        let width = Self::DEFAULT_ITEM_WIDTH;
        let height = Self::DEFAULT_ITEM_HEIGHT;
        let items = actions
            .into_iter()
            .enumerate()
            .map(|(i, action)| MenuItem {
                action,
                label: action.label().to_string(),
                bounds: Rect::new(origin.x, origin.y + i as f32 * height, width, height),
                enabled: true,
            })
            .collect();
        Self { origin, items }
    }

    /// Number of items in the menu.
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    /// Borrows an item by index.
    pub fn item_at(&self, index: usize) -> Option<&MenuItem> {
        self.items.get(index)
    }

    /// Bounds of the whole menu, or `None` when it is empty.
    pub fn bounds(&self) -> Option<Rect> {
        let first = self.items.first()?.bounds;
        let last = self.items.last()?.bounds;
        Some(Rect::new(
            first.x,
            first.y,
            first.width,
            last.y + last.height - first.y,
        ))
    }

    /// Resizes all rows and relayouts them from the origin.
    pub fn set_item_size(&mut self, width: f32, height: f32) {
        for (i, item) in self.items.iter_mut().enumerate() {
            item.bounds = Rect::new(
                self.origin.x,
                self.origin.y + i as f32 * height,
                width,
                height,
            );
        }
    }

    /// Returns the action whose row contains `point`, skipping disabled rows.
    pub fn hit_test(&self, point: Point) -> Option<ContextAction> {
        self.items
            .iter()
            .find(|item| item.enabled && item.bounds.contains(point))
            .map(|item| item.action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_menu_has_the_expected_items() {
        let menu = ContextMenu::new(Point::new(10.0, 20.0));
        assert_eq!(menu.item_count(), 5);
        let labels: Vec<&str> = menu.items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Open in new tab", "Copy link", "Back", "Reload", "Inspect"]
        );
        assert_eq!(menu.items[0].action, ContextAction::OpenInNewTab);
        assert_eq!(menu.items[4].action, ContextAction::Inspect);
        assert!(menu.items.iter().all(|item| item.enabled));
    }

    #[test]
    fn action_labels() {
        assert_eq!(ContextAction::CopyLink.label(), "Copy link");
        assert_eq!(ContextAction::Back.label(), "Back");
        assert_eq!(ContextAction::Reload.label(), "Reload");
        assert_eq!(ContextAction::Inspect.label(), "Inspect");
    }

    #[test]
    fn rect_contains_uses_half_open_bounds() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(r.contains(Point::new(0.0, 0.0)));
        assert!(r.contains(Point::new(9.9, 9.9)));
        assert!(!r.contains(Point::new(10.0, 5.0)));
        assert!(!r.contains(Point::new(5.0, 10.0)));
    }

    #[test]
    fn hit_test_finds_each_row() {
        let menu = ContextMenu::new(Point::new(10.0, 20.0));
        // Rows occupy y: [20,48), [48,76), [76,104), [104,132), [132,160).
        assert_eq!(
            menu.hit_test(Point::new(12.0, 22.0)),
            Some(ContextAction::OpenInNewTab)
        );
        assert_eq!(
            menu.hit_test(Point::new(12.0, 50.0)),
            Some(ContextAction::CopyLink)
        );
        assert_eq!(
            menu.hit_test(Point::new(12.0, 80.0)),
            Some(ContextAction::Back)
        );
        assert_eq!(
            menu.hit_test(Point::new(12.0, 110.0)),
            Some(ContextAction::Reload)
        );
        assert_eq!(
            menu.hit_test(Point::new(12.0, 140.0)),
            Some(ContextAction::Inspect)
        );
    }

    #[test]
    fn hit_test_misses_outside_the_menu() {
        let menu = ContextMenu::new(Point::new(10.0, 20.0));
        assert_eq!(menu.hit_test(Point::new(200.0, 22.0)), None); // right of rows
        assert_eq!(menu.hit_test(Point::new(12.0, 200.0)), None); // below rows
        assert_eq!(menu.hit_test(Point::new(9.0, 22.0)), None); // left of rows
    }

    #[test]
    fn hit_test_skips_disabled_items() {
        let mut menu = ContextMenu::new(Point::new(0.0, 0.0));
        menu.items[2].enabled = false; // disable Back
        assert_eq!(menu.hit_test(Point::new(5.0, 80.0)), None);
        assert_eq!(
            menu.hit_test(Point::new(5.0, 110.0)),
            Some(ContextAction::Reload)
        );
    }

    #[test]
    fn with_actions_and_item_layout() {
        let menu = ContextMenu::with_actions(Point::new(0.0, 0.0), [ContextAction::Reload]);
        assert_eq!(menu.item_count(), 1);
        assert_eq!(menu.items[0].label, "Reload");
        assert_eq!(
            menu.bounds(),
            Some(Rect::new(
                0.0,
                0.0,
                ContextMenu::DEFAULT_ITEM_WIDTH,
                ContextMenu::DEFAULT_ITEM_HEIGHT
            ))
        );

        let mut menu = ContextMenu::new(Point::new(0.0, 0.0));
        menu.set_item_size(200.0, 30.0);
        assert_eq!(menu.items[1].bounds, Rect::new(0.0, 30.0, 200.0, 30.0));
        assert_eq!(menu.bounds(), Some(Rect::new(0.0, 0.0, 200.0, 150.0)));
    }

    #[test]
    fn empty_menu_has_no_bounds_and_no_hits() {
        let menu = ContextMenu::with_actions(Point::new(0.0, 0.0), []);
        assert_eq!(menu.item_count(), 0);
        assert_eq!(menu.bounds(), None);
        assert_eq!(menu.hit_test(Point::new(0.0, 0.0)), None);
    }
}
