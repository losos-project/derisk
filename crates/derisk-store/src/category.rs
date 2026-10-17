//! The store's categories, and which of them an app belongs in.
//!
//! An app's AppStream `<categories>` are freedesktop menu categories: a few
//! main ones (`Game`, `Office`) and many additional ones (`ArcadeGame`,
//! `WebBrowser`) that the menu specification says belong under them. Most
//! apps name a main category; some name only additional ones, and a few name
//! none at all. So an app goes in every main category it names, then every
//! one its additional categories belong under, and failing both, the one its
//! keywords, name and summary speak of most. The additional categories are
//! also the sections a category page is split into.

use crate::appstream::{Component, Kind};

/// A section of the store.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum Category {
    /// `AudioVideo`, `Audio`, `Video`.
    AudioVideo,
    /// `Development`.
    Development,
    /// `Education`.
    Education,
    /// `Game`.
    Games,
    /// `Graphics`.
    Graphics,
    /// `Network`.
    Internet,
    /// `Office`.
    Productivity,
    /// `Science`.
    Science,
    /// `System` and `Settings`.
    System,
    /// `Utility`.
    Utilities,
}

impl Category {
    /// Every category, in the order the front page lists them.
    pub const ALL: [Self; 10] = [
        Self::Productivity,
        Self::Graphics,
        Self::AudioVideo,
        Self::Games,
        Self::Internet,
        Self::Development,
        Self::Education,
        Self::Science,
        Self::System,
        Self::Utilities,
    ];

    /// The name shown for it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::AudioVideo => "Audio & Video",
            Self::Development => "Developer Tools",
            Self::Education => "Education",
            Self::Games => "Games",
            Self::Graphics => "Graphics & Photography",
            Self::Internet => "Internet",
            Self::Productivity => "Productivity",
            Self::Science => "Science",
            Self::System => "System",
            Self::Utilities => "Utilities",
        }
    }

    /// Its icon, a name in derisk's icon set.
    pub const fn icon(self) -> &'static str {
        match self {
            Self::AudioVideo => "applications-multimedia",
            Self::Development => "applications-development",
            Self::Education => "applications-education",
            Self::Games => "applications-games",
            Self::Graphics => "applications-graphics",
            Self::Internet => "applications-internet",
            Self::Productivity => "applications-office",
            Self::Science => "applications-science",
            Self::System => "applications-system",
            Self::Utilities => "applications-utilities",
        }
    }

    /// The main category a freedesktop category name is, if it is one.
    pub fn main(name: &str) -> Option<Self> {
        Some(match name {
            "AudioVideo" | "Audio" | "Video" => Self::AudioVideo,
            "Development" => Self::Development,
            "Education" => Self::Education,
            "Game" => Self::Games,
            "Graphics" => Self::Graphics,
            "Network" => Self::Internet,
            "Office" => Self::Productivity,
            "Science" => Self::Science,
            "System" | "Settings" => Self::System,
            "Utility" => Self::Utilities,
            _ => return None,
        })
    }

    /// The main categories an additional category belongs under, from the
    /// menu specification's table of additional categories ("Related
    /// Categories"), with `Utility` dropped where a narrower one is listed.
    pub fn related(name: &str) -> &'static [Self] {
        use Category::*;
        match name {
            "Building" | "Debugger" | "IDE" | "GUIDesigner" | "Profiling" | "RevisionControl"
            | "Translation" | "WebDevelopment" => &[Development],
            "Calendar" | "ContactManagement" | "Database" | "Chart" | "Finance" | "FlowChart"
            | "PDA" | "ProjectManagement" | "Presentation" | "Spreadsheet" | "WordProcessor"
            | "Publishing" => &[Productivity],
            "Dictionary" => &[Productivity, Education],
            "Email" => &[Productivity, Internet],
            "2DGraphics" | "VectorGraphics" | "RasterGraphics" | "3DGraphics" | "Scanning"
            | "OCR" | "Photography" => &[Graphics],
            "Viewer" => &[Graphics, Productivity],
            "Dialup" | "InstantMessaging" | "Chat" | "IRCClient" | "Feed" | "FileTransfer"
            | "HamRadio" | "News" | "P2P" | "RemoteAccess" | "Telephony" | "VideoConference"
            | "WebBrowser" => &[Internet],
            "Midi" | "Mixer" | "Sequencer" | "Tuner" | "TV" | "AudioVideoEditing" | "Player"
            | "Recorder" | "DiscBurning" | "Music" => &[AudioVideo],
            "ActionGame" | "AdventureGame" | "ArcadeGame" | "BoardGame" | "BlocksGame"
            | "CardGame" | "KidsGame" | "LogicGame" | "RolePlaying" | "Shooter" | "Simulation"
            | "SportsGame" | "StrategyGame" | "Amusement" => &[Games],
            "Emulator" => &[Games, System],
            "Art" | "Construction" | "Languages" | "History" | "Humanities" | "Literature"
            | "Spirituality" | "Sports" | "Geography" => &[Education],
            "ArtificialIntelligence"
            | "Astronomy"
            | "Biology"
            | "Chemistry"
            | "ComputerScience"
            | "DataVisualization"
            | "Economy"
            | "Electricity"
            | "Geology"
            | "Geoscience"
            | "ImageProcessing"
            | "Maps"
            | "Math"
            | "NumericalAnalysis"
            | "MedicalSoftware"
            | "Physics"
            | "Robotics"
            | "ParallelComputing"
            | "Electronics"
            | "Engineering" => &[Science],
            "FileManager" | "TerminalEmulator" | "Filesystem" | "Monitor" | "Security"
            | "PackageManager" | "Core" => &[System],
            "Archiving" | "Compression" | "FileTools" | "Accessibility" | "Calculator"
            | "Clock" | "TextEditor" | "TextTools" | "Documentation" => &[Utilities],
            _ => &[],
        }
    }

    /// Words that, in an app's keywords, name or summary, speak of this
    /// category, for an app that names no category at all.
    fn words(self) -> &'static [&'static str] {
        match self {
            Self::AudioVideo => &[
                "music", "audio", "video", "player", "podcast", "radio", "sound", "movie",
                "stream", "media",
            ],
            Self::Development => &[
                "developer",
                "development",
                "programming",
                "code",
                "git",
                "compiler",
                "debug",
                "sdk",
                "api",
                "ide",
                "build",
                "flatpak",
                "appstream",
            ],
            Self::Education => &[
                "learn",
                "learning",
                "education",
                "school",
                "teach",
                "language",
            ],
            Self::Games => &["game", "games", "play", "puzzle", "arcade", "chess"],
            Self::Graphics => &[
                "image", "images", "photo", "draw", "paint", "3d", "svg", "picture",
            ],
            Self::Internet => &[
                "browser",
                "chat",
                "email",
                "mail",
                "download",
                "torrent",
                "ssh",
                "vpn",
                "network",
                "web",
                "messenger",
            ],
            Self::Productivity => &[
                "document",
                "spreadsheet",
                "pdf",
                "notes",
                "calendar",
                "finance",
                "markdown",
                "office",
                "todo",
            ],
            Self::Science => &[
                "math",
                "physics",
                "chemistry",
                "astronomy",
                "science",
                "opencl",
            ],
            Self::System => &[
                "system", "disk", "monitor", "firmware", "driver", "terminal", "shell", "backup",
                "security", "vulkan", "gpu", "device",
            ],
            Self::Utilities => &[
                "file",
                "files",
                "tool",
                "utility",
                "converter",
                "calculator",
            ],
        }
    }
}

/// Where an app goes: the categories it is listed in, the first of them the
/// one named on its card.
pub fn categorise(app: &Component) -> Vec<Category> {
    let mut named: Vec<Category> = Vec::new();
    let push = |category: Category, into: &mut Vec<Category>| {
        if !into.contains(&category) {
            into.push(category);
        }
    };
    for name in &app.categories {
        if let Some(main) = Category::main(name) {
            push(main, &mut named);
        }
    }
    let mut related = Vec::new();
    for name in &app.categories {
        for &main in Category::related(name) {
            push(main, &mut related);
        }
    }
    // Additional categories are the more specific: an app that says
    // `Utility;Game;ArcadeGame` is a game first. Utilities, the catch-all,
    // goes last whenever anything narrower is there.
    let mut all = Vec::new();
    for &category in related.iter().filter(|c| named.contains(c)) {
        push(category, &mut all);
    }
    for &category in named.iter().chain(&related) {
        push(category, &mut all);
    }
    if all.len() > 1
        && let Some(at) = all.iter().position(|&c| c == Category::Utilities)
    {
        let utilities = all.remove(at);
        all.push(utilities);
    }
    if !all.is_empty() {
        return all;
    }
    match guess(app) {
        Some(category) => vec![category],
        // A program for the terminal that says nothing of itself is most
        // likely a tool for developers or the system's administrator.
        None if app.kind == Kind::Console => vec![Category::Development],
        None => vec![Category::Utilities],
    }
}

/// The category an app that names none speaks of most: its keywords count
/// twice, the words of its name and summary once.
fn guess(app: &Component) -> Option<Category> {
    let words = |text: &str| {
        text.split(|c: char| !c.is_alphanumeric())
            .map(str::to_lowercase)
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
    };
    let keywords: Vec<String> = app.keywords.iter().flat_map(|k| words(k)).collect();
    let text: Vec<String> = words(&app.name)
        .into_iter()
        .chain(words(&app.summary))
        .chain(app.id.rsplit('.').next().map(str::to_lowercase))
        .collect();
    Category::ALL
        .into_iter()
        .map(|category| {
            let hits = |list: &[String]| {
                list.iter()
                    .filter(|w| category.words().contains(&w.as_str()))
                    .count()
            };
            (2 * hits(&keywords) + hits(&text), category)
        })
        .filter(|(score, _)| *score > 0)
        // The first category in `ALL` wins a tie.
        .min_by_key(|&(score, category)| {
            (
                std::cmp::Reverse(score),
                Category::ALL.iter().position(|&c| c == category),
            )
        })
        .map(|(_, category)| category)
}

/// The sections of `category` that `app` is in: its additional categories
/// that belong under it.
pub fn sections(app: &Component, category: Category) -> impl Iterator<Item = &str> {
    app.categories
        .iter()
        .map(String::as_str)
        .filter(move |name| Category::related(name).contains(&category))
}

/// A section's name as shown: `ArcadeGame` is "Arcade", `2DGraphics` "2D
/// Graphics", `WebBrowser` "Web Browsers".
pub fn section_name(name: &str) -> String {
    let named = match name {
        "AudioVideoEditing" => "Editing",
        "IDE" => "IDEs",
        "P2P" => "Peer to Peer",
        "OCR" => "Text Recognition",
        "TV" => "TV",
        "PDA" => "Organisers",
        "IRCClient" => "IRC",
        "RolePlaying" => "Role Playing",
        "Shooter" => "Shooters",
        "WebBrowser" => "Web Browsers",
        "InstantMessaging" => "Messaging",
        "TerminalEmulator" => "Terminals",
        "FileManager" => "File Managers",
        "TextEditor" => "Text Editors",
        "WordProcessor" => "Word Processors",
        "Player" => "Players",
        "Recorder" => "Recorders",
        "Emulator" => "Emulators",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_owned();
    }
    let name = name
        .strip_suffix("Game")
        .filter(|n| !n.is_empty())
        .unwrap_or(name);
    // Splits CamelCase, keeping a run of capitals or digits ("2D", "3D")
    // together with the word it starts.
    let mut out = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
        let prev = i.checked_sub(1).map(|p| chars[p]);
        let boundary = c.is_uppercase()
            && prev.is_some_and(|p| p.is_lowercase() || (p.is_uppercase() && next_lower));
        if boundary {
            out.push(' ');
        }
        out.push(c);
    }
    out
}
