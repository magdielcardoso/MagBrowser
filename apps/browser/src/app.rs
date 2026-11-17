//! The interactive application loop.

use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::rc::Rc;

use browser_core::Browser;
use browser_dom::NodeData;

use crate::address_bar::AddressBar;
use crate::commands::{self, Command};
use crate::tabs::TabManager;
use crate::window::{FramePresenter, PngPresenter};

/// The browser application.
pub struct App {
    /// Shared browser state.
    pub browser: Rc<RefCell<Browser>>,
    /// Address bar.
    pub address_bar: AddressBar,
    /// Tab strip.
    pub tabs: TabManager,
    /// Frame presenter.
    pub presenter: Box<dyn FramePresenter>,
    /// Whether the app should keep running.
    running: bool,
}

impl App {
    /// Creates an app with a PNG presenter writing to `frames_dir`.
    pub fn new(browser: Browser, frames_dir: impl Into<std::path::PathBuf>) -> Self {
        let browser = Rc::new(RefCell::new(browser));
        Self {
            tabs: TabManager::new(browser.clone()),
            browser,
            address_bar: AddressBar::new(),
            presenter: Box::new(PngPresenter::new(frames_dir)),
            running: true,
        }
    }

    /// Runs the scripted commands, presenting a frame after each navigation.
    pub fn run_script(&mut self, script: &[String]) -> Result<(), String> {
        for line in script {
            let command = commands::parse(line);
            let quit = matches!(command, Command::Quit);
            self.execute(command)?;
            if quit {
                break;
            }
        }
        Ok(())
    }

    /// Runs the interactive loop on stdin/stdout.
    pub fn run_interactive(&mut self) -> Result<(), String> {
        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        self.print_help(&mut stdout)?;
        while self.running {
            write!(stdout, "mag> ").map_err(|err| err.to_string())?;
            stdout.flush().map_err(|err| err.to_string())?;
            let mut line = String::new();
            let read = stdin
                .lock()
                .read_line(&mut line)
                .map_err(|err| err.to_string())?;
            if read == 0 {
                break;
            }
            let command = commands::parse(&line);
            if let Err(error) = self.execute(command) {
                println!("error: {error}");
            }
        }
        Ok(())
    }

    /// Executes one command.
    pub fn execute(&mut self, command: Command) -> Result<(), String> {
        match command {
            Command::Open(target) => {
                self.address_bar.set(&target);
                let resolved = resolve_target(&target);
                self.browser
                    .borrow_mut()
                    .navigate(&resolved)
                    .map_err(|err| err.to_string())?;
                self.address_bar.sync(&self.browser.borrow());
                self.report();
            }
            Command::Reload => {
                self.browser
                    .borrow_mut()
                    .reload()
                    .map_err(|err| err.to_string())?;
                self.report();
            }
            Command::Back => {
                self.browser
                    .borrow_mut()
                    .back()
                    .map_err(|err| err.to_string())?;
                self.address_bar.sync(&self.browser.borrow());
                self.report();
            }
            Command::Forward => {
                self.browser
                    .borrow_mut()
                    .forward()
                    .map_err(|err| err.to_string())?;
                self.address_bar.sync(&self.browser.borrow());
                self.report();
            }
            Command::NewTab(target) => {
                self.tabs.open(target.as_deref())?;
                self.address_bar.sync(&self.browser.borrow());
                self.report();
            }
            Command::SelectTab(index) => {
                self.tabs.select(index)?;
                self.address_bar.sync(&self.browser.borrow());
                self.report();
            }
            Command::CloseTab => {
                self.tabs.close_active()?;
                self.address_bar.sync(&self.browser.borrow());
                self.report();
            }
            Command::Click(x, y) => {
                let followed = self.browser.borrow_mut().click(x, y);
                match followed {
                    Some(Ok(snapshot)) => {
                        println!("followed link -> {}", snapshot.title);
                        self.address_bar.sync(&self.browser.borrow());
                        self.report();
                    }
                    Some(Err(err)) => return Err(err.to_string()),
                    None => println!("no link at ({x}, {y})"),
                }
            }
            Command::Scroll(x, y, delta) => {
                let scrolled = self.browser.borrow_mut().scroll_at(x, y, delta);
                println!(
                    "{}",
                    if scrolled {
                        "scrolled"
                    } else {
                        "nothing to scroll here"
                    }
                );
                self.report();
            }
            Command::Render(path) => {
                let image = self.browser.borrow().render();
                image.save_png(&path).map_err(|err| err.to_string())?;
                println!("wrote {path} ({}x{})", image.width, image.height);
            }
            Command::Tree => {
                let browser = self.browser.borrow();
                let page = browser.page();
                let document = page.document.borrow();
                print_tree(&document, document.root(), 0);
            }
            Command::Styles(selector) => self.print_styles(selector.as_deref())?,
            Command::Text => println!("{}", self.browser.borrow().snapshot().text),
            Command::Help => self.print_help(&mut std::io::stdout())?,
            Command::Quit => {
                self.running = false;
            }
            Command::Empty => {}
            Command::Invalid(message) => return Err(message),
        }
        Ok(())
    }

    fn report(&mut self) {
        let snapshot = self.browser.borrow().snapshot();
        println!("── {} ─────────────────────────", snapshot.title);
        if !self.address_bar.text().is_empty() {
            println!("address: {}", self.address_bar.text());
        }
        println!("{}", snapshot.url.as_deref().unwrap_or("(no url)"));
        println!(
            "nodes {} · boxes {} · commands {} · height {:.0}px",
            snapshot.node_count,
            self.browser.borrow().page().layout.boxes.len(),
            snapshot.command_count,
            snapshot.content_height
        );
        let (titles, active) = self.tabs.titles();
        if titles.len() > 1 {
            let strip: Vec<String> = titles
                .iter()
                .enumerate()
                .map(|(index, title)| {
                    if index == active {
                        format!("[{title}]")
                    } else {
                        format!(" {title} ")
                    }
                })
                .collect();
            println!("tabs: {}", strip.join(" | "));
        }
        match self.presenter.present(&self.browser.borrow()) {
            Ok(message) => println!("{message}"),
            Err(error) => println!("frame error: {error}"),
        }
    }

    fn print_styles(&self, selector: Option<&str>) -> Result<(), String> {
        let browser = self.browser.borrow();
        let page = browser.page();
        let document = page.document.borrow();
        let nodes = match selector {
            Some(selector) => {
                browser_selectors::query_selector_all(&document, document.root(), selector)
                    .map_err(|err| err.to_string())?
            }
            None => document.elements_in_order(),
        };
        for node in nodes {
            if let Some(style) = page.styles.get(node) {
                println!(
                    "{} -> display {:?}, font-size {:.1}px, color {:?}",
                    document.element_name(node).unwrap_or("?"),
                    style.display(),
                    style.font_size(),
                    style.color()
                );
            }
        }
        Ok(())
    }

    fn print_help(&self, out: &mut impl Write) -> Result<(), String> {
        writeln!(
            out,
            "Mag shell — commands:\n\
             \x20 open <url|file>   navigate\n\
             \x20 back / forward    history\n\
             \x20 reload            reload\n\
             \x20 tab [url]         new tab\n\
             \x20 select <n>        switch tab\n\
             \x20 close             close tab\n\
             \x20 click <x> <y>     follow a link\n\
             \x20 scroll <x> <y> <d> scroll\n\
             \x20 render <file>     save PNG\n\
             \x20 tree | styles | text | help | quit"
        )
        .map_err(|err| err.to_string())
    }
}

fn resolve_target(target: &str) -> String {
    if target.contains("://") || target.starts_with("about:") {
        return target.to_string();
    }
    let path = std::path::Path::new(target);
    if path.exists()
        && let Ok(canonical) = std::fs::canonicalize(path)
    {
        return format!("file://{}", canonical.display());
    }
    target.to_string()
}

fn print_tree(document: &browser_dom::Document, node: browser_dom::NodeId, depth: usize) {
    let indent = "  ".repeat(depth);
    match &document.node(node).data {
        NodeData::Document => println!("{indent}#document"),
        NodeData::Doctype { name, .. } => println!("{indent}<!DOCTYPE {name}>"),
        NodeData::Comment(text) => println!("{indent}<!-- {text} -->"),
        NodeData::Text(text) => {
            let text = text.data.trim();
            if !text.is_empty() {
                println!("{indent}\"{text}\"");
            }
            return;
        }
        NodeData::Element(element) => {
            let attributes: Vec<String> = element
                .attributes
                .iter()
                .map(|(k, v)| format!(" {k}=\"{v}\""))
                .collect();
            println!("{indent}<{}{}>", element.name, attributes.join(""));
        }
    }
    for child in document.children(node) {
        print_tree(document, *child, depth + 1);
    }
}
