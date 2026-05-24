use memchr::memchr_iter;
use regex::{Regex, RegexBuilder};

const PREVIEW_CONTEXT_CHARS: usize = 120;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct SearchOptions {
    pub match_case: bool,
    pub whole_word: bool,
    pub use_regex: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TextSearchMatch {
    pub range: std::ops::Range<usize>,
    pub line_index: usize,
    pub line_start: usize,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SearchError {
    EmptyQuery,
    InvalidRegex(String),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::EmptyQuery => write!(formatter, "search query cannot be empty"),
            SearchError::InvalidRegex(error) => write!(formatter, "invalid regex: {error}"),
        }
    }
}

impl std::error::Error for SearchError {}

#[derive(Debug, Clone)]
pub struct CompiledSearch {
    options: SearchOptions,
    regex: Regex,
}

impl CompiledSearch {
    pub fn new(query: &str, options: SearchOptions) -> Result<Self, SearchError> {
        Ok(Self {
            options,
            regex: build_search_regex(query, options)?,
        })
    }

    #[must_use]
    pub fn find_matches(&self, text: &str, limit: usize) -> Vec<TextSearchMatch> {
        if limit == 0 {
            return Vec::new();
        }

        let mut line_tracker = LineTracker::new(text);
        let mut matches = Vec::new();

        for regex_match in self.regex.find_iter(text) {
            if regex_match.is_empty() {
                continue;
            }

            let range = regex_match.start()..regex_match.end();

            if self.options.whole_word && !is_whole_word_match(text, range.clone()) {
                continue;
            }

            let (line_index, line_start) = line_tracker.line_at(range.start);
            matches.push(TextSearchMatch {
                range,
                line_index,
                line_start,
            });

            if matches.len() >= limit {
                break;
            }
        }

        matches
    }

    #[must_use]
    pub fn replace_all(&self, text: &str, replacement: &str) -> (String, usize) {
        let mut replaced = String::with_capacity(text.len());
        let mut last_end = 0;
        let mut count = 0;

        for captures in self.regex.captures_iter(text) {
            let Some(regex_match) = captures.get(0) else {
                continue;
            };

            if regex_match.is_empty() {
                continue;
            }

            let range = regex_match.start()..regex_match.end();

            if self.options.whole_word && !is_whole_word_match(text, range.clone()) {
                continue;
            }

            replaced.push_str(&text[last_end..range.start]);
            if self.options.use_regex {
                captures.expand(replacement, &mut replaced);
            } else {
                replaced.push_str(replacement);
            }
            last_end = range.end;
            count += 1;
        }

        if count == 0 {
            return (text.to_owned(), 0);
        }

        replaced.push_str(&text[last_end..]);
        (replaced, count)
    }
}

pub fn find_text_matches(
    text: &str,
    query: &str,
    options: SearchOptions,
    limit: usize,
) -> Result<Vec<TextSearchMatch>, SearchError> {
    Ok(CompiledSearch::new(query, options)?.find_matches(text, limit))
}

pub fn replace_all(
    text: &str,
    query: &str,
    replacement: &str,
    options: SearchOptions,
) -> Result<(String, usize), SearchError> {
    Ok(CompiledSearch::new(query, options)?.replace_all(text, replacement))
}

pub fn line_preview(text: &str, byte_index: usize) -> String {
    let mut byte_index = byte_index.min(text.len());

    while byte_index > 0 && !text.is_char_boundary(byte_index) {
        byte_index -= 1;
    }

    let start = text[..byte_index]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let end = text[byte_index..]
        .find('\n')
        .map(|index| byte_index + index)
        .unwrap_or(text.len());

    truncated_line_preview(text[start..end].trim_end_matches('\r'), byte_index - start)
}

fn truncated_line_preview(line: &str, match_offset: usize) -> String {
    let character_count = line.chars().count();

    if character_count <= PREVIEW_CONTEXT_CHARS * 2 {
        return line.to_owned();
    }

    let match_character_index = line[..match_offset.min(line.len())].chars().count();
    let start_character = match_character_index.saturating_sub(PREVIEW_CONTEXT_CHARS);
    let end_character = match_character_index
        .saturating_add(PREVIEW_CONTEXT_CHARS)
        .min(character_count);
    let mut preview = String::new();

    if start_character > 0 {
        preview.push_str("...");
    }

    preview.extend(
        line.chars()
            .skip(start_character)
            .take(end_character - start_character),
    );

    if end_character < character_count {
        preview.push_str("...");
    }

    preview
}

fn build_search_regex(query: &str, options: SearchOptions) -> Result<Regex, SearchError> {
    if query.is_empty() {
        return Err(SearchError::EmptyQuery);
    }

    let pattern = if options.use_regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };

    RegexBuilder::new(&pattern)
        .case_insensitive(!options.match_case)
        .multi_line(true)
        .build()
        .map_err(|error| SearchError::InvalidRegex(error.to_string()))
}

fn is_whole_word_match(text: &str, range: std::ops::Range<usize>) -> bool {
    !previous_char(text, range.start).is_some_and(is_word_char)
        && !next_char(text, range.end).is_some_and(is_word_char)
}

fn previous_char(text: &str, byte_index: usize) -> Option<char> {
    text.get(..byte_index)?.chars().next_back()
}

fn next_char(text: &str, byte_index: usize) -> Option<char> {
    text.get(byte_index..)?.chars().next()
}

fn is_word_char(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

struct LineTracker<'a> {
    newline_offsets: memchr::Memchr<'a>,
    line_index: usize,
    line_start: usize,
    next_line_start: Option<usize>,
}

impl<'a> LineTracker<'a> {
    fn new(text: &'a str) -> Self {
        let mut newline_offsets = memchr_iter(b'\n', text.as_bytes());
        let next_line_start = newline_offsets.next().map(|index| index + 1);

        Self {
            newline_offsets,
            line_index: 0,
            line_start: 0,
            next_line_start,
        }
    }

    fn line_at(&mut self, byte_index: usize) -> (usize, usize) {
        while self
            .next_line_start
            .is_some_and(|line_start| line_start <= byte_index)
        {
            self.line_start = self
                .next_line_start
                .expect("line start should be present when advancing line tracker");
            self.line_index += 1;
            self.next_line_start = self.newline_offsets.next().map(|index| index + 1);
        }

        (self.line_index, self.line_start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_search_is_case_insensitive_by_default() {
        let matches =
            find_text_matches("Alpha\nbeta\nALPHA", "alpha", SearchOptions::default(), 10).unwrap();

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line_index, 0);
        assert_eq!(matches[0].line_start, 0);
        assert_eq!(matches[1].line_index, 2);
        assert_eq!(matches[1].line_start, "Alpha\nbeta\n".len());
    }

    #[test]
    fn whole_word_search_filters_embedded_matches() {
        let matches = find_text_matches(
            "cat scatter cat_ cat",
            "cat",
            SearchOptions {
                whole_word: true,
                ..SearchOptions::default()
            },
            10,
        )
        .unwrap();

        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn regex_replace_supports_capture_expansion() {
        let (text, count) = replace_all(
            "a1 b2",
            r"([a-z])(\d)",
            "$2:$1",
            SearchOptions {
                use_regex: true,
                match_case: true,
                whole_word: false,
            },
        )
        .unwrap();

        assert_eq!(count, 2);
        assert_eq!(text, "1:a 2:b");
    }

    #[test]
    fn compiled_search_reuses_pattern_for_find_and_replace() {
        let search = CompiledSearch::new(
            r"item_(\d)",
            SearchOptions {
                use_regex: true,
                match_case: true,
                whole_word: false,
            },
        )
        .unwrap();

        let matches = search.find_matches("item_1\nitem_2", 10);
        let (text, count) = search.replace_all("item_1 item_2", "#$1");

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[1].line_index, 1);
        assert_eq!(count, 2);
        assert_eq!(text, "#1 #2");
    }

    #[test]
    fn literal_replace_does_not_expand_dollar_syntax() {
        let (text, count) = replace_all("a a", "a", "$1", SearchOptions::default()).unwrap();

        assert_eq!(count, 2);
        assert_eq!(text, "$1 $1");
    }

    #[test]
    fn replace_all_with_empty_replacement_deletes_matches() {
        let (text, count) =
            replace_all("foo bar foo", "foo", "", SearchOptions::default()).unwrap();

        assert_eq!(count, 2);
        assert_eq!(text, " bar ");
    }

    #[test]
    fn line_preview_returns_current_line_without_cr() {
        assert_eq!(line_preview("one\r\ntwo\r\n", 6), "two");
    }

    #[test]
    fn line_preview_truncates_very_long_lines_around_match() {
        let text = format!("{}needle{}", "a".repeat(1_000), "b".repeat(1_000));
        let preview = line_preview(&text, 1_000);

        assert!(preview.starts_with("..."));
        assert!(preview.contains("needle"));
        assert!(preview.ends_with("..."));
        assert!(preview.len() < text.len());
    }
}
