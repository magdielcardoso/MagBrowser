//! User-agent default stylesheet.
//!
//! This is the engine's built-in style sheet, equivalent in spirit to the HTML
//! rendering spec's suggested defaults. It is intentionally small but covers every
//! element the HTML parser can produce.

/// Default styles applied with the `user-agent` origin.
pub const DEFAULT_STYLESHEET: &str = r#"
html, body, div, p, section, article, aside, header, footer, nav, main,
address, figure, figcaption, blockquote, pre, form, fieldset, legend,
h1, h2, h3, h4, h5, h6, ul, ol, li, dl, dt, dd, table, thead, tbody,
tfoot, tr, td, th, caption, details, summary, hr, center {
    display: block;
}

head, title, meta, link, style, script, base, template, noscript, param, source, track {
    display: none;
}

li { display: list-item; }
table { display: table; }
tr { display: table-row; }
td, th { display: table-cell; vertical-align: middle; }
thead, tbody, tfoot { display: block; }

body { margin: 8px; }

h1 { font-size: 2em; font-weight: bold; margin-top: 0.67em; margin-bottom: 0.67em; }
h2 { font-size: 1.5em; font-weight: bold; margin-top: 0.83em; margin-bottom: 0.83em; }
h3 { font-size: 1.17em; font-weight: bold; margin-top: 1em; margin-bottom: 1em; }
h4 { font-size: 1em; font-weight: bold; margin-top: 1.33em; margin-bottom: 1.33em; }
h5 { font-size: 0.83em; font-weight: bold; margin-top: 1.67em; margin-bottom: 1.67em; }
h6 { font-size: 0.67em; font-weight: bold; margin-top: 2.33em; margin-bottom: 2.33em; }

p, pre, blockquote, dl, figure { margin-top: 1em; margin-bottom: 1em; }
blockquote { margin-left: 40px; margin-right: 40px; }

ul, ol { margin-top: 1em; margin-bottom: 1em; padding-left: 40px; }
ul { list-style-type: disc; }
ol { list-style-type: decimal; }
li { list-style-type: disc; }

pre, code, kbd, samp, tt { font-family: monospace; }
pre { white-space: pre; }
code { font-size: 0.95em; }

a:link { color: rgb(0, 0, 238); text-decoration: underline; }
a:visited { color: rgb(85, 26, 139); text-decoration: underline; }

strong, b { font-weight: bold; }
em, i, cite, var { font-style: italic; }
u, ins { text-decoration: underline; }
s, strike, del { text-decoration: line-through; }
big { font-size: 1.2em; }
small { font-size: 0.83em; }
sub, sup { font-size: 0.83em; }
sup { vertical-align: super; }
sub { vertical-align: sub; }

hr { border-top: 1px solid rgb(200, 200, 200); border-right: 0; border-bottom: 0; border-left: 0; margin-top: 0.5em; margin-bottom: 0.5em; }

img { display: inline-block; }
br { display: inline; }

input, select, textarea, button { display: inline-block; }
button, input { font-size: 13px; }

center { text-align: center; }
th { font-weight: bold; text-align: center; }
td { text-align: left; }

caption { text-align: center; }

[hidden] { display: none; }
"#;
