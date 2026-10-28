//! Encryption for Encrypted Note.
//!
//! - **AES-256-GCM** (via `aes-gcm`) encrypts note contents at rest with
//!   authenticated encryption.
//! - **Argon2id** (via `argon2`) derives the 256-bit key from the master
//!   password. We never store the password itself — only a random salt and a
//!   verifier (a small ciphertext) used to check the password on unlock.
//!
//! Encrypted note files are framed as:
//!
//! ```text
//! magic ("ENCD1", 5 bytes) || nonce (12 bytes) || AES-256-GCM ciphertext+tag
//! ```
//!
//! The per-vault master salt lives in the keyfile so that every file written
//! with the same password produces different ciphertext (each write uses a
//! fresh random nonce).

use std::path::PathBuf;

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use argon2::Argon2;
use serde::{Deserialize, Serialize};

/// File magic identifying an encrypted note.
const MAGIC: &[u8; 5] = b"ENCD1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// Plaintext sealed into the keyfile so we can verify a password on unlock.
const VERIFIER_PLAIN: &[u8] = b"encrypted-note::password-verifier::v1";

/// Minimum password length required by the app.
pub const MIN_PASSWORD_LEN: usize = 16;

/// Persisted keyfile: salt + password verifier. Contains no secret material
/// that would let an attacker decrypt notes without the password.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Keyfile {
    /// Format version.
    pub version: u32,
    /// Hex-encoded Argon2id salt.
    pub salt: String,
    /// Hex-encoded nonce for the verifier ciphertext.
    pub verifier_nonce: String,
    /// Hex-encoded verifier ciphertext.
    pub verifier_ct: String,
}

impl Keyfile {
    /// Path of the keyfile under the config directory.
    pub fn path() -> PathBuf {
        crate::config::dir().join("keyfile.json")
    }

    /// Load the keyfile, if present.
    pub fn load() -> Option<Keyfile> {
        let data = std::fs::read_to_string(Self::path()).ok()?;
        serde_json::from_str(&data).ok()
    }

    /// Whether a master password has been configured.
    pub fn exists() -> bool {
        Self::path().is_file()
    }

    /// Persist the keyfile to disk (mode 0600 on Unix).
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// Human-readable description of what is wrong with a candidate password.
    /// Returns an empty list when the password satisfies the policy.
    pub fn password_problems(password: &str) -> Vec<&'static str> {
        let mut problems = Vec::new();
        if password.chars().count() < MIN_PASSWORD_LEN {
            problems.push("must be at least 16 characters long");
        }
        if !password.chars().any(|c| c.is_ascii_digit()) {
            problems.push("must contain at least one number");
        }
        if !password.chars().any(|c| c.is_uppercase()) {
            problems.push("must contain at least one capital letter");
        }
        if !password.chars().any(|c| c.is_lowercase()) {
            problems.push("must contain at least one lower-case letter");
        }
        if !password.chars().any(|c| !c.is_alphanumeric()) {
            problems.push("must contain at least one special character");
        }
        // Reject runs of 4+ letters, which rules out meaningful words such as
        // "pass", "correct" or "horse" and pushes toward randomized input.
        if has_letter_run(password, 4) {
            problems.push("must not contain 4 letters in a row (no words)");
        }
        problems
    }

    /// Check whether a password satisfies the policy.
    #[cfg(test)]
    fn password_is_valid(password: &str) -> bool {
        Self::password_problems(password).is_empty()
    }
}

/// A derived AES-256 key plus the operations that use it. The key bytes are
/// zeroed on drop.
pub struct VaultCrypto {
    key: [u8; KEY_LEN],
}

impl Drop for VaultCrypto {
    fn drop(&mut self) {
        // Best-effort scrubbing; `write_volatile` prevents the compiler from
        // optimizing the wipe away.
        for byte in self.key.iter_mut() {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

impl VaultCrypto {
    /// Create a brand-new keyfile for `password`, returning the keyfile and a
    /// ready-to-use crypto context.
    pub fn create(password: &str) -> std::io::Result<(Keyfile, VaultCrypto)> {
        let salt = random_bytes::<SALT_LEN>();
        let key = derive_key(password, &salt);
        let crypto = VaultCrypto { key };

        let nonce = random_bytes::<NONCE_LEN>();
        let cipher = Aes256Gcm::new_from_slice(&key).expect("valid key size");
        let verifier_ct = cipher
            .encrypt(Nonce::from_slice(&nonce), VERIFIER_PLAIN)
            .expect("verifier encryption cannot fail");

        let keyfile = Keyfile {
            version: 1,
            salt: hex_encode(&salt),
            verifier_nonce: hex_encode(&nonce),
            verifier_ct: hex_encode(&verifier_ct),
        };
        Ok((keyfile, crypto))
    }

    /// Verify `password` against the keyfile, returning a crypto context on
    /// success.
    pub fn unlock(password: &str, keyfile: &Keyfile) -> Option<VaultCrypto> {
        let salt = hex_decode(&keyfile.salt)?;
        let nonce = hex_decode(&keyfile.verifier_nonce)?;
        let ct = hex_decode(&keyfile.verifier_ct)?;
        if nonce.len() != NONCE_LEN {
            return None;
        }
        let key = derive_key(password, &salt);
        let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
        let plaintext = cipher
            .decrypt(Nonce::from_slice(&nonce), ct.as_slice())
            .ok()?;
        if plaintext.as_slice() == VERIFIER_PLAIN {
            Some(VaultCrypto { key })
        } else {
            None
        }
    }

    /// Encrypt UTF-8 text into a framed byte buffer.
    pub fn encrypt(&self, plaintext: &str) -> Vec<u8> {
        let nonce = random_bytes::<NONCE_LEN>();
        let cipher = Aes256Gcm::new_from_slice(&self.key).expect("valid key size");
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
            .expect("encryption cannot fail");
        let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + ciphertext.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        out
    }

    /// Decrypt a framed buffer back into UTF-8 text.
    pub fn decrypt(&self, data: &[u8]) -> Option<String> {
        if data.len() < MAGIC.len() + NONCE_LEN || &data[..MAGIC.len()] != MAGIC {
            return None;
        }
        let nonce = &data[MAGIC.len()..MAGIC.len() + NONCE_LEN];
        let ciphertext = &data[MAGIC.len() + NONCE_LEN..];
        let cipher = Aes256Gcm::new_from_slice(&self.key).ok()?;
        let plaintext = cipher.decrypt(Nonce::from_slice(nonce), ciphertext).ok()?;
        String::from_utf8(plaintext).ok()
    }
}

/// Whether a byte buffer looks like an encrypted note (has our magic header).
pub fn is_encrypted(data: &[u8]) -> bool {
    data.len() >= MAGIC.len() && &data[..MAGIC.len()] == MAGIC
}

/// Magic identifying a password-encrypted export (distinct from note files).
const EXPORT_MAGIC: &[u8; 5] = b"ENCP1";

/// Encrypt `plaintext` under an arbitrary `password`, embedding a fresh salt so
/// the result is self-contained and can be decrypted on another machine:
///
/// ```text
/// magic ("ENCP1", 5) || salt (16) || nonce (12) || AES-256-GCM ciphertext+tag
/// ```
pub fn encrypt_with_password(password: &str, plaintext: &[u8]) -> Vec<u8> {
    let salt = random_bytes::<SALT_LEN>();
    let key = derive_key(password, &salt);
    let nonce = random_bytes::<NONCE_LEN>();
    let cipher = Aes256Gcm::new_from_slice(&key).expect("valid key size");
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .expect("encryption cannot fail");
    let mut out = Vec::with_capacity(EXPORT_MAGIC.len() + SALT_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(EXPORT_MAGIC);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    out
}

/// Whether a buffer looks like a password-encrypted export.
pub fn is_password_encrypted(data: &[u8]) -> bool {
    data.len() >= EXPORT_MAGIC.len() && &data[..EXPORT_MAGIC.len()] == EXPORT_MAGIC
}

/// Decrypt a blob produced by [`encrypt_with_password`], or `None` on a wrong
/// password or tampering.
pub fn decrypt_with_password(password: &str, data: &[u8]) -> Option<Vec<u8>> {
    if !is_password_encrypted(data) {
        return None;
    }
    let body = &data[EXPORT_MAGIC.len()..];
    if body.len() < SALT_LEN + NONCE_LEN {
        return None;
    }
    let salt = &body[..SALT_LEN];
    let nonce = &body[SALT_LEN..SALT_LEN + NONCE_LEN];
    let ciphertext = &body[SALT_LEN + NONCE_LEN..];
    let key = derive_key(password, salt);
    let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
    cipher.decrypt(Nonce::from_slice(nonce), ciphertext).ok()
}

/// Derive a 256-bit key from a password using Argon2id with default parameters
/// (Argon2id v19, 19 MiB memory, 2 iterations, 1 lane).
fn derive_key(password: &str, salt: &[u8]) -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .expect("Argon2 key derivation cannot fail for a 32-byte output");
    key
}

/// Fill an array with cryptographically secure random bytes.
fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).expect("OS randomness must be available");
    buf
}

/// Whether the string contains `n` or more alphabetic characters in a row.
pub fn has_letter_run(s: &str, n: usize) -> bool {
    let mut run = 0usize;
    for c in s.chars() {
        if c.is_alphabetic() {
            run += 1;
            if run >= n {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Character sets used by the password generator (confusable glyphs omitted).
const GEN_UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
const GEN_LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
const GEN_DIGITS: &[u8] = b"23456789";
const GEN_SPECIAL: &[u8] = b"!@#$%^&*?-_=+";

fn rand_below(n: u32) -> u32 {
    random_below(n)
}

/// A uniformly random `u32` in `0..n` (rejection sampling, so no modulo bias —
/// important for password generation).
pub fn random_below(n: u32) -> u32 {
    if n <= 1 {
        return 0;
    }
    let n = n as u64;
    let total = 1u64 << 32;
    let limit = total - (total % n); // largest multiple of n that fits in 2^32
    loop {
        let x = u32::from_le_bytes(random_bytes::<4>()) as u64;
        if x < limit {
            return (x % n) as u32;
        }
    }
}

fn pick(set: &[u8]) -> char {
    set[rand_below(set.len() as u32) as usize] as char
}

/// If no character satisfies `pred`, replace a character of the right kind.
fn ensure_category(chars: &mut [char], want_letter: bool, pred: impl Fn(char) -> bool, set: &[u8]) {
    if chars.iter().any(|&c| pred(c)) {
        return;
    }
    for c in chars.iter_mut() {
        if c.is_ascii_alphabetic() == want_letter {
            *c = pick(set);
            return;
        }
    }
}

/// Generate a random password that satisfies [`Keyfile::password_problems`].
///
/// The generator never emits more than two letters in a row, so the result can
/// never trip the "no 4 letters in a row" rule, and it guarantees that all four
/// character classes are present.
pub fn generate_password() -> String {
    const LEN: usize = 20;
    let mut chars: Vec<char> = Vec::with_capacity(LEN);
    let mut run = 0u32;
    while chars.len() < LEN {
        // Take a letter at most twice in a row, and only ~3/4 of the time.
        let letter = run < 2 && rand_below(4) != 0;
        if letter {
            chars.push(if rand_below(2) == 0 {
                pick(GEN_UPPER)
            } else {
                pick(GEN_LOWER)
            });
            run += 1;
        } else {
            chars.push(if rand_below(2) == 0 {
                pick(GEN_DIGITS)
            } else {
                pick(GEN_SPECIAL)
            });
            run = 0;
        }
    }
    ensure_category(&mut chars, true, |c| c.is_ascii_uppercase(), GEN_UPPER);
    ensure_category(&mut chars, true, |c| c.is_ascii_lowercase(), GEN_LOWER);
    ensure_category(&mut chars, false, |c| c.is_ascii_digit(), GEN_DIGITS);
    ensure_category(&mut chars, false, |c| !c.is_alphanumeric(), GEN_SPECIAL);
    chars.into_iter().collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_val(bytes[i])?;
        let lo = hex_val(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Some(out)
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let (_, crypto) = VaultCrypto::create("CorrectHorse9!Battery").unwrap();
        let secret = "note body with unicode ✓ and [[links]]";
        let blob = crypto.encrypt(secret);
        assert!(is_encrypted(&blob));
        assert_eq!(crypto.decrypt(&blob).as_deref(), Some(secret));
    }

    #[test]
    fn ciphertext_is_not_plaintext() {
        let (_, crypto) = VaultCrypto::create("CorrectHorse9!Battery").unwrap();
        let blob = crypto.encrypt("top secret");
        assert!(!blob.windows(10).any(|w| w == b"top secret"));
    }

    #[test]
    fn each_encryption_uses_a_fresh_nonce() {
        let (_, crypto) = VaultCrypto::create("CorrectHorse9!Battery").unwrap();
        let a = crypto.encrypt("same");
        let b = crypto.encrypt("same");
        assert_ne!(a, b, "nonces must differ between writes");
    }

    #[test]
    fn unlock_accepts_correct_and_rejects_wrong_password() {
        let (keyfile, _) = VaultCrypto::create("CorrectHorse9!Battery").unwrap();
        assert!(VaultCrypto::unlock("CorrectHorse9!Battery", &keyfile).is_some());
        assert!(VaultCrypto::unlock("wrong-password-000", &keyfile).is_none());
    }

    #[test]
    fn wrong_key_cannot_decrypt() {
        let (_, crypto_a) = VaultCrypto::create("CorrectHorse9!Battery").unwrap();
        let (_, crypto_b) = VaultCrypto::create("AnotherPass123!xyz").unwrap();
        let blob = crypto_a.encrypt("secret");
        assert!(crypto_b.decrypt(&blob).is_none());
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let (_, crypto) = VaultCrypto::create("CorrectHorse9!Battery").unwrap();
        let mut blob = crypto.encrypt("secret");
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        assert!(crypto.decrypt(&blob).is_none());
    }

    #[test]
    fn password_policy_enforced() {
        assert!(!Keyfile::password_is_valid("short1A!")); // too short
        assert!(!Keyfile::password_is_valid("NoNumbersHere!!!!")); // no digit
        assert!(!Keyfile::password_is_valid("noncapital123456!")); // no capital
        assert!(!Keyfile::password_is_valid("NOLOWERCASE123!ABC")); // no lower-case
        assert!(!Keyfile::password_is_valid("NoSpecialChar12345")); // no special
        assert!(Keyfile::password_is_valid("aB3!cD4!eF5!gH6!")); // valid, randomized
    }

    #[test]
    fn policy_rejects_words_and_letter_runs() {
        // Any run of 4+ letters means a word could be hiding in there.
        assert!(!Keyfile::password_is_valid("Passw0rd!Passw0rd!"));
        assert!(!Keyfile::password_is_valid("CorrectHorse9!Battery"));
        assert!(!Keyfile::password_is_valid("abcD1234!efgh5678"));
        // Three letters is fine.
        assert!(Keyfile::password_is_valid("aBc3!dEf4!gHi5!jK"));
    }

    #[test]
    fn generated_passwords_satisfy_policy() {
        for _ in 0..200 {
            let pw = generate_password();
            assert!(
                Keyfile::password_is_valid(&pw),
                "generated password failed policy: {pw}"
            );
            assert_eq!(pw.chars().count(), 20);
        }
    }

    #[test]
    fn plaintext_is_not_mistaken_for_encrypted() {
        assert!(!is_encrypted(b"# just a normal note"));
        assert!(!is_encrypted(b""));
    }

    #[test]
    fn hex_roundtrip() {
        let data = [0u8, 1, 15, 16, 254, 255];
        assert_eq!(hex_decode(&hex_encode(&data)).unwrap(), data);
    }

    #[test]
    fn random_below_is_in_range() {
        assert_eq!(random_below(0), 0);
        assert_eq!(random_below(1), 0);
        for _ in 0..1000 {
            let v = random_below(7);
            assert!(v < 7);
        }
        // Large n that does not divide 2^32 still stays in range.
        for _ in 0..1000 {
            assert!(random_below(1_000_003) < 1_000_003);
        }
    }

    #[test]
    fn password_export_roundtrips() {
        let secret = b"{\"entries\":[]}";
        let blob = encrypt_with_password("CorrectHorse9!Battery", secret);
        assert!(is_password_encrypted(&blob));
        assert!(!is_encrypted(&blob), "export magic differs from note magic");
        assert_eq!(
            decrypt_with_password("CorrectHorse9!Battery", &blob).as_deref(),
            Some(&secret[..])
        );
    }

    #[test]
    fn password_export_rejects_wrong_password_and_uses_fresh_salt() {
        let a = encrypt_with_password("CorrectHorse9!Battery", b"hi");
        let b = encrypt_with_password("CorrectHorse9!Battery", b"hi");
        assert_ne!(a, b, "a fresh salt must be embedded each time");
        assert!(decrypt_with_password("WrongPassword123!", &a).is_none());
    }

    #[test]
    fn note_file_is_encrypted_on_disk_and_roundtrips() {
        let (keyfile, crypto) = VaultCrypto::create("CorrectHorse9!Battery").unwrap();
        let dir = std::env::temp_dir().join(format!("encd-atrest-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("note.md");
        let secret = "# Secret\n\nmy private thoughts";

        std::fs::write(&path, crypto.encrypt(secret)).unwrap();
        let raw = std::fs::read(&path).unwrap();

        // The plaintext must not be recoverable from the raw file.
        assert!(!raw.windows(secret.len()).any(|w| w == secret.as_bytes()));
        assert!(is_encrypted(&raw));
        // And the correct key decrypts it back.
        assert_eq!(crypto.decrypt(&raw).as_deref(), Some(secret));
        // A wrong password can neither unlock nor decrypt.
        assert!(VaultCrypto::unlock("WrongPassword123!", &keyfile).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
