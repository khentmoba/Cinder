//! A fast screen for the log lines that can change the collected stats.
//!
//! The session logs are gigabytes of JSONL and almost none of it can move a
//! number on the dashboard, so every line is screened for a handful of marker
//! strings before it is handed to serde_json. The screening uses FFF's
//! SIMD-accelerated substring search (AVX2 on x86-64, NEON on aarch64),
//! which picks the two rarest bytes of the needle and compares them in
//! parallel — several times faster than a plain `str::contains` over the same
//! line.
//!
//! FFF's search is ASCII case-insensitive, so the markers are matched
//! regardless of case. That is a superset of the case-sensitive markers the
//! parsers used before: a line can now reach the parser that would have been
//! skipped, never the other way round.

use fff_search::simd_string_utils::memmem;

/// A set of markers, screened for in one pass over a line.
pub struct Prefilter {
    /// Pre-lowercased needles. FFF's search wants the needle lowered.
    needles: Vec<Vec<u8>>,
}

impl Prefilter {
    pub fn new(keys: &[&str]) -> Self {
        let mut needles: Vec<Vec<u8>> = keys
            .iter()
            .filter(|k| !k.is_empty())
            .map(|k| {
                let mut n = k.as_bytes().to_vec();
                n.make_ascii_lowercase();
                n
            })
            .collect();
        // The rarest markers first: the common ones ("usage") sit in most of
        // the lines that matter, so they are tried last.
        needles.sort_by_key(|n| n.len());
        Self { needles }
    }

    /// True when the line carries at least one marker.
    #[inline]
    pub fn matches(&self, line: &str) -> bool {
        let hay = line.as_bytes();
        self.needles.iter().any(|n| memmem::find(hay, n).is_some())
    }

    /// True when the line carries none of the markers.
    #[inline]
    pub fn skips(&self, line: &str) -> bool {
        !self.matches(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_with_a_marker_is_kept() {
        let p = Prefilter::new(&["usage", "session"]);
        assert!(p.matches(r#"{"type":"message","message":{"usage":{"input":1}}}"#));
        assert!(p.matches(r#"{"type":"session","timestamp":"2026-01-01"}"#));
    }

    #[test]
    fn a_line_without_a_marker_is_dropped() {
        let p = Prefilter::new(&["usage", "session"]);
        assert!(p.skips(r#"{"type":"user","message":{"role":"user"}}"#));
        assert!(p.skips(""));
    }

    #[test]
    fn markers_match_regardless_of_case() {
        let p = Prefilter::new(&["PLANNER_RESPONSE"]);
        assert!(p.matches(r#"{"type":"planner_response"}"#));
        assert!(p.matches(r#"{"type":"PLANNER_RESPONSE"}"#));
    }

    #[test]
    fn an_empty_marker_set_matches_nothing() {
        let p = Prefilter::new(&[]);
        assert!(p.skips("anything"));
    }
}
