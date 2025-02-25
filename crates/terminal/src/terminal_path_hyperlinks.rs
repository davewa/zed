use std::{
    borrow::Cow,
    fmt::Display,
    iter,
    ops::Range,
    path::{Path, PathBuf, MAIN_SEPARATOR},
    str,
};

// TODO(davewa): Figure out why only maybe_path_word is underlined as a link now.
// TODO(davewa): Change most (all?) info! messages into debug! or trace!

use log::info;
use util::{paths::PathWithPosition, TakeUntilExt};

// TODO(davewa): `file:` IRIs
/// `file:` IRIs are treated as local paths
const _FILE_IRI_PREFIX: &str = "file:";

/// These are valid in paths and are not matched by [WORD_REGEX](terminal::WORD_REGEX).
/// We use them to find potential path words with a path line.
///
/// - **`\u{c}`** is **`\f`** (form feed - new page)
/// - **`\u{b}`** is **`\v`** (vertical tab)
///
/// See [C++ Escape sequences](https://en.cppreference.com/w/cpp/language/escape)
const PATH_WHITESPACE_CHARS: &str = "\t\u{c}\u{b} ";

#[derive(Clone, Debug)]
pub enum MaybePathMode {
    Default,
    Advanced(Range<usize>),
    Exhaustive(Range<usize>),
}

#[derive(Clone, Debug)]
pub struct MaybePath {
    /// The terminal text containing maybe_paths.
    pub(self) line: String,
    /// From `terminal.enable_enhanced_path_hyperlinks` setting.
    pub(self) mode: MaybePathMode,
}

impl MaybePath {
    pub fn new(line: String, mode: MaybePathMode) -> MaybePath {
        MaybePath { line, mode }
    }

    pub fn range(&self) -> Range<usize> {
        match &self.mode {
            MaybePathMode::Default => 0..self.line.len(),
            MaybePathMode::Advanced(range) | MaybePathMode::Exhaustive(range) => range.clone(),
        }
    }

    pub fn maybe_path_word(&self) -> &str {
        &self.line[self.range()]
    }

    pub fn enhanced_maybe_path(&self, range: &Range<usize>) -> &str {
        &self.line[range.clone()]
    }

    /// Computes all the possible paths in `line`,
    pub fn compute_maybe_paths_variations(&self) -> Vec<MaybePathVariations> {
        let mut maybe_paths_variations =
            vec![MaybePathVariations::new(&self.line, self.range().clone())];

        if let MaybePathMode::Advanced(_range) | MaybePathMode::Exhaustive(_range) = &self.mode {
            if let Some(expanded) = self.expanded_maybe_path() {
                maybe_paths_variations.push(MaybePathVariations::new(&self.line, expanded));
            }

            // TODO(davewa): Advanced expanded_outer_common_surrounding_symbols
        }

        if let MaybePathMode::Exhaustive(_range) = &self.mode {
            // TODO(davewa): Exhaustive

            // /// Looks for a path under `cursor`
            // // Note: Does not handle paths that start or end in space(s) or that do not start
            // // or end on a word match boundary--except for those ending with a line and column
            // // TODO: paths with surrounding ' " ( ) [ ]
            // // Note: Once we handle the surrounding delimiter cases, paths that start or end in
            // // space(s) _will_ be handled.
            // fn find_path_hyperlink(
            //     &mut self,
            //     term: &mut Term<ZedListener>,
            //     cursor: AlacPoint,
            // ) -> Option<RangeInclusive<AlacPoint>> {
            //     let line_start = term.line_search_left(cursor);
            //     let line_end = term.line_search_right(cursor);

            //     let line_words = RegexIter::new(
            //         line_start,
            //         line_end,
            //         AlacDirection::Right,
            //         term,
            //         &mut self.word_regex,
            //     )
            //     .into_iter()
            //     .collect::<Vec<_>>();

            //     let mut longest_path_found = cursor..=cursor.sub(term, Boundary::Grid, 1);

            //     for start_word in &line_words {
            //         if start_word.start().cmp(&cursor) == Ordering::Greater {
            //             // we are past the word under the cursor, stop.
            //             break;
            //         }

            //         for end_word in line_words.iter().rev() {
            //             if end_word.end().cmp(&cursor) == Ordering::Less {
            //                 // we are past the word under the cursor, stop.
            //                 break;
            //             }

            //             if longest_path_found.contains_inclusive(&(*start_word.start()..*end_word.end())) {
            //                 // We have already found a path that is longer than any
            //                 // path starting with the current start_word, so we are done.
            //                 return Some(*longest_path_found.start()..=*longest_path_found.end());
            //             }

            //             // Otherwise, we have a potential path that is longer than the current longest_path_found,
            //             // Check if it exists, and if it does, make it the new longest_path_found.

            //             // Check for potential :<line>:<column> endings before fs::exists()
            //             let maybe_path = term.bounds_to_string(*start_word.start(), *end_word.end());
            //             let maybe_path_no_line_column =
            //                 if let Some(captures) = self.line_column_regex.captures(&maybe_path) {
            //                     &maybe_path[0..maybe_path.len() - captures["line_column"].len()]
            //                 } else {
            //                     &maybe_path[0..]
            //                 };

            //             match fs::exists(Path::new(&maybe_path_no_line_column)) {
            //                 Ok(true) => {
            //                     longest_path_found = *start_word.start()..=*end_word.end();
            //                     debug!("Updated longest path found to: {}", maybe_path);
            //                     // The rest can only be shorter.
            //                     break;
            //                 }
            //                 _ => {
            //                     trace!(
            //                         "Not an error, no file found for path: {}",
            //                         maybe_path_no_line_column
            //                     )
            //                 }
            //             }
            //         }
            //     }

            //     if !longest_path_found.is_empty() {
            //         return Some(*longest_path_found.start()..=*longest_path_found.end());
            //     }

            //     None
            // }
        }

        maybe_paths_variations
    }

    /// Expands the `range` within `line` to the longest potential path.
    /// The start is expanded to the start of the first word in line which contains a path separator.
    /// The and is expanded to the end of the last word which contains a path separator.
    ///
    /// # Example
    ///
    /// _(maybe_path is_ **bold** _)_
    ///
    /// _before:_ this is\ an **example\of** how\this works
    ///
    /// _after:_ this **is\ an example\of how\this** works
    fn expanded_maybe_path(&self) -> Option<Range<usize>> {
        let mut range = self.range();
        if let Some(first_separator) = self.line.find(MAIN_SEPARATOR) {
            if first_separator < range.start {
                let word_start = first_separator
                    - self.line[..first_separator]
                        .chars()
                        .rev()
                        .take_until(|&c| PATH_WHITESPACE_CHARS.contains(c))
                        .count();

                if word_start == 0 {
                    // We stopped at the start of the line, that is the word_start.
                    range.start = word_start;
                } else {
                    // We stopped at a whitespace character, advance by 1
                    range.start = word_start + 1;
                }

                info!(
                    "Terminal: Expanded maybe path left: {}",
                    self.enhanced_maybe_path(&range)
                );
            }
        }

        if let Some(last_separator) = self.line.rfind(MAIN_SEPARATOR) {
            if last_separator >= range.end {
                let word_end = self.line[last_separator..]
                    .find(PATH_WHITESPACE_CHARS)
                    .unwrap_or(self.line.len());
                range.end = word_end;
                info!(
                    "Terminal: Expanded maybe path right: {}",
                    self.enhanced_maybe_path(&range)
                );
            }
        }

        if range != self.range() {
            Some(range)
        } else {
            None
        }
    }
}

impl Display for MaybePath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.mode {
            MaybePathMode::Default => formatter.write_fmt(format_args!("{:?}", self)),
            _ => formatter.write_fmt(format_args!("{:?} «{}»", self, self.maybe_path_word())),
        }
    }
}

// TODO(davewa): Why do these need Eq, PartialEq? ANSWER: Test asserts. Maybe only for cfg!(test)?
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineColumn {
    pub line: u32,
    pub column: Option<u32>,
}

#[derive(Debug)]
pub struct MaybePathVariations {
    variations: Vec<Range<usize>>,
    positioned_variation: Option<(Range<usize>, LineColumn)>,
}

impl MaybePathVariations {
    pub fn new(line: &str, mut range: Range<usize>) -> Self {
        // We add variation longest to shortest
        let mut maybe_path = &line[range.clone()];
        let mut positioned_variation = None::<(Range<usize>, LineColumn)>;

        // Start with full range
        let mut variations = vec![range.clone()];

        // For all of these, path must be at least 2 characters
        if maybe_path.len() > 2 {
            const COMMON_PATH_SURROUNDING_SYMBOLS: &[(char, char)] =
                &[('"', '"'), ('\'', '\''), ('[', ']'), ('(', ')')];

            // Strip common surrounding symbols, if any
            if 1 == COMMON_PATH_SURROUNDING_SYMBOLS
                .iter()
                .skip_while(|(start, end)| {
                    !maybe_path.starts_with(*start) || !maybe_path.ends_with(*end)
                })
                .take(1)
                .count()
            {
                range = range.start + 1..range.end - 1;
                variations.push(range.clone());
                maybe_path = &line[range.clone()];
            }

            // Git diff and line column, mutually exclusive
            if (maybe_path.starts_with('a') || maybe_path.starts_with('b'))
                && maybe_path[1..].starts_with(MAIN_SEPARATOR)
            {
                variations.push(range.start + 2..range.end);
            } else if let (suffix_start, Some(line), column) =
                PathWithPosition::parse_row_column(maybe_path)
            {
                positioned_variation = Some((
                    range.start..range.end - (maybe_path.len() - suffix_start),
                    LineColumn { line, column },
                ));
            }
        }

        Self {
            variations,
            positioned_variation,
        }
    }

    pub fn variations<'a>(
        &'a self,
        maybe_paths: &'a MaybePath,
    ) -> Box<dyn Iterator<Item = (&'a Path, Option<LineColumn>)> + 'a> {
        let variations = self
            .variations
            .iter()
            .cloned()
            .map(|range| (Path::new(maybe_paths.enhanced_maybe_path(&range)), None));
        if let Some((ref range, position)) = self.positioned_variation {
            Box::new(variations.chain(iter::once((
                Path::new(maybe_paths.enhanced_maybe_path(range)),
                Some(position),
            ))))
        } else {
            Box::new(variations)
        }
    }

    fn absolutize<'a>(
        maybe_paths: &'a MaybePath,
        cwd: &Option<PathBuf>,
        home_dir: &Option<PathBuf>,
        range: &Range<usize>,
        position: Option<LineColumn>,
    ) -> Vec<(Cow<'a, Path>, Option<LineColumn>)> {
        let maybe_path = Path::new(maybe_paths.enhanced_maybe_path(&range));
        let mut absolutized = Vec::new();
        if maybe_path.is_absolute() {
            absolutized.push((Cow::Borrowed(maybe_path), position));
            return absolutized;
        }

        if let Some(cwd) = cwd {
            absolutized.push((Cow::Owned(cwd.join(maybe_path)), position));
        }

        if let Some(home_dir) = home_dir {
            if let Ok(stripped) = maybe_path.strip_prefix("~") {
                absolutized.push((Cow::Owned(home_dir.join(stripped)), position));
            }
        }

        absolutized
    }

    pub fn absolutized_variations<'a>(
        &'a self,
        maybe_paths: &'a MaybePath,
        cwd: &Option<PathBuf>,
        home_dir: &Option<PathBuf>,
    ) -> Vec<(Cow<'a, Path>, Option<LineColumn>)> {
        let mut variations = Vec::new();
        for range in &self.variations {
            variations.append(&mut Self::absolutize(
                maybe_paths,
                cwd,
                home_dir,
                &range,
                None,
            ));
        }

        if let Some((ref range, position)) = self.positioned_variation {
            variations.append(&mut Self::absolutize(
                maybe_paths,
                cwd,
                home_dir,
                range,
                Some(position),
            ));
        }

        variations
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use fs::{FakeFs, Fs};
    use gpui::TestAppContext;
    use serde_json::json;

    #[gpui::test]
    async fn test_maybe_path(cx: &mut TestAppContext) {
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
            "/root2",
            json!({
                "three.txt": "",
            }),
        )
        .await;

        let path = "(/root2/three.txt)";
        let line = "+++ a/hello   super/cool path: (/root2/three.txt)".to_string();
        let path_match = line.find(path).unwrap()..line.len();
        assert_eq!(&line[path_match.clone()], path);
        let expected = [
            "(/root2/three.txt)",
            "/root2/three.txt",
            "a/hello   super/cool path: (/root2/three.txt)",
            "hello   super/cool path: (/root2/three.txt)",
        ]
        .into_iter()
        .map(|str| (Path::new(str), None))
        .collect::<Vec<_>>();

        let maybe_paths = MaybePath::new(line, MaybePathMode::Advanced(path_match.clone()));
        let maybe_paths_variations = maybe_paths.compute_maybe_paths_variations();

        let actual = maybe_paths_variations
            .iter()
            .map(|maybe_path_variations| {
                maybe_path_variations
                    .variations(&maybe_paths)
                    .collect::<Vec<_>>()
            })
            .flatten();

        assert_eq!(
            actual.clone().count(),
            4,
            "{:#?}",
            actual.clone().collect::<Vec<_>>()
        );

        for (actual, expected) in actual.clone().zip(expected.iter()) {
            assert_eq!(actual, *expected)
        }

        let mut canonical_paths = Vec::new();
        for (path, position) in actual {
            println!("Checking maybe_path: {:?} at {:?}", path, position);
            if let Ok(canonical_path) = fs.canonicalize(&path).await {
                canonical_paths.push(canonical_path);
            }
        }

        assert_eq!(canonical_paths.len(), 1);
        assert_eq!(canonical_paths[0], expected[1].0)
    }
}
