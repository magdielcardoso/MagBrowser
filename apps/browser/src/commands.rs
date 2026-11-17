//! Command parsing for the interactive browser shell.

/// A command typed into the browser shell.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Navigate to a URL or local file.
    Open(String),
    /// Reload the current page.
    Reload,
    /// Go back.
    Back,
    /// Go forward.
    Forward,
    /// Open a new tab.
    NewTab(Option<String>),
    /// Switch to a tab index (0-based).
    SelectTab(usize),
    /// Close the current tab.
    CloseTab,
    /// Click at viewport coordinates.
    Click(f32, f32),
    /// Scroll by a delta at coordinates (or center).
    Scroll(f32, f32, f32),
    /// Render the page to a PNG.
    Render(String),
    /// Print the DOM tree.
    Tree,
    /// Print the computed styles of a selector (or all elements).
    Styles(Option<String>),
    /// Print page text.
    Text,
    /// Show state and help.
    Help,
    /// Quit.
    Quit,
    /// Empty line.
    Empty,
    /// A command that could not be parsed.
    Invalid(String),
}

/// Parses one line of shell input.
pub fn parse(line: &str) -> Command {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Command::Empty;
    }
    let (verb, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((verb, rest)) => (verb, rest.trim()),
        None => (trimmed, ""),
    };
    match verb.to_ascii_lowercase().as_str() {
        "open" | "o" if !rest.is_empty() => Command::Open(rest.to_string()),
        "reload" | "r" => Command::Reload,
        "back" | "b" => Command::Back,
        "forward" | "f" => Command::Forward,
        "tab" => {
            if rest.is_empty() {
                Command::NewTab(None)
            } else {
                Command::NewTab(Some(rest.to_string()))
            }
        }
        "select" => match rest.parse::<usize>() {
            Ok(index) => Command::SelectTab(index),
            Err(_) => Command::Invalid(format!("invalid tab index: {rest}")),
        },
        "close" => Command::CloseTab,
        "click" => match parse_numbers(rest, 2) {
            Some(values) => Command::Click(values[0], values[1]),
            None => Command::Invalid("click needs x and y".to_string()),
        },
        "scroll" => match parse_numbers(rest, 3) {
            Some(values) => Command::Scroll(values[0], values[1], values[2]),
            None => Command::Invalid("scroll needs x, y and delta".to_string()),
        },
        "render" | "shot" if !rest.is_empty() => Command::Render(rest.to_string()),
        "tree" => Command::Tree,
        "styles" => Command::Styles(if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        }),
        "text" => Command::Text,
        "help" | "?" => Command::Help,
        "quit" | "exit" | "q" => Command::Quit,
        other => Command::Invalid(format!("unknown command: {other}")),
    }
}

fn parse_numbers(input: &str, count: usize) -> Option<Vec<f32>> {
    let values: Vec<f32> = input
        .split_whitespace()
        .filter_map(|value| value.parse().ok())
        .collect();
    if values.len() == count {
        Some(values)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(
            parse("open https://a.test"),
            Command::Open("https://a.test".into())
        );
        assert_eq!(parse(""), Command::Empty);
        assert_eq!(parse("click 10 20"), Command::Click(10.0, 20.0));
        assert_eq!(parse("scroll 1 2 30"), Command::Scroll(1.0, 2.0, 30.0));
        assert_eq!(parse("tab"), Command::NewTab(None));
        assert_eq!(parse("select 2"), Command::SelectTab(2));
        assert!(matches!(parse("nope"), Command::Invalid(_)));
        assert!(matches!(parse("click 1"), Command::Invalid(_)));
    }
}
