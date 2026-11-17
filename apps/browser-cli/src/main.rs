//! `browser-cli`: inspect every stage of the Mag engine.

use std::path::PathBuf;
use std::process::ExitCode;

use browser_core::{Browser, Settings};
use browser_dom::{Document, Namespace, NodeData, NodeId, serialize_children};
use browser_url::Url;

const HELP: &str = "\
mag-cli — inspect the Mag engine

USAGE:
  mag-cli tree      <file|url>            print the DOM tree
  mag-cli html      <file|url>            print serialised body HTML
  mag-cli css       <file|url>            print collected author CSS
  mag-cli styles    <file|url> [selector] print computed styles
  mag-cli layout    <file|url>            print the layout box tree
  mag-cli commands  <file|url>            print the display list
  mag-cli render    <file|url> -o <png>   render to PNG
  mag-cli fetch     <url> -o <file>       download a URL
  mag-cli run       <file|url>            run the full pipeline and print a summary

OPTIONS:
  -o, --out <path>      output file
  --width <px>          viewport width (default 1024)
  --height <px>         viewport height (default 768)
  --script              enable JavaScript execution
  --network             allow network access for <link>/<script> resources
  -h, --help            show this help
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let Some(command) = args.first() else {
        print!("{HELP}");
        return Ok(());
    };
    if matches!(command.as_str(), "-h" | "--help" | "help") {
        print!("{HELP}");
        return Ok(());
    }
    let options = Options::parse(&args[1..])?;
    let target = options
        .positional
        .first()
        .ok_or_else(|| format!("`{command}` needs a file or URL"))?;

    match command.as_str() {
        "tree" => {
            let (doc, _) = load_document(target, &options)?;
            print_tree(&doc, doc.root(), 0);
        }
        "html" => {
            let (doc, _) = load_document(target, &options)?;
            let root = doc.head_and_body().1.unwrap_or(doc.root());
            println!("{}", serialize_children(&doc, root));
        }
        "css" => {
            let browser = load_browser(target, &options)?;
            for sheet in browser.page().author_css() {
                println!("{sheet}");
            }
            println!(
                "/* {} author stylesheet(s) */",
                browser.page().author_css().len()
            );
        }
        "styles" => {
            let browser = load_browser(target, &options)?;
            let page = browser.page();
            let selector = options.positional.get(1).map(String::as_str);
            let document = page.document.borrow();
            let nodes: Vec<NodeId> = match selector {
                Some(selector) => {
                    browser_selectors::query_selector_all(&document, document.root(), selector)
                        .map_err(|e| e.message)?
                }
                None => document.elements_in_order(),
            };
            for node in nodes {
                let name = document.element_name(node).unwrap_or("?");
                let id = document.id_attr(node).unwrap_or("");
                println!("{name}#{id} {{");
                if let Some(style) = page.styles.get(node) {
                    let mut entries: Vec<(String, String)> = style
                        .iter()
                        .map(|(property, value)| {
                            (property.name().to_string(), format!("{value:?}"))
                        })
                        .collect();
                    entries.sort_by(|a, b| a.0.cmp(&b.0));
                    for (property, value) in entries {
                        println!("  {property}: {value};");
                    }
                }
                println!("}}");
            }
        }
        "layout" => {
            let browser = load_browser(target, &options)?;
            let layout = &browser.page().layout;
            if let Some(root) = layout.root {
                print_layout(layout, root, 0);
            }
            println!(
                "document height: {:.1}px, boxes: {}",
                layout.document_height,
                layout.boxes.len()
            );
        }
        "commands" => {
            let browser = load_browser(target, &options)?;
            let list = &browser.page().display_list;
            for (index, command) in list.commands.iter().enumerate() {
                println!("{index:4}: {command:?}");
            }
            println!("{} command(s)", list.len());
        }
        "render" => {
            let browser = load_browser(target, &options)?;
            let out = options
                .out
                .clone()
                .unwrap_or_else(|| PathBuf::from("mag-render.png"));
            let image = browser.render();
            image
                .save_png(&out)
                .map_err(|err| format!("could not write {}: {err}", out.display()))?;
            println!("wrote {} ({}x{})", out.display(), image.width, image.height);
        }
        "fetch" => {
            let settings = Settings {
                enable_network: true,
                ..Settings::default()
            };
            let mut browser = Browser::new(settings);
            let url = Url::parse(target).map_err(|e| e.to_string())?;
            let response = browser.client_mut().get(url).map_err(|e| e.to_string())?;
            let out = options
                .out
                .clone()
                .unwrap_or_else(|| PathBuf::from("mag-fetch.out"));
            std::fs::write(&out, &response.body)
                .map_err(|err| format!("could not write {}: {err}", out.display()))?;
            println!(
                "{} -> {} (status {}, {} bytes)",
                target,
                out.display(),
                response.status,
                response.body.len()
            );
        }
        "run" => {
            let browser = load_browser(target, &options)?;
            let snapshot = browser.snapshot();
            println!("url: {}", snapshot.url.as_deref().unwrap_or("(none)"));
            println!("title: {}", snapshot.title);
            println!("nodes: {}", snapshot.node_count);
            println!("commands: {}", snapshot.command_count);
            println!("content height: {:.1}px", snapshot.content_height);
            println!("text: {}", truncate(&snapshot.text, 400));
        }
        other => return Err(format!("unknown command `{other}` (try --help)")),
    }
    Ok(())
}

#[derive(Debug, Default)]
struct Options {
    positional: Vec<String>,
    out: Option<PathBuf>,
    width: Option<f32>,
    height: Option<f32>,
    scripting: bool,
    network: bool,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            match arg.as_str() {
                "-o" | "--out" => {
                    index += 1;
                    let value = args.get(index).ok_or("missing value for --out")?;
                    options.out = Some(PathBuf::from(value));
                }
                "--width" => {
                    index += 1;
                    options.width = Some(
                        args.get(index)
                            .ok_or("missing value for --width")?
                            .parse()
                            .map_err(|_| "invalid --width")?,
                    );
                }
                "--height" => {
                    index += 1;
                    options.height = Some(
                        args.get(index)
                            .ok_or("missing value for --height")?
                            .parse()
                            .map_err(|_| "invalid --height")?,
                    );
                }
                "--script" => options.scripting = true,
                "--network" => options.network = true,
                other if other.starts_with('-') && other.len() > 1 => {
                    return Err(format!("unknown option `{other}`"));
                }
                other => options.positional.push(other.to_string()),
            }
            index += 1;
        }
        Ok(options)
    }

    fn settings(&self) -> Settings {
        let mut settings = Settings {
            enable_scripting: self.scripting,
            enable_network: self.network,
            ..Settings::default()
        };
        if let Some(width) = self.width {
            settings.viewport_width = width;
        }
        if let Some(height) = self.height {
            settings.viewport_height = height;
        }
        settings
    }
}

fn is_url(target: &str) -> bool {
    target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with("file://")
        || target.starts_with("about:")
}

fn load_browser(target: &str, options: &Options) -> Result<Browser, String> {
    let mut browser = Browser::new(options.settings());
    if is_url(target) {
        browser.settings_mut().enable_network = true;
        browser.navigate(target).map_err(|err| err.to_string())?;
    } else {
        let html = std::fs::read_to_string(target)
            .map_err(|err| format!("could not read {target}: {err}"))?;
        let base = std::fs::canonicalize(target)
            .ok()
            .map(|path| format!("file://{}", path.display()));
        browser
            .load_html(&html, base.as_deref())
            .map_err(|err| err.to_string())?;
    }
    Ok(browser)
}

fn load_document(target: &str, options: &Options) -> Result<(Document, Option<Url>), String> {
    if is_url(target) {
        let browser = load_browser(target, options)?;
        let page = browser.page();
        let document = page.document.borrow().clone();
        Ok((document, page.url.clone()))
    } else {
        let html = std::fs::read_to_string(target)
            .map_err(|err| format!("could not read {target}: {err}"))?;
        Ok((browser_html_parser::parse_document(&html), None))
    }
}

fn print_tree(doc: &Document, node: NodeId, depth: usize) {
    let indent = "  ".repeat(depth);
    match &doc.node(node).data {
        NodeData::Document => println!("{indent}#document"),
        NodeData::Doctype { name, .. } => println!("{indent}<!DOCTYPE {name}>"),
        NodeData::Comment(text) => println!("{indent}<!-- {text} -->"),
        NodeData::Text(text) => {
            let trimmed = text.data.trim();
            if !trimmed.is_empty() {
                println!("{indent}\"{}\"", truncate(trimmed, 80));
            }
            return;
        }
        NodeData::Element(element) => {
            let ns = match element.namespace {
                Namespace::Html => "",
                Namespace::Svg => " (svg)",
                Namespace::MathMl => " (mathml)",
            };
            let attrs: Vec<String> = element
                .attributes
                .iter()
                .map(|(k, v)| format!(" {k}=\"{v}\""))
                .collect();
            println!("{indent}<{}{ns}{}>", element.name, attrs.join(""));
        }
    }
    for child in doc.children(node) {
        print_tree(doc, *child, depth + 1);
    }
}

fn print_layout(tree: &browser_layout::LayoutTree, index: usize, depth: usize) {
    let Some(b) = tree.get(index) else { return };
    let indent = "  ".repeat(depth);
    println!(
        "{indent}[{:?}] node={:?} rect=({:.0},{:.0} {:.0}x{:.0}) content=({:.0},{:.0} {:.0}x{:.0})",
        b.box_type,
        b.node,
        b.rect.x,
        b.rect.y,
        b.rect.width,
        b.rect.height,
        b.content.x,
        b.content.y,
        b.content.width,
        b.content.height
    );
    for line in &b.lines {
        for fragment in &line.fragments {
            println!(
                "{indent}  text \"{}\" at ({:.1}, {:.1}) w={:.1}",
                truncate(&fragment.text, 30),
                fragment.x,
                fragment.baseline,
                fragment.width
            );
        }
    }
    for child in &b.children {
        print_layout(tree, *child, depth + 1);
    }
}

fn truncate(text: &str, max: usize) -> String {
    let single_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= max {
        single_line
    } else {
        let mut out: String = single_line.chars().take(max).collect();
        out.push('…');
        out
    }
}
