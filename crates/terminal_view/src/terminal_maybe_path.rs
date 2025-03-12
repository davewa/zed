// TODO(davewa): Change most (all?) info! messages into debug! or trace!
// TODO(davewa): Bugs found while testing this feature:
// - Navigation to line and column navigates to the wrong column when line
// contains unicode. I suspect it is using char's instead of graphemes.
// - [ ] When sending NewNaviagationTarget(None), we were not also clearning last_hovered_word, but we should.
// - [ ] When holding Cmd, and the terminal output is scrolling, the link is highlighted, but after scrolling
// away, it is still highlighting whatever new text is where the original link was.
// - [ ] When holding Cmd, and the terminal contents are not scrolling, but a command is running that is adding
// output off screen, the hovered link move down one line for each new line of content added off screen
// - [ ] Tooltips don't render markdown tables correctly
//

// TODO(davewa) TASK LIST
//
// - [ ] Add Exhaustive expected to unit test
// - [ ] Add a ton more targeted unit test cases
// - [ ] Fix tests on Windows
// - [ ] Test file:// Urls
// - [ ] Test non-file:// Urls
// - [ ] Implement background (and main thread?) timeout for searching variations
//   - Use the executor's? timer so that test virtual time works correctly

use regex::Regex;
use std::{
    borrow::Cow,
    fmt::Display,
    iter,
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};
use terminal::terminal_hovered_maybe_path::{
    longest_surrounding_symbols_match, path_regex_match, preapproved_path_hyperlink_regexes,
    HoveredMaybePath, COMMON_PATH_SURROUNDING_SYMBOLS, MAIN_SEPARATORS,
};
#[cfg(doc)]
use terminal::terminal_settings::PathHyperlinkNavigation;
use util::paths::PathWithPosition;

fn word_regex() -> &'static Regex {
    static WORD_REGEX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(terminal::WORD_REGEX).unwrap());
    &WORD_REGEX
}

/// The original matched maybe path from hover or Cmd-click in the terminal
#[derive(Clone, Debug)]
pub struct MaybePath {
    line: String,
    hovered_word_range: Range<usize>,
    path_hyperlink_regexes: Arc<Vec<Regex>>,
}

impl Display for MaybePath {
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

impl MaybePath {
    pub(super) fn from_hovered_maybe_path(
        hovered_maybe_path: &HoveredMaybePath,
        path_regexes: Arc<Vec<Regex>>,
    ) -> Self {
        Self {
            line: hovered_maybe_path.line.clone(),
            hovered_word_range: hovered_maybe_path.hovered_word_range.clone(),
            path_hyperlink_regexes: path_regexes,
        }
    }

    #[cfg(test)]
    fn new(line: &str, hovered_word_range: Range<usize>, path_regexes: Arc<Vec<Regex>>) -> Self {
        Self {
            line: line.to_string(),
            hovered_word_range,
            path_hyperlink_regexes: path_regexes,
        }
    }

    const MAX_MAIN_THREAD_PREFIX_WORDS: usize = 2;
    const MAX_BACKGROUND_THREAD_PREFIX_WORDS: usize = usize::MAX;

    /// All [PathHyperlinkNavigation::Default] maybe path variants. These
    /// need to be kept to a small well-defined set of variants.
    ///
    /// On the main thread, these will be checked against worktrees only.
    ///
    /// *Local Only*--If no worktree match found they will also be checked for existence in the workspace's real file
    /// system on the background thread.
    pub fn default_maybe_path_variants(&self) -> impl Iterator<Item = MaybePathVariant<'_>> + '_ {
        [MaybePathVariant::new(
            &self.line,
            self.hovered_word_range.clone(),
        )]
        .into_iter()
        .chain(iter::once_with(|| self.longest_surrounding_symbols_variants()).flatten())
        .chain(
            iter::once_with(|| {
                self.path_regex_variants(preapproved_path_hyperlink_regexes())
                    .into_iter()
            })
            .flatten(),
        )
        .chain(
            iter::once_with(|| {
                self.line_ends_in_a_path_maybe_path_variants(0, Self::MAX_MAIN_THREAD_PREFIX_WORDS)
                    .collect::<Vec<_>>()
                    .into_iter()
                    // One prefix stripped is the most likely path
                    .rev()
            })
            .flatten(),
        )
    }

    /// All [PathHyperlinkNavigation::Advanced] maybe path variants.
    pub fn advanced_maybe_path_variants(&self) -> impl Iterator<Item = MaybePathVariant<'_>> + '_ {
        self.path_regex_variants(&self.path_hyperlink_regexes)
            .into_iter()
            .chain(self.line_ends_in_a_path_maybe_path_variants(
                Self::MAX_MAIN_THREAD_PREFIX_WORDS,
                Self::MAX_BACKGROUND_THREAD_PREFIX_WORDS,
            ))
    }

    /// [PathHyperlinkNavigation::Default] variant for the longest surrounding symbols match, if any
    fn longest_surrounding_symbols_variants(
        &self,
    ) -> impl Iterator<Item = MaybePathVariant<'_>> + '_ {
        if let Some(surrounding_range) =
            longest_surrounding_symbols_match(&self.line, &self.hovered_word_range)
        {
            if surrounding_range != self.hovered_word_range {
                return vec![MaybePathVariant::new(
                    &self.line,
                    surrounding_range.start + 1..surrounding_range.end - 1,
                )]
                .into_iter();
            }
        }

        vec![].into_iter()
    }

    fn path_regex_variants(&self, path_regexes: &Vec<Regex>) -> Option<MaybePathVariant<'_>> {
        if let Some(path) = path_regex_match(&self.line, &self.hovered_word_range, path_regexes) {
            Some(MaybePathVariant::new(&self.line, path.clone()))
        } else {
            None
        }
    }

    /// [PathHyperlinkNavigation::Advanced] maybe path variants that start on the hovered word or a
    /// word before it and end at the end of the line.
    fn line_ends_in_a_path_maybe_path_variants(
        &self,
        start_prefix_words: usize,
        max_prefix_words: usize,
    ) -> impl Iterator<Item = MaybePathVariant<'_>> + '_ {
        // TODO(davewa): Some way to assert we are not called on the main thread...
        word_regex()
            .find_iter(&self.line[..self.hovered_word_range.end])
            .skip(start_prefix_words)
            .take(max_prefix_words)
            .map(|match_| MaybePathVariant::new(&self.line, match_.start()..self.line.len()))
    }

    /// All [PathHyperlinkNavigation::Exhaustive] maybe path variants that start on the hovered word or a
    /// word before it and end the hovered word or a word after it.
    pub fn exhaustive_maybe_path_variants(
        &self,
    ) -> impl Iterator<Item = MaybePathVariant<'_>> + '_ {
        // TODO(davewa): Some way to assert we are not called on the main thread...
        let starts = word_regex()
            .find_iter(&self.line[..self.hovered_word_range.end])
            .map(|match_| match_.start());

        starts
            .into_iter()
            .map(move |start| {
                word_regex()
                    .find_iter(&self.line[self.hovered_word_range.start..])
                    .map(|match_| match_.end())
                    .map(move |end| {
                        MaybePathVariant::new(
                            &self.line,
                            start..self.hovered_word_range.start + end,
                        )
                    })
            })
            .flatten()
    }
}

/// Line and column suffix information
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct RowColumn {
    pub row: u32,
    pub column: Option<u32>,
    /// [MaybePathWithPosition::range] stores the range of the path after any line and column
    /// suffix has been stripped. Storing the length of the suffix here allows us to linkify it
    /// correctly.
    suffix_length: usize,
}

/// Like [PathWithPosition], with enhancements for [MaybePath] processing
///
/// Specifically, we:
/// - Don't require allocation
/// - Model row and column restrictions directly (cannot have a column without a row)
/// - Include our range within our source [MaybePath], and the length of the line and column suffix
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct MaybePathWithPosition<'a> {
    pub path: Cow<'a, Path>,
    pub position: Option<RowColumn>,
    range: Range<usize>,
}

impl<'a> MaybePathWithPosition<'a> {
    fn new(range: &Range<usize>, path: Cow<'a, Path>, position: Option<RowColumn>) -> Self {
        Self {
            path,
            position,
            range: range.clone(),
        }
    }

    pub fn into_owned_with_path(self, path: Cow<'static, Path>) -> MaybePathWithPosition<'static> {
        MaybePathWithPosition { path, ..self }
    }

    pub fn into_owned(self) -> MaybePathWithPosition<'static> {
        MaybePathWithPosition {
            path: Cow::Owned(self.path.into_owned()),
            ..self
        }
    }

    pub fn hyperlink_range(&self) -> Range<usize> {
        match self.position {
            Some(position) => self.range.start..self.range.end + position.suffix_length,
            None => self.range.clone(),
        }
    }
}

/// A contiguous sequence of words which includes the hovered word of a [MaybePath]
///
/// Yields all substring variations of the contained path:
/// - With and without stripped common surrounding symbols: `"` `'` `(` `)` `[` `]`
/// - With and without line and column suffix: `:4:2` or `(4,2)`
/// - With and without git diff prefixes: `a/` or `b/`
///
/// # Notes
/// - The original path is always the first variation
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
// Note: The above table renders perfectly in docs, but currenlty does not render correctly in the tooltip in Zed.
#[derive(Debug)]
pub struct MaybePathVariant<'a> {
    line: &'a str,
    variations: Vec<Range<usize>>,
    positioned_variation: Option<(Range<usize>, RowColumn)>,
    /// `a/~/foo.rs` is a valid path on it's own. If we parsed a git diff path like `+++ a/~/foo.rs`, never absolutize it.
    absolutize_home_dir: bool,
}

impl<'a> MaybePathVariant<'a> {
    pub fn new(line: &'a str, mut path: Range<usize>) -> Self {
        // We add variations from longest to shortest
        let mut maybe_path = &line[path.clone()];
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
                // Insert at the front, since stripped varation is more likely to be path
                variations.insert(0, path.clone());
                maybe_path = &line[path.clone()];
            }

            // Git diff parsing--only if we did not strip common symbols
            if !common_symbols_stripped
                && (maybe_path.starts_with('a') || maybe_path.starts_with('b'))
                && maybe_path[1..].starts_with(MAIN_SEPARATORS)
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
            line,
            variations,
            positioned_variation,
            absolutize_home_dir,
        }
    }

    pub fn relative_variations(
        &self,
        prefix_to_strip: Option<&Path>,
    ) -> Vec<MaybePathWithPosition<'a>> {
        let mut variations = Vec::new();

        // The positioned variation, if any, is the most likely path
        if let Some((range, position)) = &self.positioned_variation {
            variations.push((range, Some(position)));
        }

        for range in &self.variations {
            variations.push((range, None));
        }

        variations
            .into_iter()
            .filter_map(|(range, position)| {
                let maybe_path = Path::new(&self.line[range.clone()]);
                maybe_path.is_relative().then(|| {
                    MaybePathWithPosition::new(
                        range,
                        Cow::Borrowed(prefix_to_strip.map_or(maybe_path, |prefix_to_strip| {
                            maybe_path
                                .strip_prefix(prefix_to_strip)
                                .unwrap_or(maybe_path)
                        })),
                        position.cloned(),
                    )
                })
            })
            .collect()
    }

    fn absolutize<'b>(
        &self,
        roots: impl Iterator<Item = &'b Path> + Clone + 'b,
        home_dir: &PathBuf,
        variation_range: &Range<usize>,
        position: Option<RowColumn>,
    ) -> Vec<MaybePathWithPosition<'a>> {
        let variation_path = Path::new(&self.line[variation_range.clone()]);
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
            if let Ok(tildeless_path) = variation_path.strip_prefix("~") {
                absolutized.push(MaybePathWithPosition::new(
                    variation_range,
                    Cow::Owned(home_dir.join(tildeless_path)),
                    position,
                ));
            }
        }

        absolutized
    }

    pub fn absolutized_variations<'b>(
        &self,
        roots: impl Iterator<Item = &'b Path> + Clone + 'b,
        home_dir: &PathBuf,
    ) -> Vec<MaybePathWithPosition<'a>> {
        let mut variations = Vec::new();
        for variation_range in &self.variations {
            variations.append(&mut self.absolutize(
                roots.clone(),
                home_dir,
                &variation_range,
                None,
            ));
        }

        if let Some((variation_range, position)) = &self.positioned_variation {
            variations.append(&mut self.absolutize(
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
    use super::*;
    use collections::HashMap;
    use fs::{FakeFs, Fs};
    use gpui::TestAppContext;
    use serde_json::json;
    use std::{path::Path, sync::Arc};
    use terminal::terminal_settings::PathHyperlinkNavigation;

    struct ExpectedMaybePathVariations<'a> {
        relative: Vec<MaybePathWithPosition<'a>>,
        absolutized: Vec<MaybePathWithPosition<'a>>,
        open_target: Option<MaybePathWithPosition<'static>>,
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

        () => { Vec::new() };
    }

    macro_rules! relative {
        ($($tail:tt)*) => { maybe_path_with_positions![ $($tail)* ] }
    }

    macro_rules! absolutized {
        ($($tail:tt)*) => { maybe_path_with_positions![ $($tail)* ] }
    }

    macro_rules! open_target {
        ($path:literal, $row:literal, $column:literal) => {
            Some(MaybePathWithPosition::new(
                &(0..0),
                Cow::Borrowed(Path::new($path)),
                Some(RowColumn {
                    row: $row,
                    column: Some($column),
                    suffix_length: 4,
                }),
            ))
        };
        ($path:literal, $row:literal) => {
            Some(MaybePathWithPosition::new(
                &(0..0),
                Cow::Borrowed(Path::new($path)),
                Some(RowColumn {
                    row: $row,
                    column: None,
                    suffix_length: 2,
                }),
            ))
        };
        ($path:literal) => {
            Some(MaybePathWithPosition::new(
                &(0..0),
                Cow::Borrowed(Path::new($path)),
                None,
            ))
        };
    }

    macro_rules! expected {
        ($relative:expr, $absolutized:expr) => {
            ExpectedMaybePathVariations {
                relative: $relative,
                absolutized: $absolutized,
                open_target: None,
            }
        };

        ($relative:expr, $absolutized:expr, $open_target:expr) => {
            ExpectedMaybePathVariations {
                relative: $relative,
                absolutized: $absolutized,
                open_target: $open_target,
            }
        };
    }

    #[gpui::test]
    async fn simple_maybe_paths(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/root1",
            json!({
                "one.txt": "",
                "two.txt": "",
            }),
        )
        .await;
        fs.insert_tree(
            "/root 2",
            json!({
                "שיתופית.rs": "",
            }),
        )
        .await;
        let mut expected = ExpectedMap::from_iter([]);

        expected.insert(
            PathHyperlinkNavigation::Default,
            Vec::from_iter([
                expected!{
                    relative![
                        "+++";
                        "+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    absolutized![
                        "/root 2/+++";
                        "/Some/cool/place/+++";
                        "/root 2/+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ]
                },
                expected!{
                    relative![
                        "a/~/협동조합";
                        "~/협동조합";
                        "a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
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
                        "/root 2/+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ]
                },
                expected!{
                    relative![
                        "~/super/cool";
                        "a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    absolutized![
                        "/root 2/~/super/cool";
                        "/Some/cool/place/~/super/cool";
                        "/Usors/uzer/super/cool";
                        "/root 2/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/root 2/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/root 2/+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ]
                },
                expected!{
                    relative![
                        "b/path", 4, 2;
                        "b/path:4:2";
                        "path:4:2";
                        "b/path", 4;
                        "b/path:4";
                        "path:4";
                        "a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        ],
                    absolutized![
                        "/root 2/b/path:4:2";
                        "/Some/cool/place/b/path:4:2";
                        "/root 2/path:4:2";
                        "/Some/cool/place/path:4:2";
                        "/root 2/b/path", 4, 2;
                        "/Some/cool/place/b/path", 4, 2;
                        "/root 2/b/path:4";
                        "/Some/cool/place/b/path:4";
                        "/root 2/path:4";
                        "/Some/cool/place/path:4";
                        "/root 2/b/path", 4;
                        "/Some/cool/place/b/path", 4;
                        "/root 2/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/root 2/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/root 2/+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ]
                },
                expected!{
                    relative![
                        "(/root";
                        "a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    absolutized![
                        "/root 2/(/root";
                        "/Some/cool/place/(/root";
                        "/root 2/שיתופית.rs";
                        "/root 2/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/root 2/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/root 2/+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    open_target!("/root 2/שיתופית.rs")
                },
                expected!{
                    relative![
                        "2/שיתופית.rs)";
                        "a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    absolutized![
                        "/root 2/2/שיתופית.rs)";
                        "/Some/cool/place/2/שיתופית.rs)";
                        "/root 2/שיתופית.rs";
                        "/root 2/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/root 2/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/root 2/+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        "/Some/cool/place/+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                    ],
                    open_target!("/root 2/שיתופית.rs")
                }
            ].into_iter()),
        );

        expected.insert(
            PathHyperlinkNavigation::Advanced,
            Vec::from_iter(
                [
                    expected! {
                        relative![
                        ],
                        absolutized![
                        ]
                    },
                    expected! {
                        relative![
                        ],
                        absolutized![
                        ]
                    },
                    expected! {
                        relative![
                            "~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        ],
                        absolutized![
                            "/root 2/~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                            "/Some/cool/place/~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                            "/Usors/uzer/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                        ]
                    },
                    expected! {
                        relative![
                            "~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                            "b/path:4:2 (/root 2/שיתופית.rs)";
                            "path:4:2 (/root 2/שיתופית.rs)";
                        ],
                        absolutized![
                            "/root 2/~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                            "/Some/cool/place/~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                            "/Usors/uzer/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                            "/root 2/b/path:4:2 (/root 2/שיתופית.rs)";
                            "/Some/cool/place/b/path:4:2 (/root 2/שיתופית.rs)";
                            "/root 2/path:4:2 (/root 2/שיתופית.rs)";
                            "/Some/cool/place/path:4:2 (/root 2/שיתופית.rs)";
                        ]
                    },
                    expected! {
                        relative![
                            "~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                            "b/path:4:2 (/root 2/שיתופית.rs)";
                            "path:4:2 (/root 2/שיתופית.rs)";
                            "(/root 2/שיתופית.rs)";
                        ],
                        absolutized![
                            "/root 2/~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                            "/Some/cool/place/~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                            "/Usors/uzer/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                            "/root 2/b/path:4:2 (/root 2/שיתופית.rs)";
                            "/Some/cool/place/b/path:4:2 (/root 2/שיתופית.rs)";
                            "/root 2/path:4:2 (/root 2/שיתופית.rs)";
                            "/Some/cool/place/path:4:2 (/root 2/שיתופית.rs)";
                            "/root 2/שיתופית.rs";
                            "/root 2/(/root 2/שיתופית.rs)";
                            "/Some/cool/place/(/root 2/שיתופית.rs)";
                        ],
                        open_target!("/root 2/שיתופית.rs")
                    },
                    expected! {
                        relative![
                            "~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                            "b/path:4:2 (/root 2/שיתופית.rs)";
                            "path:4:2 (/root 2/שיתופית.rs)";
                            "(/root 2/שיתופית.rs)";
                            "2/שיתופית.rs)";
                            ],
                            absolutized![
                                "/root 2/~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                                "/Some/cool/place/~/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                                "/Usors/uzer/super/cool b/path:4:2 (/root 2/שיתופית.rs)";
                                "/root 2/b/path:4:2 (/root 2/שיתופית.rs)";
                                "/Some/cool/place/b/path:4:2 (/root 2/שיתופית.rs)";
                                "/root 2/path:4:2 (/root 2/שיתופית.rs)";
                                "/Some/cool/place/path:4:2 (/root 2/שיתופית.rs)";
                                "/root 2/שיתופית.rs";
                                "/root 2/(/root 2/שיתופית.rs)";
                                "/Some/cool/place/(/root 2/שיתופית.rs)";
                                "/root 2/2/שיתופית.rs)";
                                "/Some/cool/place/2/שיתופית.rs)";
                        ],
                        open_target!("/root 2/שיתופית.rs")
                    },
                ]
                .into_iter(),
            ),
        );

        test_line_maybe_path_variants(
            fs,
            &Path::new("/root 2"),
            "+++ a/~/협동조합   ~/super/cool b/path:4:2 (/root 2/שיתופית.rs)",
            &expected,
        )
        .await
    }

    async fn test_line_maybe_path_variants<'a>(
        fs: Arc<FakeFs>,
        worktree_root: &Path,
        line: &str,
        expected: &ExpectedMap<'a>,
    ) {
        let custom_path_regexes = Arc::new(Vec::new());
        let word_expected = expected.get(&PathHyperlinkNavigation::Default).unwrap();
        for (matched, expected) in word_regex().find_iter(&line).zip(word_expected) {
            let maybe_path =
                MaybePath::new(line, matched.range(), Arc::clone(&custom_path_regexes));
            println!("\n\nTesting Default: {}", maybe_path);

            test_maybe_path(
                Arc::clone(&fs),
                worktree_root,
                &maybe_path,
                || maybe_path.default_maybe_path_variants(),
                &expected,
            )
            .await
        }

        let advanced_expected = expected.get(&PathHyperlinkNavigation::Advanced).unwrap();
        for (matched, expected) in word_regex().find_iter(&line).zip(advanced_expected) {
            let maybe_path =
                MaybePath::new(line, matched.range(), Arc::clone(&custom_path_regexes));
            println!("\n\nTesting Advanced: {}", maybe_path);

            test_maybe_path(
                Arc::clone(&fs),
                worktree_root,
                &maybe_path,
                || maybe_path.advanced_maybe_path_variants(),
                &expected,
            )
            .await
        }
    }

    fn check_variations<'a>(
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
            println!("\nActual:");
            actual
                .iter()
                .for_each(|MaybePathWithPosition { path, .. }| println!("    {path:?};"));
            println!("\nExpected:");
            expected
                .iter()
                .for_each(|MaybePathWithPosition { path, .. }| println!("    {path:?};"));
            assert!(false);
        }
    }

    async fn test_maybe_path<'a, VariantIterator>(
        fs: Arc<FakeFs>,
        worktree_root: &Path,
        maybe_path: &'a MaybePath,
        variants: impl Fn() -> VariantIterator,
        expected: &ExpectedMaybePathVariations<'a>,
    ) where
        VariantIterator: Iterator<Item = MaybePathVariant<'a>> + 'a,
    {
        //assert_eq!(variants().len(), 3);

        println!(
            "\nVariants: {:#?}",
            variants()
                .map(|maybe_path_variant| maybe_path_variant
                    .relative_variations(None)
                    .into_iter()
                    .map(|variation| &maybe_path.line[variation.range.clone()])
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );

        println!("\nTesting Relative: strip_prefix = {worktree_root:?}");

        let actual_relative: Vec<_> = variants()
            .map(|maybe_path_variant| maybe_path_variant.relative_variations(Some(worktree_root)))
            .flatten()
            .collect();

        check_variations(&actual_relative, &expected.relative);

        const HOME_DIR: &str = "/Usors/uzer";
        const CWD: &str = "/Some/cool/place";

        let home_dir = Path::new(HOME_DIR).to_path_buf();
        let roots = [worktree_root, Path::new(CWD)];

        println!("\nTesting Absolutized: home_dir: {home_dir:?}, roots: {roots:?}",);

        let actual_absolutized: Vec<_> = variants()
            .map(|maybe_path_variant| {
                maybe_path_variant.absolutized_variations(roots.into_iter(), &home_dir)
            })
            .flatten()
            .collect();

        check_variations(&actual_absolutized, &expected.absolutized);

        let actual_open_target = async || {
            for maybe_path_with_position in &actual_absolutized {
                let normalized_path = fs::normalize_path(&maybe_path_with_position.path);
                assert_eq!(
                    maybe_path_with_position.path, normalized_path,
                    "Normalized was not a noop"
                );
                if let Ok(Some(_metadata)) =
                    fs.metadata(&fs::normalize_path(&normalized_path)).await
                {
                    // TODO(davewa): assert_eq!(metadata.is_dir, expected_open_target.is_dir)
                    return Some(MaybePathWithPosition {
                        path: Cow::Owned(maybe_path_with_position.path.to_path_buf()),
                        ..maybe_path_with_position.clone()
                    });
                }
            }

            None
        };

        if let Some(actual_open_target) = actual_open_target().await {
            if let Some(expected_open_target) = expected.open_target.as_ref() {
                assert_eq!(
                    *expected_open_target.path, actual_open_target.path,
                    "Mismatched open target paths"
                );
                assert_eq!(
                    expected_open_target.position, actual_open_target.position,
                    "Mismatched open target positions"
                );
            } else {
                assert!(
                    false,
                    "Expected no open target, but found: {:?}",
                    actual_open_target
                );
            }
        } else if let Some(expected_open_target) = expected.open_target.as_ref() {
            assert!(
                false,
                "No open target found, expected: {:?}",
                expected_open_target
            );
        }
    }
}
