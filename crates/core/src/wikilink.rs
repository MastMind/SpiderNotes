//! Obsidian-style `[[wikilinks]]` and `#tags`.
//!
//! Links are resolved against the vault: exact relative path first, then
//! basename (case-sensitive, then case-insensitive). Tags are
//! case-insensitive single tokens (`/` allows nesting, e.g. `#проект/альфа`).

use crate::{NoteSummary, Vault};
use std::collections::HashMap;
use uuid::Uuid;

/// Raw `[[...]]` targets (alias `|...` and `#heading` stripped).
pub fn extract_wikilinks(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = content;
    while let Some(s) = rest.find("[[") {
        let after = &rest[s + 2..];
        let Some(e) = after.find("]]") else {
            break;
        };
        let inner = &after[..e];
        if !inner.contains('[') && !inner.contains(']') && !inner.contains('\n') {
            let target = inner
                .split('|')
                .next()
                .unwrap_or("")
                .split('#')
                .next()
                .unwrap_or("")
                .trim();
            if !target.is_empty() {
                out.push(target.to_string());
            }
        }
        rest = &after[e + 2..];
    }
    out
}

/// Resolve a link target to a live note by path or by basename.
pub fn resolve_link(vault: &Vault, target: &str) -> Option<Uuid> {
    let notes = vault.live_notes();
    let t = target.trim().trim_start_matches('/');
    let t_no_ext = t.strip_suffix(".md").unwrap_or(t);

    if let Some(n) = notes
        .iter()
        .find(|n| n.path == t || n.path == t_no_ext)
    {
        return Some(n.uuid);
    }

    let base = t_no_ext.rsplit('/').next().unwrap_or(t_no_ext);
    if let Some(n) = notes.iter().find(|n| {
        let b = n.path.rsplit('/').next().unwrap_or("");
        b == base || b == format!("{base}.md")
    }) {
        return Some(n.uuid);
    }

    let base_ci = base.to_lowercase();
    notes
        .iter()
        .find(|n| {
            let b = n.path.rsplit('/').next().unwrap_or("").to_lowercase();
            b == base_ci || b == format!("{base_ci}.md")
        })
        .map(|n| n.uuid)
}

/// Notes that link to `uuid`, with the number of links in each.
pub fn backlinks(vault: &Vault, uuid: &Uuid) -> Vec<(NoteSummary, usize)> {
    let mut out = Vec::new();
    for s in vault.live_notes() {
        if s.uuid == *uuid {
            continue;
        }
        let Ok(note) = vault.read(&s.uuid) else {
            continue;
        };
        let count = extract_wikilinks(&note.content)
            .iter()
            .filter(|t| resolve_link(vault, t) == Some(*uuid))
            .count();
        if count > 0 {
            out.push((s, count));
        }
    }
    out.sort_by(|a, b| a.0.path.cmp(&b.0.path));
    out
}

/// `#tag` tokens. Skips ATX headings (`# Title`), URL fragments
/// (`https://x/#a`, `x/#a`) and word-glued hashes (`v1.2#3`).
pub fn extract_tags(content: &str) -> Vec<String> {
    let chars: Vec<char> = content.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '#' {
            i += 1;
            continue;
        }
        let prev = chars.get(i.wrapping_sub(1)).copied();
        // ATX heading: '#' at line start followed by whitespace (or EOL).
        let at_line_start = prev.is_none_or(|p| {
            p == '\n'
                || (p.is_whitespace() && {
                    let mut k = i;
                    while k > 0 && (chars[k - 1] == ' ' || chars[k - 1] == '\t') {
                        k -= 1;
                    }
                    k == 0 || chars[k - 1] == '\n'
                })
        });
        let after_space = chars.get(i + 1).copied().is_some_and(char::is_whitespace);
        let heading = at_line_start && after_space;
        let url_fragment = matches!(prev, Some('/') | Some(':'));
        let word_glued = prev.is_some_and(|p| {
            p.is_ascii_alphanumeric() || matches!(p, '_' | '-' | '.')
        });
        if heading || url_fragment || word_glued {
            i += 1;
            continue;
        }
        let mut m = i + 1;
        let mut tag = String::new();
        while m < chars.len() {
            let c = chars[m];
            if c.is_alphanumeric() || c == '_' || c == '-' || c == '/' {
                tag.push(c);
                m += 1;
            } else {
                break;
            }
        }
        while tag.ends_with('/') {
            tag.pop();
        }
        if !tag.is_empty() && !tag.starts_with('/') {
            out.push(tag);
        }
        i = m;
    }
    out
}

/// All tags in the vault with occurrence counts (tag lowercased).
pub fn all_tags(vault: &Vault) -> Vec<(String, usize)> {
    let mut m: HashMap<String, usize> = HashMap::new();
    for s in vault.live_notes() {
        if let Ok(note) = vault.read(&s.uuid) {
            for t in extract_tags(&note.content) {
                *m.entry(t.to_lowercase()).or_insert(0) += 1;
            }
        }
    }
    let mut v: Vec<(String, usize)> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// Live notes containing the given tag (case-insensitive).
pub fn notes_with_tag(vault: &Vault, tag: &str) -> Vec<NoteSummary> {
    let t = tag.to_lowercase();
    vault
        .live_notes()
        .into_iter()
        .filter(|s| {
            vault
                .read(&s.uuid)
                .map(|n| {
                    extract_tags(&n.content)
                        .iter()
                        .any(|x| x.eq_ignore_ascii_case(&t))
                })
                .unwrap_or(false)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeviceIdentity;

    fn tmpdir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "sn-wiki-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    struct Guard(std::path::PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn links_extracted_with_alias_and_heading() {
        let c = "текст [[a.md]] и [[b|алиас]] и [[c#секция]] и [[\nстрока]] и [[broken";
        let v = extract_wikilinks(c);
        assert_eq!(v, vec!["a.md", "b", "c"]);
    }

    #[test]
    fn tags_basic() {
        let c = "# Заголовок\nтело #тег1 и #тег2/\nURL https://x.com/#frag и v1.2#3 и (в #скобках)\n#привет";
        let v = extract_tags(c);
        assert_eq!(v, vec!["тег1", "тег2", "скобках", "привет"]);
    }

    #[test]
    fn tags_nesting_and_case() {
        let c = "#проект/альфа #проект/АЛЬФА";
        let v = extract_tags(c);
        assert_eq!(v, vec!["проект/альфа", "проект/АЛЬФА"]);
    }

    #[test]
    fn resolve_and_backlinks() {
        let dir = tmpdir();
        let dev = DeviceIdentity::generate("t").unwrap();
        let g = Guard(dir.clone());
        let mut v = Vault::open(&dir, dev).unwrap();
        let a = v.write("notes/alpha.md", "alpha\nсм. [[beta]] и [[notes/beta]]").unwrap();
        let b = v.write("beta.md", "beta\nссылка [[alpha]]\nи [[Alpha]]").unwrap();
        let _ = v.write("gamma.md", "без ссылок").unwrap();

        assert_eq!(resolve_link(&v, "beta"), Some(b.uuid));
        assert_eq!(resolve_link(&v, "notes/beta.md"), Some(b.uuid));
        assert_eq!(resolve_link(&v, "Alpha"), Some(a.uuid));
        assert_eq!(resolve_link(&v, "нет-такого"), None);

        let bl = backlinks(&v, &b.uuid);
        assert_eq!(bl.len(), 1);
        assert_eq!(bl[0].0.uuid, a.uuid);
        assert_eq!(bl[0].1, 2);

        let bl_a = backlinks(&v, &a.uuid);
        assert_eq!(bl_a.len(), 1);
        assert_eq!(bl_a[0].0.uuid, b.uuid);
        assert_eq!(bl_a[0].1, 2);

        v.write("tags.md", "см. #проект/альфа и #Проект/АЛЬФА и #один").unwrap();
        let tags = all_tags(&v);
        assert_eq!(tags[0].0, "проект/альфа");
        assert_eq!(tags[0].1, 2);
        assert_eq!(tags[1].0, "один");
        assert_eq!(tags[1].1, 1);
        let with = notes_with_tag(&v, "ПРОЕКТ/альфа");
        assert_eq!(with.len(), 1);
        assert_eq!(with[0].path, "tags.md");
        drop(g);
    }
}
