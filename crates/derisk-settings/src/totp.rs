//! Verification codes for logging in: TOTP (RFC 6238), the six digits an
//! authenticator app shows, in the file pam_google_authenticator reads.
//!
//! The secret lives in `~/.google_authenticator`, the format the module and
//! the `google-authenticator` tool share: the secret in base32 on the first
//! line, options as `" ` lines, then one-time recovery codes. It is in the
//! person's own home, so turning codes on or off needs no administrator;
//! the system decides whether logins ask for one at all (its PAM stack runs
//! the module with `nullok`, so someone without the file is not asked).

use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use derisk_sys::{
    fs,
    unix::{OpenOptionsExt, PermissionsExt},
};

use hmac::{Hmac, Mac};
use sha1::Sha1;

/// Seconds each code is valid for.
const STEP: u64 = 30;

/// How many recovery codes a new secret comes with.
pub const RECOVERY_CODES: usize = 5;

/// A TOTP secret: 160 random bits, as RFC 4226 recommends.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Secret(pub Vec<u8>);

impl Secret {
    /// A new secret from the kernel's random source.
    pub fn generate() -> io::Result<Self> {
        Ok(Self(random(20)?))
    }

    /// The secret in base32, as apps take it typed in and as the file
    /// stores it.
    pub fn base32(&self) -> String {
        base32(&self.0)
    }

    /// The six-digit code for the 30-second step that `unix_time` falls in.
    pub fn code_at(&self, unix_time: u64) -> u32 {
        hotp(&self.0, unix_time / STEP)
    }

    /// Whether `typed` is the code for `unix_time`, or for the step just
    /// before or after it, so a clock a little off or a code typed as it
    /// changed still counts.
    pub fn verify(&self, typed: &str, unix_time: u64) -> bool {
        let digits: String = typed.chars().filter(|c| !c.is_whitespace()).collect();
        if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        let Ok(typed) = digits.parse::<u32>() else {
            return false;
        };
        let step = unix_time / STEP;
        [step.saturating_sub(1), step, step + 1]
            .iter()
            .any(|&s| self.code_at(s * STEP) == typed)
    }

    /// The `otpauth://` link an app reads from the QR code.
    pub fn uri(&self, account: &str, issuer: &str) -> String {
        format!(
            "otpauth://totp/{issuer}:{account}?secret={}&issuer={issuer}&algorithm=SHA1&digits=6&period={STEP}",
            self.base32(),
            issuer = encode(issuer),
            account = encode(account),
        )
    }
}

/// RFC 4226's HOTP, truncated to six digits.
fn hotp(key: &[u8], counter: u64) -> u32 {
    let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("HMAC takes keys of any length");
    mac.update(&counter.to_be_bytes());
    let hash = mac.finalize().into_bytes();
    let offset = usize::from(hash[19] & 0x0f);
    let value = u32::from_be_bytes([
        hash[offset] & 0x7f,
        hash[offset + 1],
        hash[offset + 2],
        hash[offset + 3],
    ]);
    value % 1_000_000
}

/// RFC 4648 base32, without padding, as authenticator apps expect.
fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let mut buffer = 0u32;
    let mut bits = 0;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((buffer >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((buffer << (5 - bits)) & 31) as usize]));
    }
    out
}

/// Percent-encodes everything but unreserved characters, for the link.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn random(len: usize) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; len];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Eight-digit recovery codes, each good for one login instead of a code.
pub fn recovery_codes() -> io::Result<Vec<u32>> {
    let bytes = random(4 * RECOVERY_CODES)?;
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| 10_000_000 + u32::from_le_bytes(*c) % 90_000_000)
        .collect())
}

/// The file's text: the secret, the options the `google-authenticator`
/// tool writes for a time-based secret (three logins per 30 seconds, a code
/// used once only, a window of one step either side), and the recovery
/// codes.
pub fn file_text(secret: &Secret, recovery: &[u32]) -> String {
    let mut text = format!(
        "{}\n\" RATE_LIMIT 3 30\n\" WINDOW_SIZE 3\n\" DISALLOW_REUSE\n\" TOTP_AUTH\n",
        secret.base32()
    );
    for code in recovery {
        text.push_str(&format!("{code}\n"));
    }
    text
}

/// `~/.google_authenticator`.
pub fn file_path() -> Option<PathBuf> {
    derisk_sys::env::var_os("HOME").map(|home| Path::new(&home).join(".google_authenticator"))
}

/// Writes the file at `path`, readable by its owner only, as the module
/// insists. An old one is replaced: it is read-only, so it is removed
/// first.
pub fn write_file(path: &Path, text: &str) -> io::Result<()> {
    remove_file(path)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o400))
}

/// Removes the file at `path`; nothing there is fine.
pub fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238's SHA-1 test key.
    fn rfc() -> Secret {
        Secret(b"12345678901234567890".to_vec())
    }

    #[test]
    fn matches_rfc_6238() {
        // The RFC lists eight digits; six are their last six.
        for (time, code) in [
            (59, 287_082),
            (1_111_111_109, 81_804),
            (1_111_111_111, 50_471),
            (1_234_567_890, 5_924),
            (2_000_000_000, 279_037),
        ] {
            assert_eq!(rfc().code_at(time), code, "{time}");
        }
    }

    #[test]
    fn a_step_either_side_counts() {
        let secret = rfc();
        let code = format!("{:06}", secret.code_at(1_111_111_109));
        assert!(secret.verify(&code, 1_111_111_109));
        assert!(secret.verify(&code, 1_111_111_109 + 30));
        assert!(!secret.verify(&code, 1_111_111_109 + 90));
        assert!(secret.verify(&format!("{} {}", &code[..3], &code[3..]), 1_111_111_109));
        assert!(!secret.verify("12345", 0));
        assert!(!secret.verify("abcdef", 0));
    }

    #[test]
    fn base32_is_rfc_4648() {
        assert_eq!(base32(b""), "");
        assert_eq!(base32(b"f"), "MY");
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(rfc().base32(), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
    }

    #[test]
    fn the_link_names_the_account() {
        let uri = rfc().uri("ada", "LosOS on laptop");
        assert!(uri.starts_with("otpauth://totp/LosOS%20on%20laptop:ada?secret=GEZDG"));
        assert!(uri.ends_with("&issuer=LosOS%20on%20laptop&algorithm=SHA1&digits=6&period=30"));
    }

    #[test]
    fn the_file_is_what_the_module_reads() {
        let text = file_text(&rfc(), &[12_345_678, 87_654_321]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert!(lines.contains(&"\" TOTP_AUTH"));
        assert_eq!(&lines[lines.len() - 2..], ["12345678", "87654321"]);
    }

    #[test]
    fn the_file_is_owner_only_and_replaceable() {
        let dir = std::env::temp_dir().join(format!("derisk-totp-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".google_authenticator");
        write_file(&path, "A\n").unwrap();
        write_file(&path, "B\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "B\n");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o400);
        remove_file(&path).unwrap();
        remove_file(&path).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn recovery_codes_have_eight_digits() {
        let codes = recovery_codes().unwrap();
        assert_eq!(codes.len(), RECOVERY_CODES);
        assert!(codes.iter().all(|c| (10_000_000..100_000_000).contains(c)));
    }
}
