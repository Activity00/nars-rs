//! Bounded unified-diff generation for workspace writes.
//!
//! The agent does not need the minimal diff: it needs a correct, readable, and
//! linear-time preview. We strip common leading and trailing lines and emit one
//! hunk for the changed middle.

use nars_agent_ir::Diff;

/// Computes a unified diff between two texts.
#[must_use]
pub fn unified_diff(path: &str, before: &str, after: &str, before_existed: bool) -> Diff {
    let old_lines = split_keepends(before);
    let new_lines = split_keepends(after);

    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len().saturating_sub(prefix)
        && suffix < new_lines.len().saturating_sub(prefix)
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let old_middle = &old_lines[prefix..old_lines.len() - suffix];
    let new_middle = &new_lines[prefix..new_lines.len() - suffix];

    let mut unified = String::new();
    if before_existed {
        unified.push_str(&format!("--- a/{path}\n"));
    } else {
        unified.push_str("--- /dev/null\n");
    }
    unified.push_str(&format!("+++ b/{path}\n"));
    let old_start = if old_middle.is_empty() {
        prefix
    } else {
        prefix + 1
    };
    let new_start = if new_middle.is_empty() {
        prefix
    } else {
        prefix + 1
    };
    unified.push_str(&format!(
        "@@ -{old_start},{} +{new_start},{} @@\n",
        old_middle.len(),
        new_middle.len()
    ));
    for line in old_middle {
        unified.push('-');
        unified.push_str(line);
        if !line.ends_with('\n') {
            unified.push('\n');
        }
    }
    for line in new_middle {
        unified.push('+');
        unified.push_str(line);
        if !line.ends_with('\n') {
            unified.push('\n');
        }
    }

    Diff {
        path: path.to_owned(),
        before_existed,
        unified,
        added: u32::try_from(new_middle.len()).unwrap_or(u32::MAX),
        removed: u32::try_from(old_middle.len()).unwrap_or(u32::MAX),
    }
}

fn split_keepends(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split_inclusive('\n').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffs_a_changed_line() {
        let before = "one\ntwo\nthree\n";
        let after = "one\nTWO\nthree\n";
        let diff = unified_diff("file.txt", before, after, true);
        assert_eq!(diff.added, 1);
        assert_eq!(diff.removed, 1);
        assert!(diff.unified.contains("--- a/file.txt"));
        assert!(diff.unified.contains("-two"));
        assert!(diff.unified.contains("+TWO"));
        assert!(diff.unified.contains("@@ -2,1 +2,1 @@"));
    }

    #[test]
    fn diffs_a_new_file() {
        let diff = unified_diff("new.txt", "", "hello\n", false);
        assert!(diff.unified.contains("--- /dev/null"));
        assert_eq!(diff.added, 1);
    }

    #[test]
    fn identical_text_produces_an_empty_hunk() {
        let diff = unified_diff("same.txt", "a\nb\n", "a\nb\n", true);
        assert_eq!(diff.added, 0);
        assert_eq!(diff.removed, 0);
    }
}
