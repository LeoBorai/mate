//! Small string-truncation helpers shared by the panel widgets (§9.4/§9.6/§9.7/§9.8, `M12`).
//! Three flavors, because *which* end carries the identifying part differs by field: a path's
//! filename is at the end (middle-truncate), a model id's distinguishing suffix is at the end
//! too but the whole string reads left-to-right so the *drop* has to come from the front
//! (left-truncate), and a short derived note has no landmark worth preserving at all
//! (end-truncate).

/// Truncates `s` to at most `budget` chars, keeping the tail intact by dropping from the
/// middle (§9.8: `crates/…/lib.rs` rather than `crates/mate-…`) — the filename is the
/// identifying part of a path.
pub(crate) fn middle_truncate(s: &str, budget: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= budget {
        return s.to_string();
    }
    if budget <= 1 {
        return "…".to_string();
    }
    let keep = budget - 1;
    let tail = keep * 2 / 3;
    let head = keep - tail;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// Truncates `s` to at most `budget` chars, dropping from the front (§9.4: a model id's
/// distinguishing suffix — `Coder-480B-A35B-Instruct` — is what's worth keeping on screen, not
/// the `Qwen/Qwen3-` prefix every sibling model shares).
pub(crate) fn truncate_left(s: &str, budget: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= budget {
        return s.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    if budget == 1 {
        return "…".to_string();
    }
    let keep = budget - 1;
    let mut out = String::from("…");
    out.extend(&chars[chars.len() - keep..]);
    out
}

/// Truncates `s` to at most `budget` chars, dropping from the end — for a derived note or
/// activity line (§9.6) where nothing past the front is the identifying part.
pub(crate) fn truncate_end(s: &str, budget: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= budget {
        return s.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    if budget == 1 {
        return "…".to_string();
    }
    let keep = budget - 1;
    let mut out: String = chars[..keep].iter().collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn middle_truncate_keeps_the_tail_and_a_third_of_that_tail_as_head() {
        assert_eq!(
            middle_truncate("crates/mate-core/src/lib.rs", 12),
            "crat…/lib.rs"
        );
    }

    #[test]
    fn middle_truncate_is_a_no_op_under_budget() {
        assert_eq!(middle_truncate("short.rs", 20), "short.rs");
    }

    #[test]
    fn truncate_left_drops_the_front_keeping_the_distinguishing_suffix() {
        assert_eq!(
            truncate_left("Qwen/Qwen3-Coder-480B-A35B-Instruct", 12),
            "…5B-Instruct"
        );
    }

    #[test]
    fn truncate_end_drops_the_back_keeping_the_leading_verb_and_subject() {
        assert_eq!(
            truncate_end("reading a-very-long-file-name.rs", 12),
            "reading a-v…"
        );
    }

    #[test]
    fn every_flavor_is_a_no_op_under_budget() {
        assert_eq!(truncate_left("short", 20), "short");
        assert_eq!(truncate_end("short", 20), "short");
    }
}
