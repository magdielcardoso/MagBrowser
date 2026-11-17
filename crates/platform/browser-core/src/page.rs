//! A single rendered page: document, styles, layout, paint and interaction.

use std::cell::RefCell;
use std::rc::Rc;

use browser_compositor::LayerTree;
use browser_css::{Color, Origin};
use browser_dom::{Document, NodeId, serialize_children};
use browser_layout::{LayoutTree, Size};
use std::collections::HashMap;

use browser_dom::NodeId as DomNodeId;
use browser_paint::{DisplayList, ImageBuffer, ImageData};
use browser_style::{StyleEngine, StyledTree};
use browser_url::Url;

/// Lifecycle state of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageState {
    /// No document loaded.
    Empty,
    /// The document has been parsed and styled.
    Loaded,
    /// Loading or scripting failed.
    Failed(String),
}

/// A rendered document.
pub struct Page {
    /// Current URL, when the page came from the network or a file.
    pub url: Option<Url>,
    /// Parsed DOM, shared with the JavaScript runtime's DOM bindings.
    pub document: Rc<RefCell<Document>>,
    /// Computed styles.
    pub styles: StyledTree,
    /// Layout tree.
    pub layout: LayoutTree,
    /// Display list.
    pub display_list: DisplayList,
    /// Compositor layers and damage state.
    pub layers: LayerTree,
    /// Decoded images keyed by `<img>` node.
    pub images: HashMap<DomNodeId, ImageData>,
    /// Lifecycle state.
    pub state: PageState,
    /// Author CSS in application order (`<style>` and linked sheets).
    author_css: Vec<String>,
    /// JavaScript runtime, created on first script execution.
    pub runtime: Option<browser_js::Runtime>,
    /// Host bindings state shared with the runtime.
    pub host: Option<Rc<RefCell<browser_web_apis::HostState>>>,
    /// True once the document's inline scripts have executed.
    pub scripts_executed: bool,
    viewport: Size,
    prefers_color_scheme: String,
    background: Color,
}

impl Page {
    /// Creates an empty page for a viewport.
    pub fn new(viewport: Size) -> Self {
        let mut page = Self {
            url: None,
            document: Rc::new(RefCell::new(Document::new())),
            styles: StyledTree::new(),
            layout: LayoutTree::new(),
            display_list: DisplayList::new(),
            layers: LayerTree::default(),
            images: HashMap::new(),
            state: PageState::Empty,
            author_css: Vec::new(),
            runtime: None,
            host: None,
            scripts_executed: false,
            viewport,
            prefers_color_scheme: "light".to_string(),
            background: Color::rgb(255, 255, 255),
        };
        page.recompute();
        page
    }

    /// Sets the page background colour.
    pub fn set_background(&mut self, color: Color) {
        self.background = color;
    }

    /// Sets the preferred colour scheme used by media queries.
    pub fn set_prefers_color_scheme(&mut self, scheme: &str) {
        self.prefers_color_scheme = scheme.to_string();
    }

    /// The current viewport.
    pub fn viewport(&self) -> Size {
        self.viewport
    }

    /// Resizes the page and re-runs layout and paint.
    pub fn resize(&mut self, viewport: Size) {
        self.viewport = viewport;
        self.recompute();
    }

    /// Replaces the document, collects `<style>` blocks and re-renders.
    pub fn load_html(&mut self, html: &str, url: Option<Url>) {
        // A new document means a new runtime and a fresh binding state.
        self.runtime = None;
        self.host = None;
        self.scripts_executed = false;
        self.images.clear();
        let mut document = browser_html_parser::parse_document(html);
        if let Some(url) = &url {
            document.base_url = Some(url.as_str());
        }
        self.document = Rc::new(RefCell::new(document));
        self.url = url;
        self.author_css.clear();
        self.collect_style_elements();
        self.state = PageState::Loaded;
        self.recompute();
    }

    /// Adds a stylesheet (for example one fetched from a `<link>` element).
    pub fn add_author_css(&mut self, css: &str) {
        self.author_css.push(css.to_string());
        self.recompute();
    }

    /// Author stylesheets currently applied, in order.
    pub fn author_css(&self) -> &[String] {
        &self.author_css
    }

    /// Collects the text of every `<style>` element in the document.
    fn collect_style_elements(&mut self) {
        let document = self.document.borrow();
        let style_nodes = document.get_elements_by_tag_name("style");
        for node in style_nodes {
            let css = document.text_content(node);
            if !css.trim().is_empty() {
                self.author_css.push(css);
            }
        }
    }

    /// Recomputes style, layout, display list and layer metadata.
    pub fn recompute(&mut self) {
        let mut engine = StyleEngine::new(browser_style::Viewport {
            width: self.viewport.width,
            height: self.viewport.height,
        });
        engine.set_prefers_color_scheme(&self.prefers_color_scheme);
        for css in &self.author_css {
            engine.add_css(css, Origin::Author);
        }
        let document = self.document.borrow();
        self.styles = engine.compute(&document);
        self.layout = browser_layout::layout(&document, &self.styles, self.viewport);
        self.display_list = browser_paint::build_with_images(&self.layout, &self.images);
        self.layers = LayerTree::build(&self.layout, self.viewport);
    }

    /// Damage accumulated since the last call (for incremental rendering).
    pub fn take_damage(&mut self) -> browser_compositor::Damage {
        self.layers.take_damage()
    }

    /// Renders the page to an image buffer using the current display list.
    pub fn render(&self) -> ImageBuffer {
        browser_paint::Rasterizer::new(self.viewport, self.background).execute(&self.display_list)
    }

    /// The document title.
    pub fn title(&self) -> String {
        self.document.borrow().title.clone()
    }

    /// Visible text as laid out (script and style content is excluded).
    pub fn text(&self) -> String {
        let mut out = String::new();
        for run in self.layout.text_runs() {
            let text = &run.fragment.text;
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(text);
        }
        out
    }

    /// The DOM node at a viewport coordinate.
    pub fn hit_test_node(&self, x: f32, y: f32) -> Option<NodeId> {
        let index = self.layout.hit_test(x, y)?;
        self.layout.get(index)?.node
    }

    /// The `href` URL of the closest link at a viewport coordinate.
    pub fn link_at(&self, x: f32, y: f32) -> Option<Url> {
        let node = self.hit_test_node(x, y)?;
        self.link_for_node(node)
    }

    /// Resolves the closest `<a href>` ancestor of a node.
    pub fn link_for_node(&self, node: NodeId) -> Option<Url> {
        let document = self.document.borrow();
        let mut current = Some(node);
        while let Some(id) = current {
            if document.is_tag(id, "a")
                && let Some(href) = document.attr(id, "href")
            {
                if let Some(base) = &self.url {
                    return base.join(href).ok();
                }
                return Url::parse(href).ok();
            }
            current = document.parent(id);
        }
        None
    }

    /// Scrolls the container under a point by `(dx, dy)`.
    pub fn scroll_at(&mut self, x: f32, y: f32, dx: f32, dy: f32) -> bool {
        let Some(container) = browser_layout::scroll::scroll_container_at(&self.layout, x, y)
        else {
            return false;
        };
        if browser_layout::scroll::scroll_by(&mut self.layout, container, dx, dy) {
            self.display_list = browser_paint::build_with_images(&self.layout, &self.images);
            true
        } else {
            false
        }
    }

    /// Serialises the body as HTML (used by the CLI and tests).
    pub fn body_html(&self) -> String {
        let document = self.document.borrow();
        let body = document
            .head_and_body()
            .1
            .unwrap_or_else(|| document.root());
        serialize_children(&document, body)
    }

    /// Number of DOM nodes.
    pub fn node_count(&self) -> usize {
        self.document.borrow().node_count()
    }

    /// Dispatches a DOM event at a node, running JavaScript listeners.
    ///
    /// Returns `true` when at least one listener ran.
    pub fn dispatch_event(&mut self, node: NodeId, event_type: &str) -> bool {
        let (Some(runtime), Some(host)) = (&mut self.runtime, &self.host) else {
            return false;
        };
        browser_web_apis::dispatch_event(runtime, host, node, event_type)
    }

    /// A pending `location.href = ...` request, if a script made one.
    pub fn take_pending_navigation(&mut self) -> Option<String> {
        let (Some(_), Some(host)) = (&self.runtime, &self.host) else {
            return None;
        };
        host.borrow_mut().navigation.take()
    }
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("url", &self.url.as_ref().map(Url::as_str))
            .field("state", &self.state)
            .field("nodes", &self.node_count())
            .field("scripting", &self.runtime.is_some())
            .field("boxes", &self.layout.boxes.len())
            .field("commands", &self.display_list.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> Size {
        Size {
            width: 800.0,
            height: 600.0,
        }
    }

    #[test]
    fn loads_html_and_builds_pipeline() {
        let mut page = Page::new(viewport());
        page.load_html(
            "<!DOCTYPE html><html><head><title>T</title><style>body{background:#00ff00}</style></head><body><p>Hello</p></body></html>",
            None,
        );
        assert_eq!(page.title(), "T");
        assert_eq!(page.state, PageState::Loaded);
        assert!(page.layout.boxes.len() >= 3);
        assert!(!page.display_list.is_empty());
        assert!(page.text().contains("Hello"));
        assert!(!page.author_css().is_empty());
    }

    #[test]
    fn background_from_css_is_painted() {
        let mut page = Page::new(viewport());
        page.load_html(
            "<html><body style='background:#ff0000'><p>x</p></body></html>",
            None,
        );
        let image = page.render();
        assert_eq!(image.pixel(100, 10), [255, 0, 0, 255]);
    }

    #[test]
    fn links_are_hit_testable() {
        let mut page = Page::new(viewport());
        page.load_html(
            "<html><body><a href='/next'>go</a></body></html>",
            Some(Url::parse("https://example.com/dir/page").unwrap()),
        );
        let mut found = None;
        for y in 0..100 {
            for x in 0..100 {
                if let Some(url) = page.link_at(x as f32 + 0.5, y as f32 + 0.5) {
                    found = Some(url);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        assert_eq!(found.unwrap().as_str(), "https://example.com/next");
    }

    #[test]
    fn resize_relayouts() {
        let mut page = Page::new(viewport());
        page.load_html(
            "<html><body><div style='height:100px'>x</div></body></html>",
            None,
        );
        page.resize(Size {
            width: 400.0,
            height: 300.0,
        });
        assert_eq!(page.viewport().width, 400.0);
        assert_eq!(page.layout.document_height, 116.0);
    }
}
