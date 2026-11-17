//! `browser-headless`: deterministic rendering of local pages to PNG.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use browser_core::{Browser, Settings};

const HELP: &str = "\
mag-headless — deterministic page renderer

USAGE:
  mag-headless --html <file> [--out <png>]
  mag-headless --url <url>   [--out <png>]
  mag-headless --stdin       [--out <png>]

OPTIONS:
  --html <path>     load HTML from a file
  --url <url>       navigate to a URL (enables networking)
  --stdin           read HTML from standard input
  --out <path>      PNG output path (default: out.png)
  --width <px>      viewport width (default 1024)
  --height <px>     viewport height (default 768)
  --script          enable inline JavaScript
  --json            print a JSON snapshot to stdout
  --text            print the page text to stdout
  -h, --help        show this help
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(());
    }

    let mut html_path: Option<PathBuf> = None;
    let mut url: Option<String> = None;
    let mut stdin = false;
    let mut out = PathBuf::from("out.png");
    let mut settings = Settings::headless();
    let mut json = false;
    let mut text = false;

    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let mut next = |name: &str| -> Result<String, String> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| format!("missing value for {name}"))
        };
        match arg.as_str() {
            "--html" => html_path = Some(PathBuf::from(next("--html")?)),
            "--url" => {
                url = Some(next("--url")?);
                settings.enable_network = true;
            }
            "--out" => out = PathBuf::from(next("--out")?),
            "--width" => {
                settings.viewport_width = next("--width")?
                    .parse()
                    .map_err(|_| "invalid --width".to_string())?;
            }
            "--height" => {
                settings.viewport_height = next("--height")?
                    .parse()
                    .map_err(|_| "invalid --height".to_string())?;
            }
            "--stdin" => stdin = true,
            "--script" => settings.enable_scripting = true,
            "--json" => json = true,
            "--text" => text = true,
            other => return Err(format!("unknown option `{other}` (try --help)")),
        }
        index += 1;
    }

    let mut browser = Browser::new(settings);
    if let Some(url) = &url {
        browser.navigate(url).map_err(|err| err.to_string())?;
    } else {
        let (source, base) = if stdin {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|err| err.to_string())?;
            (buffer, None)
        } else if let Some(path) = &html_path {
            let source = std::fs::read_to_string(path)
                .map_err(|err| format!("could not read {}: {err}", path.display()))?;
            let base = std::fs::canonicalize(path)
                .ok()
                .map(|path| format!("file://{}", path.display()));
            (source, base)
        } else {
            return Err("provide --html, --url or --stdin".to_string());
        };
        browser
            .load_html(&source, base.as_deref())
            .map_err(|err| err.to_string())?;
    }

    let snapshot = browser.snapshot();
    let image = browser.render();
    image
        .save_png(&out)
        .map_err(|err| format!("could not write {}: {err}", out.display()))?;

    if json {
        println!("{}", to_json(&snapshot, &out, image.width, image.height));
    } else {
        println!(
            "rendered {} ({}x{})",
            out.display(),
            image.width,
            image.height
        );
        println!("title: {}", snapshot.title);
        println!(
            "nodes: {}  commands: {}",
            snapshot.node_count, snapshot.command_count
        );
        println!("content height: {:.1}px", snapshot.content_height);
    }
    if text {
        println!("{}", snapshot.text);
    }
    Ok(())
}

fn to_json(
    snapshot: &browser_core::Snapshot,
    out: &std::path::Path,
    width: u32,
    height: u32,
) -> String {
    let escape = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    };
    format!(
        "{{\"output\":\"{}\",\"width\":{},\"height\":{},\"url\":{},\"title\":\"{}\",\"nodes\":{},\"commands\":{},\"content_height\":{:.1},\"text\":\"{}\"}}",
        escape(&out.display().to_string()),
        width,
        height,
        match &snapshot.url {
            Some(url) => format!("\"{}\"", escape(url)),
            None => "null".to_string(),
        },
        escape(&snapshot.title),
        snapshot.node_count,
        snapshot.command_count,
        snapshot.content_height,
        escape(&snapshot.text)
    )
}
