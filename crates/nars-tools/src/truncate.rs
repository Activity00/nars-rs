//! Output truncation utilities.

/// Truncates a string to at most `limit` bytes on a UTF-8 boundary.
///
/// Returns the truncated string and whether truncation occurred. The marker is
/// appended after the limit, so callers can always detect loss.
#[must_use]
pub fn truncate_bytes(input: &str, limit: usize) -> (String, bool) {
    if input.len() <= limit {
        return (input.to_owned(), false);
    }
    let mut end = limit;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    let omitted = input.len() - end;
    let mut output = String::with_capacity(end + 48);
    output.push_str(&input[..end]);
    output.push_str(&format!("\n… [truncated {omitted} bytes]"));
    (output, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_short_strings() {
        let (output, truncated) = truncate_bytes("hello", 10);
        assert_eq!(output, "hello");
        assert!(!truncated);
    }

    #[test]
    fn truncates_on_a_char_boundary() {
        let (output, truncated) = truncate_bytes("你好世界", 7);
        assert!(truncated);
        assert!(output.starts_with("你好"));
        assert!(output.contains("truncated"));
    }
}
