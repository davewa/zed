//! Logic for when the hovered word looks like a path (depending on how hard you squint).
//!
//! # Possible future improvements
//!
//! - Support hyperlink-like paths.
//! Currently if alacrity decides something is a link it will not be considered
//! a maybe path unless it is a `file://` url. However, there could be a real file with a name that looks like
//! a non-`file://` url.
//! - Only match git diff if line starts with `+++ a/` and treat the whole rest of the line as the path
//! - Support chunk line navigation in git diff output, e.g. `@@ <line>,<lines> @@`
//! and `+ blah`.
//! ```
//! --- a/TODO.md
//! +++ b/TODO.md
//! @@ -15,7 +15,7 @@
//!   blah
//! + blah
//!   blah
//! ```
//! - Support navigation to line in rust diagnostic output, e.g. from the 'gutter'
//! ```
//!    --> Something bad happened here:
//! 200 |
//! 201 |
//!     |
//! ... |
//! 400 |
//! 401 |
//! ```
//! - Support escapes in paths, e.g. git octal escaping
//! See [core.quotePath](https://git-scm.com/docs/git-config#Documentation/git-config.txt-corequotePath)
//! > Double-quotes, backslash and control characters are always escaped
//! > regardless of the setting of this variable.". Currently we don't support any
//! > escaping in paths, so these currently do not work.
//!
//! # TODOs
//! ## [Cmd+click to linkify file in terminal doesn't work when there are whitespace or certain separators in the filename](https://github.com/zed-industries/zed/issues/12338)
//!
//! - [ ] PREAPPROVED_PATH_HYPERLINK_REGEXES should probably find a `util::paths::ROW_COL_CAPTURE_REGEX` followed
//! by a `:`, or whatever else usualy follows a line & column (need to check MSVC output).
//! - [ ] Clear `last_hovered_*` when terminal content changes. See comment at `point_within_last_hovered`
//! - [ ] best_heuristic_hovered_word currently causes false positives to flicker e.g., they get linkified
//! immediately, then get clear once we confirm they are not paths. Maybe this is fine? But I think we
//! should just not hyperlink maybe path like things until they are confirmed.
//! - [ ] Add many more tests

#[cfg(doc)]
use super::WORD_REGEX;
use crate::{HoveredWord, ZedListener};
use alacritty_terminal::{index::Boundary, term::search::Match, Term};
use log::debug;
use regex::Regex;
use std::{fmt::Display, ops::Range, path::Path, sync::LazyLock};
use unicode_segmentation::UnicodeSegmentation;

/// These are valid in paths and are not matched by [WORD_REGEX].
/// We use them to find potential paths within a line.
///
/// - **`\u{c}`** is **`\f`** (form feed - new page)
/// - **`\u{b}`** is **`\v`** (vertical tab)
///
/// See [C++ Escape sequences](https://en.cppreference.com/w/cpp/language/escape)
pub const MAIN_SEPARATORS: [char; 2] = ['\\', '/'];

/// Common symbols which often surround a path, e.g., `"` `'` `[` `]` `(` `)`
pub const COMMON_PATH_SURROUNDING_SYMBOLS: &[(char, char)] =
    &[('"', '"'), ('\'', '\''), ('[', ']'), ('(', ')')];

/// Returns the longest range of matching surrounding symbols on `line` which contains `word_range`
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

// If there is a word on the line that contains a colon that word up to (but not including)
// its last colon, it is treated as a maybe path.
// e.g., Ruby (see https://github.com/zed-industries/zed/issues/25086)
//
// Note that unlike the original fix for that issue, we don't check the characters before
// and after the colon for digit-ness so that in case the line and column suffix is in
// MSVC-style (<line>,<column>):message or some other style. Line and column suffixes are
// processed later in termainl_view.
const ROW_COLUMN_DESCRIPTION_REGEX: &str = concat!("(?<path>", crate::word_regex!(), "):");

const PREAPPROVED_PATH_HYPERLINK_REGEXES: [&str; 1] = [ROW_COLUMN_DESCRIPTION_REGEX];

/// Returns a list of the preapproved path hyperlink regexes
pub fn preapproved_path_hyperlink_regexes() -> &'static Vec<Regex> {
    static PREAPPROVED_MAYBE_PATH_REGEXES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        let mut regexes = Vec::new();
        for regex in PREAPPROVED_PATH_HYPERLINK_REGEXES {
            regexes.push(Regex::new(regex).unwrap());
        }
        regexes
    });
    &PREAPPROVED_MAYBE_PATH_REGEXES
}

/// If `hovered_word_range` overlaps the regex match, returns the matched range
pub fn path_regex_match(
    line: &str,
    hovered_word_range: &Range<usize>,
    path_regexes: &Vec<Regex>,
) -> Option<Range<usize>> {
    for regex in path_regexes.iter().chain(path_regexes.iter()) {
        let Some(captures) = regex.captures(&line) else {
            debug!("Regex should succeed if RegexSearch succeeded already");
            continue;
        };
        // Note: Do NOT use captures[CUSTOM_PATH_HYPERLINK_REGEX_CAPTURE_NAME] here because
        // it can panic. This is extra paranoid because we don't load path regexes that do not
        // contain a path named capture group in the first place (see [init_path_regexes]).
        let Some(path_capture) = captures.name("path") else {
            debug!("'path' capture not matched in regex");
            continue;
        };

        if hovered_word_range.contains(&path_capture.start())
            || hovered_word_range.contains(&path_capture.end())
        {
            return Some(path_capture.range());
        }
    }

    None
}

/// The hovered or Cmd-clicked word in the terminal
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaybePathLike {
    line: String,
    word_range: Range<usize>,
    word_match: Match,
}

impl Display for MaybePathLike {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.word_range.start != 0 || self.word_range.end != self.line.len() {
            formatter.write_fmt(format_args!(
                "{:?} «{}»",
                self,
                &self.line[self.word_range.clone()]
            ))
        } else {
            formatter.write_fmt(format_args!("{:?}", self))
        }
    }
}

impl MaybePathLike {
    /// For file IRIs, the IRI is always the 'line'
    pub(super) fn from_file_url(file_iri: &str, file_iri_match: &Match) -> Self {
        Self {
            line: file_iri.to_string(),
            word_range: 0..file_iri.len(),
            word_match: file_iri_match.clone(),
        }
    }

    pub(super) fn from_hovered_word_match<T>(term: &mut Term<T>, word_match: &Match) -> Self {
        let word = term.bounds_to_string(*word_match.start(), *word_match.end());
        let line_start = term.line_search_left(*word_match.start());
        let mut line = if line_start == *word_match.start() {
            String::new()
        } else {
            term.bounds_to_string(line_start, word_match.start().sub(term, Boundary::Grid, 1))
        };
        let word_start = line.len();
        line.push_str(&word);
        let word_end = line.len();
        let line_end = term.line_search_right(*word_match.end());
        let remainder = if line_end == *word_match.end() {
            String::new()
        } else {
            term.bounds_to_string(word_match.end().add(term, Boundary::Grid, 1), line_end)
        };
        line.push_str(&remainder);

        MaybePathLike::from_line_and_word_range(line, word_start..word_end, word_match)
    }

    fn from_line_and_word_range(
        line: String,
        word_range: Range<usize>,
        word_match: &Match,
    ) -> Self {
        Self {
            line,
            word_range,
            word_match: word_match.clone(),
        }
    }

    pub fn to_line_and_word_range(&self) -> (String, Range<usize>) {
        (self.line.clone(), self.word_range.clone())
    }

    pub fn text_at(&self, range: &Range<usize>) -> &str {
        &self.line[range.clone()]
    }

    /// Computes the best heuristic match for link highlighting in the terminal. This
    /// will be linkified immediately even though we don't yet know if it is a real path.
    /// Once we've determined (in the background) is it is a real path, the hyperlink
    /// will be updated to the real path if a real path was found, or cleared if not.
    pub(super) fn best_heuristic_hovered_word(
        &self,
        term: &mut Term<ZedListener>,
    ) -> Option<HoveredWord> {
        if let Some(surrounding_range) =
            longest_surrounding_symbols_match(&self.line, &self.word_range)
        {
            let stripped_range = surrounding_range.start + 1..surrounding_range.end - 1;
            debug!(
                "Terminal: path heuristic 'longest surrounding symbols' match: {:?}",
                self.text_at(&stripped_range)
            );
            Some(HoveredWord {
                word: self.text_at(&stripped_range).to_string(),
                word_match: self.match_from_text_range(term, &stripped_range),
            })
        } else if self.looks_like_a_path_match(&self.word_range) {
            debug!(
                "Terminal: path heuristic 'looks like a path' match: {:?}",
                &self.line[self.word_range.clone()]
            );
            Some(HoveredWord {
                word: self.line[self.word_range.clone()].to_string(),
                word_match: self.word_match.clone(),
            })
        } else if let Some(path_range) = path_regex_match(&self.line, &self.word_range, &Vec::new())
        {
            debug!(
                "Terminal: path heuristic 'path regex' match: {:?}",
                self.text_at(&path_range)
            );
            Some(HoveredWord {
                word: self.text_at(&path_range).to_string(),
                word_match: self.match_from_text_range(term, &path_range),
            })
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
        let start = if text_range.start > self.word_range.start {
            let adjust_start = self.line[self.word_range.start..text_range.start]
                .graphemes(true)
                .count();
            self.word_match
                .start()
                .add(term, Boundary::Grid, adjust_start)
        } else if text_range.start < self.word_range.start {
            let adjust_start = self.line[text_range.start..self.word_range.start]
                .graphemes(true)
                .count();
            self.word_match
                .start()
                .sub(term, Boundary::Grid, adjust_start)
        } else {
            self.word_match.start().clone()
        };

        let end = if text_range.end > self.word_range.end {
            let adjust_end = self.line[self.word_range.end..text_range.end]
                .graphemes(true)
                .count();
            self.word_match.end().add(term, Boundary::Grid, adjust_end)
        } else if text_range.end < self.word_range.end {
            let adjust_end = self.line[text_range.end..self.word_range.end]
                .graphemes(true)
                .count();
            self.word_match.end().sub(term, Boundary::Grid, adjust_end)
        } else {
            self.word_match.end().clone()
        };

        Match::new(start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::re_test;

    #[test]
    fn test_row_column_description_regex_25086() {
        // Some tools output "filename:line:col:message"
        // - Ruby (https://github.com/zed-industries/zed/issues/25086)
        re_test(
            ROW_COLUMN_DESCRIPTION_REGEX,
            "Main.cs:20:5:Error desc",
            vec!["Main.cs:20:5:"],
        );
        // Some tools output "filename(line,col):message"
        re_test(
            ROW_COLUMN_DESCRIPTION_REGEX,
            "Main.cs(20,5):Error desc",
            vec!["Main.cs(20,5):"],
        );
    }
}
