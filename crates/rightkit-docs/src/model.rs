//! Wire types: sources, read options and results, write ops and receipts. Shapes are serde
//! camelCase with a `kind` or `type` discriminator, like `rightkit-pdf`'s commands.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::limits::Limits;

/// Where `read` gets its bytes. The format is detected from the content, never the file name.
#[derive(Clone, Copy, Debug)]
pub enum Source<'a> {
    Path(&'a Path),
    Bytes(&'a [u8]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Pdf,
    Docx,
    Xlsx,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Pdf => "pdf",
            Format::Docx => "docx",
            Format::Xlsx => "xlsx",
        }
    }
}

/// Page, sheet and range selection plus the output caps.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReadOptions {
    pub limits: Limits,
    /// PDF only: 1-based pages to read. `None` reads every page.
    pub pages: Option<Vec<u32>>,
    /// XLSX only: sheet names to read. `None` reads every sheet.
    pub sheets: Option<Vec<String>>,
    /// XLSX only: an A1 range (`A1:C20`, `B2`) applied to each selected sheet.
    pub range: Option<String>,
    /// Cap on characters of text (and of markdown) returned. Past it `truncated` is set.
    pub max_chars: usize,
    /// Cap on cells returned across all sheets. Past it `truncated` is set.
    pub max_cells: usize,
    /// Build the markdown rendering (the plain-text rendering is always built).
    pub markdown: bool,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            pages: None,
            sheets: None,
            range: None,
            max_chars: 200_000,
            max_cells: 50_000,
            markdown: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRead {
    pub format: Format,
    /// SHA-256 of the exact bytes read. Pass it back as `expect_sha256` to make an edit refuse a
    /// file that changed in between.
    pub sha256: String,
    pub size_bytes: u64,
    /// Plain-text rendering, capped at `max_chars`.
    pub text: String,
    pub markdown: Option<String>,
    /// A cap (`max_chars`, `max_cells`) cut the result short.
    pub truncated: bool,
    pub content: Content,
    /// What the file holds that this read does not represent (comments, tracked changes,
    /// footnotes, images, charts, external links not followed...). Never empty by omission: if
    /// something is not in `content`, it is named here.
    pub omitted: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Content {
    Docx(DocxContent),
    Xlsx(XlsxContent),
    Pdf(PdfContent),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocxContent {
    pub blocks: Vec<DocxBlock>,
    pub headers: Vec<String>,
    pub footers: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DocxBlock {
    /// `style` is the style's display name (`heading 1`, `Title`, `List Paragraph`), not its id.
    Paragraph {
        style: Option<String>,
        list: bool,
        text: String,
    },
    Table {
        rows: Vec<Vec<String>>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XlsxContent {
    pub sheets: Vec<SheetRead>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetRead {
    pub name: String,
    /// `visible`, `hidden` or `veryHidden`.
    pub state: String,
    /// The used range of the whole sheet (`A1:D20`), before range selection; `None` when empty.
    pub used_range: Option<String>,
    /// Non-empty cells inside the selection, row-major. A formula cell with no stored result
    /// (written by an edit, not yet calculated by a spreadsheet application) has an `empty` value
    /// and a `formula`.
    pub cells: Vec<CellRead>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellRead {
    /// A1 address.
    pub cell: String,
    /// 1-based.
    pub row: u32,
    /// 1-based.
    pub col: u32,
    pub value: CellValue,
    /// The formula text without the leading `=`, with the cached result in `value`.
    pub formula: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum CellValue {
    Empty,
    Text(String),
    Number(f64),
    Bool(bool),
    /// An Excel date or time: the serial number and, when it is a plausible date, ISO 8601.
    DateTime {
        serial: f64,
        iso: Option<String>,
    },
    /// An Excel duration in days.
    Duration(f64),
    /// `#DIV/0!`, `#N/A`, ...
    Error(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfContent {
    pub page_count: u32,
    pub pages: Vec<PdfPageText>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfPageText {
    /// 1-based.
    pub page: u32,
    pub text: String,
}

// ---------------------------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------------------------

/// A block of new DOCX content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Block {
    /// `level` 1 to 6.
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        text: String,
    },
    Bullet {
        text: String,
    },
    Numbered {
        text: String,
    },
    Table {
        rows: Vec<Vec<String>>,
        header: bool,
    },
    PageBreak,
}

/// A value to write into a cell. `Text` is always a string, whatever it starts with: only
/// `Formula` is a formula.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum CellInput {
    Text(String),
    Number(f64),
    Bool(bool),
    /// With or without the leading `=`. No cached result is written.
    Formula(String),
    Blank,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetSpec {
    pub name: String,
    /// Row-major from A1.
    pub rows: Vec<Vec<CellInput>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellSet {
    /// A1 address.
    pub cell: String,
    pub value: CellInput,
}

/// One typed write op. A `write` call applies its ops in order to one in-memory document and
/// publishes the result once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Op {
    /// Create a DOCX from blocks. The target must not exist unless `overwrite`.
    CreateDocx { blocks: Vec<Block> },
    /// Create an XLSX. The target must not exist unless `overwrite`.
    CreateXlsx { sheets: Vec<SheetSpec> },
    /// DOCX (body, headers, footers; across run boundaries) or PDF (source text, per island).
    ReplaceText { find: String, replace: String },
    /// DOCX: append blocks at the end of the body.
    AppendBlocks { blocks: Vec<Block> },
    /// XLSX: set cells on a sheet.
    SetCells { sheet: String, cells: Vec<CellSet> },
    /// XLSX: append rows after the last populated row of a sheet.
    AppendRows {
        sheet: String,
        rows: Vec<Vec<CellInput>>,
    },
    /// PDF: add a line of text at a point (PDF user space, origin bottom-left, points).
    AddPdfText {
        page: u32,
        x: f64,
        y: f64,
        text: String,
        size_pt: Option<f64>,
    },
    /// PDF: keep only these 1-based pages, in this order.
    SelectPdfPages { pages: Vec<u32> },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WriteOptions {
    pub limits: Limits,
    /// For an edit: refuse (`source_changed`) unless the target's current SHA-256 equals this, and
    /// again at publish time. Use the `sha256` of the `read` the edit is based on. An edit always
    /// rechecks that the file did not change between its own read and its publish, with or without
    /// this.
    pub expect_sha256: Option<String>,
    /// Allow a create op to replace an existing file.
    pub overwrite: bool,
}

/// Something the edit could not keep, or deliberately changed. Empty `losses` means every part
/// the edit did not touch is byte-identical to the source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Loss {
    pub part: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteReceipt {
    pub path: PathBuf,
    pub format: Format,
    /// The target did not exist before.
    pub created: bool,
    /// SHA-256 of the file this edit started from; `None` for a create.
    pub source_sha256: Option<String>,
    pub sha256: String,
    pub bytes_written: u64,
    pub ops_applied: usize,
    /// Text replacements made (`ReplaceText`), cells set and rows appended are in `warnings`-free
    /// counters below.
    pub replacements: usize,
    pub cells_set: usize,
    pub rows_appended: usize,
    /// Package parts rewritten by the edit (OOXML). Every other part is byte-identical.
    pub touched_parts: Vec<String>,
    pub losses: Vec<Loss>,
    pub warnings: Vec<String>,
}
