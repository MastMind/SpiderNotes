/// Per-note at-rest encryption (password-based).
///
/// File format — the note's body is CIPHERTEXT, never plaintext:
///
/// ```text
/// ---
/// spidernotes-id: <uuid>           (added by the vault, like any note)
/// spidernotes-encrypted: true
/// spidernotes-title: <list title>  (plaintext metadata)
/// spidernotes-kdf: argon2id
/// spidernotes-kdf-memory: 65536    (KiB)
/// spidernotes-kdf-iterations: 3
/// spidernotes-kdf-parallelism: 2
/// spidernotes-kdf-salt: <base64>
/// spidernotes-cipher: aes256gcm
/// ---
/// <base64: 12-byte nonce || AES-256-GCM ciphertext+tag, 76-char lines>
/// ```
///
/// The password is never stored. The key is derived with Argon2id at
/// create/unlock and kept in app memory only while the note is open (the
/// Tauri layer discards it on lock/close), so saves re-encrypt with the
/// held key and the KDF cost is one-time per session, not per save.
///
/// Sync: the ciphertext is the note's content — hashed, LWW-compared and
/// stored by peers exactly like any note. A three-way LINE merge of
/// ciphertext is meaningless, so diverged encrypted versions always become
/// a conflict the user resolves as keep-mine / keep-theirs.

use anyhow::{bail, Context, Result};
use base64::engine::Engine;
use rand::RngCore;

use crate::frontmatter::{self, ENCRYPTED_KEY};

pub const TITLE_KEY: &str = "spidernotes-title";
pub const KDF_ALG_KEY: &str = "spidernotes-kdf";
pub const KDF_MEMORY_KEY: &str = "spidernotes-kdf-memory";
pub const KDF_ITER_KEY: &str = "spidernotes-kdf-iterations";
pub const KDF_PAR_KEY: &str = "spidernotes-kdf-parallelism";
pub const KDF_SALT_KEY: &str = "spidernotes-kdf-salt";
pub const CIPHER_KEY: &str = "spidernotes-cipher";

pub const KDF_ALG: &str = "argon2id";
pub const CIPHER: &str = "aes256gcm";

const NONCE_LEN: usize = 12;
const SALT_LEN: usize = 16;
const B64_WRAP: usize = 76;

/// Argon2id parameters: 64 MiB, 3 iterations, 2 lanes. A few hundred
/// milliseconds on a phone — paid once at create/unlock, never per save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            memory_kib: 65536,
            iterations: 3,
            parallelism: 2,
        }
    }
}

/// Everything needed to derive the key and re-encrypt, read from the note's
/// frontmatter.
pub struct EncryptedMeta {
    pub title: String,
    pub salt: Vec<u8>,
    pub params: KdfParams,
}

/// Derive the 256-bit key from the password (Argon2id, V0x13).
pub fn derive_key(password: &str, salt: &[u8], params: &KdfParams) -> Result<[u8; 32]> {
    use argon2::{password_hash::SaltString, Algorithm, Argon2, Params, PasswordHasher, Version};
    let salt = SaltString::encode_b64(salt).map_err(|e| anyhow::anyhow!("bad salt: {e}"))?;
    // output_len None = default 32 bytes
    let p = Params::new(params.memory_kib, params.iterations, params.parallelism, None)
        .map_err(|e| anyhow::anyhow!("bad kdf params: {e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, p);
    let hash = argon
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("kdf failed: {e}"))?;
    let output = hash
        .hash
        .ok_or_else(|| anyhow::anyhow!("argon2 produced no output"))?;
    let key = output.as_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(key);
    Ok(out)
}

/// The plaintext title stored in the frontmatter (for list rows).
pub fn extract_title(content: &str) -> Option<String> {
    fm_value(content, TITLE_KEY)
}

/// `key: value` from the note's frontmatter block (first `:` splits).
fn fm_value(content: &str, key: &str) -> Option<String> {
    let (start, end) = frontmatter::frontmatter_bounds(content)?;
    for line in content.lines().take(end).skip(start) {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix(key) {
            let v = rest.trim_start().strip_prefix(':')?.trim().trim_matches('"');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Salt, KDF parameters and cipher from the frontmatter; `Err` when the
/// note is not encrypted or the metadata is malformed.
pub fn parse_meta(content: &str) -> Result<EncryptedMeta> {
    if !frontmatter::is_encrypted(content) {
        bail!("note is not encrypted");
    }
    let salt_b64 = fm_value(content, KDF_SALT_KEY).context("missing kdf salt")?;
    let salt = base64::engine::general_purpose::STANDARD
        .decode(&salt_b64)
        .map_err(|e| anyhow::anyhow!("bad salt encoding: {e}"))?;
    // tolerant of missing/unknown params: fall back to defaults
    let parse_param = |key: &str, default: u32| {
        fm_value(content, key).and_then(|v| v.parse().ok()).unwrap_or(default)
    };
    let def = KdfParams::default();
    let params = KdfParams {
        memory_kib: parse_param(KDF_MEMORY_KEY, def.memory_kib),
        iterations: parse_param(KDF_ITER_KEY, def.iterations),
        parallelism: parse_param(KDF_PAR_KEY, def.parallelism),
    };
    Ok(EncryptedMeta {
        title: fm_value(content, TITLE_KEY).unwrap_or_default(),
        salt,
        params,
    })
}

/// The raw ciphertext: everything after the frontmatter, base64-decoded
/// (line wraps and blank separators ignored).
pub fn body_bytes(content: &str) -> Result<Vec<u8>> {
    let (_, end) = frontmatter::frontmatter_bounds(content).context("no frontmatter")?;
    let b64: String = content
        .lines()
        .skip(end)
        .flat_map(str::chars)
        .filter(|c| !c.is_whitespace())
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(&b64)
        .map_err(|e| anyhow::anyhow!("bad body encoding: {e}"))
}

/// AES-256-GCM encrypt: `nonce (12) || ciphertext+tag`.
pub fn encrypt_payload(plain: &str, key: &[u8; 32]) -> Vec<u8> {
    use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
    let mut nonce = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce);
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key");
    // GCM encryption cannot fail for a valid key/nonce
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plain.as_bytes())
        .expect("aes-gcm encrypt");
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    out
}

/// AES-256-GCM decrypt. Fails (wrong password / corrupted data).
pub fn decrypt_payload(data: &[u8], key: &[u8; 32]) -> Result<String> {
    use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
    if data.len() <= NONCE_LEN {
        bail!("ciphertext too short");
    }
    let (nonce, ct) = data.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key");
    let plain = cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| anyhow::anyhow!("wrong password or corrupted note"))?;
    String::from_utf8(plain).context("note is not valid UTF-8")
}

/// The full file content for a new encrypted note (no `spidernotes-id`:
/// the vault's write adds it, like for plain notes).
pub fn encode_file(title: &str, salt: &[u8], params: &KdfParams, payload: &[u8]) -> String {
    let salt_b64 = base64::engine::general_purpose::STANDARD.encode(salt);
    let b64 = base64::engine::general_purpose::STANDARD.encode(payload);
    let mut body = String::with_capacity(b64.len() + b64.len() / B64_WRAP);
    for (i, ch) in b64.chars().enumerate() {
        if i > 0 && i % B64_WRAP == 0 {
            body.push('\n');
        }
        body.push(ch);
    }
    format!(
        "---\n{ENCRYPTED_KEY}: true\n{TITLE_KEY}: {title}\n{KDF_ALG_KEY}: {KDF_ALG}\n{KDF_MEMORY_KEY}: {}\n{KDF_ITER_KEY}: {}\n{KDF_PAR_KEY}: {}\n{KDF_SALT_KEY}: {salt_b64}\n{CIPHER_KEY}: {CIPHER}\n---\n{body}\n",
        params.memory_kib, params.iterations, params.parallelism
    )
}

/// Create a brand-new encrypted note: random salt, key from the password,
/// the plaintext encrypted. Returns (file content, derived key) — the key
/// stays in app memory while the note is open.
pub fn create(plain: &str, title: &str, password: &str) -> Result<(String, [u8; 32])> {
    if password.trim().is_empty() {
        bail!("password must not be empty");
    }
    let mut salt = vec![0u8; SALT_LEN];
    rand::thread_rng().fill_bytes(&mut salt);
    let params = KdfParams::default();
    let key = derive_key(password, &salt, &params)?;
    let payload = encrypt_payload(plain, &key);
    Ok((encode_file(title, &salt, &params, &payload), key))
}

/// Verify the password (a wrong one fails the GCM tag) and return the
/// plaintext with the derived key.
pub fn unlock(content: &str, password: &str) -> Result<(String, [u8; 32])> {
    let meta = parse_meta(content)?;
    let key = derive_key(password, &meta.salt, &meta.params)?;
    let plain = decrypt_payload(&body_bytes(content)?, &key)?;
    Ok((plain, key))
}

/// Re-encrypt a modified plaintext with the held key (fresh nonce). The
/// salt, KDF parameters and title metadata are carried over; the caller
/// passes the new title (re-derived from the plaintext's heading).
pub fn reencrypt(content: &str, title: &str, plain: &str, key: &[u8; 32]) -> Result<String> {
    let meta = parse_meta(content)?;
    let payload = encrypt_payload(plain, key);
    Ok(encode_file(title, &meta.salt, &meta.params, &payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_unlock_roundtrip() {
        let (file, key) = create("# Secret\n\nbody text", "Secret", "hunter2").unwrap();
        assert!(frontmatter::is_encrypted(&file));
        assert!(!file.contains("body text"), "ciphertext must hide the plaintext");
        assert_eq!(extract_title(&file), Some("Secret".into()));

        let (plain, key2) = unlock(&file, "hunter2").unwrap();
        assert_eq!(plain, "# Secret\n\nbody text");
        assert_eq!(key, key2);

        assert!(unlock(&file, "wrong").is_err(), "wrong password must fail");
    }

    #[test]
    fn reencrypt_keeps_meta_and_stays_decryptable() {
        let (file, key) = create("# T\n\nv1", "T", "pw").unwrap();
        let file2 = reencrypt(&file, "T2", "# T\n\nv2", &key).unwrap();
        let meta = parse_meta(&file2).unwrap();
        assert_eq!(meta.title, "T2");
        let (plain, _) = unlock(&file2, "pw").unwrap();
        assert_eq!(plain, "# T\n\nv2");
        // fresh nonce: the ciphertext changed even though the key is the same
        assert_ne!(body_bytes(&file).unwrap(), body_bytes(&file2).unwrap());
    }

    #[test]
    fn body_bytes_tolerates_wrapping() {
        let (_, key) = create("# T\n\nx", "T", "pw").unwrap();
        let payload = encrypt_payload("# T\n\nx", &key);
        let salt = vec![7u8; 16];
        let f1 = encode_file("T", &salt, &KdfParams::default(), &payload);
        // unwrap everything into one line: must decode identically
        let (_, end) = frontmatter::frontmatter_bounds(&f1).unwrap();
        let b64: String = f1.lines().skip(end).collect::<Vec<_>>().join("");
        let flat = format!("---\n{ENCRYPTED_KEY}: true\n---\n{b64}\n");
        assert_eq!(body_bytes(&f1).unwrap(), body_bytes(&flat).unwrap());
    }

    #[test]
    fn parse_meta_rejects_plain_notes() {
        assert!(parse_meta("# T\nhello").is_err());
        assert!(parse_meta("---\ntitle: x\n---\nbody").is_err());
    }

    #[test]
    fn ensure_id_composes_with_encrypted_frontmatter() {
        let (file, _) = create("# T\n\nb", "T", "pw").unwrap();
        let with_id = frontmatter::ensure_id(&file, "abc-123");
        assert_eq!(frontmatter::extract_id(&with_id), Some("abc-123".into()));
        assert!(frontmatter::is_encrypted(&with_id));
        assert_eq!(extract_title(&with_id), Some("T".into()));
    }
}
