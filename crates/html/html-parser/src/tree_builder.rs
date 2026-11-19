//! The HTML tree builder.
//!
//! The tree builder consumes [`Token`]s from the tokenizer and produces a
//! [`Document`](browser_dom::Document). It implements the WHATWG insertion
//! modes, a stack of open elements and a simplified active formatting list.
//!
//! # Simplifications
//!
//! * Foster parenting of misplaced table content is not implemented; such
//!   content is inserted at the current table node instead of before it.
//! * The adoption agency algorithm closes and reopens formatting elements so
//!   that misnested input produces a well-formed tree, but it does not perform
//!   the full three-iteration WHATWG recovery loop.
//! * Frameset-specific modes are folded into [`InsertionMode::InBody`].
//! * `<template>` contents are parsed with the body rules; the template's
//!   previous insertion mode is restored when the template is closed.

use crate::insertion_modes::InsertionMode;
use crate::tokens::Token;
use crate::{ParseError, ParseOptions, ParseResult};
use browser_dom::{Document, Namespace, NodeData, NodeId};

const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "basefont", "bgsound", "br", "col", "embed", "frame", "hr", "img", "input",
    "keygen", "link", "meta", "param", "source", "track", "wbr",
];

const FORMATTING_ELEMENTS: &[&str] = &[
    "a", "b", "big", "code", "em", "font", "i", "nobr", "s", "small", "strike", "strong", "tt", "u",
];

const IMPLIED_END_TAGS: &[&str] = &[
    "dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc",
];

const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];

const SCOPE_BOUNDARIES: &[&str] = &[
    "applet", "caption", "html", "marquee", "object", "table", "td", "th", "template",
];

fn is_void(name: &str) -> bool {
    VOID_ELEMENTS.contains(&name)
}

fn is_formatting(name: &str) -> bool {
    FORMATTING_ELEMENTS.contains(&name)
}

fn is_heading(name: &str) -> bool {
    HEADINGS.contains(&name)
}

fn is_whitespace(text: &str) -> bool {
    text.chars().all(|c| c.is_ascii_whitespace())
}

fn normalize(token: Token) -> Token {
    match token {
        Token::StartTag {
            name,
            mut attributes,
            self_closing,
        } => {
            for attribute in &mut attributes {
                attribute.0 = attribute.0.to_ascii_lowercase();
            }
            Token::StartTag {
                name: name.to_ascii_lowercase(),
                attributes,
                self_closing,
            }
        }
        Token::EndTag { name } => Token::EndTag {
            name: name.to_ascii_lowercase(),
        },
        other => other,
    }
}

#[derive(Debug, Clone)]
enum FormattingEntry {
    Marker,
    Element { id: NodeId, name: String },
}

/// Builds a DOM tree from a stream of tokens.
///
/// ```
/// use browser_html_parser::{ParseOptions, Token, TreeBuilder};
/// let mut builder = TreeBuilder::new(ParseOptions::default());
/// builder.feed(Token::StartTag {
///     name: "p".to_string(),
///     attributes: Vec::new(),
///     self_closing: false,
/// });
/// builder.feed(Token::Text("hi".to_string()));
/// let result = builder.finish();
/// assert_eq!(result.document.text_content(
///     result.document.document_element().unwrap()
/// ), "hi");
/// ```
pub struct TreeBuilder {
    document: Document,
    options: ParseOptions,
    errors: Vec<ParseError>,
    mode: InsertionMode,
    original_mode: InsertionMode,
    open: Vec<NodeId>,
    formatting: Vec<FormattingEntry>,
    template_stack: Vec<(NodeId, InsertionMode)>,
    html: Option<NodeId>,
    head: Option<NodeId>,
    body: Option<NodeId>,
    title: Option<NodeId>,
    title_seen: bool,
    form: Option<NodeId>,
    frameset_ok: bool,
    position: usize,
    eof: bool,
    /// Set once the DOM node cap has been hit, so only one error is reported
    /// and further tokens can be ignored cheaply.
    node_limit_reached: bool,
    /// Set once the open-element depth cap has been hit, to avoid one error
    /// per element on deeply nested input.
    depth_limit_reached: bool,
}

impl TreeBuilder {
    /// Creates a tree builder with the given options.
    pub fn new(options: ParseOptions) -> Self {
        Self {
            document: Document::new(),
            options,
            errors: Vec::new(),
            mode: InsertionMode::Initial,
            original_mode: InsertionMode::InBody,
            open: Vec::new(),
            formatting: Vec::new(),
            template_stack: Vec::new(),
            html: None,
            head: None,
            body: None,
            title: None,
            title_seen: false,
            form: None,
            frameset_ok: true,
            position: 0,
            eof: false,
            node_limit_reached: false,
            depth_limit_reached: false,
        }
    }

    /// Creates a builder that parses fragment contents into an existing
    /// document. `context` must be the element whose children are parsed.
    pub(crate) fn for_fragment(document: Document, context: NodeId, options: ParseOptions) -> Self {
        let mut builder = Self::new(options);
        builder.document = document;
        builder.html = builder.document.document_element();
        let (head, body) = builder.document.head_and_body();
        builder.head = head;
        builder.body = body;
        builder.open = [builder.html, builder.body, Some(context)]
            .into_iter()
            .flatten()
            .collect();
        builder.mode = fragment_mode(&builder.document, context);
        builder.frameset_ok = false;
        builder
    }

    /// Sets the tokenizer position recorded with subsequent parse errors.
    pub fn set_position(&mut self, position: usize) {
        self.position = position;
    }

    /// Feeds one token into the tree builder.
    pub fn feed(&mut self, token: Token) {
        if self.eof {
            return;
        }
        let token = normalize(token);
        if matches!(token, Token::Eof) {
            self.eof = true;
            return;
        }
        // Once the node budget is exhausted, ignore further content. The tree
        // built so far stays well-formed and `finish` still repairs the basic
        // document structure.
        if self.document.node_count() >= self.options.max_nodes {
            if !self.node_limit_reached {
                self.node_limit_reached = true;
                self.error("maximum DOM node count reached; remaining content discarded");
            }
            return;
        }
        self.process(token);
    }

    /// Whether the node cap has been reached and further tokens are ignored.
    #[must_use]
    pub fn is_saturated(&self) -> bool {
        self.node_limit_reached
    }

    /// Completes parsing and returns the document together with all errors.
    pub fn finish(mut self) -> ParseResult {
        if let Some(title) = self.title {
            self.document.title = self.document.text_content(title);
        }
        self.ensure_structure();
        ParseResult {
            document: self.document,
            errors: self.errors,
        }
    }

    fn error(&mut self, message: &str) {
        self.errors.push(ParseError {
            message: message.to_string(),
            position: self.position,
        });
    }

    fn ensure_structure(&mut self) {
        let html = if let Some(html) = self.html {
            html
        } else if let Some(html) = self.document.document_element() {
            self.html = Some(html);
            html
        } else {
            let html = self.document.create_element("html");
            self.document.append_child(self.document.root(), html);
            self.html = Some(html);
            html
        };
        if self.head.is_none() {
            let head = self.document.create_element("head");
            if let Some(first) = self.document.children(html).first().copied() {
                self.document.insert_before(html, head, first);
            } else {
                self.document.append_child(html, head);
            }
            self.head = Some(head);
        }
        if self.body.is_none() {
            if let Some(body) = self.document.head_and_body().1 {
                self.body = Some(body);
            } else {
                let body = self.document.create_element("body");
                self.document.append_child(html, body);
                self.body = Some(body);
            }
        }
    }

    fn current_node(&self) -> NodeId {
        self.open
            .last()
            .copied()
            .unwrap_or_else(|| self.document.root())
    }

    fn current_name(&self) -> Option<String> {
        self.open
            .last()
            .and_then(|id| self.document.element_name(*id))
            .map(str::to_string)
    }

    fn current_is(&self, name: &str) -> bool {
        self.current_name().as_deref() == Some(name)
    }

    fn create_element(
        &mut self,
        name: &str,
        namespace: Namespace,
        attributes: Vec<(String, String)>,
    ) -> NodeId {
        let id = self.document.create_element_ns(name, namespace);
        for (key, value) in attributes {
            self.document.set_attr(id, &key, &value);
        }
        id
    }

    /// Pushes an element onto the open stack unless the depth cap is reached.
    /// All pushes go through here so the bound cannot be bypassed by implicit
    /// structure, table handling or foreign content.
    fn push_open(&mut self, id: NodeId) {
        if self.open.len() < self.options.max_depth.max(1) {
            self.open.push(id);
        } else if !self.depth_limit_reached {
            self.depth_limit_reached = true;
            self.error("maximum element depth reached");
        }
    }

    fn insert_element(
        &mut self,
        name: &str,
        namespace: Namespace,
        attributes: Vec<(String, String)>,
    ) -> NodeId {
        let parent = self.current_node();
        let id = self.create_element(name, namespace, attributes);
        self.document.append_child(parent, id);
        self.push_open(id);
        id
    }

    fn insert_void(
        &mut self,
        name: &str,
        namespace: Namespace,
        attributes: Vec<(String, String)>,
    ) -> NodeId {
        debug_assert!(
            is_void(name),
            "insert_void called with non-void element {name}"
        );
        let parent = self.current_node();
        let id = self.create_element(name, namespace, attributes);
        self.document.append_child(parent, id);
        id
    }

    fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let parent = self.current_node();
        self.document.append_text_merged(parent, text);
    }

    fn insert_comment(&mut self, parent: NodeId, text: &str) {
        let comment = self.document.create_comment(text);
        self.document.append_child(parent, comment);
    }

    fn insert_doctype(&mut self, name: &str, public_id: &str, system_id: &str) {
        let id = self.document.create_doctype(name);
        if let NodeData::Doctype {
            name: node_name,
            public_id: node_public,
            system_id: node_system,
        } = &mut self.document.node_mut(id).data
        {
            *node_name = name.to_string();
            *node_public = public_id.to_string();
            *node_system = system_id.to_string();
        }
        self.document.append_child(self.document.root(), id);
    }

    fn merge_attributes(&mut self, id: NodeId, attributes: Vec<(String, String)>) {
        for (key, value) in attributes {
            if !self.document.has_attr(id, &key) {
                self.document.set_attr(id, &key, &value);
            }
        }
    }

    fn element_attributes(&self, id: NodeId) -> Vec<(String, String)> {
        self.document
            .attributes(id)
            .map(|attributes| {
                attributes
                    .iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn process(&mut self, token: Token) {
        if !matches!(token, Token::Eof) && self.in_foreign_content() && self.process_foreign(&token)
        {
            return;
        }
        match self.mode {
            InsertionMode::Initial => self.in_initial(token),
            InsertionMode::BeforeHtml => self.in_before_html(token),
            InsertionMode::BeforeHead => self.in_before_head(token),
            InsertionMode::InHead => self.in_head(token),
            InsertionMode::InHeadNoscript => self.in_head_noscript(token),
            InsertionMode::AfterHead => self.in_after_head(token),
            InsertionMode::InBody => self.in_body(token),
            InsertionMode::Text => self.in_text(token),
            InsertionMode::InTable => self.in_table(token),
            InsertionMode::InCaption => self.in_caption(token),
            InsertionMode::InColumnGroup => self.in_column_group(token),
            InsertionMode::InTableBody => self.in_table_body(token),
            InsertionMode::InRow => self.in_row(token),
            InsertionMode::InCell => self.in_cell(token),
            InsertionMode::InSelect => self.in_select(token),
            InsertionMode::InSelectInTable => self.in_select_in_table(token),
            InsertionMode::AfterBody => self.in_after_body(token),
            InsertionMode::AfterAfterBody => self.in_after_after_body(token),
        }
    }

    // ---------------------------------------------------------------------
    // Implicit document structure
    // ---------------------------------------------------------------------

    fn create_html(&mut self, attributes: Vec<(String, String)>) -> NodeId {
        let id = self.create_element("html", Namespace::Html, attributes);
        self.document.append_child(self.document.root(), id);
        self.push_open(id);
        self.html = Some(id);
        id
    }

    fn implicit_head(&mut self) {
        let parent = self.current_node();
        let head = self.create_element("head", Namespace::Html, Vec::new());
        self.document.append_child(parent, head);
        self.push_open(head);
        self.head = Some(head);
        self.mode = InsertionMode::InHead;
    }

    fn implicit_body(&mut self) {
        let html = self.html.unwrap_or_else(|| self.document.root());
        let body = self.create_element("body", Namespace::Html, Vec::new());
        self.document.append_child(html, body);
        self.push_open(body);
        self.body = Some(body);
        self.frameset_ok = false;
        self.mode = InsertionMode::InBody;
    }

    fn in_initial(&mut self, token: Token) {
        match token {
            Token::Doctype {
                name,
                public_id,
                system_id,
            } => {
                self.insert_doctype(&name, &public_id, &system_id);
                self.mode = InsertionMode::BeforeHtml;
            }
            Token::Comment(text) => self.insert_comment(self.document.root(), &text),
            Token::Text(text) if is_whitespace(&text) => {}
            Token::Text(_) | Token::Character(_) => {
                self.error("unexpected character before the document type");
                self.mode = InsertionMode::BeforeHtml;
                self.process(token);
            }
            Token::StartTag { .. } | Token::EndTag { .. } => {
                self.error("unexpected token before the document type");
                self.mode = InsertionMode::BeforeHtml;
                self.process(token);
            }
            Token::Eof => {}
        }
    }

    fn in_before_html(&mut self, token: Token) {
        match token {
            Token::Doctype { .. } => self.error("unexpected doctype"),
            Token::Comment(text) => self.insert_comment(self.document.root(), &text),
            Token::Text(text) if is_whitespace(&text) => {}
            Token::StartTag {
                name, attributes, ..
            } if name == "html" => {
                self.create_html(attributes);
                self.mode = InsertionMode::BeforeHead;
            }
            Token::EndTag { name } if matches!(name.as_str(), "head" | "body" | "html" | "br") => {
                self.create_html(Vec::new());
                self.mode = InsertionMode::BeforeHead;
                self.process(Token::EndTag { name });
            }
            Token::EndTag { .. } => self.error("unexpected end tag before html"),
            Token::Eof => {}
            other => {
                self.create_html(Vec::new());
                self.mode = InsertionMode::BeforeHead;
                self.process(other);
            }
        }
    }

    fn in_before_head(&mut self, token: Token) {
        match token {
            Token::Doctype { .. } => self.error("unexpected doctype"),
            Token::Comment(text) => {
                let parent = self.current_node();
                self.insert_comment(parent, &text);
            }
            Token::Text(text) if is_whitespace(&text) => {}
            Token::StartTag {
                name, attributes, ..
            } if name == "html" => {
                if let Some(html) = self.html {
                    self.merge_attributes(html, attributes);
                }
            }
            Token::StartTag {
                name, attributes, ..
            } if name == "head" => {
                let parent = self.current_node();
                let head = self.create_element("head", Namespace::Html, attributes);
                self.document.append_child(parent, head);
                self.push_open(head);
                self.head = Some(head);
                self.mode = InsertionMode::InHead;
            }
            Token::EndTag { name } if matches!(name.as_str(), "head" | "body" | "html" | "br") => {
                self.implicit_head();
                self.process(Token::EndTag { name });
            }
            Token::EndTag { .. } => self.error("unexpected end tag before head"),
            Token::Eof => {}
            other => {
                self.implicit_head();
                self.process(other);
            }
        }
    }

    // ---------------------------------------------------------------------
    // Head
    // ---------------------------------------------------------------------

    fn handle_head_start(&mut self, name: &str, attributes: Vec<(String, String)>) -> bool {
        match name {
            "html" => {
                if let Some(html) = self.html {
                    self.merge_attributes(html, attributes);
                }
            }
            "base" | "basefont" | "bgsound" | "link" | "meta" => {
                let id = self.insert_void(name, Namespace::Html, attributes);
                if name == "base"
                    && self.document.base_url.is_none()
                    && let Some(href) = self.document.attr(id, "href")
                {
                    self.document.base_url = Some(href.to_string());
                }
            }
            "title" => {
                let id = self.insert_element("title", Namespace::Html, attributes);
                if !self.title_seen {
                    self.title_seen = true;
                    self.title = Some(id);
                }
                self.enter_text(self.mode);
            }
            "noscript" => {
                let id = self.insert_element("noscript", Namespace::Html, attributes);
                if self.options.scripting {
                    self.enter_text(self.mode);
                } else {
                    let _ = id;
                    self.mode = InsertionMode::InHeadNoscript;
                }
            }
            "noframes" | "style" | "script" => {
                self.insert_element(name, Namespace::Html, attributes);
                self.enter_text(self.mode);
            }
            "template" => self.start_template(),
            _ => return false,
        }
        true
    }

    fn enter_text(&mut self, original: InsertionMode) {
        self.original_mode = original;
        self.mode = InsertionMode::Text;
    }

    fn start_template(&mut self) {
        let id = self.insert_element("template", Namespace::Html, Vec::new());
        self.formatting.push(FormattingEntry::Marker);
        self.template_stack.push((id, self.mode));
        self.frameset_ok = false;
        self.mode = InsertionMode::InBody;
    }

    fn close_template(&mut self) {
        let Some((id, restore)) = self.template_stack.pop() else {
            self.error("unexpected template end tag");
            return;
        };
        self.generate_implied_end_tags(None);
        self.pop_until_id(id);
        self.clear_formatting_to_marker();
        self.mode = restore;
    }

    fn in_head(&mut self, token: Token) {
        self.in_head_rest(token);
    }

    fn in_head_rest(&mut self, token: Token) {
        match token {
            Token::Text(text) => {
                if is_whitespace(&text) {
                    self.insert_text(&text);
                } else {
                    self.close_head();
                    self.process(Token::Text(text));
                }
            }
            Token::Character(c) => {
                if c.is_ascii_whitespace() {
                    self.insert_text(&c.to_string());
                } else {
                    self.close_head();
                    self.process(Token::Character(c));
                }
            }
            Token::Comment(text) => {
                let parent = self.current_node();
                self.insert_comment(parent, &text);
            }
            Token::Doctype { .. } => self.error("unexpected doctype in head"),
            Token::StartTag {
                name,
                attributes,
                self_closing,
            } => {
                if name == "head" {
                    self.error("unexpected head start tag");
                } else if matches!(
                    name.as_str(),
                    "base"
                        | "basefont"
                        | "bgsound"
                        | "link"
                        | "meta"
                        | "noframes"
                        | "script"
                        | "style"
                        | "template"
                        | "title"
                        | "noscript"
                ) {
                    self.handle_head_start(&name, attributes);
                } else {
                    self.close_head();
                    self.process(Token::StartTag {
                        name,
                        attributes,
                        self_closing,
                    });
                }
            }
            Token::EndTag { name } => match name.as_str() {
                "head" => self.close_head(),
                "template" => self.close_template(),
                "body" | "html" | "br" => {
                    self.close_head();
                    self.process(Token::EndTag { name });
                }
                _ => {
                    self.error("unexpected end tag in head");
                    self.close_head();
                    self.process(Token::EndTag { name });
                }
            },
            Token::Eof => {}
        }
    }

    fn close_head(&mut self) {
        if let Some(head) = self.head {
            self.pop_until_id(head);
        }
        self.mode = InsertionMode::AfterHead;
    }

    fn in_head_noscript(&mut self, token: Token) {
        match token {
            Token::Doctype { .. } => self.error("unexpected doctype in noscript"),
            Token::StartTag {
                name, attributes, ..
            } if name == "html" => {
                self.in_body(Token::StartTag {
                    name,
                    attributes,
                    self_closing: false,
                });
            }
            Token::EndTag { name } if name == "noscript" => {
                self.pop_until("noscript");
                self.mode = InsertionMode::InHead;
            }
            Token::Text(text) if is_whitespace(&text) => self.insert_text(&text),
            Token::Comment(text) => {
                let parent = self.current_node();
                self.insert_comment(parent, &text);
            }
            Token::StartTag {
                name, attributes, ..
            } if matches!(
                name.as_str(),
                "basefont" | "bgsound" | "link" | "meta" | "noframes" | "style"
            ) =>
            {
                self.handle_head_start(&name, attributes);
            }
            Token::StartTag { name, .. } if matches!(name.as_str(), "head" | "noscript") => {
                self.error("unexpected start tag in noscript");
            }
            Token::EndTag { name } if name == "br" => {
                self.pop_until("noscript");
                self.mode = InsertionMode::InHead;
                self.process(Token::EndTag { name });
            }
            Token::EndTag { name } => {
                self.error("unexpected end tag in noscript");
                self.pop_until("noscript");
                self.mode = InsertionMode::InHead;
                self.process(Token::EndTag { name });
            }
            Token::Eof => {}
            other => {
                self.pop_until("noscript");
                self.mode = InsertionMode::InHead;
                self.process(other);
            }
        }
    }

    // ---------------------------------------------------------------------
    // After head / body boundaries
    // ---------------------------------------------------------------------

    fn in_after_head(&mut self, token: Token) {
        match token {
            Token::Text(text) if is_whitespace(&text) => {
                let parent = self.current_node();
                self.document.append_text_merged(parent, &text);
            }
            Token::Character(c) if c.is_ascii_whitespace() => {
                let parent = self.current_node();
                self.document.append_text_merged(parent, &c.to_string());
            }
            Token::Comment(text) => {
                let parent = self.current_node();
                self.insert_comment(parent, &text);
            }
            Token::Doctype { .. } => self.error("unexpected doctype after head"),
            Token::StartTag {
                name, attributes, ..
            } if name == "html" => {
                self.in_body(Token::StartTag {
                    name,
                    attributes,
                    self_closing: false,
                });
            }
            Token::StartTag {
                name, attributes, ..
            } if name == "body" => {
                let parent = self.current_node();
                let body = self.create_element("body", Namespace::Html, attributes);
                self.document.append_child(parent, body);
                self.push_open(body);
                self.body = Some(body);
                self.frameset_ok = false;
                self.mode = InsertionMode::InBody;
            }
            Token::StartTag {
                name, attributes, ..
            } if name == "frameset" => {
                let parent = self.current_node();
                let frameset = self.create_element("frameset", Namespace::Html, attributes);
                self.document.append_child(parent, frameset);
                self.push_open(frameset);
                self.mode = InsertionMode::InBody;
            }
            Token::StartTag {
                name, attributes, ..
            } if matches!(
                name.as_str(),
                "base"
                    | "basefont"
                    | "bgsound"
                    | "link"
                    | "meta"
                    | "noframes"
                    | "script"
                    | "style"
                    | "template"
                    | "title"
            ) =>
            {
                if let Some(head) = self.head
                    && !self.open.contains(&head)
                {
                    self.push_open(head);
                }
                self.handle_head_start(&name, attributes);
                if let Some(head) = self.head {
                    self.open.retain(|id| *id != head);
                }
            }
            Token::EndTag { name } if name == "template" => self.close_template(),
            Token::StartTag { name, .. } if name == "head" => {
                self.error("unexpected head start tag after head");
            }
            Token::EndTag { name } if matches!(name.as_str(), "body" | "html" | "br") => {
                self.implicit_body();
                self.process(Token::EndTag { name });
            }
            Token::EndTag { .. } => self.error("unexpected end tag after head"),
            Token::Eof => {}
            other => {
                self.implicit_body();
                self.process(other);
            }
        }
    }

    fn in_after_body(&mut self, token: Token) {
        match token {
            Token::Text(text) if is_whitespace(&text) => self.in_body(Token::Text(text)),
            Token::Character(c) if c.is_ascii_whitespace() => self.in_body(Token::Character(c)),
            Token::Comment(text) => {
                let parent = self.html.unwrap_or_else(|| self.document.root());
                self.insert_comment(parent, &text);
            }
            Token::Doctype { .. } => self.error("unexpected doctype after body"),
            Token::StartTag {
                name, attributes, ..
            } if name == "html" => {
                self.in_body(Token::StartTag {
                    name,
                    attributes,
                    self_closing: false,
                });
            }
            Token::EndTag { name } if name == "html" => {
                self.mode = InsertionMode::AfterAfterBody;
            }
            Token::Eof => {}
            other => {
                self.error("unexpected token after body");
                self.mode = InsertionMode::InBody;
                self.process(other);
            }
        }
    }

    fn in_after_after_body(&mut self, token: Token) {
        match token {
            Token::Comment(text) => self.insert_comment(self.document.root(), &text),
            Token::Text(text) if is_whitespace(&text) => {
                self.mode = InsertionMode::InBody;
                self.process(Token::Text(text));
            }
            Token::Doctype {
                name,
                public_id,
                system_id,
            } => {
                self.mode = InsertionMode::InBody;
                self.process(Token::Doctype {
                    name,
                    public_id,
                    system_id,
                });
            }
            Token::StartTag {
                name,
                attributes,
                self_closing,
            } if name == "html" => {
                self.mode = InsertionMode::InBody;
                self.process(Token::StartTag {
                    name,
                    attributes,
                    self_closing,
                });
            }
            Token::Eof => {}
            other => {
                self.error("unexpected token after html");
                self.mode = InsertionMode::InBody;
                self.process(other);
            }
        }
    }

    // ---------------------------------------------------------------------
    // Body
    // ---------------------------------------------------------------------

    fn in_body(&mut self, token: Token) {
        match token {
            Token::Text(text) => self.in_body_text(&text),
            Token::Character(c) => self.in_body_text(&c.to_string()),
            Token::Comment(text) => {
                let parent = self.current_node();
                self.insert_comment(parent, &text);
            }
            Token::Doctype { .. } => self.error("unexpected doctype in body"),
            Token::StartTag {
                name,
                attributes,
                self_closing,
            } => self.in_body_start_tag(&name, attributes, self_closing),
            Token::EndTag { name } => self.in_body_end_tag(&name),
            Token::Eof => {}
        }
    }

    fn in_body_text(&mut self, text: &str) {
        if !is_whitespace(text) {
            self.frameset_ok = false;
        }
        self.reconstruct_formatting();
        self.insert_text(text);
    }

    fn in_body_start_tag(
        &mut self,
        name: &str,
        attributes: Vec<(String, String)>,
        self_closing: bool,
    ) {
        if matches!(
            name,
            "base"
                | "basefont"
                | "bgsound"
                | "link"
                | "meta"
                | "noframes"
                | "script"
                | "style"
                | "template"
                | "title"
        ) {
            self.handle_head_start(name, attributes);
            return;
        }

        match name {
            "html" => {
                if let Some(html) = self.html {
                    self.merge_attributes(html, attributes);
                }
            }
            "body" => {
                if let Some(body) = self.body {
                    self.merge_attributes(body, attributes);
                    self.frameset_ok = false;
                }
            }
            "frameset" => {
                if self.frameset_ok {
                    self.insert_element("frameset", Namespace::Html, attributes);
                } else {
                    self.error("unexpected frameset");
                }
            }
            "address" | "article" | "aside" | "blockquote" | "center" | "details" | "dialog"
            | "dir" | "div" | "dl" | "fieldset" | "figcaption" | "figure" | "footer" | "header"
            | "hgroup" | "main" | "menu" | "nav" | "ol" | "p" | "section" | "summary" | "ul" => {
                self.close_p();
                self.insert_element(name, Namespace::Html, attributes);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.close_p();
                if self
                    .current_name()
                    .is_some_and(|current| is_heading(&current))
                {
                    self.open.pop();
                }
                self.insert_element(name, Namespace::Html, attributes);
            }
            "pre" | "listing" => {
                self.close_p();
                self.insert_element(name, Namespace::Html, attributes);
                self.frameset_ok = false;
            }
            "form" => {
                if self.form.is_some() {
                    self.error("nested form start tag");
                } else {
                    self.close_p();
                    let id = self.insert_element("form", Namespace::Html, attributes);
                    self.form = Some(id);
                }
            }
            "li" => {
                self.close_list_item(&["li"]);
                self.close_p();
                self.insert_element("li", Namespace::Html, attributes);
                self.frameset_ok = false;
            }
            "dd" | "dt" => {
                self.close_list_item(&["dd", "dt"]);
                self.close_p();
                self.insert_element(name, Namespace::Html, attributes);
                self.frameset_ok = false;
            }
            "plaintext" => {
                self.close_p();
                self.insert_element(name, Namespace::Html, attributes);
            }
            "button" => {
                if self.has_in_scope("button", false) {
                    self.generate_implied_end_tags(None);
                    self.pop_until("button");
                }
                self.reconstruct_formatting();
                self.insert_element("button", Namespace::Html, attributes);
                self.frameset_ok = false;
            }
            "a" => {
                if self.formatting.iter().any(
                    |entry| matches!(entry, FormattingEntry::Element { name, .. } if name == "a"),
                ) {
                    self.adoption_agency("a");
                }
                self.reconstruct_formatting();
                let id = self.insert_element("a", Namespace::Html, attributes);
                self.push_formatting(id, "a");
            }
            "nobr" => {
                self.reconstruct_formatting();
                if self.has_in_scope("nobr", false) {
                    self.adoption_agency("nobr");
                    self.reconstruct_formatting();
                }
                let id = self.insert_element("nobr", Namespace::Html, attributes);
                self.push_formatting(id, "nobr");
            }
            "applet" | "marquee" | "object" => {
                self.reconstruct_formatting();
                self.insert_element(name, Namespace::Html, attributes);
                self.formatting.push(FormattingEntry::Marker);
                self.frameset_ok = false;
            }
            _ if is_formatting(name) => {
                self.reconstruct_formatting();
                let id = self.insert_element(name, Namespace::Html, attributes);
                self.push_formatting(id, name);
            }
            "table" => {
                self.close_p();
                self.insert_element("table", Namespace::Html, attributes);
                self.frameset_ok = false;
                self.mode = InsertionMode::InTable;
            }
            "area" | "br" | "embed" | "img" | "keygen" | "wbr" => {
                self.reconstruct_formatting();
                self.insert_void(name, Namespace::Html, attributes);
                self.frameset_ok = false;
            }
            "input" => {
                self.reconstruct_formatting();
                self.insert_void("input", Namespace::Html, attributes);
                self.frameset_ok = false;
            }
            "param" | "source" | "track" => {
                self.insert_void(name, Namespace::Html, attributes);
            }
            "hr" => {
                self.close_p();
                self.insert_void("hr", Namespace::Html, attributes);
                self.frameset_ok = false;
            }
            "image" => {
                self.reconstruct_formatting();
                self.insert_void("img", Namespace::Html, attributes);
                self.frameset_ok = false;
            }
            "textarea" => {
                self.insert_element("textarea", Namespace::Html, attributes);
                self.frameset_ok = false;
                self.enter_text(InsertionMode::InBody);
            }
            "xmp" => {
                self.close_p();
                self.reconstruct_formatting();
                self.insert_element("xmp", Namespace::Html, attributes);
                self.frameset_ok = false;
                self.enter_text(InsertionMode::InBody);
            }
            "iframe" => {
                self.frameset_ok = false;
                self.insert_element("iframe", Namespace::Html, attributes);
                self.enter_text(InsertionMode::InBody);
            }
            "noembed" => {
                self.insert_element("noembed", Namespace::Html, attributes);
                self.enter_text(InsertionMode::InBody);
            }
            "noscript" if self.options.scripting => {
                self.insert_element("noscript", Namespace::Html, attributes);
                self.enter_text(InsertionMode::InBody);
            }
            "select" => {
                self.reconstruct_formatting();
                self.insert_element("select", Namespace::Html, attributes);
                self.frameset_ok = false;
                self.mode = if self.has_table_ancestor() {
                    InsertionMode::InSelectInTable
                } else {
                    InsertionMode::InSelect
                };
            }
            "option" => {
                if self.current_is("option") {
                    self.open.pop();
                }
                self.reconstruct_formatting();
                self.insert_element("option", Namespace::Html, attributes);
            }
            "optgroup" => {
                if self.current_is("option") {
                    self.open.pop();
                }
                if self.current_is("optgroup") {
                    self.open.pop();
                }
                self.reconstruct_formatting();
                self.insert_element("optgroup", Namespace::Html, attributes);
            }
            "rb" | "rtc" => {
                if self.has_in_scope("ruby", false) {
                    self.generate_implied_end_tags(None);
                }
                self.insert_element(name, Namespace::Html, attributes);
            }
            "rp" | "rt" => {
                if self.has_in_scope("ruby", false) {
                    self.generate_implied_end_tags(Some("rtc"));
                }
                self.insert_element(name, Namespace::Html, attributes);
            }
            "math" => {
                self.reconstruct_formatting();
                self.insert_element("math", Namespace::MathMl, attributes);
                if self_closing {
                    self.open.pop();
                }
            }
            "svg" => {
                self.reconstruct_formatting();
                self.insert_element("svg", Namespace::Svg, attributes);
                if self_closing {
                    self.open.pop();
                }
            }
            "caption" | "col" | "colgroup" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr" => {
                self.error("unexpected table element outside a table");
            }
            _ => {
                self.reconstruct_formatting();
                self.insert_element(name, Namespace::Html, attributes);
            }
        }
    }

    fn in_body_end_tag(&mut self, name: &str) {
        match name {
            "body" => {
                if self.has_in_scope("body", false) {
                    self.mode = InsertionMode::AfterBody;
                } else {
                    self.error("unexpected body end tag");
                }
            }
            "html" => {
                if self.has_in_scope("body", false) {
                    self.mode = InsertionMode::AfterBody;
                    self.in_after_body(Token::EndTag {
                        name: "html".to_string(),
                    });
                } else {
                    self.error("unexpected html end tag");
                }
            }
            "p" => {
                if !self.has_in_scope("p", true) {
                    self.insert_element("p", Namespace::Html, Vec::new());
                }
                self.generate_implied_end_tags(Some("p"));
                self.pop_until("p");
            }
            "li" => {
                if self.has_in_scope("li", false) {
                    self.generate_implied_end_tags(Some("li"));
                    self.pop_until("li");
                } else {
                    self.error("unexpected li end tag");
                }
            }
            "dd" | "dt" => {
                if self.has_in_scope(name, false) {
                    self.generate_implied_end_tags(Some(name));
                    self.pop_until(name);
                } else {
                    self.error("unexpected definition list end tag");
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if HEADINGS
                    .iter()
                    .any(|heading| self.has_in_scope(heading, false))
                {
                    self.generate_implied_end_tags(None);
                    if let Some(heading) = self
                        .open
                        .iter()
                        .rev()
                        .find(|id| self.document.element_name(**id).is_some_and(is_heading))
                        .copied()
                    {
                        self.pop_until_id(heading);
                    }
                } else {
                    self.error("unexpected heading end tag");
                }
            }
            "form" => {
                let Some(form) = self.form.take() else {
                    self.error("unexpected form end tag");
                    return;
                };
                if self.open.contains(&form) {
                    self.pop_until_id(form);
                }
            }
            "br" => {
                self.error("unexpected br end tag");
                self.reconstruct_formatting();
                self.insert_void("br", Namespace::Html, Vec::new());
            }
            "template" => self.close_template(),
            "select" => {
                if self.has_in_scope("select", false) {
                    self.pop_until("select");
                    self.reset_mode();
                } else {
                    self.error("unexpected select end tag");
                }
            }
            "table" => {
                if self.has_in_scope("table", false) {
                    self.pop_until("table");
                    self.reset_mode();
                } else {
                    self.error("unexpected table end tag");
                }
            }
            "applet" | "caption" | "marquee" | "object" | "td" | "th" => {
                if self.has_in_scope(name, false) {
                    self.generate_implied_end_tags(None);
                    self.pop_until(name);
                }
            }
            _ if is_formatting(name) => self.adoption_agency(name),
            _ => {
                if matches!(
                    name,
                    "address"
                        | "article"
                        | "aside"
                        | "blockquote"
                        | "button"
                        | "center"
                        | "details"
                        | "dialog"
                        | "dir"
                        | "div"
                        | "dl"
                        | "fieldset"
                        | "figcaption"
                        | "figure"
                        | "footer"
                        | "header"
                        | "hgroup"
                        | "listing"
                        | "main"
                        | "menu"
                        | "nav"
                        | "ol"
                        | "pre"
                        | "section"
                        | "summary"
                        | "ul"
                ) {
                    if self.has_in_scope(name, false) {
                        self.generate_implied_end_tags(Some(name));
                        self.pop_until(name);
                    } else {
                        self.error("unexpected end tag for a block element");
                    }
                } else {
                    self.any_other_end_tag(name);
                }
            }
        }
    }

    fn any_other_end_tag(&mut self, name: &str) {
        if let Some(position) = self
            .open
            .iter()
            .rposition(|id| self.document.is_tag(*id, name))
        {
            self.generate_implied_end_tags(Some(name));
            self.open.truncate(position);
            if self.current_name().is_none() {
                self.error("popped the last open element");
            }
        } else {
            self.error("unexpected end tag with no matching element");
        }
    }

    // ---------------------------------------------------------------------
    // Text mode
    // ---------------------------------------------------------------------

    fn in_text(&mut self, token: Token) {
        match token {
            Token::Text(text) => self.insert_text(&text),
            Token::Character(c) => self.insert_text(&c.to_string()),
            Token::EndTag { .. } => {
                let popped = self.open.pop();
                self.mode = self.original_mode;
                if let Some(id) = popped
                    && self.title == Some(id)
                {
                    self.document.title = self.document.text_content(id);
                    self.title = None;
                }
            }
            Token::Comment(text) => {
                let parent = self.current_node();
                self.insert_comment(parent, &text);
            }
            Token::StartTag { name, .. } => {
                self.error("unexpected start tag in text mode");
                self.insert_text(&format!("<{name}>"));
            }
            Token::Doctype { .. } => self.error("unexpected doctype in text mode"),
            Token::Eof => {}
        }
    }

    // ---------------------------------------------------------------------
    // Tables
    // ---------------------------------------------------------------------

    fn in_table(&mut self, token: Token) {
        match token {
            Token::Text(text) => {
                if is_whitespace(&text) {
                    self.insert_text(&text);
                } else {
                    self.in_body_text(&text);
                }
            }
            Token::Character(c) => {
                if c.is_ascii_whitespace() {
                    self.insert_text(&c.to_string());
                } else {
                    self.in_body_text(&c.to_string());
                }
            }
            Token::Comment(text) => {
                let parent = self.current_node();
                self.insert_comment(parent, &text);
            }
            Token::Doctype { .. } => self.error("unexpected doctype in table"),
            Token::StartTag {
                name,
                attributes,
                self_closing,
            } => match name.as_str() {
                "caption" => {
                    self.clear_to_table_context();
                    self.insert_element("caption", Namespace::Html, attributes);
                    self.formatting.push(FormattingEntry::Marker);
                    self.mode = InsertionMode::InCaption;
                }
                "colgroup" => {
                    self.clear_to_table_context();
                    self.insert_element("colgroup", Namespace::Html, attributes);
                    self.mode = InsertionMode::InColumnGroup;
                }
                "col" => {
                    self.clear_to_table_context();
                    self.insert_element("colgroup", Namespace::Html, Vec::new());
                    self.mode = InsertionMode::InColumnGroup;
                    self.in_column_group(Token::StartTag {
                        name: name.clone(),
                        attributes,
                        self_closing,
                    });
                }
                "tbody" | "tfoot" | "thead" => {
                    self.clear_to_table_context();
                    self.insert_element(&name, Namespace::Html, attributes);
                    self.mode = InsertionMode::InTableBody;
                }
                "td" | "th" | "tr" => {
                    self.clear_to_table_context();
                    self.insert_element("tbody", Namespace::Html, Vec::new());
                    self.mode = InsertionMode::InTableBody;
                    self.in_table_body(Token::StartTag {
                        name: name.clone(),
                        attributes,
                        self_closing,
                    });
                }
                "table" => {
                    if self.has_in_scope("table", false) {
                        self.pop_until("table");
                        self.reset_mode();
                        self.process(Token::StartTag {
                            name: name.clone(),
                            attributes,
                            self_closing,
                        });
                    } else {
                        self.error("unexpected table start tag");
                    }
                }
                "style" | "script" | "template" => {
                    self.handle_head_start(&name, attributes);
                }
                "input" => {
                    self.insert_void("input", Namespace::Html, attributes);
                }
                "form" => {
                    if self.form.is_none() {
                        let id = self.insert_element("form", Namespace::Html, attributes);
                        self.form = Some(id);
                        self.open.pop();
                    } else {
                        self.error("nested form in table");
                    }
                }
                _ => {
                    self.error("unexpected start tag in table");
                    self.in_body_start_tag(&name, attributes, self_closing);
                }
            },
            Token::EndTag { name } => match name.as_str() {
                "table" => {
                    if self.has_in_scope("table", false) {
                        self.pop_until("table");
                        self.reset_mode();
                    } else {
                        self.error("unexpected table end tag");
                    }
                }
                "body" | "caption" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot"
                | "th" | "thead" | "tr" => self.error("unexpected end tag in table"),
                "template" => self.close_template(),
                _ => self.in_body_end_tag(&name),
            },
            Token::Eof => {}
        }
    }

    fn clear_to_table_context(&mut self) {
        while let Some(name) = self.current_name() {
            if matches!(name.as_str(), "table" | "template" | "html") {
                break;
            }
            self.open.pop();
        }
    }

    fn clear_to_table_body_context(&mut self) {
        while let Some(name) = self.current_name() {
            if matches!(
                name.as_str(),
                "tbody" | "tfoot" | "thead" | "template" | "html"
            ) {
                break;
            }
            self.open.pop();
        }
    }

    fn clear_to_table_row_context(&mut self) {
        while let Some(name) = self.current_name() {
            if matches!(name.as_str(), "tr" | "template" | "html") {
                break;
            }
            self.open.pop();
        }
    }

    fn in_caption(&mut self, token: Token) {
        let original = token.clone();
        match token {
            Token::EndTag { name } if name == "caption" => self.close_caption(),
            Token::StartTag { name, .. }
                if matches!(
                    name.as_str(),
                    "caption"
                        | "col"
                        | "colgroup"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
            {
                self.close_caption();
                self.process(original);
            }
            Token::EndTag { name } if name == "table" => {
                self.close_caption();
                self.process(Token::EndTag { name });
            }
            Token::EndTag { name }
                if matches!(
                    name.as_str(),
                    "body"
                        | "col"
                        | "colgroup"
                        | "html"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
            {
                self.error("unexpected end tag in caption");
            }
            other => self.in_body(other),
        }
    }

    fn close_caption(&mut self) {
        if self.has_in_scope("caption", false) {
            self.generate_implied_end_tags(None);
            self.pop_until("caption");
            self.clear_formatting_to_marker();
        } else {
            self.error("unexpected caption end tag");
        }
        self.mode = InsertionMode::InTable;
    }

    fn in_column_group(&mut self, token: Token) {
        match token {
            Token::Text(text) if is_whitespace(&text) => self.insert_text(&text),
            Token::Character(c) if c.is_ascii_whitespace() => self.insert_text(&c.to_string()),
            Token::Comment(text) => {
                let parent = self.current_node();
                self.insert_comment(parent, &text);
            }
            Token::Doctype { .. } => self.error("unexpected doctype in colgroup"),
            Token::StartTag {
                name, attributes, ..
            } if name == "html" => {
                self.in_body(Token::StartTag {
                    name,
                    attributes,
                    self_closing: false,
                });
            }
            Token::StartTag {
                name, attributes, ..
            } if name == "col" => {
                self.insert_void("col", Namespace::Html, attributes);
            }
            Token::EndTag { name } if name == "colgroup" => {
                if self.current_is("colgroup") {
                    self.open.pop();
                }
                self.mode = InsertionMode::InTable;
            }
            Token::EndTag { name } if name == "col" => {
                self.error("unexpected col end tag");
            }
            Token::StartTag {
                name, attributes, ..
            } if name == "template" => {
                self.handle_head_start(&name, attributes);
            }
            Token::Eof => {}
            other => {
                if self.current_is("colgroup") {
                    self.open.pop();
                }
                self.mode = InsertionMode::InTable;
                self.process(other);
            }
        }
    }

    fn in_table_body(&mut self, token: Token) {
        let original = token.clone();
        match token {
            Token::StartTag {
                name, attributes, ..
            } if name == "tr" => {
                self.clear_to_table_body_context();
                self.insert_element("tr", Namespace::Html, attributes);
                self.mode = InsertionMode::InRow;
            }
            Token::StartTag {
                name,
                attributes,
                self_closing,
            } if matches!(name.as_str(), "th" | "td") => {
                self.error("cell start tag without a row");
                self.clear_to_table_body_context();
                self.insert_element("tr", Namespace::Html, Vec::new());
                self.mode = InsertionMode::InRow;
                self.in_row(Token::StartTag {
                    name,
                    attributes,
                    self_closing,
                });
            }
            Token::StartTag { name, .. }
                if matches!(
                    name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead"
                ) =>
            {
                if self.has_any_in_scope(&["tbody", "tfoot", "thead"], false) {
                    self.generate_implied_end_tags(None);
                    self.pop_until_any(&["tbody", "tfoot", "thead"]);
                }
                self.mode = InsertionMode::InTable;
                self.process(original);
            }
            Token::EndTag { name } if name == "table" => {
                if self.has_any_in_scope(&["tbody", "tfoot", "thead"], false) {
                    self.generate_implied_end_tags(None);
                    self.pop_until_any(&["tbody", "tfoot", "thead"]);
                }
                self.mode = InsertionMode::InTable;
                self.process(Token::EndTag { name });
            }
            Token::EndTag { name } if matches!(name.as_str(), "tbody" | "tfoot" | "thead") => {
                if self.has_in_scope(&name, false) {
                    self.generate_implied_end_tags(None);
                    self.pop_until(&name);
                    self.mode = InsertionMode::InTable;
                } else {
                    self.error("unexpected table section end tag");
                }
            }
            Token::EndTag { name }
                if matches!(
                    name.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th" | "tr"
                ) =>
            {
                self.error("unexpected end tag in table body");
            }
            other => self.in_table(other),
        }
    }

    fn in_row(&mut self, token: Token) {
        let original = token.clone();
        match token {
            Token::StartTag {
                name,
                attributes,
                self_closing,
            } if matches!(name.as_str(), "th" | "td") => {
                self.clear_to_table_row_context();
                self.insert_element(&name, Namespace::Html, attributes);
                self.formatting.push(FormattingEntry::Marker);
                self.mode = InsertionMode::InCell;
                let _ = self_closing;
            }
            Token::EndTag { name } if name == "tr" => {
                if self.has_in_scope("tr", false) {
                    self.generate_implied_end_tags(None);
                    self.pop_until("tr");
                    self.mode = InsertionMode::InTableBody;
                } else {
                    self.error("unexpected tr end tag");
                }
            }
            Token::StartTag { name, .. }
                if matches!(
                    name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead" | "tr"
                ) =>
            {
                self.close_row();
                self.process(original);
            }
            Token::EndTag { name } if name == "table" => {
                self.close_row();
                self.process(Token::EndTag { name });
            }
            Token::EndTag { name } if matches!(name.as_str(), "tbody" | "tfoot" | "thead") => {
                if self.has_in_scope("tr", false) {
                    self.close_row();
                    self.process(Token::EndTag { name });
                } else {
                    self.error("unexpected table section end tag in row");
                }
            }
            Token::EndTag { name }
                if matches!(
                    name.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th"
                ) =>
            {
                self.error("unexpected end tag in row");
            }
            other => self.in_table(other),
        }
    }

    fn close_row(&mut self) {
        if self.has_in_scope("tr", false) {
            self.generate_implied_end_tags(None);
            self.pop_until("tr");
        }
        self.mode = InsertionMode::InTableBody;
    }

    fn in_cell(&mut self, token: Token) {
        let original = token.clone();
        match token {
            Token::EndTag { name } if matches!(name.as_str(), "td" | "th") => {
                if self.has_in_scope(&name, false) {
                    self.close_cell(&name);
                } else {
                    self.error("unexpected cell end tag");
                }
            }
            Token::StartTag { name, .. }
                if matches!(
                    name.as_str(),
                    "caption"
                        | "col"
                        | "colgroup"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
            {
                self.close_any_cell();
                self.process(original);
            }
            Token::EndTag { name }
                if matches!(
                    name.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html"
                ) =>
            {
                self.error("unexpected end tag in cell");
            }
            Token::EndTag { name }
                if matches!(name.as_str(), "table" | "tbody" | "tfoot" | "thead" | "tr") =>
            {
                self.close_any_cell();
                self.process(Token::EndTag { name });
            }
            other => self.in_body(other),
        }
    }

    fn close_any_cell(&mut self) {
        let cell = self
            .open
            .iter()
            .rev()
            .find(|id| {
                self.document
                    .element_name(**id)
                    .is_some_and(|name| matches!(name, "td" | "th"))
            })
            .copied();
        if let Some(id) = cell
            && let Some(name) = self.document.element_name(id).map(str::to_string)
        {
            self.close_cell(&name);
        }
    }

    fn close_cell(&mut self, name: &str) {
        self.generate_implied_end_tags(None);
        self.pop_until(name);
        self.clear_formatting_to_marker();
        self.mode = InsertionMode::InRow;
    }

    // ---------------------------------------------------------------------
    // Select
    // ---------------------------------------------------------------------

    fn in_select(&mut self, token: Token) {
        match token {
            Token::Text(text) => self.insert_text(&text),
            Token::Character(c) => self.insert_text(&c.to_string()),
            Token::Comment(text) => {
                let parent = self.current_node();
                self.insert_comment(parent, &text);
            }
            Token::Doctype { .. } => self.error("unexpected doctype in select"),
            Token::StartTag {
                name, attributes, ..
            } => match name.as_str() {
                "html" => {
                    self.in_body(Token::StartTag {
                        name: name.clone(),
                        attributes,
                        self_closing: false,
                    });
                }
                "option" => {
                    if self.current_is("option") {
                        self.open.pop();
                    }
                    self.insert_element("option", Namespace::Html, attributes);
                }
                "optgroup" => {
                    if self.current_is("option") {
                        self.open.pop();
                    }
                    if self.current_is("optgroup") {
                        self.open.pop();
                    }
                    self.insert_element("optgroup", Namespace::Html, attributes);
                }
                "select" => {
                    self.error("nested select start tag");
                    if self.has_in_scope("select", false) {
                        self.pop_until("select");
                        self.reset_mode();
                    }
                }
                "input" | "keygen" | "textarea" => {
                    self.error("unexpected form control in select");
                    if self.has_in_scope("select", false) {
                        self.pop_until("select");
                        self.reset_mode();
                        self.process(Token::StartTag {
                            name: name.clone(),
                            attributes,
                            self_closing: false,
                        });
                    }
                }
                "script" | "template" => {
                    self.handle_head_start(&name, attributes);
                }
                _ => self.error("unexpected start tag in select"),
            },
            Token::EndTag { name } => match name.as_str() {
                "optgroup" => {
                    if self.current_is("option") {
                        self.open.pop();
                    }
                    if self.current_is("optgroup") {
                        self.open.pop();
                    } else {
                        self.error("unexpected optgroup end tag");
                    }
                }
                "option" => {
                    if self.current_is("option") {
                        self.open.pop();
                    } else {
                        self.error("unexpected option end tag");
                    }
                }
                "select" => {
                    if self.has_in_scope("select", false) {
                        self.pop_until("select");
                        self.reset_mode();
                    } else {
                        self.error("unexpected select end tag");
                    }
                }
                "template" => self.close_template(),
                _ => self.error("unexpected end tag in select"),
            },
            Token::Eof => {}
        }
    }

    fn in_select_in_table(&mut self, token: Token) {
        let is_table_boundary = |name: &str| {
            matches!(
                name,
                "caption" | "table" | "tbody" | "tfoot" | "thead" | "tr" | "td" | "th"
            )
        };
        let boundary = match &token {
            Token::StartTag { name, .. } | Token::EndTag { name } => is_table_boundary(name),
            _ => false,
        };
        if boundary {
            self.error("table boundary token inside select");
            if self.has_in_scope("select", false) {
                self.pop_until("select");
                self.reset_mode();
                self.process(token);
            }
        } else {
            self.in_select(token);
        }
    }

    // ---------------------------------------------------------------------
    // Scopes, implied end tags and formatting
    // ---------------------------------------------------------------------

    fn has_in_scope(&self, name: &str, button_scope: bool) -> bool {
        for id in self.open.iter().rev() {
            if self.document.is_tag(*id, name) {
                return true;
            }
            if let Some(current) = self.document.element_name(*id) {
                if SCOPE_BOUNDARIES.contains(&current) {
                    return false;
                }
                if button_scope && current == "button" {
                    return false;
                }
            }
        }
        false
    }

    fn has_any_in_scope(&self, names: &[&str], button_scope: bool) -> bool {
        names
            .iter()
            .any(|name| self.has_in_scope(name, button_scope))
    }

    fn has_in_scope_id(&self, target: NodeId) -> bool {
        for id in self.open.iter().rev() {
            if *id == target {
                return true;
            }
            if let Some(current) = self.document.element_name(*id)
                && SCOPE_BOUNDARIES.contains(&current)
            {
                return false;
            }
        }
        false
    }

    fn generate_implied_end_tags(&mut self, exclude: Option<&str>) {
        while let Some(name) = self.current_name() {
            if Some(name.as_str()) == exclude || !IMPLIED_END_TAGS.contains(&name.as_str()) {
                break;
            }
            self.open.pop();
        }
    }

    fn close_p(&mut self) {
        if self.has_in_scope("p", true) {
            self.generate_implied_end_tags(Some("p"));
            self.pop_until("p");
        }
    }

    fn close_list_item(&mut self, names: &[&str]) {
        if self.has_any_in_scope(names, false) {
            while let Some(name) = self.current_name() {
                if names.contains(&name.as_str()) {
                    break;
                }
                if !IMPLIED_END_TAGS.contains(&name.as_str()) {
                    break;
                }
                self.open.pop();
            }
            self.pop_until_any(names);
        }
    }

    fn pop_until(&mut self, name: &str) {
        if !self.open.iter().any(|id| self.document.is_tag(*id, name)) {
            return;
        }
        while let Some(id) = self.open.pop() {
            if self.document.is_tag(id, name) {
                break;
            }
        }
    }

    fn pop_until_any(&mut self, names: &[&str]) {
        while let Some(id) = self.open.pop() {
            if self
                .document
                .element_name(id)
                .is_some_and(|name| names.contains(&name))
            {
                break;
            }
        }
    }

    fn pop_until_id(&mut self, target: NodeId) {
        if !self.open.contains(&target) {
            return;
        }
        while let Some(id) = self.open.pop() {
            if id == target {
                break;
            }
        }
    }

    fn has_table_ancestor(&self) -> bool {
        self.open
            .iter()
            .any(|id| self.document.is_tag(*id, "table"))
    }

    fn push_formatting(&mut self, id: NodeId, name: &str) {
        self.formatting.push(FormattingEntry::Element {
            id,
            name: name.to_string(),
        });
    }

    fn clear_formatting_to_marker(&mut self) {
        while let Some(entry) = self.formatting.pop() {
            if matches!(entry, FormattingEntry::Marker) {
                break;
            }
        }
    }

    /// Re-creates formatting elements that remain on the active list but are
    /// no longer open, as required after a misnested block element.
    fn reconstruct_formatting(&mut self) {
        if self.formatting.is_empty() {
            return;
        }
        let mut start = 0;
        let mut found = false;
        for (index, entry) in self.formatting.iter().enumerate().rev() {
            match entry {
                FormattingEntry::Marker => {
                    start = index + 1;
                    found = true;
                    break;
                }
                FormattingEntry::Element { id, .. } => {
                    if self.open.contains(id) {
                        start = index + 1;
                        found = true;
                        break;
                    }
                }
            }
        }
        if !found {
            start = 0;
        }
        let mut index = start;
        while index < self.formatting.len() {
            let (id, name) = match &self.formatting[index] {
                FormattingEntry::Marker => {
                    index += 1;
                    continue;
                }
                FormattingEntry::Element { id, name } => (*id, name.clone()),
            };
            if !self.open.contains(&id) {
                let attributes = self.element_attributes(id);
                let new_id = self.insert_element(&name, Namespace::Html, attributes);
                self.formatting[index] = FormattingEntry::Element { id: new_id, name };
            }
            index += 1;
        }
    }

    /// Simplified adoption agency algorithm.
    ///
    /// Instead of the full WHATWG loop, the formatting element is closed along
    /// with every element above it, removed from the active formatting list,
    /// and the formatting elements that were above it are reopened. This turns
    /// `<b><i>x</b>y</i>` into `<b><i>x</i></b><i>y</i>`, a well-formed tree.
    fn adoption_agency(&mut self, name: &str) {
        let Some(formatting_index) = self.formatting.iter().rposition(|entry| {
            matches!(entry, FormattingEntry::Element { name: entry_name, .. } if entry_name == name)
        }) else {
            self.any_other_end_tag(name);
            return;
        };
        let FormattingEntry::Element { id, .. } = self.formatting[formatting_index] else {
            return;
        };
        let Some(open_index) = self.open.iter().position(|open_id| *open_id == id) else {
            self.formatting.remove(formatting_index);
            return;
        };
        if !self.has_in_scope_id(id) {
            return;
        }
        if open_index + 1 == self.open.len() {
            self.open.pop();
            self.formatting.remove(formatting_index);
            return;
        }

        let above: Vec<NodeId> = self.open[open_index + 1..].to_vec();
        self.open.truncate(open_index + 1);
        self.open.pop();
        self.formatting.remove(formatting_index);

        let mut index = formatting_index;
        while index < self.formatting.len() {
            let entry = match &self.formatting[index] {
                FormattingEntry::Marker => {
                    index += 1;
                    continue;
                }
                FormattingEntry::Element { id, name } => (*id, name.clone()),
            };
            if above.contains(&entry.0) {
                let attributes = self.element_attributes(entry.0);
                let new_id = self.insert_element(&entry.1, Namespace::Html, attributes);
                self.formatting[index] = FormattingEntry::Element {
                    id: new_id,
                    name: entry.1,
                };
            }
            index += 1;
        }
    }

    fn reset_mode(&mut self) {
        self.mode = self.mode_for_stack();
    }

    fn mode_for_stack(&self) -> InsertionMode {
        for id in self.open.iter().rev() {
            let Some(name) = self.document.element_name(*id) else {
                continue;
            };
            match name {
                "select" => return InsertionMode::InSelect,
                "td" | "th" => return InsertionMode::InCell,
                "tr" => return InsertionMode::InRow,
                "tbody" | "tfoot" | "thead" => return InsertionMode::InTableBody,
                "caption" => return InsertionMode::InCaption,
                "colgroup" => return InsertionMode::InColumnGroup,
                "table" => return InsertionMode::InTable,
                "template" => {
                    return self
                        .template_stack
                        .last()
                        .map_or(InsertionMode::InBody, |(_, restore)| *restore);
                }
                "head" => return InsertionMode::InHead,
                "body" => return InsertionMode::InBody,
                "html" => {
                    return if self.head.is_none() {
                        InsertionMode::BeforeHead
                    } else {
                        InsertionMode::AfterHead
                    };
                }
                _ => {}
            }
        }
        InsertionMode::InBody
    }

    // ---------------------------------------------------------------------
    // Foreign content (SVG and MathML)
    // ---------------------------------------------------------------------

    fn in_foreign_content(&self) -> bool {
        let Some(current) = self.open.last() else {
            return false;
        };
        let Some(element) = self.document.node(*current).element() else {
            return false;
        };
        if element.namespace == Namespace::Html {
            return false;
        }
        let lower_name = element.name.to_ascii_lowercase();
        let is_svg_integration = element.namespace == Namespace::Svg
            && matches!(lower_name.as_str(), "foreignobject" | "desc" | "title");
        let is_math_text_integration = element.namespace == Namespace::MathMl
            && matches!(lower_name.as_str(), "mi" | "mo" | "mn" | "ms" | "mtext");
        !is_svg_integration && !is_math_text_integration
    }

    /// Handles a token while in foreign content. Returns `false` when the
    /// token must be reprocessed with the HTML rules after popping foreign
    /// elements.
    fn process_foreign(&mut self, token: &Token) -> bool {
        match token {
            Token::Text(text) => {
                if !is_whitespace(text) {
                    self.frameset_ok = false;
                }
                self.insert_text(text);
                true
            }
            Token::Character(c) => {
                self.insert_text(&c.to_string());
                true
            }
            Token::Comment(text) => {
                let parent = self.current_node();
                self.insert_comment(parent, text);
                true
            }
            Token::Doctype { .. } => {
                self.error("unexpected doctype in foreign content");
                true
            }
            Token::StartTag {
                name,
                attributes,
                self_closing,
            } => {
                if is_foreign_breakout(name, attributes) {
                    self.pop_out_of_foreign_content();
                    return false;
                }
                let namespace = self.foreign_namespace(name);
                self.insert_element(name, namespace, attributes.clone());
                if *self_closing {
                    self.open.pop();
                }
                true
            }
            Token::EndTag { name } => {
                if matches!(name.as_str(), "br" | "p") {
                    self.pop_out_of_foreign_content();
                    return false;
                }
                if !self.open.iter().any(|id| self.document.is_tag(*id, name)) {
                    self.error("unexpected foreign end tag");
                    return true;
                }
                while let Some(id) = self.open.pop() {
                    if self.document.is_tag(id, name) {
                        break;
                    }
                    if self.document.namespace(id) == Some(Namespace::Html) {
                        self.push_open(id);
                        break;
                    }
                }
                true
            }
            Token::Eof => false,
        }
    }

    fn foreign_namespace(&self, name: &str) -> Namespace {
        match name {
            "svg" => Namespace::Svg,
            "math" => Namespace::MathMl,
            _ => self
                .open
                .last()
                .and_then(|id| self.document.namespace(*id))
                .unwrap_or(Namespace::Html),
        }
    }

    fn pop_out_of_foreign_content(&mut self) {
        while let Some(id) = self.open.last().copied() {
            let Some(element) = self.document.node(id).element() else {
                break;
            };
            let lower_name = element.name.to_ascii_lowercase();
            let is_integration = element.namespace != Namespace::Html
                && (matches!(lower_name.as_str(), "foreignobject" | "desc" | "title")
                    || matches!(lower_name.as_str(), "mi" | "mo" | "mn" | "ms" | "mtext"));
            if element.namespace == Namespace::Html || is_integration {
                break;
            }
            self.open.pop();
        }
    }
}

fn is_foreign_breakout(name: &str, attributes: &[(String, String)]) -> bool {
    if matches!(
        name,
        "b" | "big"
            | "blockquote"
            | "body"
            | "br"
            | "center"
            | "code"
            | "dd"
            | "div"
            | "dl"
            | "dt"
            | "em"
            | "embed"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "head"
            | "hr"
            | "i"
            | "img"
            | "li"
            | "listing"
            | "menu"
            | "meta"
            | "nobr"
            | "ol"
            | "p"
            | "pre"
            | "ruby"
            | "s"
            | "small"
            | "span"
            | "strong"
            | "strike"
            | "sub"
            | "sup"
            | "table"
            | "tt"
            | "u"
            | "ul"
            | "var"
    ) {
        return true;
    }
    name == "font"
        && attributes
            .iter()
            .any(|(key, _)| matches!(key.as_str(), "color" | "face" | "size"))
}

fn fragment_mode(document: &Document, context: NodeId) -> InsertionMode {
    match document.element_name(context) {
        Some("table") => InsertionMode::InTable,
        Some("caption") => InsertionMode::InCaption,
        Some("colgroup") => InsertionMode::InColumnGroup,
        Some("tbody" | "tfoot" | "thead") => InsertionMode::InTableBody,
        Some("tr") => InsertionMode::InRow,
        Some("td" | "th") => InsertionMode::InCell,
        Some("select") => InsertionMode::InSelect,
        _ => InsertionMode::InBody,
    }
}
