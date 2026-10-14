use std::{fmt, str::FromStr};

/// The colour a machine's windows are drawn in, so a person can tell at a
/// glance which machine a window belongs to.
///
/// These are Qubes' eight, with the same meaning by convention: the further
/// along, the more a person trusts what runs there. pm's `--label` accepts
/// exactly these names.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Label {
    /// Red.
    Red,
    /// Orange.
    Orange,
    /// Yellow.
    Yellow,
    /// Green.
    Green,
    /// Gray, and what a machine without a label gets.
    #[default]
    Gray,
    /// Blue.
    Blue,
    /// Purple.
    Purple,
    /// Black.
    Black,
}

impl Label {
    /// Every label, in the order pickers offer them.
    pub const ALL: [Label; 8] = [
        Label::Red,
        Label::Orange,
        Label::Yellow,
        Label::Green,
        Label::Gray,
        Label::Blue,
        Label::Purple,
        Label::Black,
    ];

    /// The name pm and the domain's metadata use.
    pub fn name(self) -> &'static str {
        match self {
            Label::Red => "red",
            Label::Orange => "orange",
            Label::Yellow => "yellow",
            Label::Green => "green",
            Label::Gray => "gray",
            Label::Blue => "blue",
            Label::Purple => "purple",
            Label::Black => "black",
        }
    }

    /// The colour as sRGB, before any theme restyles it.
    ///
    /// The Tango palette's, as Qubes draws them: saturated enough to read
    /// on a window border two pixels wide against a dark or light theme.
    /// A theme that wants others maps [`Label::name`] to its own.
    pub fn rgb(self) -> [u8; 3] {
        match self {
            Label::Red => [0xcc, 0x00, 0x00],
            Label::Orange => [0xf5, 0x79, 0x00],
            Label::Yellow => [0xed, 0xd4, 0x00],
            Label::Green => [0x73, 0xd2, 0x16],
            Label::Gray => [0x88, 0x8a, 0x85],
            Label::Blue => [0x34, 0x65, 0xa4],
            Label::Purple => [0x75, 0x50, 0x7b],
            // Not pure black, which a dark theme's border would swallow.
            Label::Black => [0x2e, 0x34, 0x36],
        }
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A name that is not one of the eight labels.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error(
    "{0:?} is not a label; the labels are red, orange, yellow, green, gray, blue, purple and black"
)]
pub struct UnknownLabel(pub String);

impl FromStr for Label {
    type Err = UnknownLabel;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Label::ALL
            .into_iter()
            .find(|label| label.name() == s)
            .ok_or_else(|| UnknownLabel(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for label in Label::ALL {
            assert_eq!(label.name().parse::<Label>(), Ok(label));
        }
        assert!("grey".parse::<Label>().is_err());
    }

    #[test]
    fn colours_differ() {
        let mut colours: Vec<_> = Label::ALL.into_iter().map(Label::rgb).collect();
        colours.sort();
        colours.dedup();
        assert_eq!(colours.len(), Label::ALL.len());
    }
}
