/// Minimal line-based YAML frontmatter handling.
///
/// We intentionally avoid full YAML (re)serialization so that user-written
/// frontmatter is never reformatted. We only add/replace the single line
/// `spidernotes-id: <uuid>` inside an existing frontmatter block.

pub const ID_KEY: &str = "spidernotes-id";
/// Frontmatter key marking a note whose body is encrypted at rest.
pub const ENCRYPTED_KEY: &str = "spidernotes-encrypted";

/// True when the content carries `spidernotes-encrypted: true` in its
/// frontmatter (the body is ciphertext, not markdown).
pub fn is_encrypted(content: &str) -> bool {
    let Some((start, end)) = frontmatter_bounds(content) else {
        return false;
    };
    content
        .lines()
        .take(end)
        .skip(start)
        .any(|line| {
            let t = line.trim();
            let Some(rest) = t.strip_prefix(ENCRYPTED_KEY) else {
                return false;
            };
            rest.trim_start()
                .strip_prefix(':')
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
        })
}

/// Returns the frontmatter block bounds `(start_line, end_line)` (end exclusive)
/// if the content starts with a `---` fenced block, otherwise `None`.
pub fn frontmatter_bounds(content: &str) -> Option<(usize, usize)> {
    let mut lines = content.lines();
    if lines.next()? != "---" {
        return None;
    }
    for (i, line) in lines.enumerate() {
        if line == "---" {
            // i is 0-based after the opening line; closing line abs index = i + 1,
            // exclusive end = i + 2
            return Some((0, i + 2));
        }
    }
    None
}

/// Extract `spidernotes-id` from frontmatter, if present.
pub fn extract_id(content: &str) -> Option<String> {
    let (start, end) = frontmatter_bounds(content)?;
    for line in content.lines().take(end).skip(start) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(ID_KEY) {
            let v = rest.trim_start();
            let v = v.strip_prefix(':').unwrap_or(v).trim().trim_matches('"');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Ensure the content carries `spidernotes-id: <id>`.
/// Adds a frontmatter block if missing; replaces the line if the id differs.
pub fn ensure_id(content: &str, id: &str) -> String {
    let line = format!("{ID_KEY}: {id}");
    match frontmatter_bounds(content) {
        Some((start, end)) => {
            let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
            if let Some(pos) = lines[start..end].iter().position(|l| l.trim().starts_with(ID_KEY))
            {
                lines[pos] = line;
            } else {
                // insert right after the opening `---`
                lines.insert(start + 1, line);
            }
            lines.join("\n") + if content.ends_with('\n') {
                "\n"
            } else {
                ""
            }
        }
        None => format!("---\n{line}\n---\n\n{content}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_frontmatter_gets_block_added() {
        let out = ensure_id("# Hello\nworld", "id1");
        assert_eq!(out, "---\nspidernotes-id: id1\n---\n\n# Hello\nworld");
        assert_eq!(extract_id(&out), Some("id1".into()));
    }

    #[test]
    fn existing_frontmatter_gets_line_inserted() {
        let src = "---\ntags: [a, b]\n---\nbody";
        let out = ensure_id(src, "id2");
        assert_eq!(out, "---\nspidernotes-id: id2\ntags: [a, b]\n---\nbody");
        assert_eq!(extract_id(&out), Some("id2".into()));
    }

    #[test]
    fn existing_id_line_replaced() {
        let src = "---\nspidernotes-id: old\n---\nbody";
        let out = ensure_id(src, "new");
        assert_eq!(out, "---\nspidernotes-id: new\n---\nbody");
        assert_eq!(extract_id(&out), Some("new".into()));
    }

    #[test]
    fn idempotent_when_same_id() {
        let src = "---\nspidernotes-id: same\n---\nbody\n";
        assert_eq!(ensure_id(src, "same"), src);
    }

    #[test]
    fn undetected_non_frontmatter() {
        assert!(frontmatter_bounds("no fm here").is_none());
        assert!(frontmatter_bounds("---\nunterminated").is_none());
        assert_eq!(frontmatter_bounds("---\na: 1\n---\nbody"), Some((0, 3)));
    }

    #[test]
    fn body_without_frontmatter() {
        // content hash should ignore the id line so that id addition is not a "change"
        let a = "# T\nhello";
        let b = ensure_id(a, "x");
        assert_ne!(a, b);
        assert_eq!(extract_id(&b), Some("x".into()));
    }
}
