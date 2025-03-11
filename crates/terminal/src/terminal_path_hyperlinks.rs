// TODO(davewa): Some ideas for further improvements
//
// - Support hyperlink-like paths. Currently if alacrity decides something is a link it will not be considered
// a maybe path unless it is a 'file://' url. However, there could be a real file with a name that looks like
// a non-'file://' url. Also, a 'file:/' is a valid directory
//
// - Only match git diff if line starts with "+++ a/" and treat the whole line as the path.
// - Support chunk line navigation in git diff output, e.g. `@@ <line>,<lines> @@`
// and `+ blah`.
// --- a/TODO.md
// +++ b/TODO.md
// @@ -15,7 +15,7 @@
//   blah
// + blah
//   blah
//
// - Support navigation to line in rust diagnostic output, e.g. from the 'gutter'
//    -->
// 200 |
// 201 |
//     |
// ... |
// 400 |
// 401 |
//
// - Support escapes in paths, e.g. git octal escaping
// See https://git-scm.com/docs/git-config#Documentation/git-config.txt-corequotePath
// Note that "Double-quotes, backslash and control characters are always escaped
// regardless of the setting of this variable.". Currently we don't support any
// escaping in paths, so these currently do not work.

// TODO(davewa) TASK LIST
//
// - [ ] Add Tests
// - [ ] Re-implement the fix for https://github.com/zed-industries/zed/issues/25086
// using path_hyperlink_regexes...

use crate::ZedListener;
use alacritty_terminal::{index::Boundary, term::search::Match, Term};
use log::debug;
use std::{fmt::Display, ops::Range, path::Path};
use unicode_segmentation::UnicodeSegmentation;

/// These are valid in paths and are not matched by [WORD_REGEX](terminal::WORD_REGEX).
/// We use them to find potential path words within a line.
///
/// - **`\u{c}`** is **`\f`** (form feed - new page)
/// - **`\u{b}`** is **`\v`** (vertical tab)
///
/// See [C++ Escape sequences](https://en.cppreference.com/w/cpp/language/escape)
pub const MAIN_SEPARATORS: [char; 2] = ['\\', '/'];

pub const COMMON_PATH_SURROUNDING_SYMBOLS: &[(char, char)] =
    &[('"', '"'), ('\'', '\''), ('[', ']'), ('(', ')')];

/// Returns the longest range of matching surrounding symbols on [line] which contains [word].
/// This is arguably the most common case by far, so we enable it in PathHyperlinkNavigation::Default.
pub fn longest_surrounding_symbols_match(
    line: &str,
    word_range: &Range<usize>,
) -> Option<Range<usize>> {
    let mut longest = None::<Range<usize>>;

    let surrounds_word = |current: &Range<usize>| {
        current.contains(&word_range.start) && current.contains(&(word_range.end - 1))
    };

    for (start, end) in COMMON_PATH_SURROUNDING_SYMBOLS {
        if let (Some(first), Some(last)) = (line.find(*start), line.rfind(*end)) {
            if first < last {
                let current = first..last + 1;
                if surrounds_word(&current) {
                    if let Some(longest_so_far) = &longest {
                        if current.len() > longest_so_far.len() {
                            longest = Some(current);
                        }
                    } else {
                        longest = Some(current);
                    };
                }
            }
        }
    }

    longest
}

/// The original matched maybe path from hover or Cmd-click in the terminal
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HoveredMaybePath {
    pub line: String,
    pub hovered_word_range: Range<usize>,
    hovered_word_match: Match,
}

impl Display for HoveredMaybePath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.hovered_word_range.start != 0 || self.hovered_word_range.end != self.line.len() {
            formatter.write_fmt(format_args!(
                "{:?} «{}»",
                self,
                &self.line[self.hovered_word_range.clone()]
            ))
        } else {
            formatter.write_fmt(format_args!("{:?}", self))
        }
    }
}

impl HoveredMaybePath {
    /// For file IRIs, the IRI is always the 'line'
    pub(super) fn from_file_url(file_iri: &str, file_iri_match: Match) -> Self {
        Self {
            line: file_iri.to_string(),
            hovered_word_range: 0..file_iri.len(),
            hovered_word_match: file_iri_match,
        }
    }

    pub(super) fn from_hovered_word_match(
        term: &mut Term<ZedListener>,
        hovered_word_match: Match,
    ) -> Self {
        let maybe_path_word =
            term.bounds_to_string(*hovered_word_match.start(), *hovered_word_match.end());
        let line_start = term.line_search_left(*hovered_word_match.start());
        let mut line = if line_start == *hovered_word_match.start() {
            String::new()
        } else {
            term.bounds_to_string(
                line_start,
                hovered_word_match.start().sub(term, Boundary::Grid, 1),
            )
        };
        let hovered_word_start = line.len();
        line.push_str(&maybe_path_word);
        let hovered_word_end = line.len();
        let line_end = term.line_search_right(*hovered_word_match.end());
        let remainder = if line_end == *hovered_word_match.end() {
            String::new()
        } else {
            term.bounds_to_string(
                hovered_word_match.end().add(term, Boundary::Grid, 1),
                line_end,
            )
        };
        line.push_str(&remainder);

        let maybe_path = HoveredMaybePath::from_line(
            line,
            hovered_word_start..hovered_word_end,
            hovered_word_match,
        );
        maybe_path
    }

    fn from_line(
        line: String,
        hovered_word_range: Range<usize>,
        hovered_word_match: Match,
    ) -> Self {
        Self {
            line,
            hovered_word_range,
            hovered_word_match,
        }
    }

    pub(super) fn text_at(&self, range: &Range<usize>) -> &str {
        &self.line[range.clone()]
    }

    /// Computes the best hueristic match for link highlighting in the terminal. This
    /// will be linkified immediately even though we don't yet know if it is a real path.
    /// Once we've determined (in the background) a real path for [word], the hyperlink
    /// will be updated to the real path iff a real path was found, or cleared if not.
    pub(super) fn best_hueristic_path(
        &self,
        term: &mut Term<ZedListener>,
    ) -> Option<(String, Match)> {
        if let Some(surrounding_range) =
            longest_surrounding_symbols_match(&self.line, &self.hovered_word_range)
        {
            let stripped_range = surrounding_range.start + 1..surrounding_range.end - 1;
            debug!(
                "Terminal: path hueristic 'longest surrounding symbols' match: {:?}",
                self.text_at(&stripped_range)
            );
            Some((
                self.text_at(&stripped_range).to_string(),
                self.match_from_text_range(term, &stripped_range),
            ))
        } else if self.looks_like_a_path_match(&self.hovered_word_range) {
            debug!(
                "Terminal: path hueristic 'looks like a path' match: {:?}",
                &self.line[self.hovered_word_range.clone()]
            );
            Some((
                self.line[self.hovered_word_range.clone()].to_string(),
                self.hovered_word_match.clone(),
            ))
        } else {
            None
        }
    }

    fn looks_like_a_path_match(&self, word_range: &Range<usize>) -> bool {
        let word = self.text_at(word_range);
        Path::new(word).extension().is_some()
            || word.starts_with('.')
            || word.contains(MAIN_SEPARATORS)
    }

    pub(super) fn match_from_text_range(
        &self,
        term: &mut Term<ZedListener>,
        text_range: &Range<usize>,
    ) -> Match {
        let start = if text_range.start > self.hovered_word_range.start {
            let adjust_start = self.line[self.hovered_word_range.start..text_range.start]
                .graphemes(true)
                .count();
            self.hovered_word_match
                .start()
                .add(term, Boundary::Grid, adjust_start)
        } else if text_range.start < self.hovered_word_range.start {
            let adjust_start = self.line[text_range.start..self.hovered_word_range.start]
                .graphemes(true)
                .count();
            self.hovered_word_match
                .start()
                .sub(term, Boundary::Grid, adjust_start)
        } else {
            self.hovered_word_match.start().clone()
        };

        let end = if text_range.end > self.hovered_word_range.end {
            let adjust_end = self.line[self.hovered_word_range.end..text_range.end]
                .graphemes(true)
                .count();
            self.hovered_word_match
                .end()
                .add(term, Boundary::Grid, adjust_end)
        } else if text_range.end < self.hovered_word_range.end {
            let adjust_end = self.line[text_range.end..self.hovered_word_range.end]
                .graphemes(true)
                .count();
            self.hovered_word_match
                .end()
                .sub(term, Boundary::Grid, adjust_end)
        } else {
            self.hovered_word_match.end().clone()
        };

        Match::new(start, end)
    }
}
