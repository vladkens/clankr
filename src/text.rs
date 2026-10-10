use unicode_segmentation::UnicodeSegmentation;

pub fn is_control(c: char) -> bool {
  c.is_control()
    || matches!(
      c,
      '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

// Filtering can join graphemes, so snap again after normalizing terminal controls.
pub fn clean(text: &str, cursor: usize, tabs: bool) -> (String, usize) {
  let cursor = floor(text, cursor);
  let mut value = String::new();
  let mut pos = 0;
  let mut chars = text.char_indices().peekable();
  while let Some((i, c)) = chars.next() {
    match c {
      '\r' if chars.peek().is_some_and(|&(_, c)| c == '\n') => {}
      '\r' | '\n' | '\u{2028}' | '\u{2029}' => value.push('\n'),
      '\t' if tabs => value.push_str("    "),
      '\t' => value.push(c),
      c if is_control(c) => {}
      c => value.push(c),
    }
    if i < cursor {
      pos = value.len();
    }
  }
  let pos = floor(&value, pos);
  (value, pos)
}

fn floor(text: &str, pos: usize) -> usize {
  if pos >= text.len() {
    return text.len();
  }
  text.grapheme_indices(true).map(|(i, _)| i).take_while(|&i| i <= pos).last().unwrap_or(0)
}
