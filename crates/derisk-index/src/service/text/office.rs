//! The text of OpenDocument (`.odt`, `.ods`, `.odp`) and Office Open XML
//! (`.docx`, `.xlsx`, `.pptx`) documents.
//!
//! Both are ZIP archives with the words in XML parts. Only those parts are
//! read: the ZIP's central directory says where each starts, and a part is
//! stored or deflated, which flate2 inflates. The XML's tags are dropped,
//! with a paragraph's, a cell's or a line break's end kept as a new line
//! so a match's snippet is one line of the document, and its entities are
//! turned back into characters.

use std::io::{self, Read, Seek, SeekFrom};

use flate2::read::DeflateDecoder;

use crate::TEXT_BYTES;

/// The two families, by where their words are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `content.xml`.
    OpenDocument,
    /// `word/document.xml`.
    Word,
    /// `xl/sharedStrings.xml`, where a workbook keeps its cells' text.
    Excel,
    /// `ppt/slides/slide<N>.xml`, in order.
    PowerPoint,
}

impl Kind {
    /// The kind a file extension, in lower case, names.
    pub fn of(extension: &str) -> Option<Self> {
        Some(match extension {
            "odt" | "ods" | "odp" | "odg" | "ott" | "ots" | "otp" => Self::OpenDocument,
            "docx" | "docm" | "dotx" => Self::Word,
            "xlsx" | "xlsm" | "xltx" => Self::Excel,
            "pptx" | "pptm" | "potx" => Self::PowerPoint,
            _ => return None,
        })
    }

    /// Whether `name`, a part of the archive, holds words, and where it
    /// goes among the others: slides by their number.
    fn wants(self, name: &str) -> Option<u32> {
        match self {
            Self::OpenDocument => (name == "content.xml").then_some(0),
            Self::Word => (name == "word/document.xml").then_some(0),
            Self::Excel => (name == "xl/sharedStrings.xml").then_some(0),
            Self::PowerPoint => name
                .strip_prefix("ppt/slides/slide")?
                .strip_suffix(".xml")?
                .parse()
                .ok(),
        }
    }
}

/// The most of one part that is read: XML is several times its words, and
/// a part past this is a data dump rather than a document.
const PART_BYTES: u64 = 8 * TEXT_BYTES as u64;

/// The archive's end record, and how far from the end it can start: its
/// 22 bytes and a comment of up to 64 KiB.
const END: u32 = 0x0605_4b50;
const END_SEARCH: u64 = 22 + 0xffff;
/// A central directory entry, and a local file header.
const ENTRY: u32 = 0x0201_4b50;
const LOCAL: u32 = 0x0403_4b50;

/// A part's place in the archive.
#[derive(Debug)]
struct Part {
    order: u32,
    method: u16,
    compressed: u64,
    offset: u64,
}

/// The words of the document `file` holds, or `None` when it is not the
/// ZIP it says it is, or has no part with words.
///
/// # Errors
///
/// When `file` cannot be read.
pub fn text<R: Read + Seek>(file: &mut R, kind: Kind) -> io::Result<Option<String>> {
    let Some(mut parts) = parts(file, kind)? else {
        return Ok(None);
    };
    if parts.is_empty() {
        return Ok(None);
    }
    parts.sort_by_key(|p| p.order);
    let mut out = String::new();
    for part in parts {
        let Some(xml) = inflate(file, &part)? else {
            continue;
        };
        words(&xml, &mut out);
        if out.len() >= TEXT_BYTES {
            break;
        }
    }
    Ok(Some(out))
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// The parts of `kind` the central directory lists, or `None` when there
/// is no end record to find it by.
fn parts<R: Read + Seek>(file: &mut R, kind: Kind) -> io::Result<Option<Vec<Part>>> {
    let len = file.seek(SeekFrom::End(0))?;
    let tail_len = len.min(END_SEARCH);
    file.seek(SeekFrom::Start(len - tail_len))?;
    let mut tail = vec![0; usize::try_from(tail_len).unwrap_or(0)];
    file.read_exact(&mut tail)?;
    let Some(end) = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| u32_at(&tail, i) == END)
    else {
        return Ok(None);
    };
    let entries = u16_at(&tail, end + 10);
    let size = u32_at(&tail, end + 12);
    let start = u32_at(&tail, end + 16);
    if u64::from(start) + u64::from(size) > len {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(u64::from(start)))?;
    let mut directory = vec![0; size as usize];
    file.read_exact(&mut directory)?;
    let mut parts = Vec::new();
    let mut at = 0;
    for _ in 0..entries {
        if at + 46 > directory.len() || u32_at(&directory, at) != ENTRY {
            break;
        }
        let method = u16_at(&directory, at + 10);
        let compressed = u32_at(&directory, at + 20);
        let name_len = usize::from(u16_at(&directory, at + 28));
        let extra_len = usize::from(u16_at(&directory, at + 30));
        let comment_len = usize::from(u16_at(&directory, at + 32));
        let offset = u32_at(&directory, at + 42);
        let Some(name) = directory.get(at + 46..at + 46 + name_len) else {
            break;
        };
        if let Some(order) = std::str::from_utf8(name).ok().and_then(|n| kind.wants(n)) {
            parts.push(Part {
                order,
                method,
                compressed: u64::from(compressed),
                offset: u64::from(offset),
            });
        }
        at += 46 + name_len + extra_len + comment_len;
    }
    Ok(Some(parts))
}

/// A part's bytes, at most [`PART_BYTES`] of them, or `None` for one
/// stored some way other than as is or deflated.
fn inflate<R: Read + Seek>(file: &mut R, part: &Part) -> io::Result<Option<String>> {
    file.seek(SeekFrom::Start(part.offset))?;
    let mut header = [0; 30];
    file.read_exact(&mut header)?;
    if u32_at(&header, 0) != LOCAL {
        return Ok(None);
    }
    let skip = i64::from(u16_at(&header, 26)) + i64::from(u16_at(&header, 28));
    file.seek(SeekFrom::Current(skip))?;
    let raw = file.take(part.compressed);
    let mut bytes = Vec::new();
    match part.method {
        0 => raw.take(PART_BYTES).read_to_end(&mut bytes)?,
        8 => DeflateDecoder::new(raw)
            .take(PART_BYTES)
            .read_to_end(&mut bytes)?,
        _ => return Ok(None),
    };
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// Tags that end a line of the document: paragraphs and headings, table
/// cells and rows, list items, line breaks, a workbook's shared strings.
fn ends_line(tag: &str) -> bool {
    let name = tag
        .trim_start_matches('/')
        .split([' ', '/', '\t', '\n'])
        .next()
        .unwrap_or("");
    matches!(
        name,
        "text:p"
            | "text:h"
            | "text:line-break"
            | "table:table-cell"
            | "table:table-row"
            | "w:p"
            | "w:br"
            | "w:tab"
            | "a:p"
            | "a:br"
            | "si"
    ) && (tag.starts_with('/') || tag.ends_with('/'))
}

/// Appends the words of `xml` to `out`, a line per paragraph, up to
/// [`TEXT_BYTES`].
fn words(xml: &str, out: &mut String) {
    let mut rest = xml;
    while let Some(open) = rest.find('<') {
        unescape(&rest[..open], out);
        let Some(close) = rest[open..].find('>') else {
            return;
        };
        let tag = &rest[open + 1..open + close];
        if ends_line(tag) && !out.ends_with('\n') && !out.is_empty() {
            out.push('\n');
        } else if tag.starts_with("text:s") || tag.starts_with("text:tab") {
            out.push(' ');
        }
        if out.len() >= TEXT_BYTES {
            out.truncate(out.floor_char_boundary(TEXT_BYTES));
            return;
        }
        rest = &rest[open + close + 1..];
    }
    unescape(rest, out);
}

/// Appends `text` with XML's entities turned back into characters.
fn unescape(text: &str, out: &mut String) {
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let Some(semi) = rest[amp..].find(';').filter(|&s| s <= 10) else {
            out.push('&');
            rest = &rest[amp + 1..];
            continue;
        };
        let entity = &rest[amp + 1..amp + semi];
        let c = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => out.push(c),
            None => out.push_str(&rest[amp..=amp + semi]),
        }
        rest = &rest[amp + semi + 1..];
    }
    out.push_str(rest);
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use flate2::{Compression, write::DeflateEncoder};

    use super::*;

    /// A ZIP of `parts`, each `(name, bytes, deflated)`, laid out as every
    /// writer lays one out: local headers and data, then the directory.
    fn zip(parts: &[(&str, &str, bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut directory = Vec::new();
        for (name, data, deflated) in parts {
            let body = if *deflated {
                let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
                e.write_all(data.as_bytes()).unwrap();
                e.finish().unwrap()
            } else {
                data.as_bytes().to_vec()
            };
            let method: u16 = if *deflated { 8 } else { 0 };
            let offset = out.len() as u32;
            out.extend(LOCAL.to_le_bytes());
            out.extend([20, 0, 0, 0]);
            out.extend(method.to_le_bytes());
            out.extend([0; 8]);
            out.extend((body.len() as u32).to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend([0, 0]);
            out.extend(name.as_bytes());
            out.extend(&body);
            directory.extend(ENTRY.to_le_bytes());
            directory.extend([20, 0, 20, 0, 0, 0]);
            directory.extend(method.to_le_bytes());
            directory.extend([0; 8]);
            directory.extend((body.len() as u32).to_le_bytes());
            directory.extend((data.len() as u32).to_le_bytes());
            directory.extend((name.len() as u16).to_le_bytes());
            directory.extend([0; 12]);
            directory.extend(offset.to_le_bytes());
            directory.extend(name.as_bytes());
        }
        let start = out.len() as u32;
        out.extend(&directory);
        out.extend(END.to_le_bytes());
        out.extend([0; 4]);
        out.extend((parts.len() as u16).to_le_bytes());
        out.extend((parts.len() as u16).to_le_bytes());
        out.extend((directory.len() as u32).to_le_bytes());
        out.extend(start.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    #[test]
    fn an_odt_gives_its_paragraphs_as_lines() {
        let content = "<office:document-content><office:body><office:text>\
            <text:h>Quarterly budget</text:h>\
            <text:p>Rent &amp; power<text:s/>rose by 3&#x25;.</text:p>\
            </office:text></office:body></office:document-content>";
        let file = zip(&[
            ("mimetype", "application/vnd.oasis.opendocument.text", false),
            ("content.xml", content, true),
        ]);
        let text = text(&mut Cursor::new(file), Kind::OpenDocument)
            .unwrap()
            .unwrap();
        assert_eq!(text, "Quarterly budget\nRent & power rose by 3%.\n");
    }

    #[test]
    fn slides_come_in_their_order() {
        let slide = |t: &str| format!("<p:sld><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>");
        let (one, two, ten) = (slide("first"), slide("second"), slide("tenth"));
        let file = zip(&[
            ("ppt/slides/slide10.xml", &ten, true),
            ("ppt/slides/slide2.xml", &two, false),
            ("ppt/slides/slide1.xml", &one, true),
            ("ppt/presentation.xml", "<p:presentation/>", true),
        ]);
        let text = text(&mut Cursor::new(file), Kind::PowerPoint)
            .unwrap()
            .unwrap();
        assert_eq!(text, "first\nsecond\ntenth\n");
    }

    #[test]
    fn a_workbook_gives_its_shared_strings() {
        let strings = "<sst><si><t>Invoice 42</t></si><si><t>Paid</t></si></sst>";
        let file = zip(&[("xl/sharedStrings.xml", strings, true)]);
        let text = text(&mut Cursor::new(file), Kind::Excel).unwrap().unwrap();
        assert_eq!(text, "Invoice 42\nPaid\n");
    }

    #[test]
    fn what_is_not_a_zip_has_no_text() {
        let mut file = Cursor::new(b"not an archive at all".to_vec());
        assert_eq!(text(&mut file, Kind::Word).unwrap(), None);
    }

    #[test]
    fn extensions_name_their_kind() {
        assert_eq!(Kind::of("docx"), Some(Kind::Word));
        assert_eq!(Kind::of("ods"), Some(Kind::OpenDocument));
        assert_eq!(Kind::of("txt"), None);
    }
}
