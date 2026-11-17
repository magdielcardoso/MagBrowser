//! Insertion modes used by the HTML tree builder.

/// The tree-construction insertion modes implemented by the parser.
///
/// The names follow the WHATWG HTML specification. Modes that the parser
/// deliberately simplifies (for example frameset handling) are not listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionMode {
    /// Before anything has been inserted.
    Initial,
    /// Before the `html` element exists.
    BeforeHtml,
    /// Before the `head` element exists.
    BeforeHead,
    /// Inside `head`.
    InHead,
    /// Inside `noscript` in `head` with scripting disabled.
    InHeadNoscript,
    /// After `head`, before `body`.
    AfterHead,
    /// Inside `body` (the main mode).
    InBody,
    /// Inside a rawtext/RCDATA element such as `title` or `script`.
    Text,
    /// Inside a `table` element.
    InTable,
    /// Inside a `caption` element.
    InCaption,
    /// Inside a `colgroup` element.
    InColumnGroup,
    /// Inside `tbody`, `thead` or `tfoot`.
    InTableBody,
    /// Inside a table row.
    InRow,
    /// Inside a table cell.
    InCell,
    /// Inside a `select` element.
    InSelect,
    /// Inside a `select` element that is inside a table.
    InSelectInTable,
    /// After `</body>`.
    AfterBody,
    /// After `</html>`.
    AfterAfterBody,
}
