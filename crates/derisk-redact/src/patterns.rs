//! What counts as a secret in a line of text.
//!
//! The text is usually OCR's reading of the screen, so every pattern
//! forgives what OCR gets wrong: underscores read as spaces, an `@` read as
//! a letter, a character it could not read at all as `?`. A missed secret
//! is a leak and a false alarm is a black box over something harmless, so
//! where the two pull apart the patterns lean towards blacking out.

use std::{ops::Range, sync::LazyLock};

use regex::Regex;

/// What kind of secret a match is, for saying what was blacked out.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Kind {
    /// A password field, or the value after a word such as "Password:".
    Password,
    /// An email address.
    Email,
    /// A payment card number.
    Card,
    /// An IBAN.
    Iban,
    /// An API key, token, private key or other credential.
    Key,
    /// A Tor onion address.
    Onion,
    /// A cryptocurrency address.
    Wallet,
    /// A phone number in international form.
    Phone,
    /// A public IP address.
    Address,
    /// A window that is all secrets, such as a password manager's.
    Window,
}

impl Kind {
    /// Every kind, in the order they are listed to the person.
    pub const ALL: [Self; 10] = [
        Self::Password,
        Self::Key,
        Self::Card,
        Self::Iban,
        Self::Email,
        Self::Phone,
        Self::Onion,
        Self::Wallet,
        Self::Address,
        Self::Window,
    ];

    /// A plural noun for the kind, as in "2 emails".
    pub fn plural(self) -> &'static str {
        match self {
            Self::Password => "passwords",
            Self::Email => "emails",
            Self::Card => "card numbers",
            Self::Iban => "IBANs",
            Self::Key => "keys",
            Self::Onion => "onion addresses",
            Self::Wallet => "wallet addresses",
            Self::Phone => "phone numbers",
            Self::Address => "IP addresses",
            Self::Window => "windows",
        }
    }

    /// A singular noun for the kind, as in "1 email".
    pub fn singular(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Email => "email",
            Self::Card => "card number",
            Self::Iban => "IBAN",
            Self::Key => "key",
            Self::Onion => "onion address",
            Self::Wallet => "wallet address",
            Self::Phone => "phone number",
            Self::Address => "IP address",
            Self::Window => "window",
        }
    }
}

/// A secret found in a text: its bytes and its kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Found {
    /// Byte range in the text.
    pub range: Range<usize>,
    /// What it is.
    pub kind: Kind,
}

/// Longest value taken after a word such as "Password:".
const MAX_VALUE: usize = 160;

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("the patterns are valid")
}

/// A word that says the value after it is secret, then `:` or `=`.
/// `GITHUB_TOKEN=`, `api key:` and `SECRET_KEY=` count, and so does OCR's
/// `GITHUB TOKEN=`, but not `Passed:` or `Pinned:`; the value runs to the
/// end of the line, which on a screen is where a value ends.
static LABELLED: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)(?:^|[^a-z])(?:pass(?:word|wd|phrase|code)?|pwd|pin|secret|token|api[ _\-]?key|access[ _\-]?key|private[ _\-]?key|client[ _\-]?secret|authori[sz]ation|auth|credentials?|cvv|cvc|ssn|iban|card(?:[ _\-]?(?:number|no))?|seed(?:[ _\-]?(?:phrase|words))?|mnemonic|recovery[ _\-]?(?:codes?|phrase|key)|backup[ _\-]?codes?|otp|totp|2fa[ _\-]?code)(?:[ _\-]?(?:id|value|hash|key|secret|token|code|number|no|string|phrase))?\s*[:=]\s*(\S.*)",
    )
});

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,}")
});

/// 13 to 19 digits, optionally in groups split by spaces or dashes.
static CARD: LazyLock<Regex> = LazyLock::new(|| regex(r"\b\d(?:[ \-]?\d){12,18}\b"));

/// Four groups of four digits: a card as cards print it, whether or not
/// OCR read every digit right, so it is not held to Luhn.
static CARD_GROUPS: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\b\d{4}[ \-]\d{4}[ \-]\d{4}[ \-]\d{1,7}\b"));

static IBAN: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\b[A-Z]{2}\d{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,3})?\b"));

/// Version 3 is 56 base32 characters, version 2 was 16; OCR may misread a
/// few, so any letters and digits of about that length before `.onion`.
static ONION: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\b[a-z0-9?]{16,60}\.onion\b"));

/// Credentials that announce themselves with a prefix: GitHub, GitLab,
/// Slack, Stripe, OpenAI and Anthropic (`sk-`), Hugging Face, npm, PyPI,
/// Shopify, DigitalOcean, age. The separator is optional because OCR reads
/// `_` as a space or not at all.
static PREFIXED: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b(?:gh[pousr]|github_pat|glpat|xox[abeprs]|sk|pk|rk|sk_live|sk_test|pk_live|rk_live|hf|npm|pypi|shp(?:at|ss|ca|pa)|dop_v1|AGE-SECRET-KEY-1)[ _\-]?[A-Za-z0-9_\-]{16,}",
    )
});

/// AWS access key IDs, Google API keys and JSON web tokens. A PEM or
/// OpenSSH private key's `BEGIN` line says nothing secret; its body is
/// random enough for [`random`].
static KNOWN: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b(?:AKIA|ASIA|AGPA|AIDA|AROA)[A-Z0-9]{16}\b|\bAIza[0-9A-Za-z_\-]{30,}|\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}(?:\.[A-Za-z0-9_\-]*)?",
    )
});

/// A run of characters a random token is made of. `?` is in it because OCR
/// writes it for a character it could not read, which in a random token is
/// most of them.
static RUN: LazyLock<Regex> = LazyLock::new(|| regex(r"[A-Za-z0-9+/=_\-?]{20,}"));

/// Bech32 Bitcoin addresses and Ethereum addresses; Monero's and legacy
/// Bitcoin's are random enough for [`random`].
static WALLET: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\bbc1[a-z0-9]{25,87}\b|\b0x[0-9a-fA-F]{40}\b"));

/// A `+`, a country code and 8 to 15 digits in all.
static PHONE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\+\d{1,3}[ \-.]?\(?\d{1,4}\)?(?:[ \-.]?\d{2,4}){2,5}"));

/// Four numbers with dots. No `\b` in front: OCR reads `root@203.0.113.42`
/// as `roote203.0.113.42`.
static IPV4: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?:^|[^\d.])((?:\d{1,3}\.){3}\d{1,3})(?:$|[^\d.])"));

/// A global unicast IPv6 address (2000::/3) with at least three colons, so
/// a time such as 12:30:45 never matches.
static IPV6: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\b[23][0-9a-f]{3}(?::[0-9a-f]{0,4}){3,7}\b"));

/// Every secret in `text`, sorted, overlapping ones merged into the first
/// one's kind.
pub fn find(text: &str) -> Vec<Found> {
    let mut found = Vec::new();
    let mut push = |range: Range<usize>, kind| {
        if !range.is_empty() {
            found.push(Found { range, kind });
        }
    };
    for c in LABELLED.captures_iter(text) {
        let value = c.get(1).expect("the value group always matches");
        let end = floor_char_boundary(text, (value.start() + MAX_VALUE).min(value.end()));
        push(
            value.start()..end,
            labelled_kind(&text[c.get(0).unwrap().range()]),
        );
    }
    for m in KNOWN.find_iter(text).chain(PREFIXED.find_iter(text)) {
        push(m.range(), Kind::Key);
    }
    for m in EMAIL.find_iter(text) {
        push(m.range(), Kind::Email);
    }
    for m in ONION.find_iter(text) {
        push(m.range(), Kind::Onion);
    }
    for m in WALLET.find_iter(text) {
        push(m.range(), Kind::Wallet);
    }
    for m in CARD_GROUPS.find_iter(text) {
        push(m.range(), Kind::Card);
    }
    for m in CARD.find_iter(text) {
        if luhn(m.as_str()) {
            push(m.range(), Kind::Card);
        }
    }
    for m in IBAN.find_iter(text) {
        if iban(m.as_str()) {
            push(m.range(), Kind::Iban);
        }
    }
    for m in PHONE.find_iter(text) {
        let digits = m.as_str().chars().filter(char::is_ascii_digit).count();
        if (8..=15).contains(&digits) {
            push(m.range(), Kind::Phone);
        }
    }
    for c in IPV4.captures_iter(text) {
        let m = c.get(1).expect("the address group always matches");
        if public_ipv4(m.as_str()) {
            push(m.range(), Kind::Address);
        }
    }
    for m in IPV6.find_iter(text) {
        push(m.range(), Kind::Address);
    }
    for m in RUN.find_iter(text) {
        if random(m.as_str()) {
            push(m.range(), Kind::Key);
        }
    }
    merge(found)
}

/// The kind a labelled value is, from its label.
fn labelled_kind(label: &str) -> Kind {
    let label = label.to_ascii_lowercase();
    if label.contains("iban") {
        Kind::Iban
    } else if label.contains("card") || label.contains("cvv") || label.contains("cvc") {
        Kind::Card
    } else if [
        "token",
        "key",
        "secret",
        "auth",
        "credential",
        "seed",
        "mnemonic",
        "recovery",
        "backup",
    ]
    .iter()
    .any(|w| label.contains(w))
    {
        Kind::Key
    } else {
        Kind::Password
    }
}

/// Sorts `found` and merges overlapping ranges, keeping the kind of the one
/// that starts first.
fn merge(mut found: Vec<Found>) -> Vec<Found> {
    found.sort_by_key(|f| (f.range.start, std::cmp::Reverse(f.range.end)));
    let mut merged: Vec<Found> = Vec::with_capacity(found.len());
    for f in found {
        match merged.last_mut() {
            Some(last) if f.range.start < last.range.end => {
                last.range.end = last.range.end.max(f.range.end);
            }
            _ => merged.push(f),
        }
    }
    merged
}

fn floor_char_boundary(text: &str, mut i: usize) -> usize {
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Whether the digits in `s` pass the Luhn check every card number does.
fn luhn(s: &str) -> bool {
    let digits: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| match i % 2 {
            0 => d,
            _ if d * 2 > 9 => d * 2 - 9,
            _ => d * 2,
        })
        .sum();
    sum.is_multiple_of(10)
}

/// Whether `s` is an IBAN with a valid check (ISO 13616: mod 97).
fn iban(s: &str) -> bool {
    let compact: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).collect();
    if !(15..=34).contains(&compact.len()) {
        return false;
    }
    let rearranged = compact[4..].iter().chain(&compact[..4]);
    let mut rest = 0u32;
    for c in rearranged {
        let n = match c.to_digit(36) {
            Some(n) => n,
            None => return false,
        };
        rest = if n < 10 {
            (rest * 10 + n) % 97
        } else {
            (rest * 100 + n) % 97
        };
    }
    rest == 1
}

/// Whether `s` is an IPv4 address on the internet: not loopback, private,
/// link-local or unspecified, whose numbers say nothing about anyone.
fn public_ipv4(s: &str) -> bool {
    let parts: Vec<u32> = s.split('.').filter_map(|p| p.parse().ok()).collect();
    let [a, b, _, _] = parts[..] else {
        return false;
    };
    if parts.iter().any(|&p| p > 255) {
        return false;
    }
    !matches!(
        (a, b),
        (0 | 10 | 127, _) | (169, 254) | (192, 168) | (172, 16..=31) | (255, _)
    )
}

/// Whether a run of token characters looks random rather than written:
/// letters and digits that keep switching between upper case, lower case
/// and digits, as base62 and base64 do and words, paths, dates and
/// camelCase names don't. A run in one case needs a longer stretch without
/// a separator (an onion address, a hex key, a bech32 address) before it
/// counts, which leaves Nix's 32-character store hashes, UUIDs and Git's
/// 40-digit commit hashes alone.
fn random(s: &str) -> bool {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Class {
        Upper,
        Lower,
        Digit,
    }
    let class = |c: char| match c {
        'A'..='Z' => Some(Class::Upper),
        'a'..='z' => Some(Class::Lower),
        '0'..='9' => Some(Class::Digit),
        _ => None,
    };
    // How often consecutive letters and digits change class; separators
    // are skipped, so a path's slashes don't make it look random.
    let rate = |s: &str| {
        let classes: Vec<Class> = s.chars().filter_map(class).collect();
        let switches = classes.windows(2).filter(|w| w[0] != w[1]).count();
        (
            switches as f32 / classes.len().saturating_sub(1).max(1) as f32,
            classes,
        )
    };
    let unreadable = s.chars().filter(|&c| c == '?').count();
    // A line OCR could barely read, such as a private key's body.
    if s.len() >= 24 && unreadable * 8 >= s.len() {
        return true;
    }
    let (whole, classes) = rate(s);
    let has = |k| classes.contains(&k);
    if !has(Class::Digit) || !(has(Class::Upper) || has(Class::Lower)) {
        return false;
    }
    // Mixed case: random, or as long as a line of base64 (a key's body,
    // whose first line is mostly the `AAAA` of its header) and no name is.
    if has(Class::Upper) && has(Class::Lower) {
        return classes.len() >= 48 || (classes.len() >= 20 && whole >= 0.4);
    }
    s.split(|c: char| !c.is_ascii_alphanumeric() && c != '?')
        .filter(|segment| segment.len() >= 40)
        .filter(|segment| !(segment.len() == 40 && segment.chars().all(|c| c.is_ascii_hexdigit())))
        .any(|segment| rate(segment).0 >= 0.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The secrets `find` returns, as text, with kinds.
    fn found(text: &str) -> Vec<(&str, Kind)> {
        find(text)
            .into_iter()
            .map(|f| (&text[f.range], f.kind))
            .collect()
    }

    #[test]
    fn emails() {
        assert_eq!(
            found("Signed in as matus.example@proton.me"),
            [("matus.example@proton.me", Kind::Email)]
        );
    }

    #[test]
    fn labelled_values_run_to_the_end_of_the_line() {
        assert_eq!(
            found("Password: hunter2-Correct-Horse"),
            [("hunter2-Correct-Horse", Kind::Password)]
        );
        assert_eq!(found("pin=1234"), [("1234", Kind::Password)]);
        // OCR's reading of `GITHUB_TOKEN=ghp_…`, underscores gone.
        assert_eq!(
            found("export GITHUB TOKEN=ghp R8x2Ka9LmZD4Tn7VW1Yb3Cd5Ef6GhOIi2KlMn"),
            [("ghp R8x2Ka9LmZD4Tn7VW1Yb3Cd5Ef6GhOIi2KlMn", Kind::Key)]
        );
        assert_eq!(
            found("IBAN: SK31 1200 0000 1987 4263 7541"),
            [("SK31 1200 0000 1987 4263 7541", Kind::Iban)]
        );
    }

    #[test]
    fn words_that_merely_contain_a_label_are_not_labels() {
        assert!(found("Compass: north").is_empty());
        assert!(found("Spinning: yes").is_empty());
        assert!(found("Passed: 12, Pinned: 3, Author: Matus").is_empty());
    }

    #[test]
    fn card_numbers() {
        assert_eq!(
            found("Card 4111 1111 1111 1111 exp 09/29"),
            [("4111 1111 1111 1111", Kind::Card)]
        );
        assert_eq!(
            found("4111111111111111"),
            [("4111111111111111", Kind::Card)]
        );
        // Not Luhn and not grouped like a card: an order number.
        assert!(found("Order 1234567890123").is_empty());
    }

    #[test]
    fn ibans_are_checked() {
        assert_eq!(
            found("pay to SK31 1200 0000 1987 4263 7541 today"),
            [("SK31 1200 0000 1987 4263 7541", Kind::Iban)]
        );
        assert_eq!(
            found("GB82WEST12345698765432"),
            [("GB82WEST12345698765432", Kind::Iban)]
        );
        assert!(found("GB00WEST12345698765432").is_empty());
    }

    #[test]
    fn keys() {
        // Made-up keys, each split in two so that secret scanners reading
        // this file don't take them for real ones.
        for key in [
            concat!("gh", "p_R8x2Kq9LmZp4Tn7Vw1Yb3Cd5Ef6Gh0Ij2KlMn"),
            concat!("sk", "-ant-api03-Xk2Lm9Pq7Rs4Tu1Vw8Yz3Ab6Cd"),
            concat!("AK", "IAIOSFODNN7EXAMPLE"),
            concat!("AI", "zaSyD-9tSrke72PouQMnMX-a7eZSW0jkFMBWY"),
            concat!("xo", "xb-1234567890-abcdefghijklmnop"),
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
            "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW",
        ] {
            assert_eq!(found(key), [(key, Kind::Key)], "{key}");
        }
    }

    #[test]
    fn unreadable_key_bodies() {
        let body = "b3??bnNzaC1rZXk?d?EAAAAABG5vbmU?????bm9uZ0A???????AB???AMW?A??tzc2atZW";
        assert_eq!(found(body), [(body, Kind::Key)]);
    }

    #[test]
    fn onions_even_misread() {
        for onion in [
            "duckduckgogg42xjoc72x3sjasowoarfbgcmvfimaftt6twagswzczad.onion",
            "duckduckgo42xjoc72x3sjasowoarfbgcmvfimaftt?twagswzczad.onion",
        ] {
            assert_eq!(found(onion), [(onion, Kind::Onion)]);
        }
    }

    #[test]
    fn wallets() {
        assert_eq!(
            found("btc: bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq"),
            [("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq", Kind::Wallet)]
        );
        // As OCR read it: `bcl…`, so only its randomness gives it away.
        assert_eq!(
            found("otc: bclgarosrrr7xfkvy5643lydnw9re59gtzzwf5mdq"),
            [("bclgarosrrr7xfkvy5643lydnw9re59gtzzwf5mdq", Kind::Key)]
        );
        assert_eq!(
            found("0x52908400098527886E0F7030069857D2E4169EE7"),
            [("0x52908400098527886E0F7030069857D2E4169EE7", Kind::Wallet)]
        );
    }

    #[test]
    fn phones_in_international_form() {
        assert_eq!(
            found("Phone: +421 905 123 456"),
            [("+421 905 123 456", Kind::Phone)]
        );
        assert!(found("+1 2").is_empty());
    }

    #[test]
    fn public_addresses_only() {
        assert_eq!(
            found("ssh roote203.0.113.42 -p 22"),
            [("203.0.113.42", Kind::Address)]
        );
        assert!(found("gateway 192.168.1.1, dns 10.0.0.53, lo 127.0.0.1").is_empty());
        assert_eq!(
            found("addr 2001:db8:85a3::8a2e:370:7334/64"),
            [("2001:db8:85a3::8a2e:370:7334", Kind::Address)]
        );
        assert!(found("at 12:30:45 on 2026-10-29").is_empty());
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        for text in [
            "Weather today is sunny with light wind.",
            "build finished in 4.2s, 0 warnings",
            "Screenshot 2026-10-29 17-04.png",
            "/nix/store/0c9wbxfl3rq6vqh5lsh2ffimb8nbhq3j-derisk-0.1.0",
            "123e4567-e89b-12d3-a456-426614174000",
            "XDG_RUNTIME_DIR=/run/user/1000",
            "SomeLongIdentifierNameWithoutDigits",
            "HTTP2ConnectionManagerFactory",
            "/run/user/1000/derisk/screenshots/screen-12.png",
            "commit 3d018204a9eaf323cb96280846ffd62de5077d91",
            "derisk-redact v0.1.0 (crates/derisk-redact)",
            "version 1.2.3 released",
        ] {
            assert!(found(text).is_empty(), "{text}: {:?}", found(text));
        }
    }

    #[test]
    fn overlapping_matches_merge() {
        // The label and the prefixed key both match the same value.
        let text = "token: ghp_R8x2Kq9LmZp4Tn7Vw1Yb3Cd5Ef6Gh0Ij2KlMn";
        assert_eq!(find(text).len(), 1);
    }
}
