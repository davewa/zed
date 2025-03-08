// TODO(davewa): Change most (all?) info! messages into debug! or trace!
// TODO(davewa): Some APIs may benefit from HashSet for deduplication?
// TODO(davewa): Bugs found while testing this feature:
// - Navigation to line and column navigates to the wrong column when line
// contains unicode. I suspect it is using char's instead of graphemes.
// - [ ] When sending NewNaviagationTarget(None), we were not also clearning last_hovered_word, but we should.
// - [ ] When holding Cmd, and the terminal output is scrolling, the link is highlighted, but after scrolling
// away, it is still highlighting whatever new text is where the original link was.
// - [x] When hovering, initially a link flashes, then goes away
//
// TODO(davewa): Some ideas for further improvements
//
// - Support hyperlink-like paths. Currently if alacrity decides something is a link it will not be considered
// a maybe path unless it is a 'file://' url. However, there could be a real file with a name that looks like
// a non-'file://' url. Also, a 'file:/' is a valid directory
//
// - Support navigation to line in git diff output, e.g. `@@ <line>,<lines> @@`
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

use alacritty_terminal::{index::Boundary, term::search::Match, Term};
use log::{debug, trace};
use regex::Regex;
use std::{
    borrow::Cow,
    fmt::Display,
    ops::Range,
    path::{Path, PathBuf, MAIN_SEPARATOR},
    sync::LazyLock,
};
use unicode_segmentation::UnicodeSegmentation;
use util::{paths::PathWithPosition, TakeUntilExt};

use crate::{terminal_settings::PathHyperlinkNavigation, ZedListener};

/// These are valid in paths and are not matched by [WORD_REGEX](terminal::WORD_REGEX).
/// We use them to find potential path words within a line.
///
/// - **`\u{c}`** is **`\f`** (form feed - new page)
/// - **`\u{b}`** is **`\v`** (vertical tab)
///
/// See [C++ Escape sequences](https://en.cppreference.com/w/cpp/language/escape)
const PATH_WHITESPACE_CHARS: &str = "\t\u{c}\u{b} ";

const COMMON_PATH_SURROUNDING_SYMBOLS: &[(char, char)] =
    &[('"', '"'), ('\'', '\''), ('[', ']'), ('(', ')')];

/// The original matched maybe path from hover or Cmd-click in the terminal
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaybePath {
    line: String,
    hovered_word_range: Range<usize>,
    hovered_word_match: Match,
    path_hyperlink_navigation: PathHyperlinkNavigation,
}

impl MaybePath {
    /// For file IRIs, the IRI is always the 'line'
    pub(super) fn from_file_url(file_iri: &str, file_iri_match: Match) -> Self {
        Self {
            line: file_iri.to_string(),
            hovered_word_range: 0..file_iri.len(),
            hovered_word_match: file_iri_match,
            path_hyperlink_navigation: PathHyperlinkNavigation::Default,
        }
    }

    pub(self) fn from_line(
        line: String,
        hovered_word_range: Range<usize>,
        hovered_word_match: Match,
        path_hyperlink_navigation: PathHyperlinkNavigation,
    ) -> Self {
        Self {
            line,
            hovered_word_range,
            hovered_word_match,
            path_hyperlink_navigation,
        }
    }

    pub(super) fn from_hovered_word_match(
        term: &mut Term<ZedListener>,
        hovered_word_match: Match,
        path_hyperlink_navigation: PathHyperlinkNavigation,
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

        let maybe_path = MaybePath::from_line(
            line,
            hovered_word_start..hovered_word_end,
            hovered_word_match,
            path_hyperlink_navigation,
        );
        maybe_path
    }

    pub(super) fn hovered_word(&self) -> &str {
        &self.line[self.hovered_word_range.clone()]
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
        if let Some(surrounding_range) = self.longest_surrounding_symbols_match() {
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
                self.hovered_word()
            );
            Some((
                self.hovered_word().to_string(),
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
            // TODO(davewa): Should we always just check for all platforms, e.g. '\' and '/'?
            || word.contains(MAIN_SEPARATOR)
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

    /// [PathHyperlinkNavigation::Default] and [PathHyperlinkNavigation::Advanced] maybe path variants
    pub fn maybe_path_variants(&self) -> Vec<MaybePathVariant> {
        let mut maybe_path_variants = Vec::new();
        maybe_path_variants.push(MaybePathVariant::new(
            &self.line,
            self.hovered_word_range.clone(),
        ));

        if let Some(longest_range) = self.longest_surrounding_symbols_match() {
            // A surrounded `self.word` is already covered above in the first maybe path's variations
            if longest_range != self.hovered_word_range {
                maybe_path_variants.push(MaybePathVariant::new(
                    &self.line,
                    longest_range.start + 1..longest_range.end - 1,
                ));
            }
        }

        if self.path_hyperlink_navigation > PathHyperlinkNavigation::Default {
            if let Some(expanded_range) = self.expanded_maybe_path_by_interior_spaces() {
                maybe_path_variants.push(MaybePathVariant::new(&self.line, expanded_range));
            }
        }

        maybe_path_variants
    }

    /// [PathHyperlinkNavigation::Exhaustive] maybe path variants that match the
    /// `terminal.path_hyperlink_navigation_regexes` list of path regexes.
    ///
    /// # Notes
    /// Iterators are used to enable checking for timeout and stopping early.
    // TOOD: This is just an stub to show where path regex user settings would go if we decided to support that.
    pub fn regex_maybe_path_variants(
        &self,
    ) -> impl Iterator<Item = impl Iterator<Item = MaybePathVariant> + '_> + '_ {
        // TODO(davewa): Some way to assert we are not called on the main thread...
        Vec::<Vec<MaybePathVariant>>::new()
            .into_iter()
            .map(|maybe_path_variants| maybe_path_variants.into_iter())
            .into_iter()
    }

    /// [PathHyperlinkNavigation::Exhaustive]  maybe path variants that start on [self.hovered_word] or a
    /// word before it and end [self.hovered_word] or a word after it.
    ///
    /// # Notes
    /// Iterators are used to enable checking for timeout and stopping early.
    pub fn exhaustive_maybe_path_variants(
        &self,
    ) -> impl Iterator<Item = impl Iterator<Item = MaybePathVariant> + '_> + '_ {
        // TODO(davewa): Some way to assert we are not called on the main thread...
        // TODO(davewa): Start with "line ends in a path hueristic", e.g., for each start, check only to the end of the line.
        static WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(super::WORD_REGEX).unwrap());

        let starts = WORD_RE
            .find_iter(
                if self.path_hyperlink_navigation == PathHyperlinkNavigation::Exhaustive {
                    &self.line[..self.hovered_word_range.end]
                } else {
                    ""
                },
            )
            .map(|match_| match_.start());

        starts.into_iter().map(move |start| {
            WORD_RE
                .find_iter(&self.line[self.hovered_word_range.start..])
                .map(|match_| match_.end())
                .map(move |end| {
                    MaybePathVariant::new(&self.line, start..self.hovered_word_range.start + end)
                })
        })
    }

    /// Returns the longest range of matching surrounding symbols on [line] which contains [word].
    /// This is arguably the most common case by far, so we enable it in PathHyperlinkNavigation::Default.
    fn longest_surrounding_symbols_match(&self) -> Option<Range<usize>> {
        let mut longest = None::<Range<usize>>;

        let surrounds_word = |current: &Range<usize>| {
            current.contains(&self.hovered_word_range.start)
                && current.contains(&(self.hovered_word_range.end - 1))
        };

        for (start, end) in COMMON_PATH_SURROUNDING_SYMBOLS {
            if let (Some(first), Some(last)) = (self.line.find(*start), self.line.rfind(*end)) {
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

    /// Returns the range of the longest contiguous sequence of words on [line] that
    /// start and end with a word that contains MAIN_SEPARATOR and which contains [word].
    ///
    /// - The start is expanded to the start of the first word in [line] which contains a path separator.
    /// - The and is expanded to the end of the last word in [line] which contains a path separator.
    ///
    /// This is a quick way to catch the case where there is a path on the line that is
    /// - not surrounded by common symbols,
    /// - whose first component **does not** contain spaces
    /// - whose last component **does not** contain spaces
    /// - a least one interior component **does** contain spaces
    ///
    /// # Example
    /// _(maybe_path is_ **bold** _)_
    ///
    /// _before:_ this is\ an **example\of** how\this works
    ///
    /// _after:_ this **is\ an example\of how\this** works
    ///
    /// # To Do
    /// This seems like it would be a relatively common case, thus this special handling is enabled in
    /// PathHyperlinkNavigation::Advanced. If it is not that common in reality, this could be removed. It would
    /// still be handled correctly by PathHyperlinkNavigation::Exhaustive even without this.
    // TODO(davewa): Use looks_like_a_path_match()
    fn expanded_maybe_path_by_interior_spaces(&self) -> Option<Range<usize>> {
        let mut range = self.hovered_word_range.clone();

        if let Some(first_separator) = self.line.find(MAIN_SEPARATOR) {
            if first_separator < range.start {
                let word_start = first_separator
                    - self.line[..first_separator]
                        .chars()
                        .rev()
                        .take_until(|&c| PATH_WHITESPACE_CHARS.contains(c))
                        .count();

                if word_start == 0 {
                    // We stopped at the start of the text, that is the word_start.
                    range.start = word_start;
                } else {
                    // We stopped at a whitespace character, advance by 1
                    range.start = word_start + 1;
                }

                trace!(
                    "Terminal: Expanded maybe path left: {}",
                    self.text_at(&range)
                );
            }
        }

        if let Some(last_separator) = self.line.rfind(MAIN_SEPARATOR) {
            if last_separator >= range.end {
                let word_end = self.line[last_separator..]
                    .find(PATH_WHITESPACE_CHARS)
                    .unwrap_or(self.line.len());
                range.end = word_end;
                trace!(
                    "Terminal: Expanded maybe path right: {}",
                    self.text_at(&range)
                );
            }
        }

        if range != self.hovered_word_range {
            Some(range)
        } else {
            None
        }
    }
}

impl Display for MaybePath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.hovered_word_range.start != 0 || self.hovered_word_range.end != self.line.len() {
            formatter.write_fmt(format_args!("{:?} «{}»", self, self.hovered_word()))
        } else {
            formatter.write_fmt(format_args!("{:?}", self))
        }
    }
}

/// Line and column suffix information, including the suffix length
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct RowColumn {
    pub row: u32,
    pub column: Option<u32>,
    /// [MaybePathWithPosition::range] stores the range of the path after any line and column
    /// suffix has been stripped. Storing the [self.suffix_length] here allows us to linkify it
    /// correctly.
    pub suffix_length: usize,
}

/// Like [PathWithPosition], with enhancements for [MaybePath] processing
///
/// Specifically, we:
/// - Don't require allocation
/// - Model row and column restrictions directly (cannot have a column without a row)
/// - Include the [self.range] within our source [MaybePath], and the length of the line and column suffix
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct MaybePathWithPosition<'a> {
    pub range: Range<usize>,
    pub path: Cow<'a, Path>,
    pub position: Option<RowColumn>,
}

impl<'a> Into<PathWithPosition> for MaybePathWithPosition<'a> {
    fn into(self) -> PathWithPosition {
        let (row, column) = if let Some(RowColumn { row, column, .. }) = self.position {
            (Some(row), column)
        } else {
            (None, None)
        };
        PathWithPosition {
            path: self.path.into_owned(),
            row,
            column,
        }
    }
}

impl<'a> MaybePathWithPosition<'a> {
    pub fn new(range: &Range<usize>, path: Cow<'a, Path>, position: Option<RowColumn>) -> Self {
        Self {
            range: range.clone(),
            path,
            position,
        }
    }

    pub fn hyperlink_range(&self) -> Range<usize> {
        match self.position {
            Some(position) => self.range.start..self.range.end + position.suffix_length,
            None => self.range.clone(),
        }
    }
}

/// Contains well defined substring variations of a MaybePath
/// - With and without stripped common surrounding symbols: `"` `'` `(` `)` `[` `]`
/// - With and without line and column suffix: `:4:2` or `(4,2)`
/// - With and without git diff prefixes: `a/` or `b/`
///
/// # Notes
/// - Surrounding symbols (if any) are stripped before processing the other variations
/// - Git diff prefixes are only processed if surrounding symbols are not present
/// - Row and column are never processed on a git diff variation
///
/// # Examples
///
/// | **original**         | **stripped**       | **git diff**     | **row column**                            |
/// |----------------------|--------------------|------------------|-------------------------------------------|
/// | [a/some/path.rs]:4:2 |                    |                  | [a/some/path.rs]<br>*row = 4, column = 2* |
/// | [a/some/path.rs:4:2] | a/some/path.rs:4:2 |                  | a/some/path.rs<br>*row = 4, column = 2*   |
/// | a/some/path.rs:4:2   |                    | some/path.rs:4:2 | a/some/path.rs<br>*row = 4, column = 2*   |
///
// TODO(davewa): Ideas for improvements
// - In Advance and Exhaustive, only match git diff if line starts with "+++ a/" and treat the whole line as the path.
//
#[derive(Debug)]
pub struct MaybePathVariant {
    variations: Vec<Range<usize>>,
    positioned_variation: Option<(Range<usize>, RowColumn)>,
    /// `a/~/foo.rs` is a valid path on it's own. If we parsed a git diff path like `+++ a/~/foo.rs`, never absolutize it.
    absolutize_home_dir: bool,
}

impl MaybePathVariant {
    pub fn new(text: &str, mut path: Range<usize>) -> Self {
        // We add variations from longest to shortest
        let mut maybe_path = &text[path.clone()];
        let mut positioned_variation = None::<(Range<usize>, RowColumn)>;
        let mut common_symbols_stripped = false;
        let mut absolutize_home_dir = true;

        // Start with full range
        let mut variations = vec![path.clone()];

        // For all of these, path must be at least 2 characters
        if maybe_path.len() > 2 {
            // Strip common surrounding symbols, if any
            if 1 == COMMON_PATH_SURROUNDING_SYMBOLS
                .iter()
                .skip_while(|(start, end)| {
                    !maybe_path.starts_with(*start) || !maybe_path.ends_with(*end)
                })
                .take(1)
                .count()
            {
                common_symbols_stripped = true;
                path = path.start + 1..path.end - 1;
                variations.push(path.clone());
                maybe_path = &text[path.clone()];
            }

            // Git diff parsing--only if we did not strip common symbols
            if !common_symbols_stripped
                && (maybe_path.starts_with('a') || maybe_path.starts_with('b'))
                && maybe_path[1..].starts_with(MAIN_SEPARATOR)
            {
                absolutize_home_dir = false;
                variations.push(path.start + 2..path.end);
                // Note: we do not update maybe_path here because row and column
                // should be processed with the git diff prefixes included, e.g.
                // `a/some/path:4:2` is never interpreted as `some/path`, row = 4, column = 2
                // because git diff never adds a position suffix
            }

            // Row and column parsing
            if let (suffix_start, Some(row), column) =
                PathWithPosition::parse_row_column(maybe_path)
            {
                // TODO(davewa): `PathWithPosition::parse_row_column` just uses a regex search
                // from the start of the path. Since we are only interested in two simple-to-parse
                // suffixes, it seems like a custom parser for those would be better. Or, at least
                // use regex-automata directly to do a reverse match from the end of the path, the
                // custom parsers would be simple and efficient here.
                let suffix_length = maybe_path.len() - suffix_start;
                positioned_variation = Some((
                    path.start..path.end - suffix_length,
                    RowColumn {
                        row,
                        column,
                        suffix_length,
                    },
                ));
            }
        }

        Self {
            variations,
            positioned_variation,
            absolutize_home_dir,
        }
    }

    pub fn relative_variations<'a>(
        &self,
        maybe_path: &'a MaybePath,
        prefix_to_strip: &Path,
    ) -> Vec<MaybePathWithPosition<'a>> {
        let mut variations = Vec::new();
        for range in &self.variations {
            variations.push((range, None));
        }

        if let Some((range, position)) = &self.positioned_variation {
            variations.push((range, Some(position)));
        }

        variations
            .into_iter()
            .filter_map(|(range, position)| {
                let maybe_path = Path::new(maybe_path.text_at(&range));
                maybe_path.is_relative().then(|| {
                    MaybePathWithPosition::new(
                        range,
                        Cow::Borrowed(
                            maybe_path
                                .strip_prefix(prefix_to_strip)
                                .unwrap_or(maybe_path),
                        ),
                        position.cloned(),
                    )
                })
            })
            .collect()
    }

    fn absolutize<'a, 'b>(
        &self,
        maybe_path: &'a MaybePath,
        roots: impl Iterator<Item = &'b Path> + Clone + 'b,
        home_dir: &Option<PathBuf>,
        variation_range: &Range<usize>,
        position: Option<RowColumn>,
    ) -> Vec<MaybePathWithPosition<'a>> {
        let variation_path = Path::new(maybe_path.text_at(&variation_range));
        let mut absolutized = Vec::new();
        if variation_path.is_absolute() {
            absolutized.push(MaybePathWithPosition::new(
                variation_range,
                Cow::Borrowed(variation_path),
                position,
            ));
            return absolutized;
        }

        for root in roots {
            absolutized.push(MaybePathWithPosition::new(
                variation_range,
                Cow::Owned(root.join(variation_path)),
                position,
            ));
        }

        if self.absolutize_home_dir {
            if let Some(home_dir) = home_dir {
                if let Ok(tildeless_path) = variation_path.strip_prefix("~") {
                    absolutized.push(MaybePathWithPosition::new(
                        variation_range,
                        Cow::Owned(home_dir.join(tildeless_path)),
                        position,
                    ));
                }
            }
        }

        absolutized
    }

    pub fn absolutized_variations<'a, 'b>(
        &self,
        maybe_path: &'a MaybePath,
        // TODO(davewa): Pass an array of cwd and worktree.abs_root()'s instead of just cwd
        roots: impl Iterator<Item = &'b Path> + Clone + 'b,
        home_dir: &Option<PathBuf>,
    ) -> Vec<MaybePathWithPosition<'a>> {
        let mut variations = Vec::new();
        for variation_range in &self.variations {
            variations.append(&mut self.absolutize(
                maybe_path,
                roots.clone(),
                home_dir,
                &variation_range,
                None,
            ));
        }

        if let Some((variation_range, position)) = &self.positioned_variation {
            variations.append(&mut self.absolutize(
                maybe_path,
                roots.clone(),
                home_dir,
                variation_range,
                Some(*position),
            ));
        }

        variations
    }
}

#[cfg(test)]
mod tests {
    use std::{mem, path::Path, sync::Arc};

    use crate::WORD_REGEX;

    use super::*;
    use alacritty_terminal::index::Point as AlacPoint;
    use collections::HashMap;
    use fs::{FakeFs, Fs};
    use gpui::TestAppContext;
    use serde_json::json;

    struct ExpectedMaybePathVariations<'a> {
        relative: Vec<MaybePathWithPosition<'a>>,
        absolutized: Vec<MaybePathWithPosition<'a>>,
    }

    type ExpectedMap<'a> = HashMap<PathHyperlinkNavigation, Vec<ExpectedMaybePathVariations<'a>>>;

    macro_rules! maybe_path_with_positions {
        ($variations:ident, $path:literal, $row:literal, $column:literal; $($tail:tt)*) => {
            $variations.push(MaybePathWithPosition::new(&(0..0), Cow::Borrowed(Path::new($path)), Some(RowColumn{ row: $row, column: Some($column), suffix_length: 4 })));
            maybe_path_with_positions!($variations, $($tail)*);
        };

        ($variations:ident, $path:literal, $row:literal; $($tail:tt)*) => {
            $variations.push(MaybePathWithPosition::new(&(0..0), Cow::Borrowed(Path::new($path)), Some(RowColumn{ row: $row, column: None, suffix_length: 2 })));
            maybe_path_with_positions!($variations, $($tail)*);
        };

        ($variations:ident, $path:literal; $($tail:tt)*) => {
            $variations.push(MaybePathWithPosition::new(&(0..0), Cow::Borrowed(Path::new($path)), None));
            maybe_path_with_positions!($variations, $($tail)*);
        };

        ($variations:ident,) => {
        };

        ($($tail:tt)+) => { {
            let mut maybe_path_variations = Vec::new();
            maybe_path_with_positions!(maybe_path_variations, $($tail)+);
            maybe_path_variations
        } };
    }

    macro_rules! relative {
        ($($tail:tt)+) => { maybe_path_with_positions![ $($tail)+ ] }
    }

    macro_rules! absolutized {
        ($($tail:tt)+) => { maybe_path_with_positions![ $($tail)+ ] }
    }

    macro_rules! expected {
        ($($relative:expr, $aboslutized:expr),+) => {
            [ $(ExpectedMaybePathVariations {
                relative: $relative,
                absolutized: $aboslutized,
            },)+ ].into_iter().collect()
        };
    }

    static WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(WORD_REGEX).unwrap());

    #[gpui::test]
    async fn simple_maybe_paths(cx: &mut TestAppContext) {
        let mut trees = vec![
            (
                "/root1",
                json!({
                    "one.txt": "",
                    "two.txt": "",
                }),
            ),
            (
                "/root 2",
                json!({
                    "שיתופית.rs": "",
                }),
            ),
        ];

        let expected = ExpectedMap::from_iter([
            (
                PathHyperlinkNavigation::Default,
                expected![
                    relative![
                        "+++";
                    ],
                    absolutized![
                        "/root 2/+++";
                        "/Some/cool/place/+++";
                    ],
                    relative![
                        "a/~/협동조합";
                        "~/협동조합";
                    ],
                    absolutized![
                        "/root 2/a/~/협동조합";
                        "/Some/cool/place/a/~/협동조합";
                        "/root 2/~/협동조합";
                        "/Some/cool/place/~/협동조합";
                    ],
                    relative![
                        "~/super/cool";
                    ],
                    absolutized![
                        "/root 2/~/super/cool";
                        "/Some/cool/place/~/super/cool";
                        "/Usors/uzer/super/cool";
                    ],
                    relative![
                        "b/path:4:2";
                        "path:4:2";
                        "b/path", 4, 2;
                    ],
                    absolutized![
                        "/root 2/b/path:4:2";
                        "/Some/cool/place/b/path:4:2";
                        "/root 2/path:4:2";
                        "/Some/cool/place/path:4:2";
                        "/root 2/b/path", 4, 2;
                        "/Some/cool/place/b/path", 4, 2;
                    ],
                    relative![
                        "(/root";
                    ],
                    absolutized![
                        "/root 2/(/root";
                        "/Some/cool/place/(/root";
                        // Iff longest_maybe_path_by_surrounding_symbols() hueristic enabled by default:
                        "/root 2/שיתופית.rs";
                    ],
                    relative![
                        "2/שיתופית.rs)";
                    ],
                    absolutized![
                        "/root 2/2/שיתופית.rs)";
                        "/Some/cool/place/2/שיתופית.rs)";
                        // Iff longest_maybe_path_by_surrounding_symbols() hueristic enabled by default:
                        "/root 2/שיתופית.rs";
                    ]
                ],
            ),
            (
                PathHyperlinkNavigation::Advanced,
                expected![
                    relative![
                        "+++";
                        "+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    absolutized![
                        "/root 2/+++";
                        "/Some/cool/place/+++";
                        "/root 2/+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    relative![
                        "a/~/협동조합";
                        "~/협동조합";
                        "a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    absolutized![
                        "/root 2/a/~/협동조합";
                        "/Some/cool/place/a/~/협동조합";
                        "/root 2/~/협동조합";
                        "/Some/cool/place/~/협동조합";
                        "/root 2/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/root 2/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    relative![
                        "~/super/cool";
                        "a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    absolutized![
                        "/root 2/~/super/cool";
                        "/Some/cool/place/~/super/cool";
                        "/Usors/uzer/super/cool";
                        "/root 2/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/root 2/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    relative![
                        "b/path:4:2";
                        "path:4:2";
                        "b/path", 4, 2;
                        "a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    absolutized![
                        "/root 2/b/path:4:2";
                        "/Some/cool/place/b/path:4:2";
                        "/root 2/path:4:2";
                        "/Some/cool/place/path:4:2";
                        "/root 2/b/path", 4, 2;
                        "/Some/cool/place/b/path", 4, 2;
                        "/root 2/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/root 2/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    relative![
                        "(/root";
                        "a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    absolutized![
                        "/root 2/(/root";
                        "/Some/cool/place/(/root";
                        "/root 2/שיתופית.rs";
                        "//root 2/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/root 2/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    relative![
                        "2/שיתופית.rs)";
                        "a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    absolutized![
                        "/root 2/2/שיתופית.rs)";
                        "/Some/cool/place/2/שיתופית.rs)";
                        "/root 2/שיתופית.rs";
                        "/root 2/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/root 2/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ]
                ],
            ),
        ]);

        test_line_maybe_path_variants(
            cx,
            &mut trees,
            &Path::new("/root 2"),
            "+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)",
            &expected,
        )
        .await
    }

    async fn test_line_maybe_path_variants<'a>(
        cx: &mut TestAppContext,
        trees: &mut Vec<(&str, serde_json::Value)>,
        worktree_root: &Path,
        line: &str,
        expected: &ExpectedMap<'a>,
    ) {
        let word_expected = expected.get(&PathHyperlinkNavigation::Default).unwrap();
        for (matched, expected) in WORD_RE.find_iter(&line).zip(word_expected) {
            test_maybe_path(
                cx,
                trees,
                worktree_root,
                line,
                matched.range(),
                PathHyperlinkNavigation::Default,
                &expected,
            )
            .await
        }

        let advanced_expected = expected.get(&PathHyperlinkNavigation::Advanced).unwrap();
        for (matched, expected) in WORD_RE.find_iter(&line).zip(advanced_expected) {
            test_maybe_path(
                cx,
                trees,
                worktree_root,
                line,
                matched.range(),
                PathHyperlinkNavigation::Advanced,
                &expected,
            )
            .await
        }
    }

    async fn check_variations<'a>(
        fs: Arc<FakeFs>,
        actual: &Vec<MaybePathWithPosition<'a>>,
        expected: &Vec<MaybePathWithPosition<'a>>,
    ) {
        let errors: Vec<_> = actual
            .iter()
            .zip(expected.iter())
            .filter(|(actual, expected)| {
                actual.path != expected.path || actual.position != expected.position
            })
            .inspect(|(actual, expected)| {
                println!("  left: {:?}", actual);
                println!(" right: {:?}", expected);
            })
            .collect();

        if actual.len() != expected.len() || !errors.is_empty() {
            println!("Actual:");
            println!("{:#?}", actual);
            println!("Expected:");
            println!("{:#?}", expected);
            assert!(false);
        }

        let mut canonical_paths = Vec::new();
        for MaybePathWithPosition { path, .. } in actual {
            if let Ok(canonical_path) = fs.canonicalize(&path).await {
                canonical_paths.push(canonical_path);
            }
        }

        // TODO(davewa): Metadata (file/dir?)
        // TODO(davewa): Expected navigation targets
        assert_eq!(canonical_paths.len(), 0);
        // TODO(davewa): assert_eq!(canonical_paths[0], expected[0].0)
    }

    async fn test_maybe_path<'a>(
        cx: &mut TestAppContext,
        trees: &mut Vec<(&str, serde_json::Value)>,
        worktree_root: &Path,
        line: &str,
        word: Range<usize>,
        path_hyperlink_navigation: PathHyperlinkNavigation,
        expected: &ExpectedMaybePathVariations<'a>,
    ) {
        // TODO(davewa): Currently we don't test word_match functionality
        let dummy_word_match = Match::new(
            AlacPoint::new(0.into(), 0.into()),
            AlacPoint::new(0.into(), 0.into()),
        );
        let maybe_path = MaybePath::from_line(
            line.to_string(),
            word,
            dummy_word_match,
            path_hyperlink_navigation,
        );

        println!("\nTesting {}", maybe_path);

        let fs = FakeFs::new(cx.executor());
        for tree in trees {
            fs.insert_tree(tree.0, mem::take(&mut tree.1)).await;
        }

        let maybe_path_variants = maybe_path.maybe_path_variants();
        //assert_eq!(maybe_path_variants.len(), 3);

        for maybe_path_variant in &maybe_path_variants {
            println!(
                "Maybe path: {}",
                maybe_path.text_at(&maybe_path_variant.variations[0])
            );
        }

        println!("\nTesting relative {}", maybe_path);

        let actual_relative: Vec<_> = maybe_path_variants
            .iter()
            .map(|maybe_path_variant| {
                maybe_path_variant.relative_variations(&maybe_path, worktree_root)
            })
            .flatten()
            .collect();

        check_variations(Arc::clone(&fs), &actual_relative, &expected.relative).await;

        println!("\nTesting absolutized {}", maybe_path);

        const HOME_DIR: &str = "/Usors/uzer";
        const CWD: &str = "/Some/cool/place";

        let home_dir = Some(Path::new(HOME_DIR).to_path_buf());

        let actual_absolutized: Vec<_> = maybe_path_variants
            .iter()
            .map(|maybe_path_variant| {
                maybe_path_variant.absolutized_variations(
                    &maybe_path,
                    [worktree_root, Path::new(CWD)].into_iter(),
                    &home_dir,
                )
            })
            .flatten()
            .collect();

        check_variations(Arc::clone(&fs), &actual_absolutized, &expected.absolutized).await;
    }
}
