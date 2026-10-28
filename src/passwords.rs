//! The built-in password manager: encrypted credential entries, a password
//! generator, a strength meter, and TOTP (2FA) code generation.
//!
//! Entries are just data here; the app encrypts the whole [`Store`] with the
//! vault key (AES-256-GCM) before it touches disk, exactly like notes and
//! canvases. TOTP is implemented from scratch (HMAC-SHA1 + RFC 4648 base32) so
//! the app keeps its small dependency footprint.

use serde::{Deserialize, Serialize};

/// One stored credential.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Entry {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub favorite: bool,
    /// Base32 TOTP secret for two-factor codes (optional).
    #[serde(default)]
    pub totp: String,
    /// A short glyph (typically an emoji) shown as the entry's icon.
    #[serde(default)]
    pub icon: String,
    /// A favourite-icon URL; when set, its image is fetched and shown as the icon.
    #[serde(default)]
    pub favicon: String,
    /// Unix seconds when the entry was created / last updated.
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub updated: u64,
}

/// The whole password database.
#[derive(Serialize, Deserialize, Default)]
pub struct Store {
    #[serde(default)]
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub next_id: u64,
    /// Folders the user created, even when empty.
    #[serde(default)]
    pub folders: Vec<String>,
}

impl Store {
    /// Insert a new entry, assigning it a fresh id.
    pub fn add(&mut self, mut entry: Entry) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        entry.id = id;
        self.entries.push(entry);
        id
    }

    /// Remove an entry by id.
    pub fn remove(&mut self, id: u64) {
        self.entries.retain(|e| e.id != id);
    }

    pub fn get(&self, id: u64) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    /// All folders: the ones created explicitly, plus any used by entries.
    pub fn all_folders(&self) -> Vec<String> {
        let mut folders: Vec<String> = self.folders.clone();
        for e in &self.entries {
            let c = e.category.trim();
            if !c.is_empty() {
                folders.push(c.to_string());
            }
        }
        folders.sort();
        folders.dedup();
        folders
    }

    /// Whether a folder with this (trimmed) name exists.
    pub fn folder_exists(&self, name: &str) -> bool {
        let name = name.trim();
        self.all_folders().iter().any(|f| f == name)
    }

    /// Create a folder if it does not already exist. Returns whether it was new.
    pub fn create_folder(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || self.folder_exists(name) {
            return false;
        }
        self.folders.push(name.to_string());
        self.folders.sort();
        true
    }

    /// Rename a folder (and move its entries with it). Returns whether it changed.
    pub fn rename_folder(&mut self, old: &str, new: &str) -> bool {
        let old = old.trim();
        let new = new.trim();
        if old.is_empty() || new.is_empty() || old == new || self.folder_exists(new) {
            return false;
        }
        for f in self.folders.iter_mut() {
            if f == old {
                *f = new.to_string();
            }
        }
        for e in self.entries.iter_mut() {
            if e.category.trim() == old {
                e.category = new.to_string();
            }
        }
        self.folders.sort();
        true
    }

    /// Delete a folder, moving its entries back to Uncategorized.
    pub fn delete_folder(&mut self, name: &str) {
        let name = name.trim();
        self.folders.retain(|f| f != name);
        for e in self.entries.iter_mut() {
            if e.category.trim() == name {
                e.category.clear();
            }
        }
    }

    /// Entry ids matching a search query, category filter, and favorites flag.
    /// Favorites sort first, then titles alphabetically.
    pub fn filtered(&self, query: &str, category: Option<&str>, fav_only: bool) -> Vec<u64> {
        let q = query.trim().to_ascii_lowercase();
        let mut hits: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| !fav_only || e.favorite)
            .filter(|e| match category {
                Some("") => e.category.trim().is_empty(),
                Some(c) => e.category == c,
                None => true,
            })
            .filter(|e| {
                if q.is_empty() {
                    return true;
                }
                [&e.title, &e.username, &e.url, &e.notes, &e.category]
                    .iter()
                    .any(|f| f.to_ascii_lowercase().contains(&q))
            })
            .collect();
        hits.sort_by(|a, b| {
            b.favorite.cmp(&a.favorite).then_with(|| {
                a.title
                    .to_ascii_lowercase()
                    .cmp(&b.title.to_ascii_lowercase())
            })
        });
        hits.into_iter().map(|e| e.id).collect()
    }

    /// How many entries share each password (used by the "reused" health check).
    pub fn weakest(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| !e.password.is_empty() && strength(&e.password).0 <= 1)
            .count() as u64
    }
}

/// Path of the encrypted password store under the config directory.
pub fn store_path() -> std::path::PathBuf {
    crate::config::dir().join("passwords.enc")
}

// -------------------------------------------------------- import / export

/// Parse an imported document into entries. Accepts our own store format, a bare
/// JSON array of entries, a `{ "Title": "secret" }` map (TOTP secrets or
/// passwords), or a single `otpauth://` URI.
pub fn parse_import(json: &str) -> Result<Vec<Entry>, String> {
    let trimmed = json.trim();
    if trimmed.starts_with("otpauth://") {
        return parse_otpauth(trimmed)
            .map(|e| vec![e])
            .ok_or_else(|| "invalid otpauth:// URI".to_string());
    }
    let value: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| format!("not valid JSON: {e}"))?;
    if let Some(entries) = value.get("entries").and_then(|v| v.as_array()) {
        // Our own store format (or anything with an "entries" array).
        let out: Vec<Entry> = entries.iter().map(entry_from_value).collect();
        return if out.is_empty() {
            Err("no entries found".into())
        } else {
            Ok(out)
        };
    }
    match value {
        serde_json::Value::Array(items) => {
            let out: Vec<Entry> = items.iter().map(entry_from_value).collect();
            if out.is_empty() {
                Err("no entries found".into())
            } else {
                Ok(out)
            }
        }
        serde_json::Value::Object(map) => {
            let mut out = Vec::new();
            for (key, val) in map {
                let mut e = entry_from_value(&val);
                if e.title.trim().is_empty() {
                    e.title = key;
                }
                out.push(e);
            }
            if out.is_empty() {
                Err("no entries found".into())
            } else {
                Ok(out)
            }
        }
        _ => Err("unrecognised JSON (expected an object or array)".into()),
    }
}

fn first_nonempty(items: &[String]) -> String {
    items
        .iter()
        .find(|s| !s.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

/// Turn one JSON value into an entry, tolerating many common field names. A bare
/// string is treated as a TOTP secret when it looks like base32, else a password.
fn entry_from_value(v: &serde_json::Value) -> Entry {
    if let Some(s) = v.as_str() {
        let s = s.trim();
        if s.starts_with("otpauth://") {
            return parse_otpauth(s).unwrap_or_default();
        }
        let mut e = Entry::default();
        if is_base32_secret(s) {
            e.totp = s.to_ascii_uppercase().replace(' ', "");
        } else {
            e.password = s.to_string();
        }
        return e;
    }
    let get = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    // An explicit otpauth URI takes priority for TOTP.
    let uri = first_nonempty(&[get("otpauth"), get("otp_url"), get("uri")]);
    if uri.starts_with("otpauth://") {
        if let Some(mut o) = parse_otpauth(&uri) {
            let title = first_nonempty(&[get("title"), get("name"), get("label")]);
            if !title.is_empty() {
                o.title = title;
            }
            return o;
        }
    }
    let secret = get("secret");
    let mut e = Entry {
        title: first_nonempty(&[get("title"), get("name"), get("label"), get("service")]),
        username: first_nonempty(&[
            get("username"),
            get("user"),
            get("login"),
            get("email"),
            get("account"),
        ]),
        password: first_nonempty(&[get("password"), get("pass")]),
        url: first_nonempty(&[get("url"), get("website"), get("site")]),
        notes: get("notes"),
        category: first_nonempty(&[get("category"), get("folder"), get("group"), get("tags")]),
        ..Default::default()
    };
    let totp = first_nonempty(&[get("totp"), get("otp"), get("totp_secret")]);
    if !totp.is_empty() {
        e.totp = normalize_totp(&totp);
    } else if e.password.is_empty() && is_base32_secret(&secret) {
        // A base32 `secret` with no password is a TOTP secret.
        e.totp = secret.to_ascii_uppercase().replace(' ', "");
    } else if e.password.is_empty() {
        e.password = secret;
    }
    e
}

/// Trim/uppercase a TOTP secret, extracting it from an `otpauth://` URI if given.
fn normalize_totp(s: &str) -> String {
    let s = s.trim();
    if s.starts_with("otpauth://") {
        return parse_otpauth(s).map(|e| e.totp).unwrap_or_default();
    }
    s.to_ascii_uppercase().replace(' ', "")
}

/// Whether `s` looks like a base32 secret (RFC 4648 alphabet, plausible length).
fn is_base32_secret(s: &str) -> bool {
    let s = s.trim().trim_end_matches('=');
    (16..=128).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || ('2'..='7').contains(&c))
}

/// Parse a minimal `otpauth://totp/LABEL?secret=…&issuer=…` URI.
pub fn parse_otpauth(uri: &str) -> Option<Entry> {
    let rest = uri.trim().strip_prefix("otpauth://")?;
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, q),
        None => (rest, ""),
    };
    let label = path.split_once('/').map(|(_, l)| l).unwrap_or(path);
    let label = percent_decode(label);
    let mut secret = String::new();
    let mut issuer = String::new();
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let v = percent_decode(v);
        match k {
            "secret" => secret = v,
            "issuer" => issuer = v,
            _ => {}
        }
    }
    if secret.is_empty() {
        return None;
    }
    let (title, username) = match label.split_once(':') {
        Some((iss, acct)) => (
            if issuer.trim().is_empty() {
                iss.trim().to_string()
            } else {
                issuer.clone()
            },
            acct.trim().to_string(),
        ),
        None => (
            if issuer.trim().is_empty() {
                label.trim().to_string()
            } else {
                issuer.clone()
            },
            String::new(),
        ),
    };
    Some(Entry {
        title,
        username,
        totp: secret.to_ascii_uppercase().replace(' ', ""),
        ..Default::default()
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match (hexval(bytes[i + 1]), hexval(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push((h << 4) | l);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Current Unix time in seconds.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Format a Unix timestamp as `YYYY-MM-DD`.
pub fn fmt_day(secs: u64) -> String {
    let (y, m, d) = crate::dates::civil_from_days((secs / 86_400) as i64);
    crate::dates::format("YYYY-MM-DD", y, m, d)
}

// ----------------------------------------------------------- strength meter

/// Rough strength score (0–4) and a label.
pub fn strength(password: &str) -> (u8, &'static str) {
    if password.is_empty() {
        return (0, "Empty");
    }
    let len = password.chars().count();
    let classes = [
        password.chars().any(|c| c.is_ascii_uppercase()),
        password.chars().any(|c| c.is_ascii_lowercase()),
        password.chars().any(|c| c.is_ascii_digit()),
        password.chars().any(|c| !c.is_alphanumeric()),
    ]
    .iter()
    .filter(|&&b| b)
    .count();

    let mut score = 0u8;
    if len >= 8 {
        score += 1;
    }
    if len >= 12 {
        score += 1;
    }
    if len >= 16 {
        score += 1;
    }
    if classes >= 2 {
        score += 1;
    }
    if classes >= 3 {
        score += 1;
    }
    if classes >= 4 {
        score += 1;
    }
    // A password built from a single character class never rates highly,
    // however long it is.
    if classes <= 1 {
        score = score.min(1);
    }
    let score = score.min(4);
    let label = match score {
        0 | 1 => "Weak",
        2 => "Fair",
        3 => "Good",
        _ => "Strong",
    };
    (score, label)
}

// ------------------------------------------------------------- generator

const GEN_UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
const GEN_LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
const GEN_DIGITS: &[u8] = b"23456789";
const GEN_SPECIAL: &[u8] = b"!@#$%^&*?-_=+";

fn pick(set: &[u8]) -> char {
    set[crate::crypto::random_below(set.len() as u32) as usize] as char
}

/// Generate a random password of `len` characters drawn from the selected
/// character classes (at least one from each selected class). Falls back to
/// lower-case if no class is selected.
pub fn generate(len: usize, upper: bool, lower: bool, digits: bool, special: bool) -> String {
    let mut sets: Vec<&[u8]> = Vec::new();
    if upper {
        sets.push(GEN_UPPER);
    }
    if lower {
        sets.push(GEN_LOWER);
    }
    if digits {
        sets.push(GEN_DIGITS);
    }
    if special {
        sets.push(GEN_SPECIAL);
    }
    if sets.is_empty() {
        sets.push(GEN_LOWER);
    }
    let len = len.clamp(4, 128);
    let mut chars: Vec<char> = sets.iter().map(|s| pick(s)).collect();
    while chars.len() < len {
        let set = sets[crate::crypto::random_below(sets.len() as u32) as usize];
        chars.push(pick(set));
    }
    // Fisher–Yates shuffle so the guaranteed characters aren't always first.
    for i in (1..chars.len()).rev() {
        let j = crate::crypto::random_below((i + 1) as u32) as usize;
        chars.swap(i, j);
    }
    chars.into_iter().collect()
}

/// Estimate the entropy of a password in bits, from its length and the size of
/// the character pool implied by the classes it uses.
pub fn entropy_bits(password: &str) -> f64 {
    if password.is_empty() {
        return 0.0;
    }
    let mut pool = 0u32;
    if password.chars().any(|c| c.is_ascii_lowercase()) {
        pool += 26;
    }
    if password.chars().any(|c| c.is_ascii_uppercase()) {
        pool += 26;
    }
    if password.chars().any(|c| c.is_ascii_digit()) {
        pool += 10;
    }
    if password.chars().any(|c| !c.is_alphanumeric()) {
        pool += 33;
    }
    if pool == 0 {
        pool = 1;
    }
    let len = password.chars().count() as f64;
    (len * (pool as f64).log2()).round()
}

/// Word lists for the random-username generator.
const USER_ADJ: &[&str] = &[
    "brave", "calm", "swift", "lunar", "amber", "vivid", "quiet", "noble", "cosmic", "rusty",
    "misty", "sunny", "witty", "zesty", "brisk", "gentle", "clever", "silent", "rapid", "frosty",
    "golden", "crimson", "mellow", "shadow",
];
const USER_NOUN: &[&str] = &[
    "otter", "falcon", "cedar", "comet", "harbor", "pixel", "ember", "willow", "raven", "maple",
    "tiger", "rook", "lynx", "koala", "heron", "onyx", "quartz", "zephyr", "badger", "orchid",
    "puma", "finch", "walrus", "gecko",
];

/// Generate a friendly random username such as `brave_otter42`.
pub fn generate_username() -> String {
    let adj = USER_ADJ[crate::crypto::random_below(USER_ADJ.len() as u32) as usize];
    let noun = USER_NOUN[crate::crypto::random_below(USER_NOUN.len() as u32) as usize];
    let num = crate::crypto::random_below(100);
    format!("{adj}_{noun}{num:02}")
}

// ------------------------------------------------------------------- TOTP

/// Decode an RFC 4648 base32 string (padding and spaces ignored).
pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer: u64 = 0;
    let mut bits = 0u32;
    for c in s.chars() {
        if c == '=' || c.is_whitespace() {
            continue;
        }
        let up = c.to_ascii_uppercase();
        let value = match up {
            'A'..='Z' => up as u8 - b'A',
            '2'..='7' => up as u8 - b'2' + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | value as u64;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

/// The 6-digit TOTP code for `secret` (base32) at `unix_time`.
pub fn totp(secret: &str, unix_time: u64) -> Option<String> {
    totp_digits(secret, unix_time, 6)
}

/// The TOTP code with a chosen number of digits (for tests / generators).
pub fn totp_digits(secret: &str, unix_time: u64, digits: u32) -> Option<String> {
    let key = base32_decode(secret)?;
    if key.is_empty() {
        return None;
    }
    let counter = unix_time / 30;
    let mac = hmac_sha1(&key, &counter.to_be_bytes());
    let offset = (mac[19] & 0x0f) as usize;
    let bin = ((mac[offset] as u32 & 0x7f) << 24)
        | ((mac[offset + 1] as u32) << 16)
        | ((mac[offset + 2] as u32) << 8)
        | (mac[offset + 3] as u32);
    let code = bin % 10u32.pow(digits.min(9));
    Some(format!("{code:0width$}", width = digits as usize))
}

/// Seconds remaining in the current 30-second TOTP window.
pub fn totp_remaining(unix_time: u64) -> u64 {
    30 - (unix_time % 30)
}

/// SHA-1 (RFC 3174).
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let ml = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());

    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// HMAC-SHA1 (RFC 2104).
fn hmac_sha1(key: &[u8], msg: &[u8]) -> [u8; 20] {
    let mut k = if key.len() > 64 {
        sha1(key).to_vec()
    } else {
        key.to_vec()
    };
    k.resize(64, 0);
    let mut ipad = vec![0x36u8; 64];
    let mut opad = vec![0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    ipad.extend_from_slice(msg);
    let inner = sha1(&ipad);
    opad.extend_from_slice(&inner);
    sha1(&opad)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn sha1_known_vectors() {
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(&sha1(b"The quick brown fox jumps over the lazy dog")),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
    }

    #[test]
    fn hmac_sha1_known_vector() {
        let key = [0x0bu8; 20];
        assert_eq!(
            hex(&hmac_sha1(&key, b"Hi There")),
            "b617318655057264e28bc0b6fb378c8ef146be00"
        );
    }

    #[test]
    fn base32_decodes_rfc_secret() {
        let decoded = base32_decode("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ").unwrap();
        assert_eq!(decoded, b"12345678901234567890");
    }

    #[test]
    fn totp_matches_rfc6238_vectors() {
        // RFC 6238 test secret "12345678901234567890" in base32.
        let secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
        assert_eq!(totp_digits(secret, 59, 8).as_deref(), Some("94287082"));
        assert_eq!(
            totp_digits(secret, 1_111_111_109, 8).as_deref(),
            Some("07081804")
        );
        assert_eq!(
            totp_digits(secret, 1_234_567_890, 8).as_deref(),
            Some("89005924")
        );
        assert_eq!(
            totp_digits(secret, 2_000_000_000, 8).as_deref(),
            Some("69279037")
        );
        // Six-digit variant.
        assert_eq!(totp(secret, 59).as_deref(), Some("287082"));
    }

    #[test]
    fn totp_window_countdown() {
        assert_eq!(totp_remaining(0), 30);
        assert_eq!(totp_remaining(29), 1);
        assert_eq!(totp_remaining(30), 30);
    }

    #[test]
    fn strength_scales_with_length_and_variety() {
        assert_eq!(strength("").1, "Empty");
        assert_eq!(strength("abc").0, 0);
        assert!(strength("aB3!cD4!eF5!gH6!").0 >= 4); // long + all classes
        assert!(strength("aaaaaaaaaaaaaaaa").0 <= 1); // long but one class
    }

    #[test]
    fn generator_respects_selected_classes_and_length() {
        let pw = generate(24, true, true, true, true);
        assert_eq!(pw.chars().count(), 24);
        assert!(pw.chars().any(|c| c.is_ascii_uppercase()));
        assert!(pw.chars().any(|c| c.is_ascii_lowercase()));
        assert!(pw.chars().any(|c| c.is_ascii_digit()));
        assert!(pw.chars().any(|c| !c.is_alphanumeric()));

        let digits_only = generate(10, false, false, true, false);
        assert_eq!(digits_only.chars().count(), 10);
        assert!(digits_only.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn store_crud_and_filtering() {
        let mut store = Store::default();
        let a = store.add(Entry {
            title: "GitHub".into(),
            username: "alice".into(),
            category: "Work".into(),
            favorite: true,
            ..Default::default()
        });
        let b = store.add(Entry {
            title: "Reddit".into(),
            username: "bob".into(),
            category: "Social".into(),
            ..Default::default()
        });
        assert_eq!(store.next_id, 2);
        assert_eq!(store.all_folders(), vec!["Social", "Work"]);

        // Search matches username and title, case-insensitively.
        assert_eq!(store.filtered("github", None, false), vec![a]);
        assert_eq!(store.filtered("bob", None, false), vec![b]);
        // Category filter.
        assert_eq!(store.filtered("", Some("Work"), false), vec![a]);
        // Favorites-only, and favorites sort first.
        assert_eq!(store.filtered("", None, true), vec![a]);
        assert_eq!(store.filtered("", None, false), vec![a, b]);

        store.remove(a);
        assert!(store.get(a).is_none());
        assert_eq!(store.entries.len(), 1);
    }

    #[test]
    fn fmt_day_formats_epoch() {
        assert_eq!(fmt_day(0), "1970-01-01");
        // 2024-01-05 00:00:00 UTC
        assert_eq!(fmt_day(1_704_412_800), "2024-01-05");
    }

    #[test]
    fn entropy_grows_with_length_and_variety() {
        assert_eq!(entropy_bits(""), 0.0);
        // 10 lowercase chars: 10 * log2(26) = 47 bits.
        assert_eq!(entropy_bits("abcdefghij"), 47.0);
        // Adding classes raises the pool, so entropy per character rises.
        assert!(entropy_bits("aB3!aB3!aB3!") > entropy_bits("abcdefghijkl"));
    }

    #[test]
    fn usernames_look_reasonable_and_vary() {
        let a = generate_username();
        let b = generate_username();
        assert!(a.contains('_'));
        assert!(a.len() >= 8);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        // Two draws are overwhelmingly unlikely to collide.
        assert_ne!(a, b);
    }

    #[test]
    fn store_roundtrips_through_json() {
        let mut store = Store::default();
        store.add(Entry {
            title: "X".into(),
            password: "s3cr3t".into(),
            totp: "GEZDGNBVGY3TQOJQ".into(),
            ..Default::default()
        });
        let json = serde_json::to_string(&store).unwrap();
        let back: Store = serde_json::from_str(&json).unwrap();
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].password, "s3cr3t");
        assert_eq!(back.entries[0].totp, "GEZDGNBVGY3TQOJQ");
    }

    #[test]
    fn imports_our_own_store_format() {
        let mut store = Store::default();
        store.add(Entry {
            title: "GitHub".into(),
            username: "alice".into(),
            password: "pw".into(),
            ..Default::default()
        });
        let json = serde_json::to_string(&store).unwrap();
        let entries = parse_import(&json).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "GitHub");
        assert_eq!(entries[0].password, "pw");
    }

    #[test]
    fn imports_a_totp_secret_map() {
        let json = r#"{ "GitHub": "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ" }"#;
        let entries = parse_import(json).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "GitHub");
        assert_eq!(entries[0].totp, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert!(entries[0].password.is_empty());
    }

    #[test]
    fn imports_an_array_with_common_field_names() {
        let json = r#"[
            { "name": "Mail", "login": "bob", "pass": "s3cret", "website": "https://mail" },
            { "service": "Bank", "account": "alice", "secret": "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ" }
        ]"#;
        let entries = parse_import(json).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].title, "Mail");
        assert_eq!(entries[0].username, "bob");
        assert_eq!(entries[0].password, "s3cret");
        assert_eq!(entries[0].url, "https://mail");
        assert_eq!(entries[1].title, "Bank");
        assert_eq!(entries[1].totp, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
    }

    #[test]
    fn imports_otpauth_uri() {
        let uri =
            "otpauth://totp/GitHub:alice?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=GitHub";
        let entries = parse_import(uri).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "GitHub");
        assert_eq!(entries[0].username, "alice");
        assert_eq!(entries[0].totp, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
    }

    #[test]
    fn rejects_unusable_json() {
        assert!(parse_import("not json").is_err());
        assert!(parse_import("[]").is_err());
        assert!(parse_import("{\"entries\": []}").is_err());
    }

    #[test]
    fn folders_are_created_and_listed() {
        let mut store = Store::default();
        assert!(!store.folder_exists("Work"));
        assert!(store.create_folder("Work"));
        assert!(!store.create_folder("Work"), "duplicates are ignored");
        store.add(Entry {
            title: "Mail".into(),
            category: "Personal".into(),
            ..Default::default()
        });
        // Explicit folders plus those implied by entries, sorted and unique.
        assert_eq!(store.all_folders(), vec!["Personal", "Work"]);
    }

    #[test]
    fn entry_icon_roundtrips() {
        let mut store = Store::default();
        store.add(Entry {
            title: "Git".into(),
            icon: "🐙".into(),
            ..Default::default()
        });
        let json = serde_json::to_string(&store).unwrap();
        let back: Store = serde_json::from_str(&json).unwrap();
        assert_eq!(back.entries[0].icon, "🐙");
    }

    #[test]
    fn folders_rename_and_delete_move_entries() {
        let mut store = Store::default();
        store.create_folder("Work");
        let id = store.add(Entry {
            title: "Mail".into(),
            category: "Work".into(),
            ..Default::default()
        });
        assert!(store.rename_folder("Work", "Job"));
        assert_eq!(store.get(id).unwrap().category, "Job");
        assert_eq!(store.all_folders(), vec!["Job"]);
        // Renaming onto an existing folder is refused.
        store.create_folder("Other");
        assert!(!store.rename_folder("Job", "Other"));

        store.delete_folder("Job");
        assert!(store.get(id).unwrap().category.is_empty());
        assert!(!store.all_folders().contains(&"Job".to_string()));
    }

    #[test]
    fn store_is_encrypted_at_rest_with_the_vault_key() {
        let (_, crypto) = crate::crypto::VaultCrypto::create("CorrectHorse9!Battery").unwrap();
        let mut store = Store::default();
        store.add(Entry {
            title: "Bank".into(),
            password: "hunter2!".into(),
            ..Default::default()
        });
        let json = serde_json::to_string(&store).unwrap();
        let blob = crypto.encrypt(&json);
        assert!(crate::crypto::is_encrypted(&blob));
        // The plaintext password must not be recoverable from the ciphertext.
        assert!(!blob.windows(8).any(|w| w == b"hunter2!"));
        let decrypted = crypto.decrypt(&blob).unwrap();
        let back: Store = serde_json::from_str(&decrypted).unwrap();
        assert_eq!(back.entries[0].password, "hunter2!");
    }
}
