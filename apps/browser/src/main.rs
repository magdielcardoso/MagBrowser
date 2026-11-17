//! Mag browser shell.
//!
//! Without a native window backend the shell renders frames to PNG and accepts
//! commands from stdin (`open`, `back`, `click`, `scroll`, `render`, …). This is
//! the same engine core the future `winit`/`wgpu` window will drive.

mod address_bar;
mod app;
mod commands;
mod tabs;
mod window;

use std::process::ExitCode;

use browser_core::{Browser, Settings};

const HELP: &str = "\
mag — browser shell

USAGE:
  mag                          interactive shell
  mag <file|url>               open and render one page
  mag --exec <cmd> --exec <cmd> [--frames <dir>] [--width <px>] [--height <px>] [--script]

OPTIONS:
  --exec <command>   run a shell command (repeatable), then exit
  --frames <dir>     directory for rendered frames (default frames)
  --width <px>       viewport width (default 1024)
  --height <px>      viewport height (default 768)
  --script           enable JavaScript
  -h, --help         show this help
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
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{HELP}");
        return Ok(());
    }

    let mut settings = Settings::desktop();
    let mut frames = std::path::PathBuf::from("frames");
    let mut exec: Vec<String> = Vec::new();
    let mut target: Option<String> = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--exec" => {
                index += 1;
                let value = args.get(index).ok_or("missing value for --exec")?;
                exec.push(value.clone());
            }
            "--frames" => {
                index += 1;
                frames =
                    std::path::PathBuf::from(args.get(index).ok_or("missing value for --frames")?);
            }
            "--width" => {
                index += 1;
                settings.viewport_width = args
                    .get(index)
                    .ok_or("missing value for --width")?
                    .parse()
                    .map_err(|_| "invalid --width")?;
            }
            "--height" => {
                index += 1;
                settings.viewport_height = args
                    .get(index)
                    .ok_or("missing value for --height")?
                    .parse()
                    .map_err(|_| "invalid --height")?;
            }
            "--script" => settings.enable_scripting = true,
            other => target = Some(other.to_string()),
        }
        index += 1;
    }

    let mut browser = Browser::new(settings);
    let mut app = app::App::new(browser_init(&mut browser, &target)?, &frames);
    if exec.is_empty() {
        app.run_interactive()
    } else {
        app.run_script(&exec)
    }
}

fn browser_init(browser: &mut Browser, target: &Option<String>) -> Result<Browser, String> {
    if let Some(target) = target {
        let resolved = if target.contains("://") {
            target.clone()
        } else {
            let path = std::path::Path::new(target);
            match std::fs::canonicalize(path) {
                Ok(canonical) => format!("file://{}", canonical.display()),
                Err(err) => return Err(format!("{}: {err}", path.display())),
            }
        };
        browser.navigate(&resolved).map_err(|err| err.to_string())?;
    }
    let settings = browser.settings().clone();
    Ok(std::mem::replace(browser, Browser::new(settings)))
}
