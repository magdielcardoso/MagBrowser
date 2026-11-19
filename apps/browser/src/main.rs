//! Mag browser shell.
//!
//! Without `--features window` the shell renders frames to PNG and accepts
//! commands from stdin (`open`, `back`, `click`, `scroll`, `render`, …).
//! With the `window` feature, `mag <file|url>` opens a real desktop window
//! driven by the same engine core. `--window-selfcheck` renders the complete
//! window composition (chrome + page) to a PNG in either build, so the design
//! can be reviewed without a display.
//!
//! Window shortcuts: `Ctrl+F` find-in-page (Enter / Shift+Enter navigate,
//! Esc closes), `Ctrl+Plus` / `Ctrl+Minus` / `Ctrl+0` zoom the page render
//! scale, `F12` toggles DevTools, and right-clicking the page opens a context
//! menu. Dragging a tab reorders its display slot (the engine's `TabSet` has
//! no reorder API, so the order is kept in the window layer).

mod address_bar;
mod app;
mod chrome;
mod commands;
mod devtools;
mod selfcheck;
mod tabs;
mod target;
mod ui_extra;
mod window;
#[cfg(feature = "window")]
mod window_app;

use std::path::PathBuf;
use std::process::ExitCode;

use browser_core::{Browser, Settings};
#[cfg(feature = "window")]
use browser_layout::Size;

#[cfg(feature = "window")]
use crate::chrome::{ChromeLayout, LoadStatus};

const HELP: &str = "\
mag — browser shell

USAGE:
  mag <file|url>               open a page (window with --features window)
  mag                          interactive shell
  mag --window-selfcheck <out.png> [--width <px>] [--height <px>] [<file|url>]
  mag --exec <cmd> --exec <cmd> [--frames <dir>] [--width <px>] [--height <px>] [--script]

OPTIONS:
  --exec <command>   run a shell command (repeatable), then exit
  --frames <dir>     directory for rendered frames (default frames)
  --width <px>       window/page width (default 1280)
  --height <px>      window/page height (default 800)
  --script           enable JavaScript
  --window           open the native window (requires --features window)
  --shell            force the interactive shell instead of a window
  --window-selfcheck <out.png>
                     render chrome + page to a PNG without opening a window
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
    let mut frames = PathBuf::from("frames");
    let mut exec: Vec<String> = Vec::new();
    let mut target: Option<String> = None;
    let mut self_check: Option<PathBuf> = None;
    let mut force_window = false;
    let mut shell_only = false;

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
                frames = PathBuf::from(args.get(index).ok_or("missing value for --frames")?);
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
            "--window" => force_window = true,
            "--shell" => shell_only = true,
            "--window-selfcheck" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or("missing value for --window-selfcheck")?;
                self_check = Some(PathBuf::from(value));
            }
            other => target = Some(other.to_string()),
        }
        index += 1;
    }

    #[cfg(not(feature = "window"))]
    let _ = shell_only;

    if let Some(output) = self_check {
        return selfcheck::run(&selfcheck::SelfCheck {
            output,
            width: settings.viewport_width,
            height: settings.viewport_height,
            target,
        });
    }

    #[cfg(feature = "window")]
    if !shell_only && exec.is_empty() && (target.is_some() || force_window) {
        let window_width = settings.viewport_width;
        let window_height = settings.viewport_height;
        let page = page_size(window_width, window_height);
        settings.viewport_width = page.width;
        settings.viewport_height = page.height;
        let browser = Browser::new(settings);
        let window_target = target.or_else(|| Some("about:blank".to_string()));
        return window_app::run(browser, window_target, window_width, window_height);
    }
    #[cfg(not(feature = "window"))]
    if force_window {
        return Err("this build has no native window; rebuild with --features window".to_string());
    }

    let mut browser = Browser::new(settings);
    let mut app = app::App::new(browser_init(&mut browser, &target)?, &frames);
    if exec.is_empty() {
        app.run_interactive()
    } else {
        app.run_script(&exec)
    }
}

#[cfg(feature = "window")]
fn page_size(width: f32, height: f32) -> Size {
    ChromeLayout::new(
        Size {
            width: width.max(1.0),
            height: height.max(1.0),
        },
        1,
        LoadStatus::Ready,
    )
    .page_size()
}

fn browser_init(browser: &mut Browser, target: &Option<String>) -> Result<Browser, String> {
    if let Some(target) = target {
        let resolved = target::resolve_target(target);
        browser.navigate(&resolved).map_err(|err| err.to_string())?;
    }
    let settings = browser.settings().clone();
    Ok(std::mem::replace(browser, Browser::new(settings)))
}
