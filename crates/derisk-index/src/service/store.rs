//! The index on disk: one tantivy document per file or folder under home,
//! and searching it.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use tantivy::{
    Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term,
    collector::{DocSetCollector, TopDocs},
    query::{BooleanQuery, BoostQuery, Occur, Query, RegexQuery, TermQuery},
    schema::{
        Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions,
        Value,
    },
    tokenizer::{
        AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer,
        TokenStream,
    },
};

use super::text;
use crate::Match;

/// Written beside the index; an index with another one, or none, is
/// deleted and built again. Raised whenever the schema or the analyzer
/// below changes, since an old index would answer differently.
const VERSION: &str = "1";

/// The analyzer for names, folders, text and the typed search alike:
/// runs of letters and digits, lowercased, accents folded (`café` is
/// found by `cafe`), and nothing over 40 bytes, which is never a word.
const WORDS: &str = "words";

fn words() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(40))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
}

/// What a file looked like when it was indexed. A walk that finds the same
/// leaves it alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// Modification time, in nanoseconds since the epoch.
    pub modified: u64,
    /// Size in bytes; 0 for a folder.
    pub size: u64,
    /// Whether it is a folder.
    pub folder: bool,
}

impl Stamp {
    /// `metadata`'s stamp.
    pub fn of(metadata: &fs::Metadata) -> Self {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        let folder = metadata.is_dir();
        Self {
            modified,
            size: if folder { 0 } else { metadata.len() },
            folder,
        }
    }
}

/// The schema's fields.
#[derive(Clone, Copy, Debug)]
struct Fields {
    /// The absolute path, whole: what a document is replaced and deleted by.
    path: Field,
    /// The folder it is in, whole: what a folder's direct entries are
    /// found by.
    dir: Field,
    /// Every folder it is in, from home down, whole: what a folder's whole
    /// tree is found and deleted by.
    ancestors: Field,
    /// Its name, in words.
    name: Field,
    /// The folders it is in under home, in words.
    folders: Field,
    /// A text file's words; not stored, since a snippet is read from the
    /// file as it is when it is searched.
    content: Field,
    modified: Field,
    size: Field,
    folder: Field,
}

fn schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let words = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(WORDS)
            .set_index_option(IndexRecordOption::WithFreqs),
    );
    let fields = Fields {
        path: builder.add_text_field("path", STRING | STORED),
        dir: builder.add_text_field("dir", STRING),
        ancestors: builder.add_text_field("ancestors", STRING),
        name: builder.add_text_field("name", words.clone()),
        folders: builder.add_text_field("folders", words.clone()),
        content: builder.add_text_field("content", words),
        modified: builder.add_u64_field("modified", STORED),
        size: builder.add_u64_field("size", STORED),
        folder: builder.add_bool_field("folder", STORED | INDEXED),
    };
    (builder.build(), fields)
}

/// Empties `dir`, leaving `dir` itself: under `CacheDirectory=` it is a
/// mount in a read-only home, which cannot be removed or made again.
fn clear(dir: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// What can go wrong with the index.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// tantivy could not read or write it.
    #[error("the index: {0}")]
    Index(#[from] tantivy::TantivyError),
    /// Its directory could not be made or cleared.
    #[error("the index's directory: {0}")]
    Io(#[from] std::io::Error),
}

/// The index, opened once and shared by the thread that writes it and the
/// ones that search it.
pub struct Store {
    index: Index,
    reader: IndexReader,
    fields: Fields,
    home: PathBuf,
}

impl Store {
    /// Opens the index of `home`'s files in `dir`, making it, or making it
    /// again when it was made by another version.
    ///
    /// # Errors
    ///
    /// When `dir` cannot be made or written, or tantivy cannot open what is
    /// there.
    pub fn open(dir: &Path, home: &Path) -> Result<Self, Error> {
        let (schema, fields) = schema();
        let stamp = dir.join("VERSION");
        if fs::read_to_string(&stamp).ok().as_deref() != Some(VERSION) {
            fs::create_dir_all(dir)?;
            clear(dir)?;
            Index::create_in_dir(dir, schema)?;
            fs::write(&stamp, VERSION)?;
        }
        let index = Index::open_in_dir(dir)?;
        index.tokenizers().register(WORDS, words());
        // Reloaded by hand after each commit, so a search never waits on a
        // file watcher to see what was just written.
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Self {
            index,
            reader,
            fields,
            home: home.to_owned(),
        })
    }

    /// The home folder indexed.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The writer, for the one thread that writes. Its memory is kept to
    /// the least tantivy takes for a thread, since the index grows in
    /// small batches after the first walk.
    ///
    /// # Errors
    ///
    /// When another process holds the index's lock.
    pub fn writer(&self) -> Result<IndexWriter, Error> {
        Ok(self.index.writer_with_num_threads(1, 32 << 20)?)
    }

    /// Makes what the writer committed searchable.
    ///
    /// # Errors
    ///
    /// When the new segments cannot be opened.
    pub fn reload(&self) -> Result<(), Error> {
        Ok(self.reader.reload()?)
    }

    /// What is indexed in `dir`: its direct entries, or with `tree`
    /// everything under it, each with the stamp it was indexed with.
    ///
    /// # Errors
    ///
    /// When the index cannot be read.
    pub fn indexed(&self, dir: &Path, tree: bool) -> Result<HashMap<PathBuf, Stamp>, Error> {
        let Some(dir) = dir.to_str() else {
            return Ok(HashMap::new());
        };
        let field = if tree {
            self.fields.ancestors
        } else {
            self.fields.dir
        };
        let searcher = self.reader.searcher();
        let query = TermQuery::new(Term::from_field_text(field, dir), IndexRecordOption::Basic);
        let mut out = HashMap::new();
        for address in searcher.search(&query, &DocSetCollector)? {
            let doc: TantivyDocument = searcher.doc(address)?;
            let Some(path) = doc.get_first(self.fields.path).and_then(|v| v.as_str()) else {
                continue;
            };
            let number = |field| doc.get_first(field).and_then(|v| v.as_u64()).unwrap_or(0);
            let stamp = Stamp {
                modified: number(self.fields.modified),
                size: number(self.fields.size),
                folder: doc
                    .get_first(self.fields.folder)
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            };
            out.insert(PathBuf::from(path), stamp);
        }
        Ok(out)
    }

    /// Indexes `path` as `stamp` says it is, with `text` for a text file,
    /// replacing what was indexed for it. A path that is not UTF-8 cannot
    /// be told to the session and is left out.
    ///
    /// # Errors
    ///
    /// When the writer has failed.
    pub fn put(
        &self,
        writer: &IndexWriter,
        path: &Path,
        stamp: Stamp,
        text: Option<&str>,
    ) -> Result<(), Error> {
        let Some(whole) = path.to_str() else {
            return Ok(());
        };
        let f = self.fields;
        writer.delete_term(Term::from_field_text(f.path, whole));
        let mut doc = TantivyDocument::default();
        doc.add_text(f.path, whole);
        let parent = path.parent().unwrap_or(Path::new("/"));
        doc.add_text(f.dir, parent.to_string_lossy());
        for ancestor in parent.ancestors() {
            doc.add_text(f.ancestors, ancestor.to_string_lossy());
            if ancestor == self.home {
                break;
            }
        }
        if let Some(name) = path.file_name() {
            doc.add_text(f.name, name.to_string_lossy());
        }
        if let Ok(under) = parent.strip_prefix(&self.home) {
            doc.add_text(f.folders, under.to_string_lossy());
        }
        if let Some(text) = text {
            doc.add_text(f.content, text);
        }
        doc.add_u64(f.modified, stamp.modified);
        doc.add_u64(f.size, stamp.size);
        doc.add_bool(f.folder, stamp.folder);
        writer.add_document(doc)?;
        Ok(())
    }

    /// Forgets `path`, and with `folder` everything under it.
    pub fn remove(&self, writer: &IndexWriter, path: &Path, folder: bool) {
        let whole = path.to_string_lossy();
        writer.delete_term(Term::from_field_text(self.fields.path, &whole));
        if folder {
            writer.delete_term(Term::from_field_text(self.fields.ancestors, &whole));
        }
    }

    /// The files and folders matching `text`, best first, at most `limit`.
    ///
    /// Every word of `text` has to be in an entry's name, its folders'
    /// names or its text; the last one, unless a space follows it, may be
    /// the start of a word. Names weigh most. A file found by its text
    /// comes with the line that has the most of the words.
    ///
    /// # Errors
    ///
    /// When the index cannot be read.
    pub fn search(&self, text: &str, limit: usize) -> Result<Vec<Match>, Error> {
        let mut analyzer = words();
        let typed = tokens(&mut analyzer, text);
        let Some(last) = typed.len().checked_sub(1) else {
            return Ok(Vec::new());
        };
        // Still being typed: the last word may go on.
        let open = !text.ends_with(char::is_whitespace);
        let f = self.fields;
        let words: Vec<(Occur, Box<dyn Query>)> = typed
            .iter()
            .enumerate()
            .map(|(i, (word, _))| {
                let mut any: Vec<(Occur, Box<dyn Query>)> = vec![
                    (Occur::Should, exact(f.name, word, 4.0)),
                    (Occur::Should, exact(f.folders, word, 1.0)),
                    (Occur::Should, exact(f.content, word, 1.0)),
                ];
                if open && i == last {
                    // A prefix is matched against every word in the
                    // field's dictionary that starts with it, which for
                    // text and a single letter is most of them: text waits
                    // for a second.
                    let len = word.chars().count();
                    let prefixes = [(f.name, 1, 2.0), (f.folders, 1, 0.5), (f.content, 2, 0.5)];
                    for (field, least, boost) in prefixes {
                        if len >= least
                            && let Some(query) = prefix(field, word, boost)
                        {
                            any.push((Occur::Should, query));
                        }
                    }
                }
                (
                    Occur::Must,
                    Box::new(BooleanQuery::new(any)) as Box<dyn Query>,
                )
            })
            .collect();
        let query = BooleanQuery::new(words);
        let searcher = self.reader.searcher();
        let top = searcher.search(
            &query,
            &TopDocs::with_limit(limit.clamp(1, crate::MAX_MATCHES)).order_by_score(),
        )?;
        let mut out = Vec::with_capacity(top.len());
        for (_, address) in top {
            let doc: TantivyDocument = searcher.doc(address)?;
            let Some(path) = doc.get_first(f.path).and_then(|v| v.as_str()) else {
                continue;
            };
            let path = PathBuf::from(path);
            let folder = doc
                .get_first(f.folder)
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let (line, snippet) = if folder || self.named(&mut analyzer, &path, &typed, open) {
                (None, None)
            } else {
                match line(&mut analyzer, &path, &typed, open) {
                    Some((number, snippet)) => (Some(number), Some(snippet)),
                    None => (None, None),
                }
            };
            out.push(Match {
                path,
                folder,
                line,
                snippet,
            });
        }
        Ok(out)
    }

    /// Whether `path`'s name and folders hold every typed word, so it
    /// matched by name and needs no line from its text.
    fn named(
        &self,
        analyzer: &mut TextAnalyzer,
        path: &Path,
        typed: &[(String, usize)],
        open: bool,
    ) -> bool {
        let under = path.strip_prefix(&self.home).unwrap_or(path);
        let have = tokens(analyzer, &under.to_string_lossy());
        typed
            .iter()
            .enumerate()
            .all(|(i, (word, _))| found(&have, word, open && i + 1 == typed.len()).is_some())
    }
}

/// A query for `word` whole in `field`, weighted by `boost`.
fn exact(field: Field, word: &str, boost: f32) -> Box<dyn Query> {
    let term = TermQuery::new(
        Term::from_field_text(field, word),
        IndexRecordOption::WithFreqs,
    );
    Box::new(BoostQuery::new(Box::new(term), boost))
}

/// A query for any word in `field` starting with `word`, weighted by
/// `boost`.
fn prefix(field: Field, word: &str, boost: f32) -> Option<Box<dyn Query>> {
    // The analyzer leaves only letters and digits, which a regex takes
    // literally; anything else is escaped all the same.
    let mut pattern = String::new();
    for c in word.chars() {
        if !c.is_alphanumeric() {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push_str(".*");
    let query = RegexQuery::from_pattern(&pattern, field).ok()?;
    Some(Box::new(BoostQuery::new(Box::new(query), boost)))
}

/// `text`'s words as the index holds them, each with the byte offset it
/// starts at.
fn tokens(analyzer: &mut TextAnalyzer, text: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut stream = analyzer.token_stream(text);
    while stream.advance() {
        let token = stream.token();
        out.push((token.text.clone(), token.offset_from));
    }
    out
}

/// Where in `have` `word` is: whole, or with `open` as the start of a word.
fn found(have: &[(String, usize)], word: &str, open: bool) -> Option<usize> {
    have.iter()
        .find(|(w, _)| w == word || (open && w.starts_with(word)))
        .map(|(_, at)| *at)
}

/// Characters of a matching line shown.
const SNIPPET: usize = 120;

/// The line of `path`'s text with the most typed words, earliest first, as
/// its number from 1 and the line cut around its first match.
fn line(
    analyzer: &mut TextAnalyzer,
    path: &Path,
    typed: &[(String, usize)],
    open: bool,
) -> Option<(u32, String)> {
    let text = text::read(path).ok()??;
    let mut best: Option<(usize, u32, &str, usize)> = None;
    for (number, line) in (1u32..).zip(text.lines()) {
        if line.trim().is_empty() {
            continue;
        }
        let have = tokens(analyzer, line);
        let hits: Vec<usize> = typed
            .iter()
            .enumerate()
            .filter_map(|(i, (word, _))| found(&have, word, open && i + 1 == typed.len()))
            .collect();
        let Some(&first) = hits.iter().min() else {
            continue;
        };
        if best.is_none_or(|(count, ..)| hits.len() > count) {
            best = Some((hits.len(), number, line, first));
            if hits.len() == typed.len() {
                break;
            }
        }
    }
    best.map(|(_, number, line, at)| (number, text::cut(line, at, SNIPPET)))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Home(PathBuf);

    impl Home {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("derisk-index-store-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("home/notes")).unwrap();
            Self(dir)
        }

        fn home(&self) -> PathBuf {
            self.0.join("home")
        }

        fn put(&self, store: &Store, writer: &IndexWriter, rel: &str, text: Option<&str>) {
            let path = self.home().join(rel);
            if let Some(text) = text {
                fs::write(&path, text).unwrap();
            } else {
                fs::create_dir_all(&path).unwrap();
            }
            let stamp = Stamp::of(&fs::metadata(&path).unwrap());
            store.put(writer, &path, stamp, text).unwrap();
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn names(matches: &[Match]) -> Vec<String> {
        matches
            .iter()
            .map(|m| m.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn finds_files_by_name_and_by_text() {
        let home = Home::new("search");
        let store = Store::open(&home.0.join("index"), &home.home()).unwrap();
        let mut writer = store.writer().unwrap();
        home.put(
            &store,
            &writer,
            "notes/groceries.txt",
            Some("Buy milk\n\nand the Café's eggs\n"),
        );
        home.put(
            &store,
            &writer,
            "notes/budget.md",
            Some("# Q3\nRent 900\nMilk money 12\n"),
        );
        home.put(&store, &writer, "milk", None);
        writer.commit().unwrap();
        store.reload().unwrap();

        // The folder named milk first, then both files by their text.
        let mut found = store.search("milk", 10).unwrap();
        assert_eq!(names(&found)[0], "milk");
        assert!(found[0].folder);
        found[1..].sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(names(&found[1..]), ["budget.md", "groceries.txt"]);
        assert_eq!(found[1].line, Some(3));
        assert_eq!(found[1].snippet.as_deref(), Some("Milk money 12"));
        assert_eq!(found[2].line, Some(1));
        assert_eq!(found[2].snippet.as_deref(), Some("Buy milk"));

        // Accents are folded, and the last word may be unfinished.
        let found = store.search("cafe eg", 10).unwrap();
        assert_eq!(names(&found), ["groceries.txt"]);
        assert_eq!(found[0].line, Some(3));
        assert_eq!(found[0].snippet.as_deref(), Some("and the Café's eggs"));

        // A name match needs no line; a folder's name counts as the file's.
        let found = store.search("notes budg", 10).unwrap();
        assert_eq!(names(&found), ["budget.md"]);
        assert_eq!(found[0].line, None);

        // Every word has to be somewhere; a finished word is whole.
        assert!(store.search("milk rent eggs", 10).unwrap().is_empty());
        assert!(store.search("bud ", 10).unwrap().is_empty());
    }

    #[test]
    fn folders_go_with_everything_under_them() {
        let home = Home::new("tree");
        let store = Store::open(&home.0.join("index"), &home.home()).unwrap();
        let mut writer = store.writer().unwrap();
        home.put(&store, &writer, "notes", None);
        home.put(&store, &writer, "notes/a.txt", Some("alpha"));
        home.put(&store, &writer, "b.txt", Some("beta"));
        writer.commit().unwrap();
        store.reload().unwrap();

        let all = store.indexed(&home.home(), true).unwrap();
        assert_eq!(all.len(), 3);
        let direct = store.indexed(&home.home(), false).unwrap();
        assert_eq!(direct.len(), 2, "{direct:?}");
        assert!(direct[&home.home().join("notes")].folder);

        store.remove(&writer, &home.home().join("notes"), true);
        writer.commit().unwrap();
        store.reload().unwrap();
        let left = store.indexed(&home.home(), true).unwrap();
        assert_eq!(
            left.keys().collect::<Vec<_>>(),
            [&home.home().join("b.txt")]
        );
    }

    #[test]
    fn another_version_is_built_again() {
        let home = Home::new("version");
        let dir = home.0.join("index");
        {
            let store = Store::open(&dir, &home.home()).unwrap();
            let mut writer = store.writer().unwrap();
            home.put(&store, &writer, "a.txt", Some("alpha"));
            writer.commit().unwrap();
        }
        fs::write(dir.join("VERSION"), "0").unwrap();
        let store = Store::open(&dir, &home.home()).unwrap();
        assert!(store.indexed(&home.home(), true).unwrap().is_empty());
    }

    #[test]
    fn the_directory_is_emptied_where_it_cannot_be_removed() {
        use std::os::unix::fs::PermissionsExt;
        // A read-only parent stands in for the read-only home that
        // CacheDirectory= mounts the index's directory into.
        let home = Home::new("mounted");
        let parent = home.0.join("cache");
        let dir = parent.join("index");
        fs::create_dir_all(dir.join("old")).unwrap();
        fs::write(dir.join("VERSION"), "0").unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
        let opened = Store::open(&dir, &home.home());
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        opened.unwrap();
        assert!(!dir.join("old").exists());
    }
}
