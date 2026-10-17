//! A remote's AppStream catalogue, read as a stream.
//!
//! flatpak keeps each remote's catalogue at
//! `appstream/<remote>/<arch>/active/appstream.xml.gz` in its installation,
//! with the icons it names under `icons/<size>x<size>/` beside it. Flathub's
//! is some 50 MB of XML once inflated, every string in dozens of languages,
//! so it is read element by element and only what the store shows, in the
//! person's language or else untranslated, is kept.

use std::io::{BufRead, BufReader, Read};

use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};

/// What a component is, as its `type` attribute says.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// `desktop-application`: an app with a window.
    Desktop,
    /// `console-application`: a program for a terminal.
    Console,
    /// Runtimes, extensions, fonts, codecs: nothing a person opens.
    Other,
}

/// A block of a description: AppStream's `<p>`, `<li>` of `<ul>` or `<ol>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Block {
    /// A paragraph.
    Paragraph(String),
    /// A list item.
    Item(String),
}

impl Block {
    /// The block's text.
    pub fn text(&self) -> &str {
        match self {
            Self::Paragraph(text) | Self::Item(text) => text,
        }
    }
}

/// One `<release>`: a version, when it came out, and its notes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Release {
    /// Its version string.
    pub version: String,
    /// When it came out, in seconds since 1970, from `timestamp` or `date`.
    pub timestamp: Option<i64>,
    /// Whether it is a development release (`type="development"`).
    pub development: bool,
    /// Its release notes, which many apps leave out.
    pub notes: Vec<Block>,
}

/// One component of the catalogue, with what the store shows of it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Component {
    /// Its AppStream ID, which for a Flatpak is its application ID.
    pub id: String,
    /// What it is.
    pub kind: Kind,
    /// Its name.
    pub name: String,
    /// Its one-line summary.
    pub summary: String,
    /// Its long description.
    pub description: Vec<Block>,
    /// Who makes it.
    pub developer: String,
    /// Its licence, an SPDX expression.
    pub license: String,
    /// Its freedesktop menu categories, main and additional, as listed.
    pub categories: Vec<String>,
    /// Its search keywords.
    pub keywords: Vec<String>,
    /// The file name of its cached icon, and the size it was cached at.
    pub icon: Option<(String, u32)>,
    /// Its releases, newest first, as AppStream lists them. Only the first
    /// [`RELEASES`] are kept.
    pub releases: Vec<Release>,
    /// Its links: `homepage`, `bugtracker`, `donation`, ... and the URL.
    pub urls: Vec<(String, String)>,
    /// Whether Flathub has verified that its developer publishes it.
    pub verified: bool,
    /// Its Flatpak ref, `app/<id>/<arch>/<branch>`.
    pub bundle: Option<String>,
    /// The desktop file it is opened through.
    pub launchable: Option<String>,
}

/// How many releases of each component are kept: the front page shows the
/// newest, and an app's page the last few.
pub const RELEASES: usize = 6;

/// The person's language, as gettext reads it, for picking translations.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Language {
    /// The full locale name without its encoding: `pt_BR`, `de`.
    full: String,
    /// Its language alone: `pt`, `de`.
    language: String,
}

impl Language {
    /// From `LANGUAGE`, `LC_ALL`, `LC_MESSAGES` or `LANG`, the first set.
    /// `C` and `POSIX` mean untranslated.
    pub fn from_env() -> Self {
        let value = ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .filter_map(|name| derisk_sys::env::var(name).ok())
            .find(|value| !value.is_empty())
            .unwrap_or_default();
        // LANGUAGE is a colon-separated list; the first is the one wanted.
        Self::new(value.split(':').next().unwrap_or_default())
    }

    /// From a locale name such as `pt_BR.UTF-8` or `de`.
    pub fn new(locale: &str) -> Self {
        let full = locale
            .split(['.', '@'])
            .next()
            .unwrap_or_default()
            .to_owned();
        if full.is_empty() || full == "C" || full == "POSIX" {
            return Self::default();
        }
        let language = full.split(['_', '-']).next().unwrap_or_default().to_owned();
        Self { full, language }
    }

    /// How well an `xml:lang` value fits: 0 for untranslated text, which
    /// every language falls back to, then 1 for the language alone and 2 for
    /// the exact locale. `None` for any other language.
    fn fit(&self, lang: Option<&str>) -> Option<u8> {
        match lang {
            None => Some(0),
            // An English "translation" is the untranslated text again.
            Some(lang) if self.language.is_empty() => (lang == "en" || lang == "C").then_some(0),
            Some(lang) if lang.replace('-', "_") == self.full => Some(2),
            Some(lang) if lang == self.language => Some(1),
            Some(_) => None,
        }
    }
}

/// A translated string: the best-fitting text seen so far.
#[derive(Default)]
struct Text {
    value: String,
    fit: Option<u8>,
}

impl Text {
    /// Starts a new string in a language that fits as `fit`; false when a
    /// better-fitting one is already kept, and this one is to be skipped.
    fn start(&mut self, fit: u8) -> bool {
        if self.fit.is_some_and(|kept| kept > fit) {
            return false;
        }
        self.value.clear();
        self.fit = Some(fit);
        true
    }
}

/// A description being read, kept per fit so a translated one, which comes
/// as a `<description xml:lang>` of its own or as `<p xml:lang>` among the
/// untranslated ones, wins without mixing the two.
#[derive(Default)]
struct Description {
    blocks: [Vec<Block>; 3],
}

impl Description {
    fn best(self) -> Vec<Block> {
        self.blocks
            .into_iter()
            .rev()
            .find(|blocks| !blocks.is_empty())
            .unwrap_or_default()
    }
}

/// Where in a component the reader is: the element whose text is wanted,
/// with the fit of its language.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Field {
    Id,
    Name(u8),
    Summary(u8),
    Developer,
    DeveloperName,
    License,
    Category,
    Keyword(u8),
    Icon,
    Url,
    Launchable,
    Bundle,
    Verified,
    /// A `<p>` or `<li>` of the component's description.
    Block(u8),
    /// A `<p>` or `<li>` of the newest release's notes.
    Notes(u8),
}

/// The component being read.
#[derive(Default)]
struct Draft {
    id: String,
    kind: Option<Kind>,
    name: Text,
    summary: Text,
    description: Description,
    developer: String,
    developer_name: String,
    license: String,
    categories: Vec<String>,
    keywords: [Vec<String>; 3],
    icon: Option<(String, u32)>,
    icon_size: u32,
    releases: Vec<Release>,
    /// Whether the `<release>` being read is one of those kept, whose notes
    /// are read into `notes`.
    in_release: bool,
    notes: Description,
    urls: Vec<(String, String)>,
    url_kind: String,
    verified: bool,
    bundle: Option<String>,
    launchable: Option<String>,
    /// The text of the element being read, whitespace not yet collapsed.
    text: String,
    field: Option<Field>,
}

impl Draft {
    fn finish(mut self) -> Option<Component> {
        if self.id.is_empty() {
            return None;
        }
        if self.in_release
            && let Some(release) = self.releases.last_mut()
        {
            release.notes = std::mem::take(&mut self.notes).best();
        }
        let developer = if self.developer.is_empty() {
            self.developer_name
        } else {
            self.developer
        };
        let keywords = self
            .keywords
            .into_iter()
            .rev()
            .find(|k| !k.is_empty())
            .unwrap_or_default();
        Some(Component {
            name: if self.name.value.is_empty() {
                self.id.clone()
            } else {
                self.name.value
            },
            id: self.id,
            kind: self.kind.unwrap_or(Kind::Other),
            summary: self.summary.value,
            description: self.description.best(),
            developer,
            license: self.license,
            categories: self.categories,
            keywords,
            icon: self.icon,
            releases: self.releases,
            urls: self.urls,
            verified: self.verified,
            bundle: self.bundle,
            launchable: self.launchable,
        })
    }
}

/// Collapses runs of whitespace, as XML text in AppStream is wrapped and
/// indented however the file's writer liked.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An attribute's value, unescaped.
fn attribute(element: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    element
        .attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .and_then(|a| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A release's `date` (`2024-05-01`, maybe with a time after it) in seconds
/// since 1970, at midnight UTC.
pub fn parse_date(date: &str) -> Option<i64> {
    let mut parts = date.get(..10)?.split('-');
    let year = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400)
}

/// What [`parse`] could not read.
#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Reads a catalogue, gzipped or not (the gzip magic decides), keeping the
/// text in `language` where it is translated into it.
pub fn parse(bytes: impl Read, language: &Language) -> Result<Vec<Component>, Error> {
    let mut input = BufReader::with_capacity(1 << 16, bytes);
    let gzipped = input
        .fill_buf()
        .map_err(|e| Error(e.to_string()))?
        .starts_with(&[0x1f, 0x8b]);
    if gzipped {
        let inflated = flate2::bufread::GzDecoder::new(input);
        parse_xml(BufReader::with_capacity(1 << 16, inflated), language)
    } else {
        parse_xml(input, language)
    }
}

fn parse_xml(input: impl BufRead, language: &Language) -> Result<Vec<Component>, Error> {
    let mut reader = Reader::from_reader(input);
    let mut components = Vec::new();
    let mut buf = Vec::new();
    // Element names from the component down, with each one's language fit;
    // `None` for an element in a language that is not kept, whose children
    // are skipped with it.
    let mut path: Vec<(Vec<u8>, Option<u8>)> = Vec::new();
    let mut draft: Option<Draft> = None;
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| Error(format!("at byte {}: {e}", reader.buffer_position())))?;
        match event {
            Event::Start(element) => {
                let name = element.local_name().as_ref().to_vec();
                if name == b"component" && draft.is_none() {
                    let kind = match attribute(&element, b"type").as_deref() {
                        Some("desktop-application") => Kind::Desktop,
                        Some("console-application") => Kind::Console,
                        _ => Kind::Other,
                    };
                    draft = Some(Draft {
                        kind: Some(kind),
                        ..Draft::default()
                    });
                    path.clear();
                    continue;
                }
                let Some(component) = draft.as_mut() else {
                    continue;
                };
                let inherited = path.last().map_or(Some(0), |(_, fit)| *fit);
                let fit = match attribute(&element, b"xml:lang") {
                    Some(lang) => inherited.and(language.fit(Some(&lang))),
                    None => inherited,
                };
                path.push((name, fit));
                if let Some(fit) = fit {
                    start(component, &path, &element, fit);
                }
            }
            Event::Empty(element) => {
                // `<release version="1" date="…"/>` and the like: no text,
                // but a release all the same.
                let Some(component) = draft.as_mut() else {
                    continue;
                };
                let name = element.local_name().as_ref().to_vec();
                path.push((name, path.last().map_or(Some(0), |(_, fit)| *fit)));
                if let Some(fit) = path.last().and_then(|(_, fit)| *fit) {
                    start(component, &path, &element, fit);
                }
                end(component, &path);
                path.pop();
            }
            Event::Text(text) => {
                if let Some(component) = draft.as_mut()
                    && component.field.is_some()
                {
                    let text = text.decode().map_err(|e| Error(e.to_string()))?;
                    component.text.push_str(&text);
                }
            }
            Event::CData(text) => {
                if let Some(component) = draft.as_mut()
                    && component.field.is_some()
                {
                    component.text.push_str(&String::from_utf8_lossy(&text));
                }
            }
            Event::GeneralRef(reference) => {
                if let Some(component) = draft.as_mut()
                    && component.field.is_some()
                {
                    if let Ok(Some(c)) = reference.resolve_char_ref() {
                        component.text.push(c);
                    } else if let Ok(name) = reference.decode()
                        && let Some(value) = quick_xml::escape::resolve_predefined_entity(&name)
                    {
                        component.text.push_str(value);
                    }
                }
            }
            Event::End(element) => {
                let name = element.local_name();
                if path.is_empty() && name.as_ref() == b"component" {
                    if let Some(done) = draft.take().and_then(Draft::finish) {
                        components.push(done);
                    }
                    continue;
                }
                if let Some(component) = draft.as_mut() {
                    end(component, &path);
                    path.pop();
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(components)
}

/// The names in `path`, for matching where an element is.
fn names(path: &[(Vec<u8>, Option<u8>)]) -> impl DoubleEndedIterator<Item = &[u8]> {
    path.iter().map(|(name, _)| name.as_slice())
}

/// An element the store reads has opened at the end of `path`.
fn start(component: &mut Draft, path: &[(Vec<u8>, Option<u8>)], element: &BytesStart<'_>, fit: u8) {
    let names: Vec<&[u8]> = names(path).collect();
    let field = match names.as_slice() {
        // Inline markup inside a paragraph (`<em>`, `<code>`) keeps adding
        // to the paragraph's text.
        [b"description", .., b"p" | b"li", _]
        | [b"releases", b"release", b"description", .., b"p" | b"li", _] => return,
        [b"id"] => Some(Field::Id),
        [b"name"] => component.name.start(fit).then_some(Field::Name(fit)),
        [b"summary"] => component.summary.start(fit).then_some(Field::Summary(fit)),
        [b"developer", b"name"] if fit == 0 => Some(Field::Developer),
        [b"developer_name"] if fit == 0 => Some(Field::DeveloperName),
        [b"project_license"] => Some(Field::License),
        [b"categories", b"category"] => Some(Field::Category),
        [b"keywords", b"keyword"] => Some(Field::Keyword(fit)),
        [b"icon"] => icon(component, element),
        [b"url"] => {
            component.url_kind = attribute(element, b"type").unwrap_or_default();
            Some(Field::Url)
        }
        [b"launchable"] => (attribute(element, b"type").as_deref() == Some("desktop-id"))
            .then_some(Field::Launchable),
        [b"bundle"] => {
            (attribute(element, b"type").as_deref() == Some("flatpak")).then_some(Field::Bundle)
        }
        [b"custom", b"value"] => (attribute(element, b"key").as_deref()
            == Some("flathub::verification::verified"))
        .then_some(Field::Verified),
        [b"description", .., b"p" | b"li"] => Some(Field::Block(fit)),
        [b"releases", b"release"] => {
            release(component, element);
            None
        }
        [b"releases", b"release", b"description", .., b"p" | b"li"] if component.in_release => {
            Some(Field::Notes(fit))
        }
        _ => None,
    };
    component.text.clear();
    component.field = field;
}

/// A cached `<icon>` at the size the store draws best, 64 or 128 pixels.
fn icon(component: &mut Draft, element: &BytesStart<'_>) -> Option<Field> {
    if attribute(element, b"type").as_deref() != Some("cached") {
        return None;
    }
    // A HiDPI copy lives in `<size>x<size>@2`; the plain one is enough.
    if attribute(element, b"scale").is_some_and(|s| s != "1") {
        return None;
    }
    let size: u32 = attribute(element, b"width")
        .and_then(|w| w.parse().ok())
        .unwrap_or(64);
    let better = |kept: u32| (size == 64) || (kept != 64 && size > kept);
    if component.icon.is_some() && !better(component.icon_size) {
        return None;
    }
    component.icon_size = size;
    Some(Field::Icon)
}

/// A `<release>` opened: the previous one's notes are done, and this one
/// is kept if there is room.
fn release(component: &mut Draft, element: &BytesStart<'_>) {
    if component.in_release
        && let Some(previous) = component.releases.last_mut()
    {
        previous.notes = std::mem::take(&mut component.notes).best();
    }
    component.notes = Description::default();
    component.in_release = component.releases.len() < RELEASES;
    if !component.in_release {
        return;
    }
    let timestamp = attribute(element, b"timestamp")
        .and_then(|t| t.parse().ok())
        .or_else(|| attribute(element, b"date").and_then(|d| parse_date(&d)));
    component.releases.push(Release {
        version: attribute(element, b"version").unwrap_or_default(),
        timestamp,
        development: attribute(element, b"type").as_deref() == Some("development"),
        notes: Vec::new(),
    });
}

/// The element at the end of `path` closed: what it held goes where it
/// belongs.
fn end(component: &mut Draft, path: &[(Vec<u8>, Option<u8>)]) {
    let Some(field) = component.field else {
        return;
    };
    // Inline markup closing inside a paragraph leaves the paragraph open.
    let closing = names(path).next_back().unwrap_or_default();
    let block_closing = matches!(closing, b"p" | b"li");
    if matches!(field, Field::Block(_) | Field::Notes(_)) && !block_closing {
        return;
    }
    let text = collapse(&component.text);
    component.text.clear();
    component.field = None;
    let block = |text: String| {
        if closing == b"li" {
            Block::Item(text)
        } else {
            Block::Paragraph(text)
        }
    };
    match field {
        Field::Id => component.id = text,
        Field::Name(_) => component.name.value = text,
        Field::Summary(_) => component.summary.value = text,
        Field::Developer => component.developer = text,
        Field::DeveloperName => component.developer_name = text,
        Field::License => component.license = text,
        Field::Category if !text.is_empty() => component.categories.push(text),
        Field::Keyword(fit) if !text.is_empty() => {
            component.keywords[usize::from(fit)].push(text.to_lowercase());
        }
        Field::Icon => component.icon = Some((text, component.icon_size)),
        Field::Url if !text.is_empty() => {
            component
                .urls
                .push((std::mem::take(&mut component.url_kind), text));
        }
        Field::Launchable => component.launchable = Some(text),
        Field::Bundle => component.bundle = Some(text),
        Field::Verified => component.verified = text == "true",
        Field::Block(fit) if !text.is_empty() => {
            component.description.blocks[usize::from(fit)].push(block(text));
        }
        Field::Notes(fit) if !text.is_empty() => {
            component.notes.blocks[usize::from(fit)].push(block(text));
        }
        _ => {}
    }
}
