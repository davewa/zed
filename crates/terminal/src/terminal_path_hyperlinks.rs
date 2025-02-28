use std::{
    borrow::Cow,
    fmt::Display,
    ops::Range,
    path::{Path, PathBuf, MAIN_SEPARATOR},
    str,
};

// TODO(davewa): Change most (all?) info! messages into debug! or trace!

use log::info;
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

#[derive(Clone, Debug)]
pub struct MatchedMaybePath {
    /// The terminal word or line containing maybe_path.
    text: String,
    /// Iff `text` is a line, the range of the hovered or Cmd-clicked word within the line
    word: Range<usize>,
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

    pub(super) fn matched(&self) -> &str {
        &self.text[self.word.clone()]
    }

    pub(super) fn matched_range(&self) -> &Range<usize> {
        &self.word
    }

    pub(super) fn text_at(&self, range: &Range<usize>) -> &str {
        &self.text[range.clone()]
    }

    /// All possible word and advanced paths
    /// TODO(davewa): `-> HashSet<MaybePathWithPosition>`
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

    /// All possible exhaustive paths
    /// TODO(davewa): `-> HashSet<MaybePathWithPosition>`
    pub fn exhaustive_maybe_paths(&self) -> Vec<MaybePath> {
        let maybe_path_variations = Vec::new();

        // TODO: Some way to assert we are not called on the main thread...
        if self.path_hyperlink_navigation < PathHyperlinkNavigation::Exhaustive {
            return maybe_path_variations;
        }

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

        maybe_path_variations
    }

    /// Expands the `word` within `text` to the longest matching pair of surrounding symbols.
    /// This is arguably the most common case by far, so we enable it in MaybePathMode::Word.
    pub(super) fn longest_maybe_path_by_surrounding_symbols(&self) -> Option<Range<usize>> {
        let mut longest = None::<Range<usize>>;

        for (start, end) in COMMON_PATH_SURROUNDING_SYMBOLS {
            if let (Some(first), Some(last)) = (self.text.find(*start), self.text.rfind(*end)) {
                if first < last && first <= self.word.start && last >= self.word.end - 1 {
                    let current = first..last + 1;
                    if let Some(longest) = &mut longest {
                        if first < longest.start && last > longest.end - 1 {
                            *longest = current;
                        }
                    } else {
                        longest = Some(current);
                    }
                }
            }
        }

        longest.as_ref().inspect(|longest| {
            info!(
                "Terminal: Longest surrounding symbols: {:?}",
                self.text_at(longest)
            )
        });

        longest
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
            formatter.write_fmt(format_args!("{:?} «{}»", self, self.matched()))
        } else {
            formatter.write_fmt(format_args!("{:?}", self))
        }
    }
}

#[cfg(not(test))]
#[derive(Clone, Copy, Debug)]
pub struct RowColumn {
    pub row: u32,
    pub column: Option<u32>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RowColumn {
    pub row: u32,
    pub column: Option<u32>,
}

/// Contains well defined sub range variations of a MaybePath
/// - Line and column suffix
/// - Stripped common surrounding symbols
/// - Git diff prefxes
#[derive(Debug)]
pub struct MaybePath {
    variations: Vec<Range<usize>>,
    positioned_variation: Option<(Range<usize>, RowColumn)>,
}

impl MaybePath {
    pub fn new(text: &str, mut path: Range<usize>) -> Self {
        // We add variation longest to shortest
        let mut maybe_path = &text[path.clone()];
        let mut positioned_variation = None::<(Range<usize>, RowColumn)>;

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
                path = path.start + 1..path.end - 1;
                variations.push(path.clone());
                maybe_path = &text[path.clone()];
            }

            // Git diff and row column, mutually exclusive
            if (maybe_path.starts_with('a') || maybe_path.starts_with('b'))
                && maybe_path[1..].starts_with(MAIN_SEPARATOR)
            {
                variations.push(path.start + 2..path.end);
            } else if let (suffix_start, Some(row), column) =
                PathWithPosition::parse_row_column(maybe_path)
            {
                positioned_variation = Some((
                    path.start..path.end - (maybe_path.len() - suffix_start),
                    RowColumn { row, column },
                ));
            }
        }

        Self {
            variations,
            positioned_variation,
        }
    }

    pub fn relative_variations<'a>(
        &self,
        matched_maybe_path: &'a MatchedMaybePath,
    ) -> Vec<(&'a Path, Option<RowColumn>)> {
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
                maybe_path.is_relative().then(|| (maybe_path, position))
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
    ) -> Vec<(Cow<'a, Path>, Option<RowColumn>)> {
        let maybe_path = Path::new(matched_maybe_path.text_at(&range));
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
        &self,
        matched_maybe_path: &'a MatchedMaybePath,
        cwd: &Option<PathBuf>,
        home_dir: &Option<PathBuf>,
    ) -> Vec<(Cow<'a, Path>, Option<RowColumn>)> {
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
            // TODO: "/root2/three.txt",
            "a/hello   super/cool path: (/root2/three.txt)",
            "hello   super/cool path: (/root2/three.txt)",
        ]
        .into_iter()
        .map(|str| (Path::new(str), None))
        .collect::<Vec<_>>();

        let maybe_path =
            MatchedMaybePath::from_line(line, path_match, PathHyperlinkNavigation::Advanced);
        let maybe_path_variations = maybe_path.maybe_paths();
        assert_eq!(maybe_path_variations.len(), 2);

        let actual = maybe_path_variations
            .iter()
            .map(|maybe_path_variations| maybe_path_variations.relative_variations(&maybe_path))
            .flatten();

        assert_eq!(
            actual.clone().count(),
            3,
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

        assert_eq!(canonical_paths.len(), 0);
        // TODO: assert_eq!(canonical_paths[0], expected[0].0)
    }
}
