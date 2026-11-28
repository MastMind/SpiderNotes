//! Vault media assets: files under the hidden `<vault>/.assets/` dir
//! (imported by drag&drop or the attach button; pre-existing vaults still
//! carry the legacy `assets/` dir, which the scanner covers too). They are
//! NOT notes — no index entry — so when the note referencing an asset is
//! deleted, the asset becomes an orphan. This module scans for such
//! orphans and deletes them (guarded to the asset dirs only).

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::path::Path;

use crate::note::{AssetRef, sha256_hex};

/// One asset file; `path` is vault-relative, slash-separated.
#[derive(Debug, Clone, Serialize)]
pub struct AssetFile {
    pub path: String,
    pub size: u64,
}

/// The result of an orphan scan.
#[derive(Debug, Clone, Serialize)]
pub struct AssetReport {
    pub files: Vec<AssetFile>,
    /// Paths (subset of `files`) referenced by no readable note and by no
    /// conflict version.
    pub orphans: Vec<String>,
    /// Encrypted notes whose content could not be read (no key held): an
    /// asset referenced ONLY inside them is still listed as an orphan —
    /// the UI must warn about this number.
    pub encrypted_unchecked: usize,
    pub total_bytes: u64,
    pub orphan_bytes: u64,
}

/// Local (non-URL) reference sources in markdown: `![alt](src)` and
/// `[text](src)`. External URLs (any scheme — http, data, blob, mailto,
/// …), `//host` links, `#anchors` and empty sources are skipped, as is a
/// bare `](` that is not preceded by `[`/`!`. Hand-rolled scan: the
/// pattern is simple and the core has no regex dependency.
pub fn local_refs(content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0usize;
    while i + 2 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b'(' {
            // a reference has a matching `[` before the `]` (for `![alt](src)`
            // the char right before `]` is the alt text, so scan back); a
            // line break ends the search — the `]` is not on this line's ref
            let mut j = i;
            let mut is_ref = false;
            while j > 0 {
                j -= 1;
                match bytes[j] {
                    b'[' => {
                        is_ref = true;
                        break;
                    }
                    b'\n' => break,
                    _ => {}
                }
            }
            if is_ref {
                if let Some(end) = content[i + 2..].find(')') {
                    let inner = &content[i + 2..i + 2 + end];
                    // a real src never spans a line break: without this a
                    // stray `](` would swallow the next line's paren
                    if !inner.contains('\n') {
                        // a markdown title only appears as quoted `src "t"` /
                        // `src 't'` — cut there so spaces in file names survive
                        let mut end = inner.len();
                        if let Some(p) = inner.find(" \"") {
                            end = p.min(end);
                        }
                        if let Some(p) = inner.find(" '") {
                            end = p.min(end);
                        }
                        let src = inner[..end].trim();
                        if is_local_src(src) {
                            out.push(src.to_string());
                        }
                    }
                }
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

/// True for a src that points at a local file: no `#`/`//` prefix and no
/// URL scheme. A scheme is `letter [alnum + . -]* :` with at least two
/// leading chars — so a Windows drive (`C:\x`) is NOT a scheme.
fn is_local_src(src: &str) -> bool {
    if src.is_empty() || src.starts_with('#') || src.starts_with("//") {
        return false;
    }
    if let Some(p) = src.bytes().position(|b| b == b':') {
        let pre = &src[..p];
        let is_scheme = pre.len() >= 2
            && pre
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.');
        if is_scheme && src.as_bytes()[0].is_ascii_alphabetic() {
            return false;
        }
    }
    true
}

/// Normalize a reference for comparison against asset paths: forward
/// slashes, `%20` decoded (imports percent-encode spaces), leading `./`
/// stripped.
fn norm_ref(src: &str) -> String {
    let mut s = src.replace('\\', "/").replace("%20", " ");
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    s
}

/// Walk `<root>/.assets/` (and the legacy `<root>/assets/`, kept for
/// vaults that predate the rename) recursively and cross-reference every
/// file against the references collected from `bodies` (all readable note
/// content + conflict versions). References may use either dir name — the
/// migration rewrites plaintext notes, but encrypted notes and conflict
/// sidecars keep the legacy form. `encrypted_unchecked` is passed through
/// from the caller, which knows how many encrypted notes it could not read.
pub fn scan(root: &Path, bodies: &[String], encrypted_unchecked: usize) -> AssetReport {
    let mut used: Vec<String> = Vec::new();
    for body in bodies {
        for r in local_refs(body) {
            used.push(norm_ref(&r));
        }
    }
    let root_s = root.to_string_lossy().replace('\\', "/");

    let mut files: Vec<AssetFile> = Vec::new();
    let mut orphans: Vec<String> = Vec::new();
    let mut total = 0u64;
    let mut orphan_bytes = 0u64;

    for name in [".assets", "assets"] {
        let assets_dir = root.join(name);
        if !assets_dir.is_dir() {
            continue;
        }
        let mut stack = vec![assets_dir];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                // a .md file is a note, never an asset (it can only be in the
                // legacy dir if the migration was skipped to protect it)
                if p.extension().is_some_and(|x| x == "md") {
                    continue;
                }
                let Ok(rel) = p.strip_prefix(root) else { continue };
                let rel_s = rel.to_string_lossy().replace('\\', "/");
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                total += size;
                let is_used = used.iter().any(|u| ref_matches(u, &rel_s, &root_s));
                if !is_used {
                    orphans.push(rel_s.clone());
                    orphan_bytes += size;
                }
                files.push(AssetFile { path: rel_s, size });
            }
        }
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    orphans.sort();
    AssetReport {
        files,
        orphans,
        encrypted_unchecked,
        total_bytes: total,
        orphan_bytes,
    }
}

/// Whether a normalized reference points at the asset at `rel_s`, allowing
/// either dir name (`.assets/` or the legacy `assets/`) and an optional
/// vault-root prefix.
fn ref_matches(ref_s: &str, rel_s: &str, root_s: &str) -> bool {
    let alt = legacy_rel(rel_s);
    for base in [rel_s, alt.as_str()] {
        if ref_s == base || ref_s == format!("{root_s}/{base}") {
            return true;
        }
    }
    false
}

/// The other spelling of an asset path: `.assets/x` <-> `assets/x`.
fn legacy_rel(rel_s: &str) -> String {
    if let Some(r) = rel_s.strip_prefix(".assets/") {
        format!("assets/{r}")
    } else if let Some(r) = rel_s.strip_prefix("assets/") {
        format!(".assets/{r}")
    } else {
        rel_s.to_string()
    }
}

/// The media files a note body references that actually exist in this
/// vault: for every local `![alt](src)` / `[text](src)` whose src points
/// into `.assets/` (or the legacy `assets/`), the file's real
/// vault-relative path + sha256. URL references are skipped (there is
/// nothing to transfer), as are references whose file is missing.
/// Encrypted bodies must be filtered out by the caller (the references
/// are unknowable inside ciphertext).
pub fn referenced_assets(root: &Path, content: &str) -> Vec<AssetRef> {
    let mut out: Vec<AssetRef> = Vec::new();
    for r in local_refs(content) {
        let n = norm_ref(&r);
        // accept either dir spelling; the rest must stay inside it
        let rest = match n.strip_prefix(".assets/") {
            Some(r) => r,
            None => match n.strip_prefix("assets/") {
                Some(r) => r,
                None => continue,
            },
        };
        if rest.is_empty() || rest.split('/').any(|p| p == ".." || p == ".") {
            continue;
        }
        for dir in [".assets", "assets"] {
            let abs = root.join(dir).join(rest);
            if abs.is_file() {
                if let Ok(data) = std::fs::read(&abs) {
                    let path = format!("{dir}/{rest}");
                    if !out.iter().any(|a| a.path == path) {
                        out.push(AssetRef { path, hash: sha256_hex(&data) });
                    }
                }
                break; // the file's real location is found; the other spelling cannot also hold it
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Store a media file in the vault's asset dir (sync receiver side),
/// guarded to vault-relative paths UNDER `.assets/` (or the legacy
/// `assets/`); any `..`/absolute/foreign path is rejected. Parent dirs are
/// created; an existing file is overwritten (the sender's hash is what the
/// note's summary claims, so the bytes are authoritative).
pub fn write(root: &Path, rel: &str, data: &[u8]) -> Result<()> {
    let rel = rel.trim().replace('\\', "/");
    if !rel.starts_with(".assets/") && !rel.starts_with("assets/") {
        bail!("not an asset path: {rel}");
    }
    for part in rel.split('/') {
        if part == ".." || part == "." {
            bail!("not an asset path: {rel}");
        }
    }
    let abs = root.join(&rel);
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir {}", parent.display()))?;
    }
    std::fs::write(&abs, data).with_context(|| format!("write {}", abs.display()))?;
    Ok(())
}

/// Delete asset files, guarded to vault-relative paths UNDER `.assets/`
/// (or the legacy `assets/`); any `..`/absolute/foreign path is rejected.
/// Missing files are skipped. Returns the number of freed bytes.
pub fn delete(root: &Path, paths: &[String]) -> Result<u64> {
    let mut freed = 0u64;
    for p in paths {
        let rel = p.trim().replace('\\', "/");
        if !rel.starts_with(".assets/") && !rel.starts_with("assets/") {
            bail!("not an asset path: {p}");
        }
        for part in rel.split('/') {
            if part == ".." || part == "." {
                bail!("not an asset path: {p}");
            }
        }
        let abs = root.join(&rel);
        if let Ok(md) = std::fs::metadata(&abs) {
            if md.is_file() {
                freed += md.len();
                std::fs::remove_file(&abs)
                    .with_context(|| format!("remove {}", abs.display()))?;
            }
        }
    }
    Ok(freed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "sn-assets-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn refs_extracted_images_links_and_urls_skipped() {
        let md = "# t\n![a](assets/a.png)\n![b](./assets/b c.png)\n\
                  [titled](assets/titled.png \"the title\")\n\
                  [doc](assets/doc.pdf)\n[x](https://e.com/p)\n[y](data:x\n)\
                  [z](mailto:a@b.c)\n[w](#sec)\n[v](//cdn.com/f)\nno-paren ]( skipped\n\
                  ![](assets/empty-alt.png)";
        let refs = local_refs(md);
        assert!(refs.contains(&"assets/a.png".to_string()));
        // raw src as written in the doc (./ normalization happens in scan)
        assert!(refs.contains(&"./assets/b c.png".to_string()));
        // the quoted title is cut off, not treated as part of the src
        assert!(refs.contains(&"assets/titled.png".to_string()));
        assert!(refs.contains(&"assets/doc.pdf".to_string()));
        assert!(refs.contains(&"assets/empty-alt.png".to_string()));
        assert!(!refs.iter().any(|r| r.contains("https") || r.contains("data:")));
        assert!(!refs.contains(&"a@b.c".to_string()));
        assert!(!refs.contains(&"#sec".to_string()));
        assert!(!refs.contains(&"cdn.com/f".to_string()));
        assert!(!refs.contains(&" skipped".to_string()));
    }

    #[test]
    fn windows_drive_is_not_a_scheme() {
        assert!(is_local_src("C:\\pics\\a.png"));
        assert!(!is_local_src("https://x"));
        assert!(!is_local_src("mailto:a@b"));
    }

    #[test]
    fn scan_finds_orphans_and_counts() {
        let d = tempdir("scan");
        std::fs::create_dir_all(d.join(".assets")).unwrap();
        std::fs::write(d.join(".assets/used.png"), b"12345").unwrap();
        std::fs::write(d.join(".assets/orphan.gif"), b"123").unwrap();
        std::fs::create_dir_all(d.join(".assets/sub")).unwrap();
        std::fs::write(d.join(".assets/sub/deep.mp4"), b"1234567890").unwrap();
        std::fs::write(d.join("note.md"), b"not an asset").unwrap();

        let bodies = vec![
            "# a\n![u](.assets/used.png)\n".to_string(),
            // an UNMIGRATED reference (encrypted note / conflict sidecar
            // keep the legacy dir name) must still count as a use
            "# b\n![d](assets/sub/deep.mp4)\n".to_string(),
        ];
        let r = scan(&d, &bodies, 2);
        assert_eq!(r.files.len(), 3);
        assert_eq!(r.total_bytes, 5 + 3 + 10);
        assert_eq!(r.encrypted_unchecked, 2);
        assert_eq!(r.orphans, vec![".assets/orphan.gif".to_string()]);
        assert_eq!(r.orphan_bytes, 3);

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn scan_covers_legacy_assets_dir() {
        let d = tempdir("legacy");
        // a pre-migration vault: the dir is still assets/
        std::fs::create_dir_all(d.join("assets")).unwrap();
        std::fs::write(d.join("assets/a.png"), b"12345").unwrap();
        let r = scan(&d, &["# a\n![u](assets/a.png)\n".to_string()], 0);
        assert_eq!(r.files.len(), 1);
        assert!(r.orphans.is_empty());

        // and the inverse: a .assets/ file referenced with the legacy name
        std::fs::create_dir_all(d.join(".assets")).unwrap();
        std::fs::write(d.join(".assets/b.png"), b"123").unwrap();
        let r = scan(
            &d,
            &["# b\n![u](assets/b.png)\n".to_string(), "# a\n![u](assets/a.png)\n".to_string()],
            0,
        );
        assert_eq!(r.files.len(), 2);
        assert!(r.orphans.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn scan_empty_when_no_assets() {
        let d = tempdir("empty");
        let r = scan(&d, &[], 0);
        assert!(r.files.is_empty() && r.orphans.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn referenced_assets_maps_refs_to_existing_files() {
        let d = tempdir("refs");
        std::fs::create_dir_all(d.join(".assets")).unwrap();
        std::fs::write(d.join(".assets/a.png"), b"12345").unwrap();
        std::fs::create_dir_all(d.join(".assets/sub")).unwrap();
        std::fs::write(d.join(".assets/sub/deep.mp4"), b"1234567890").unwrap();
        // a file in the LEGACY dir: the ref may use either spelling
        std::fs::create_dir_all(d.join("assets")).unwrap();
        std::fs::write(d.join("assets/legacy.gif"), b"123").unwrap();
        let h = |b: &[u8]| crate::note::sha256_hex(b);
        let md = "# t\n\
                  ![a](.assets/a.png)\n\
                  ![d](assets/sub/deep.mp4)\n\
                  ![l](.assets/legacy.gif)\n\
                  [u](https://e.com/x)\n\
                  ![m](.assets/missing.png)\n\
                  ![bad](.assets/../outside.png)\n";
        std::fs::write(d.join("outside.png"), b"nope").unwrap();
        let refs = referenced_assets(&d, md);
        assert_eq!(
            refs,
            vec![
                AssetRef { path: ".assets/a.png".into(), hash: h(b"12345") },
                AssetRef { path: ".assets/sub/deep.mp4".into(), hash: h(b"1234567890") },
                // the ref spells it `.assets/legacy.gif` but the file lives in assets/
                AssetRef { path: "assets/legacy.gif".into(), hash: h(b"123") },
            ]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_guarded_to_assets() {
        let d = tempdir("wri");
        assert!(
            std::fs::write(d.join(".assets/x/y.png"), b"").is_err(),
            "no parent dir yet"
        );
        write(&d, ".assets/x/y.png", b"1234").unwrap();
        assert_eq!(std::fs::read(d.join(".assets/x/y.png")).unwrap(), b"1234");
        write(&d, "assets/z.png", b"12").unwrap();
        assert_eq!(std::fs::read(d.join("assets/z.png")).unwrap(), b"12");
        // traversal / foreign paths are rejected
        assert!(write(&d, "../outside.txt", b"x").is_err());
        assert!(write(&d, ".assets/../../outside.txt", b"x").is_err());
        assert!(write(&d, "/etc/passwd", b"x").is_err());
        assert!(write(&d, "note.md", b"x").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn delete_guarded_to_assets() {
        let d = tempdir("del");
        std::fs::create_dir_all(d.join(".assets")).unwrap();
        std::fs::write(d.join(".assets/a.png"), b"1234").unwrap();
        std::fs::create_dir_all(d.join("assets")).unwrap();
        std::fs::write(d.join("assets/legacy.png"), b"12").unwrap();
        std::fs::write(d.join("outside.txt"), b"keep").unwrap();
        let freed = delete(&d, &[".assets/a.png".to_string()]).unwrap();
        assert_eq!(freed, 4);
        assert!(!d.join(".assets/a.png").exists());
        // the legacy dir name is still deletable (pre-migration vaults)
        assert_eq!(delete(&d, &["assets/legacy.png".to_string()]).unwrap(), 2);
        // traversal / foreign paths are rejected, files untouched
        assert!(delete(&d, &["../outside.txt".to_string()]).is_err());
        assert!(delete(&d, &[".assets/../../outside.txt".to_string()]).is_err());
        assert!(delete(&d, &["/etc/passwd".to_string()]).is_err());
        assert!(delete(&d, &["note.md".to_string()]).is_err());
        assert!(d.join("outside.txt").exists());
        // a missing file is skipped, not an error
        assert_eq!(delete(&d, &[".assets/gone.png".to_string()]).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }
}
