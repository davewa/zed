use std::{
    borrow::Cow,
    fmt::Display,
    ops::Range,
    path::{Path, PathBuf, MAIN_SEPARATOR},
    sync::LazyLock,
};

// TODO(davewa): Change most (all?) info! messages into debug! or trace!
// TODO(davewa): Some APIs may benefit from HashSet for deduplication?
// TODO(davewa): Bugs found while testing this feature:
// - Navigation to line and column navigates to the wrong column when line
// contains unicode. I suspect it is using char's instead of graphemes.
// TODO(davewa): Some ideas for further improvements
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
// Note that "Double-quotes, backslash and control characters are always escaped\
//  regardless of the setting of this variable.". Currently we don't support any
// escaping in paths.

use log::info;
use regex::Regex;
use util::{paths::PathWithPosition, TakeUntilExt};

use crate::terminal_settings::PathHyperlinkNavigation;

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

/// The original matched maybe path from hover or Cmd-click in the terminal.
#[derive(Clone, Debug)]
pub struct MatchedMaybePath {
    /// The terminal hovered or Cmd-clicked word or line containing the word.
    text: String,
    /// Iff `text` is a line, the range of the hovered or Cmd-clicked word within the line,
    /// the full range of `text` otherwise.
    word: Range<usize>,
    /// The user settings `terminal.path_hyperlink_navigation`.
    path_hyperlink_navigation: PathHyperlinkNavigation,
}

impl MatchedMaybePath {
    pub(super) fn from_word(word: String) -> Self {
        let word_len = word.len();
        Self {
            text: word,
            word: 0..word_len,
            path_hyperlink_navigation: PathHyperlinkNavigation::Word,
        }
    }

    pub(super) fn from_line(
        line: String,
        word: Range<usize>,
        path_hyperlink_navigation: PathHyperlinkNavigation,
    ) -> Self {
        Self {
            text: line,
            word,
            path_hyperlink_navigation,
        }
    }

    pub(super) fn word(&self) -> &str {
        &self.text[self.word.clone()]
    }

    pub(super) fn word_range(&self) -> &Range<usize> {
        &self.word
    }

    pub(super) fn text_at(&self, range: &Range<usize>) -> &str {
        &self.text[range.clone()]
    }

    /// All possible word and advanced paths
    pub fn maybe_paths(&self) -> Vec<MaybePath> {
        let mut maybe_paths = Vec::new();
        maybe_paths.push(MaybePath::new(&self.text, self.word.clone()));

        if let Some(longest) = self.longest_maybe_path_by_surrounding_symbols() {
            if longest != self.word {
                maybe_paths.push(MaybePath::new(
                    &self.text,
                    longest.start + 1..longest.end - 1,
                ));
            }
        }

        if self.path_hyperlink_navigation > PathHyperlinkNavigation::Word {
            if let Some(expanded) = self.expanded_maybe_path_by_interior_spaces() {
                maybe_paths.push(MaybePath::new(&self.text, expanded));
            }
        }

        maybe_paths
    }

    /// Returns all maybe paths that match the `terminal.path_hyperlink_navigation_regexes` list of path regexes.
    /// # Notes
    /// The top level here is an iterator so that we can check for timeout.
    // TOOD: This is just an stub to show where path regex user settings would go if we decided to support that.
    pub fn regex_maybe_paths(&self) -> Vec<impl IntoIterator<Item = MaybePath> + '_> {
        // TODO(davewa): Some way to assert we are not called on the main thread...
        info!("Terminal: MaybePaths settings path regexes");
        Vec::<Vec<MaybePath>>::new()
    }

    /// Returns all maybe paths that start on `self.matched` or a word before it and end `self.matched` or a word after it.
    ///
    /// # Notes
    /// The top level here is an iterator so that we can check for timeout.
    pub fn exhaustive_maybe_paths(&self) -> Vec<impl Iterator<Item = MaybePath> + '_> {
        // TODO(davewa): Some way to assert we are not called on the main thread...
        info!("Terminal: MaybePaths exhaustive");

        static WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(super::WORD_REGEX).unwrap());

        let starts: Vec<_> = WORD_RE
            .find_iter(
                if self.path_hyperlink_navigation == PathHyperlinkNavigation::Exhaustive {
                    &self.text[..self.word.end]
                } else {
                    ""
                },
            )
            .map(|match_| match_.start())
            .collect();

        starts
            .into_iter()
            .map(move |start| {
                WORD_RE
                    .find_iter(&self.text[self.word.start..])
                    .map(|match_| match_.end())
                    .map(move |end| MaybePath::new(&self.text, start..self.word.start + end))
            })
            .collect::<Vec<_>>()
    }

    /// Expands the `word` within `text` to the longest matching pair of surrounding symbols.
    /// This is arguably the most common case by far, so we enable it in MaybePathMode::Word.
    pub(super) fn longest_maybe_path_by_surrounding_symbols(&self) -> Option<Range<usize>> {
        let mut longest = None::<Range<usize>>;

        let surrounds_word = |current: &Range<usize>| {
            current.contains(&self.word.start) && current.contains(&(self.word.end - 1))
        };

        for (start, end) in COMMON_PATH_SURROUNDING_SYMBOLS {
            if let (Some(first), Some(last)) = (self.text.find(*start), self.text.rfind(*end)) {
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

        // It's possible that `self.word` is the longest, but that will be processed elsewhere.
        if let Some(longest) = longest {
            if longest != self.word {
                return Some(longest);
            }
        }

        None
    }

    /// Expands the `word` within `text` to the longest potential path using the following heuristic:
    /// - The start is expanded to the start of the first word in `text` which contains a path separator.
    /// - The and is expanded to the end of the last word in `text` which contains a path separator.
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
    /// MaybePathMode::Advanced. If it is not that common in reality, this could be removed. It would
    /// still be handled correctly in MaybePathMode::Exhaustive.
    pub(super) fn expanded_maybe_path_by_interior_spaces(&self) -> Option<Range<usize>> {
        let mut range = self.word.clone();

        if let Some(first_separator) = self.text.find(MAIN_SEPARATOR) {
            if first_separator < range.start {
                let word_start = first_separator
                    - self.text[..first_separator]
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

                info!(
                    "Terminal: Expanded maybe path left: {}",
                    self.text_at(&range)
                );
            }
        }

        if let Some(last_separator) = self.text.rfind(MAIN_SEPARATOR) {
            if last_separator >= range.end {
                let word_end = self.text[last_separator..]
                    .find(PATH_WHITESPACE_CHARS)
                    .unwrap_or(self.text.len());
                range.end = word_end;
                info!(
                    "Terminal: Expanded maybe path right: {}",
                    self.text_at(&range)
                );
            }
        }

        if range != self.word {
            Some(range)
        } else {
            None
        }
    }
}

impl Display for MatchedMaybePath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.word.start != 0 || self.word.end != self.text.len() {
            formatter.write_fmt(format_args!("{:?} «{}»", self, self.word()))
        } else {
            formatter.write_fmt(format_args!("{:?}", self))
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(test, derive(Eq, PartialEq))]
pub struct RowColumn {
    pub row: u32,
    pub column: Option<u32>,
}

/// Like [PathWithPosition], but doesn't require an allocation, and models row and column
/// restrictions directly (cannot have a column without a row).
#[derive(Clone, Debug)]
#[cfg_attr(test, derive(Eq, PartialEq))]
pub struct MaybePathWithPosition<'a> {
    pub path: Cow<'a, Path>,
    pub position: Option<RowColumn>,
}

impl<'a> MaybePathWithPosition<'a> {
    fn new(path: Cow<'a, Path>, position: Option<RowColumn>) -> Self {
        Self { path, position }
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
/// - Row and column are never process on a git diff variation
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
pub struct MaybePath {
    variations: Vec<Range<usize>>,
    positioned_variation: Option<(Range<usize>, RowColumn)>,
    absolutize_home_dir: bool,
}

impl MaybePath {
    pub fn new(text: &str, mut path: Range<usize>) -> Self {
        // We add variation longest to shortest
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

            // Git diff--only if we did not strip common symbols
            if !common_symbols_stripped
                && (maybe_path.starts_with('a') || maybe_path.starts_with('b'))
                && maybe_path[1..].starts_with(MAIN_SEPARATOR)
            {
                absolutize_home_dir = false;
                variations.push(path.start + 2..path.end);
                // Note: we do not update maybe_path here because row and column
                // should be processed with the git diff prefixes included, e.g.
                // `a/some/path:4:2` is never interpreted as `some/path`, row = 4, column = 2
                // because git diff never adds a row and column suffix
            }

            // Row and column parsing
            if let (suffix_start, Some(row), column) =
                PathWithPosition::parse_row_column(maybe_path)
            {
                // TODO(davewa): `PathWithPosition::parse_row_column` just uses a regex search
                // from the start of the path. Since we are only interested in two simple to parse
                // suffixes, it seems like a custom parser for those would be better. Or, at least
                // use regex-automata directly to do a reverse match from the end of the path, the
                // custom parsers would be simple and efficient here.
                positioned_variation = Some((
                    path.start..path.end - (maybe_path.len() - suffix_start),
                    RowColumn { row, column },
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
        matched_maybe_path: &'a MatchedMaybePath,
    ) -> Vec<MaybePathWithPosition<'a>> {
        let mut variations = Vec::new();
        for range in &self.variations {
            variations.push((matched_maybe_path.text_at(range), None));
        }

        if let Some((range, position)) = &self.positioned_variation {
            variations.push((matched_maybe_path.text_at(range), Some(*position)));
        }

        variations
            .into_iter()
            .filter_map(|(variation, position)| {
                let maybe_path = Path::new(variation);
                maybe_path
                    .is_relative()
                    .then(|| MaybePathWithPosition::new(Cow::Borrowed(maybe_path), position))
            })
            .collect()
    }

    fn absolutize<'a>(
        &self,
        matched_maybe_path: &'a MatchedMaybePath,
        cwd: &Option<PathBuf>,
        home_dir: &Option<PathBuf>,
        range: &Range<usize>,
        position: Option<RowColumn>,
    ) -> Vec<MaybePathWithPosition<'a>> {
        let maybe_path = Path::new(matched_maybe_path.text_at(&range));
        let mut absolutized = Vec::new();
        if maybe_path.is_absolute() {
            absolutized.push(MaybePathWithPosition::new(
                Cow::Borrowed(maybe_path),
                position,
            ));
            return absolutized;
        }

        if let Some(cwd) = cwd {
            absolutized.push(MaybePathWithPosition::new(
                Cow::Owned(cwd.join(maybe_path)),
                position,
            ));
        }

        if self.absolutize_home_dir {
            if let Some(home_dir) = home_dir {
                if let Ok(stripped) = maybe_path.strip_prefix("~") {
                    absolutized.push(MaybePathWithPosition::new(
                        Cow::Owned(home_dir.join(stripped)),
                        position,
                    ));
                }
            }
        }

        absolutized
    }

    pub fn absolutized_variations<'a>(
        &self,
        matched_maybe_path: &'a MatchedMaybePath,
        cwd: &Option<PathBuf>,
        home_dir: &Option<PathBuf>,
    ) -> Vec<MaybePathWithPosition<'a>> {
        let mut variations = Vec::new();
        for range in &self.variations {
            variations.append(&mut self.absolutize(
                matched_maybe_path,
                cwd,
                home_dir,
                &range,
                None,
            ));
        }

        if let Some((range, position)) = &self.positioned_variation {
            variations.append(&mut self.absolutize(
                matched_maybe_path,
                cwd,
                home_dir,
                range,
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
            $variations.push(MaybePathWithPosition::new(Cow::Borrowed(Path::new($path)), Some(RowColumn{ row: $row, column: Some($column) })));
            maybe_path_with_positions!($variations, $($tail)*);
        };

        ($variations:ident, $path:literal, $row:literal; $($tail:tt)*) => {
            $variations.push(MaybePathWithPosition::new(Cow::Borrowed(Path::new($path)), Some(RowColumn{ row: $row, column: None })));
            maybe_path_with_positions!($variations, $($tail)*);
        };

        ($variations:ident, $path:literal; $($tail:tt)*) => {
            $variations.push(MaybePathWithPosition::new(Cow::Borrowed(Path::new($path)), None));
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
                    "three.txt": "",
                }),
            ),
        ];

        let expected = ExpectedMap::from_iter([(
            PathHyperlinkNavigation::Advanced,
            expected![
                relative![
                    "+++";
                    "+++ a/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                ],
                absolutized![
                    "/Some/cool/place/+++";
                    "/Some/cool/place/+++ a/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                ],
                relative![
                    "a/~/hello";
                    "~/hello";
                    "a/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                    "~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                ],
                absolutized![
                    "/Some/cool/place/a/~/hello";
                    "/Some/cool/place/~/hello";
                    "/Some/cool/place/a/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                    "/Some/cool/place/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                ],
                relative![
                    "~/super/cool";
                    "a/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                    "~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                ],
                absolutized![
                    "/Some/cool/place/~/super/cool";
                    "/Usors/uzer/super/cool";
                    "/Some/cool/place/a/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                    "/Some/cool/place/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                ],
                relative![
                    "b/path:4:2";
                    "path:4:2";
                    "b/path", 4, 2;
                    "a/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                    "~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                ],
                absolutized![
                    "/Some/cool/place/b/path:4:2";
                    "/Some/cool/place/path:4:2";
                    "/Some/cool/place/b/path", 4, 2;
                    "/Some/cool/place/a/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                    "/Some/cool/place/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                ],
                relative![
                    "(/root";
                    "a/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                    "~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                ],
                absolutized![
                    "/Some/cool/place/(/root";
                    "/root 2/three.txt";
                    "/Some/cool/place/a/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                    "/Some/cool/place/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                ],
                relative![
                    "2/three.txt)";
                    "a/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                    "~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                ],
                absolutized![
                    "/Some/cool/place/2/three.txt)";
                    "/root 2/three.txt";
                    "/Some/cool/place/a/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                    "/Some/cool/place/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)";
                ]
            ],
        )]);

        test_line_maybe_paths(
            cx,
            &mut trees,
            "+++ a/~/hello   ~/super/cool b/path:4:2 (/root 2/three.txt)",
            &expected,
        )
        .await
    }

    async fn test_line_maybe_paths<'a>(
        cx: &mut TestAppContext,
        trees: &mut Vec<(&str, serde_json::Value)>,
        line: &str,
        expected: &ExpectedMap<'a>,
    ) {
        let advanced_expected = expected.get(&PathHyperlinkNavigation::Advanced).unwrap();
        for (matched, expected) in WORD_RE.find_iter(&line).zip(advanced_expected) {
            test_matched_maybe_paths(cx, trees, line, matched.range(), &expected).await
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
            .filter(|(actual, expected)| *actual != *expected)
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

    async fn test_matched_maybe_paths<'a>(
        cx: &mut TestAppContext,
        trees: &mut Vec<(&str, serde_json::Value)>,
        line: &str,
        word: Range<usize>,
        expected: &ExpectedMaybePathVariations<'a>,
    ) {
        let path_hyperlink_navigation = PathHyperlinkNavigation::Exhaustive;
        let matched_maybe_path =
            MatchedMaybePath::from_line(line.to_string(), word, path_hyperlink_navigation);

        println!("\nTesting {}", matched_maybe_path);

        let fs = FakeFs::new(cx.executor());
        for tree in trees {
            fs.insert_tree(tree.0, mem::take(&mut tree.1)).await;
        }

        let maybe_paths = matched_maybe_path.maybe_paths();
        //assert_eq!(maybe_paths.len(), 3);

        for maybe_path in &maybe_paths {
            println!(
                "Maybe path: {}",
                matched_maybe_path.text_at(&maybe_path.variations[0])
            );
        }

        println!("\nTesting relative {}", matched_maybe_path);

        let actual_relative: Vec<_> = maybe_paths
            .iter()
            .map(|maybe_path| maybe_path.relative_variations(&matched_maybe_path))
            .flatten()
            .collect();

        check_variations(Arc::clone(&fs), &actual_relative, &expected.relative).await;

        println!("\nTesting absolutized {}", matched_maybe_path);

        const HOME_DIR: &str = "/Usors/uzer";
        const CWD: &str = "/Some/cool/place";

        let home_dir = Some(Path::new(HOME_DIR).to_path_buf());
        let cwd = Some(Path::new(CWD).to_path_buf());

        let actual_absolutized: Vec<_> = maybe_paths
            .iter()
            .map(|maybe_path| {
                maybe_path.absolutized_variations(&matched_maybe_path, &cwd, &home_dir)
            })
            .flatten()
            .collect();

        check_variations(Arc::clone(&fs), &actual_absolutized, &expected.absolutized).await;
    }
}
