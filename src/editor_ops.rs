use std::ops::Range;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TextSelection {
    pub anchor: usize,
    pub head: usize,
}

impl TextSelection {
    #[must_use]
    pub const fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    #[must_use]
    pub const fn cursor(position: usize) -> Self {
        Self {
            anchor: position,
            head: position,
        }
    }

    #[must_use]
    pub fn start(self) -> usize {
        self.anchor.min(self.head)
    }

    #[must_use]
    pub fn end(self) -> usize {
        self.anchor.max(self.head)
    }

    #[must_use]
    pub fn range(self) -> Range<usize> {
        self.start()..self.end()
    }

    #[must_use]
    pub fn is_cursor(self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CaseTransform {
    Upper,
    Lower,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct EditResult {
    pub text: String,
    pub selections: Vec<TextSelection>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct LineRange {
    start: usize,
    content_end: usize,
    end: usize,
}

pub fn selected_text(text: &str, selection: TextSelection) -> String {
    let characters = text_chars(text);
    characters[selection.start().min(characters.len())..selection.end().min(characters.len())]
        .iter()
        .collect()
}

pub fn normalize_selections(selections: &[TextSelection]) -> Vec<TextSelection> {
    let mut normalized = selections.to_vec();
    normalized.sort_by_key(|selection| (selection.start(), selection.end()));
    normalized.dedup_by_key(|selection| (selection.start(), selection.end()));
    normalized
}

pub fn select_current_lines(text: &str, selections: &[TextSelection]) -> Vec<TextSelection> {
    let characters = text_chars(text);
    let lines = line_ranges(&characters);
    let selected_lines = selected_line_span(&lines, selections);

    selected_lines
        .map(|line_span| {
            TextSelection::new(lines[line_span.start].start, lines[line_span.end - 1].end)
        })
        .into_iter()
        .collect()
}

pub fn add_next_occurrence(text: &str, selections: &[TextSelection]) -> Vec<TextSelection> {
    let mut selections = normalize_selections(selections);
    let Some(active_selection) = selections.last().copied() else {
        return selections;
    };

    if active_selection.is_cursor() {
        return selections;
    }

    let characters = text_chars(text);
    let needle: Vec<char> = characters[active_selection.start()..active_selection.end()].to_vec();

    if needle.is_empty() {
        return selections;
    }

    let existing_ranges = selections
        .iter()
        .map(|selection| selection.range())
        .collect::<Vec<_>>();

    let search_order =
        (active_selection.end()..characters.len()).chain(0..active_selection.start());

    for start in search_order {
        let end = start + needle.len();

        if end > characters.len() {
            continue;
        }

        if characters[start..end] == needle
            && !existing_ranges
                .iter()
                .any(|range| range.start == start && range.end == end)
        {
            selections.push(TextSelection::new(start, end));
            break;
        }
    }

    normalize_selections(&selections)
}

pub fn select_all_occurrences(text: &str, selection: TextSelection) -> Vec<TextSelection> {
    if selection.is_cursor() {
        return vec![selection];
    }

    let characters = text_chars(text);
    let needle: Vec<char> = characters[selection.start()..selection.end()].to_vec();

    if needle.is_empty() {
        return vec![selection];
    }

    let mut selections = Vec::new();
    let mut start = 0;

    while start + needle.len() <= characters.len() {
        if characters[start..start + needle.len()] == needle {
            selections.push(TextSelection::new(start, start + needle.len()));
            start += needle.len();
        } else {
            start += 1;
        }
    }

    selections
}

pub fn replace_selections(
    text: &str,
    selections: &[TextSelection],
    replacement: &str,
) -> EditResult {
    replace_selections_with(text, selections, |_| replacement.to_owned())
}

pub fn backspace_targets(text: &str, selections: &[TextSelection]) -> Vec<TextSelection> {
    let character_count = text.chars().count();

    normalize_selections(selections)
        .into_iter()
        .filter_map(|selection| {
            if !selection.is_cursor() {
                Some(selection)
            } else if selection.head > 0 {
                Some(TextSelection::new(selection.head - 1, selection.head))
            } else {
                None
            }
        })
        .filter(|selection| {
            selection.start() <= character_count && selection.end() <= character_count
        })
        .collect()
}

pub fn delete_targets(text: &str, selections: &[TextSelection]) -> Vec<TextSelection> {
    let character_count = text.chars().count();

    normalize_selections(selections)
        .into_iter()
        .filter_map(|selection| {
            if !selection.is_cursor() {
                Some(selection)
            } else if selection.head < character_count {
                Some(TextSelection::new(selection.head, selection.head + 1))
            } else {
                None
            }
        })
        .collect()
}

pub fn convert_case(
    text: &str,
    selections: &[TextSelection],
    transform: CaseTransform,
) -> EditResult {
    let targets = word_or_selection_targets(text, selections);

    replace_selections_with(text, &targets, |selected| match transform {
        CaseTransform::Upper => selected.to_uppercase(),
        CaseTransform::Lower => selected.to_lowercase(),
    })
}

pub fn delete_lines(text: &str, selections: &[TextSelection]) -> EditResult {
    let line_selection = select_current_lines(text, selections);

    replace_selections(text, &line_selection, "")
}

pub fn move_lines(text: &str, selections: &[TextSelection], up: bool) -> EditResult {
    let characters = text_chars(text);
    let lines = line_ranges(&characters);
    let Some(line_span) = selected_line_span(&lines, selections) else {
        return EditResult {
            text: text.to_owned(),
            selections: selections.to_vec(),
        };
    };

    if up && line_span.start == 0 {
        return EditResult {
            text: text.to_owned(),
            selections: selections.to_vec(),
        };
    }

    if !up && line_span.end >= lines.len() {
        return EditResult {
            text: text.to_owned(),
            selections: selections.to_vec(),
        };
    }

    let block_start = lines[line_span.start].start;
    let block_end = lines[line_span.end - 1].end;
    let block = characters[block_start..block_end].to_vec();
    let mut output = Vec::with_capacity(characters.len());

    let new_start = if up {
        let previous_start = lines[line_span.start - 1].start;
        let previous_end = lines[line_span.start - 1].end;
        output.extend_from_slice(&characters[..previous_start]);
        output.extend_from_slice(&block);
        output.extend_from_slice(&characters[previous_start..previous_end]);
        output.extend_from_slice(&characters[block_end..]);
        previous_start
    } else {
        let next_start = lines[line_span.end].start;
        let next_end = lines[line_span.end].end;
        output.extend_from_slice(&characters[..block_start]);
        output.extend_from_slice(&characters[next_start..next_end]);
        output.extend_from_slice(&block);
        output.extend_from_slice(&characters[next_end..]);
        block_start + (next_end - next_start)
    };

    EditResult {
        text: output.into_iter().collect(),
        selections: vec![TextSelection::new(new_start, new_start + block.len())],
    }
}

pub fn copy_lines(text: &str, selections: &[TextSelection], up: bool) -> EditResult {
    let characters = text_chars(text);
    let lines = line_ranges(&characters);
    let Some(line_span) = selected_line_span(&lines, selections) else {
        return EditResult {
            text: text.to_owned(),
            selections: selections.to_vec(),
        };
    };

    let block_start = lines[line_span.start].start;
    let block_end = lines[line_span.end - 1].end;
    let block = characters[block_start..block_end].to_vec();
    let insert_at = if up { block_start } else { block_end };
    let mut output = Vec::with_capacity(characters.len() + block.len());
    output.extend_from_slice(&characters[..insert_at]);
    output.extend_from_slice(&block);
    output.extend_from_slice(&characters[insert_at..]);

    EditResult {
        text: output.into_iter().collect(),
        selections: vec![TextSelection::new(insert_at, insert_at + block.len())],
    }
}

pub fn rectangular_selections(text: &str, selection: TextSelection) -> Vec<TextSelection> {
    let characters = text_chars(text);
    let lines = line_ranges(&characters);

    if lines.is_empty() {
        return vec![selection];
    }

    let anchor_line = line_index_at(&lines, selection.anchor);
    let head_line = line_index_at(&lines, selection.head);
    let start_line = anchor_line.min(head_line);
    let end_line = anchor_line.max(head_line);
    let anchor_column = selection.anchor.saturating_sub(lines[anchor_line].start);
    let head_column = selection.head.saturating_sub(lines[head_line].start);
    let start_column = anchor_column.min(head_column);
    let end_column = anchor_column.max(head_column);

    (start_line..=end_line)
        .map(|line_index| {
            let line = lines[line_index];
            let start = (line.start + start_column).min(line.content_end);
            let end = (line.start + end_column).min(line.content_end);
            TextSelection::new(start, end)
        })
        .collect()
}

pub fn rectangular_text(text: &str, selections: &[TextSelection]) -> String {
    normalize_selections(selections)
        .into_iter()
        .map(|selection| selected_text(text, selection))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn paste_rectangular(text: &str, selections: &[TextSelection], block: &str) -> EditResult {
    let lines = block.split('\n').collect::<Vec<_>>();

    if lines.is_empty() {
        return EditResult {
            text: text.to_owned(),
            selections: selections.to_vec(),
        };
    }

    replace_selections_with_index(text, selections, |selected, index| {
        if lines.len() == 1 {
            lines[0].to_owned()
        } else {
            lines
                .get(index)
                .map(|line| (*line).to_owned())
                .unwrap_or_else(|| selected.to_owned())
        }
    })
}

pub fn word_at(text: &str, char_index: usize) -> Option<TextSelection> {
    let characters = text_chars(text);

    if characters.is_empty() {
        return None;
    }

    let mut index = char_index.min(characters.len().saturating_sub(1));

    if index > 0 && !is_word_char(characters[index]) && is_word_char(characters[index - 1]) {
        index -= 1;
    }

    if !is_word_char(characters[index]) {
        return None;
    }

    let mut start = index;
    while start > 0 && is_word_char(characters[start - 1]) {
        start -= 1;
    }

    let mut end = index + 1;
    while end < characters.len() && is_word_char(characters[end]) {
        end += 1;
    }

    Some(TextSelection::new(start, end))
}

pub fn char_to_byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(byte_index, _)| byte_index)
        .unwrap_or(text.len())
}

fn replace_selections_with(
    text: &str,
    selections: &[TextSelection],
    replacement_for: impl Fn(&str) -> String,
) -> EditResult {
    replace_selections_with_index(text, selections, |selected, _index| {
        replacement_for(selected)
    })
}

fn replace_selections_with_index(
    text: &str,
    selections: &[TextSelection],
    replacement_for: impl Fn(&str, usize) -> String,
) -> EditResult {
    let characters = text_chars(text);
    let selections = normalize_selections(selections);
    let mut output = String::new();
    let mut cursor = 0;
    let mut next_selections = Vec::new();
    let mut offset: isize = 0;

    for (index, selection) in selections.iter().copied().enumerate() {
        let start = selection.start().min(characters.len());
        let end = selection.end().min(characters.len());

        if start < cursor {
            continue;
        }

        output.extend(characters[cursor..start].iter());
        let selected = characters[start..end].iter().collect::<String>();
        let replacement = replacement_for(&selected, index);
        let replacement_len = replacement.chars().count();
        let next_start = (start as isize + offset).max(0) as usize;
        next_selections.push(TextSelection::cursor(next_start + replacement_len));
        output.push_str(&replacement);
        offset += replacement_len as isize - (end - start) as isize;
        cursor = end;
    }

    output.extend(characters[cursor..].iter());

    EditResult {
        text: output,
        selections: next_selections,
    }
}

fn word_or_selection_targets(text: &str, selections: &[TextSelection]) -> Vec<TextSelection> {
    let mut targets = Vec::new();

    for selection in normalize_selections(selections) {
        if selection.is_cursor() {
            if let Some(word_selection) = word_at(text, selection.head) {
                targets.push(word_selection);
            }
        } else {
            targets.push(selection);
        }
    }

    targets
}

fn selected_line_span(lines: &[LineRange], selections: &[TextSelection]) -> Option<Range<usize>> {
    if lines.is_empty() || selections.is_empty() {
        return None;
    }

    let selections = normalize_selections(selections);
    let start_line = selections
        .iter()
        .map(|selection| line_index_at(lines, selection.start()))
        .min()?;
    let end_line = selections
        .iter()
        .map(|selection| {
            let end = if selection.end() > selection.start() {
                selection.end().saturating_sub(1)
            } else {
                selection.end()
            };

            line_index_at(lines, end)
        })
        .max()?;

    Some(start_line..end_line + 1)
}

fn line_ranges(characters: &[char]) -> Vec<LineRange> {
    let mut ranges = Vec::new();
    let mut start = 0;

    for (index, character) in characters.iter().copied().enumerate() {
        if character == '\n' {
            let content_end = if index > start && characters[index - 1] == '\r' {
                index - 1
            } else {
                index
            };
            ranges.push(LineRange {
                start,
                content_end,
                end: index + 1,
            });
            start = index + 1;
        }
    }

    if start <= characters.len() {
        ranges.push(LineRange {
            start,
            content_end: characters.len(),
            end: characters.len(),
        });
    }

    ranges
}

fn line_index_at(lines: &[LineRange], char_index: usize) -> usize {
    lines
        .partition_point(|line| line.start <= char_index)
        .saturating_sub(1)
        .min(lines.len().saturating_sub(1))
}

fn text_chars(text: &str) -> Vec<char> {
    text.chars().collect()
}

fn is_word_char(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_next_occurrence_appends_next_literal_match() {
        let selections = add_next_occurrence("cat dog cat", &[TextSelection::new(0, 3)]);

        assert_eq!(
            selections,
            vec![TextSelection::new(0, 3), TextSelection::new(8, 11)]
        );
    }

    #[test]
    fn select_current_lines_expands_multiline_selection() {
        let selections = select_current_lines("one\ntwo\nthree", &[TextSelection::new(1, 6)]);

        assert_eq!(selections, vec![TextSelection::new(0, 8)]);
    }

    #[test]
    fn replace_selections_replaces_multiple_ranges() {
        let result = replace_selections(
            "one two three",
            &[TextSelection::new(0, 3), TextSelection::new(8, 13)],
            "x",
        );

        assert_eq!(result.text, "x two x");
        assert_eq!(
            result.selections,
            vec![TextSelection::cursor(1), TextSelection::cursor(7)]
        );
    }

    #[test]
    fn backspace_and_delete_targets_expand_cursors() {
        assert_eq!(
            backspace_targets("abc", &[TextSelection::cursor(2)]),
            vec![TextSelection::new(1, 2)]
        );
        assert_eq!(
            delete_targets("abc", &[TextSelection::cursor(2)]),
            vec![TextSelection::new(2, 3)]
        );
    }

    #[test]
    fn case_conversion_uses_selected_text_or_word_at_cursor() {
        let result = convert_case(
            "alpha beta",
            &[TextSelection::cursor(7)],
            CaseTransform::Upper,
        );

        assert_eq!(result.text, "alpha BETA");
    }

    #[test]
    fn move_lines_moves_selected_block_up() {
        let result = move_lines("one\ntwo\nthree\n", &[TextSelection::new(4, 7)], true);

        assert_eq!(result.text, "two\none\nthree\n");
    }

    #[test]
    fn copy_lines_duplicates_selected_block_down() {
        let result = copy_lines("one\ntwo\n", &[TextSelection::new(0, 3)], false);

        assert_eq!(result.text, "one\none\ntwo\n");
    }

    #[test]
    fn delete_lines_removes_selected_lines() {
        let result = delete_lines("one\ntwo\nthree", &[TextSelection::new(4, 7)]);

        assert_eq!(result.text, "one\nthree");
    }

    #[test]
    fn rectangular_selection_slices_same_columns_per_line() {
        let selections = rectangular_selections("abcd\nefgh\nij", TextSelection::new(1, 7));

        assert_eq!(
            selections,
            vec![TextSelection::new(1, 2), TextSelection::new(6, 7)]
        );
    }

    #[test]
    fn rectangular_paste_replaces_each_rectangular_slice() {
        let result = paste_rectangular(
            "abcd\nefgh",
            &[TextSelection::new(1, 3), TextSelection::new(6, 8)],
            "X\nY",
        );

        assert_eq!(result.text, "aXd\neYh");
    }

    #[test]
    fn rectangular_paste_repeats_single_line_without_cycling_multiline_blocks() {
        let repeated = paste_rectangular(
            "abcd\nefgh",
            &[TextSelection::new(1, 2), TextSelection::new(6, 7)],
            "X",
        );
        let partial = paste_rectangular(
            "abcd\nefgh\nijkl",
            &[
                TextSelection::new(1, 2),
                TextSelection::new(6, 7),
                TextSelection::new(11, 12),
            ],
            "X\nY",
        );

        assert_eq!(repeated.text, "aXcd\neXgh");
        assert_eq!(partial.text, "aXcd\neYgh\nijkl");
    }

    #[test]
    fn operations_preserve_utf8_character_boundaries() {
        let result = replace_selections(
            "éclair 🍰 éclair",
            &[TextSelection::new(0, 6), TextSelection::new(9, 15)],
            "sweet",
        );

        assert_eq!(result.text, "sweet 🍰 sweet");
    }
}
