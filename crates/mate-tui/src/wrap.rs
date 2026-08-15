//! Wrap cache (`M7-4`): word-wraps one transcript entry's text at a given width and caches the
//! result keyed by entry id, recomputed only when that id's cached width no longer matches
//! (a resize) or the caller explicitly invalidates it (the entry's own text changed while
//! still streaming). Rendering only ever wraps the entries actually visible in the viewport
//! (`ui.rs`), so a 10k-entry transcript costs nothing beyond what's on screen.

use std::collections::HashMap;

use crate::transcript::EntryId;

pub struct WrapCache {
    cached: HashMap<EntryId, (u16, Vec<String>)>,
}

impl WrapCache {
    pub fn new() -> Self {
        Self {
            cached: HashMap::new(),
        }
    }

    pub fn wrapped(&mut self, id: EntryId, text: &str, width: u16) -> &[String] {
        let stale = match self.cached.get(&id) {
            Some((cached_width, _)) => *cached_width != width,
            None => true,
        };
        if stale {
            self.cached.insert(id, (width, wrap(text, width)));
        }
        &self.cached[&id].1
    }

    pub fn invalidate(&mut self, id: EntryId) {
        self.cached.remove(&id);
    }
}

impl Default for WrapCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Greedy word wrap: fills each line up to `width` columns, breaking at spaces. A single word
/// longer than `width` is left to overflow that one line rather than hard-broken mid-word —
/// rare in chat text, and the render side clips it harmlessly. Explicit `\n`s in `text` always
/// start a new line.
fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = width.max(1) as usize;
    let mut lines = Vec::new();
    for source_line in text.split('\n') {
        if source_line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in source_line.split(' ') {
            let extra = if current.is_empty() { 0 } else { 1 };
            let fits = current.chars().count() + extra + word.chars().count() <= width;
            if !current.is_empty() && !fits {
                lines.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_the_given_width_on_word_boundaries() {
        assert_eq!(
            wrap("the quick brown fox", 10),
            vec!["the quick", "brown fox"]
        );
    }

    #[test]
    fn explicit_newlines_always_start_a_new_line() {
        assert_eq!(wrap("a\nb", 10), vec!["a", "b"]);
    }

    #[test]
    fn an_overlong_word_is_left_on_its_own_line_rather_than_split() {
        assert_eq!(
            wrap("supercalifragilistic ok", 5),
            vec!["supercalifragilistic", "ok"]
        );
    }

    #[test]
    fn caches_and_recomputes_only_on_a_width_change() {
        let mut cache = WrapCache::new();
        let first = cache.wrapped(0, "hello world", 5).to_vec();
        assert_eq!(first, vec!["hello", "world"]);

        // Same width: cached value returned even though the text argument now differs — the
        // cache trusts the caller not to reuse an id for different content without inserting a
        // new entry (`Transcript`'s ids are never reused).
        let same_width = cache.wrapped(0, "ignored", 5).to_vec();
        assert_eq!(same_width, first);

        let rewrapped = cache.wrapped(0, "hello world", 11).to_vec();
        assert_eq!(rewrapped, vec!["hello world"]);
    }

    #[test]
    fn invalidate_forces_a_recompute_even_at_the_same_width() {
        let mut cache = WrapCache::new();
        cache.wrapped(0, "hello", 5);
        cache.invalidate(0);
        let after = cache.wrapped(0, "hello!", 5).to_vec();
        assert_eq!(after, vec!["hello!"]);
    }
}
