use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers as Mods};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{Error, Res};

pub const LIMIT: usize = 1024 * 1024;

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

pub struct Row {
  pub text: String,
  pub stops: Vec<(usize, u16)>,
}

pub struct Layout {
  pub rows: Vec<Row>,
}

impl Layout {
  // Keep the last terminal cell for the cursor instead of triggering automatic wrapping.
  // On tiny screens shorten the prefix, then replace only graphemes that cannot fit.
  pub fn new(text: &str, cols: u16, first: &str, rest: &str) -> Self {
    let width = usize::from(cols.saturating_sub(1).max(1));
    let prefix = |s: &str| {
      if s.width() < width { s.to_owned() } else { String::new() }
    };
    let row = |s: &str| Row { text: prefix(s), stops: Vec::new() };
    let mut line = row(first);
    let mut used = line.text.width();
    let mut indent = used;
    let mut rows = Vec::new();
    let col = |used: usize| used.min(usize::from(cols.saturating_sub(1))) as u16;
    for (i, g) in text.grapheme_indices(true) {
      let display = if g == "\t" { "    " } else { g };
      let size = display.width().max(1);
      if g != "\n" && used + size > width && used > indent {
        line.stops.push((i, col(used)));
        rows.push(line);
        line = row(rest);
        used = line.text.width();
        indent = used;
      }
      line.stops.push((i, col(used)));
      if g == "\n" {
        rows.push(line);
        line = row(rest);
        used = line.text.width();
        indent = used;
      } else if size > width - used {
        if g == "\t" {
          line.text.push_str(&" ".repeat(width - used));
          used = width;
        } else {
          line.text.push('�');
          used += 1;
        }
      } else {
        if display.width() == 0 {
          line.text.push('◌');
        }
        line.text.push_str(display);
        used += size;
      }
    }
    line.stops.push((text.len(), col(used)));
    rows.push(line);
    Self { rows }
  }

  pub fn input(text: &str, cols: u16) -> Self {
    Self::new(text, cols, "", "")
  }

  // A soft boundary has two caret positions with the same byte offset. Home/End and vertical
  // movement retain the chosen side; ordinary horizontal movement uses the following row.
  pub fn cursor(&self, pos: usize, trailing: bool) -> (usize, u16) {
    let mut positions =
      self.rows.iter().enumerate().filter_map(|(i, row)| {
        row.stops.iter().find(|&&(p, _)| p == pos).map(|&(_, col)| (i, col))
      });
    if trailing { positions.next() } else { positions.next_back() }.unwrap_or((0, 0))
  }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Input {
  pub text: String,
  pub pos: usize,
  pub trailing: bool,
  pub skills: Vec<Mention>,
  column: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mention {
  pub range: Range<usize>,
  pub name: String,
}

impl Input {
  pub fn insert(&mut self, value: &str) -> Res<()> {
    self.replace(self.pos..self.pos, value)
  }

  pub fn replace(&mut self, mut range: Range<usize>, value: &str) -> Res<()> {
    // Picked skills are atomic on deletion; pasted lookalikes remain ordinary text.
    if !range.is_empty() {
      for skill in &self.skills {
        if range.start < skill.range.end && skill.range.start < range.end {
          range.start = range.start.min(skill.range.start);
          range.end = range.end.max(skill.range.end);
        }
      }
    }
    if value.len() > LIMIT.saturating_sub(self.text.len() - range.len()) {
      return Err(Error::InputTooLarge);
    }
    let value = clean(value, 0, false).0;
    self.skills.retain_mut(|skill| {
      if skill.range.end <= range.start {
        return true;
      }
      if skill.range.start >= range.end {
        skill.range.start = skill.range.start - range.len() + value.len();
        skill.range.end = skill.range.end - range.len() + value.len();
        return true;
      }
      false
    });
    self.text.replace_range(range.clone(), &value);
    self.pos = range.start + value.len();
    self.snap();
    self.column = None;
    self.trailing = false;
    Ok(())
  }

  pub fn mention(&mut self, range: Range<usize>, name: &str) -> Res<()> {
    let start = range.start;
    let token = format!("${name}");
    let space = self.text[range.end..].chars().next().is_none_or(|c| !c.is_whitespace());
    self.replace(range, &format!("{token}{}", if space { " " } else { "" }))?;
    self.skills.push(Mention { range: start..start + token.len(), name: name.into() });
    self.skills.sort_by_key(|skill| skill.range.start);
    Ok(())
  }

  fn snap(&mut self) {
    self.pos = self
      .text
      .grapheme_indices(true)
      .map(|(i, _)| i)
      .find(|&i| i >= self.pos)
      .unwrap_or(self.text.len());
  }

  fn word(&self, back: bool) -> usize {
    let mut pos = self.pos;
    if back {
      for (i, g) in self.text[..pos].grapheme_indices(true).rev() {
        if !g.chars().all(char::is_whitespace) {
          break;
        }
        pos = i;
      }
      for (i, g) in self.text[..pos].grapheme_indices(true).rev() {
        if g.chars().all(char::is_whitespace) {
          break;
        }
        pos = i;
      }
    } else {
      for g in self.text[pos..].graphemes(true) {
        if g.chars().all(char::is_whitespace) {
          break;
        }
        pos += g.len();
      }
      for g in self.text[pos..].graphemes(true) {
        if !g.chars().all(char::is_whitespace) {
          break;
        }
        pos += g.len();
      }
    }
    pos
  }

  // Return false when Up/Down reaches the document edge.
  pub fn edit(&mut self, key: KeyEvent, cols: u16) -> Res<bool> {
    let ctrl = key.modifiers.contains(Mods::CONTROL);
    let alt = key.modifiers.contains(Mods::ALT);
    if key.code == KeyCode::Enter {
      self.insert("\n")?;
      return Ok(true);
    }
    if let KeyCode::Char(c) = key.code
      && !key
        .modifiers
        .intersects(Mods::CONTROL | Mods::ALT | Mods::SUPER | Mods::HYPER | Mods::META)
    {
      self.insert(&c.to_string())?;
      return Ok(true);
    }
    if matches!(key.code, KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End) {
      let layout = Layout::input(&self.text, cols);
      let (row, col) = layout.cursor(self.pos, self.trailing);
      let target = match key.code {
        KeyCode::Up if row > 0 => row - 1,
        KeyCode::Down if row + 1 < layout.rows.len() => row + 1,
        KeyCode::Up | KeyCode::Down => return Ok(false),
        _ => row,
      };
      let stops = &layout.rows[target].stops;
      let &(pos, _) = match key.code {
        KeyCode::Home => {
          self.column = None;
          stops.first().unwrap()
        }
        KeyCode::End => {
          self.column = None;
          stops.last().unwrap()
        }
        _ => {
          let col = *self.column.get_or_insert(col);
          stops.iter().rev().find(|&&(_, c)| c <= col).unwrap_or(&stops[0])
        }
      };
      self.pos = pos;
      self.trailing = layout.cursor(pos, false).0 != target;
      return Ok(true);
    }
    self.column = None;
    self.trailing = false;
    let prev = self.text[..self.pos].grapheme_indices(true).next_back().map_or(0, |(i, _)| i);
    let next =
      self.text[self.pos..].graphemes(true).next().map_or(self.pos, |s| self.pos + s.len());
    let start = self.text[..self.pos].rfind('\n').map_or(0, |i| i + 1);
    let end = self.text[self.pos..].find('\n').map_or(self.text.len(), |i| self.pos + i);
    // Ghostty's macOS Cmd+Backspace binding also sends Ctrl+U.
    if (key.code == KeyCode::Backspace && key.modifiers.contains(Mods::SUPER))
      || (key.code == KeyCode::Char('u') && ctrl)
    {
      let mut range = start..end;
      if range.is_empty() {
        if start > 0 {
          range.start -= 1;
        } else if end < self.text.len() {
          range.end += 1;
        }
      }
      self.replace(range, "")?;
      return Ok(true);
    }
    match key.code {
      KeyCode::Left => self.pos = if alt { self.word(true) } else { prev },
      KeyCode::Right => self.pos = if alt { self.word(false) } else { next },
      KeyCode::Char('b') if alt => self.pos = self.word(true),
      KeyCode::Char('f') if alt => self.pos = self.word(false),
      KeyCode::Backspace => {
        let start = if alt { self.word(true) } else { prev };
        self.replace(start..self.pos, "")?;
      }
      KeyCode::Delete => {
        self.replace(self.pos..next, "")?;
      }
      KeyCode::Char('a') if ctrl => self.pos = start,
      KeyCode::Char('e') if ctrl => self.pos = end,
      KeyCode::Char('w') if ctrl => {
        let start = self.word(true);
        self.replace(start..self.pos, "")?;
      }
      _ => {}
    }
    self.snap();
    Ok(true)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn picked_skills_delete_atomically_and_pasted_names_remain_plain_text() {
    for (code, mods) in [
      (KeyCode::Backspace, Mods::NONE),
      (KeyCode::Backspace, Mods::ALT),
      (KeyCode::Char('w'), Mods::CONTROL),
    ] {
      let mut dat = input("huj! $age");
      dat.mention(5..9, "agent-rules").unwrap();
      assert_eq!(dat.text, "huj! $agent-rules ");
      if mods == Mods::NONE {
        dat.edit(key(KeyCode::Backspace), 80).unwrap();
      }
      dat.edit(KeyEvent::new(code, mods), 80).unwrap();
      assert_eq!(dat.text, "huj! ");
      assert!(dat.skills.is_empty());
    }
    let mut dat = input("$age after");
    dat.mention(0..4, "agent-rules").unwrap();
    dat.pos = 0;
    dat.edit(key(KeyCode::Delete), 80).unwrap();
    assert_eq!(dat.text, " after");
    assert!(dat.skills.is_empty());
    let mut dat = input("copy paste $agent-rules");
    dat.edit(key(KeyCode::Backspace), 80).unwrap();
    assert_eq!(dat.text, "copy paste $agent-rule");
    assert!(dat.skills.is_empty());
  }

  #[test]
  fn selected_ranges_follow_unicode_edits_and_preserve_neighbouring_mentions() {
    let mut dat = input("$ag");
    dat.mention(0..3, "agent-rules").unwrap();
    let start = dat.pos;
    dat.insert("$ru").unwrap();
    dat.mention(start..dat.pos, "code-style-rust").unwrap();
    dat.pos = 0;
    dat.insert("界\n").unwrap();
    for skill in &dat.skills {
      assert_eq!(&dat.text[skill.range.clone()], format!("${}", skill.name));
    }
    dat.pos = dat.skills[0].range.start + 3;
    dat.edit(key(KeyCode::Delete), 80).unwrap();
    assert_eq!(dat.text, "界\n $code-style-rust ");
    assert_eq!(dat.skills.len(), 1);
    assert_eq!(&dat.text[dat.skills[0].range.clone()], "$code-style-rust");
    dat.pos = dat.text.len();
    dat.edit(KeyEvent::new(KeyCode::Backspace, Mods::SUPER), 80).unwrap();
    assert_eq!(dat.text, "界\n");
    assert!(dat.skills.is_empty());
  }

  fn input(text: &str) -> Input {
    let mut input = Input::default();
    input.insert(text).unwrap();
    input
  }

  fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, Mods::NONE)
  }

  fn edit(input: &mut Input, code: KeyCode, cols: u16) {
    assert!(input.edit(key(code), cols).unwrap());
  }

  #[test]
  fn edits_whole_unicode_graphemes_and_joined_sequences() {
    let mut dat = input("👩‍💻e\u{301}界x");
    edit(&mut dat, KeyCode::Left, 80);
    edit(&mut dat, KeyCode::Backspace, 80);
    edit(&mut dat, KeyCode::Backspace, 80);
    assert_eq!(dat.text, "👩‍💻x");
    edit(&mut dat, KeyCode::Home, 80);
    edit(&mut dat, KeyCode::Delete, 80);
    assert_eq!(dat.text, "x");
    let mut dat = input("👩💻");
    edit(&mut dat, KeyCode::Left, 80);
    dat.insert("\u{200d}").unwrap();
    assert_eq!(dat.text, "👩‍💻");
    assert_eq!(dat.pos, dat.text.len());
    edit(&mut dat, KeyCode::Backspace, 80);
    assert!(dat.text.is_empty());
    for modifiers in [Mods::ALT, Mods::SHIFT] {
      dat.edit(KeyEvent::new(KeyCode::Enter, modifiers), 80).unwrap();
    }
    assert_eq!(dat.text, "\n\n");
    assert_eq!(dat.pos, 2);
  }

  #[test]
  fn paste_normalizes_controls_and_rejects_over_limit_atomically() {
    let mut dat = input("before after");
    dat.pos = "before ".len();
    dat.insert("a\r\n界\rb\t\x1b\u{009b}\u{202e}\u{2028}e\u{301}").unwrap();
    assert_eq!(dat.text, "before a\n界\nb\t\ne\u{301}after");
    let saved = dat.clone();
    assert!(matches!(dat.insert(&"x".repeat(LIMIT)), Err(Error::InputTooLarge)));
    assert_eq!(dat, saved);
    let mut dat = input(&"x".repeat(LIMIT));
    assert!(matches!(dat.insert("x"), Err(Error::InputTooLarge)));
    assert_eq!(dat.text.len(), LIMIT);
  }

  #[test]
  fn visual_navigation_keeps_cell_column_across_short_lines() {
    let mut dat = input("ab界z\nx\n12345");
    dat.pos = "ab界".len();
    edit(&mut dat, KeyCode::Down, 80);
    assert_eq!(dat.pos, "ab界z\nx".len());
    edit(&mut dat, KeyCode::Down, 80);
    assert_eq!(dat.pos, "ab界z\nx\n1234".len());
    edit(&mut dat, KeyCode::Up, 80);
    edit(&mut dat, KeyCode::Up, 80);
    assert_eq!(dat.pos, "ab界".len());
    edit(&mut dat, KeyCode::Right, 80);
    edit(&mut dat, KeyCode::Down, 80);
    edit(&mut dat, KeyCode::Down, 80);
    assert_eq!(dat.pos, dat.text.len());
  }

  #[test]
  fn visual_home_end_and_logical_home_end_are_distinct() {
    let mut dat = input("abcdefghi\nlast");
    dat.pos = 5;
    edit(&mut dat, KeyCode::Home, 5);
    assert_eq!(dat.pos, 4);
    assert_eq!(Layout::input(&dat.text, 5).cursor(dat.pos, dat.trailing), (1, 0));
    edit(&mut dat, KeyCode::End, 5);
    assert_eq!(dat.pos, 8);
    assert_eq!(Layout::input(&dat.text, 5).cursor(dat.pos, dat.trailing), (1, 4));
    dat.edit(KeyEvent::new(KeyCode::Char('a'), Mods::CONTROL), 5).unwrap();
    assert_eq!(dat.pos, 0);
    dat.edit(KeyEvent::new(KeyCode::Char('e'), Mods::CONTROL), 5).unwrap();
    assert_eq!(dat.pos, 9);
    dat.pos = 1;
    edit(&mut dat, KeyCode::Down, 5);
    assert_eq!(dat.pos, 5);
    edit(&mut dat, KeyCode::Down, 5);
    assert_eq!(dat.pos, 9);
    edit(&mut dat, KeyCode::Up, 5);
    assert_eq!(dat.pos, 5);
  }

  #[test]
  fn empty_and_final_rows_are_editable_without_leaving_document_edges() {
    let mut dat = Input::default();
    assert!(!dat.edit(key(KeyCode::Up), 80).unwrap());
    assert!(!dat.edit(key(KeyCode::Down), 80).unwrap());
    dat.insert("first\n\n").unwrap();
    assert_eq!(Layout::input(&dat.text, 80).cursor(dat.pos, false), (2, 0));
    assert!(!dat.edit(key(KeyCode::Down), 80).unwrap());
    edit(&mut dat, KeyCode::Up, 80);
    assert_eq!(dat.pos, 6);
    edit(&mut dat, KeyCode::Up, 80);
    assert_eq!(dat.pos, 0);
    assert!(!dat.edit(key(KeyCode::Up), 80).unwrap());
  }

  #[test]
  fn words_and_deletion_use_grapheme_boundaries() {
    let mut dat = input("界e\u{301} one\n 👩‍💻 two");
    let alt = |code| KeyEvent::new(code, Mods::ALT);
    dat.edit(alt(KeyCode::Left), 80).unwrap();
    assert_eq!(dat.pos, "界e\u{301} one\n 👩‍💻 ".len());
    dat.edit(alt(KeyCode::Left), 80).unwrap();
    assert_eq!(dat.pos, "界e\u{301} one\n ".len());
    dat.edit(alt(KeyCode::Right), 80).unwrap();
    dat.edit(KeyEvent::new(KeyCode::Char('w'), Mods::CONTROL), 80).unwrap();
    assert_eq!(dat.text, "界e\u{301} one\n two");
    assert_eq!(dat.pos, "界e\u{301} one\n ".len());
    dat.edit(KeyEvent::new(KeyCode::Char('w'), Mods::CONTROL), 80).unwrap();
    assert_eq!(dat.text, "界e\u{301} two");
  }

  #[test]
  fn modified_backspace_deletes_words_or_only_the_current_logical_line() {
    for key in [
      KeyEvent::new(KeyCode::Backspace, Mods::ALT),
      KeyEvent::new(KeyCode::Char('w'), Mods::CONTROL),
    ] {
      let mut dat = input("keep\n界e\u{301} 👩‍💻  suffix");
      dat.pos = "keep\n界e\u{301} 👩‍💻  ".len();
      dat.edit(key, 7).unwrap();
      assert_eq!(dat.text, "keep\n界e\u{301} suffix");
      assert_eq!(dat.pos, "keep\n界e\u{301} ".len());
      let mut empty = Input::default();
      empty.edit(key, 7).unwrap();
      assert_eq!(empty, Input::default());
    }
    for key in [
      KeyEvent::new(KeyCode::Backspace, Mods::SUPER),
      KeyEvent::new(KeyCode::Char('u'), Mods::CONTROL),
    ] {
      for pos in ["keep\n".len(), "keep\n界e\u{301} 👩‍💻 ".len(), "keep\n界e\u{301} 👩‍💻 suffix".len()]
      {
        let mut dat = input("keep\n界e\u{301} 👩‍💻 suffix\nlast");
        dat.pos = pos;
        dat.edit(key, 7).unwrap();
        assert_eq!(dat.text, "keep\n\nlast");
        assert_eq!(dat.pos, "keep\n".len());
        dat.edit(key, 7).unwrap();
        assert_eq!(dat.text, "keep\nlast");
        assert_eq!(dat.pos, "keep".len());
      }
      for (text, pos, expected, cursor) in [
        ("\nnext", 0, "next", 0),
        ("keep\n", 5, "keep", 4),
        ("keep\n\n\nlast", 6, "keep\n\nlast", 5),
        ("", 0, "", 0),
        ("\n", 0, "", 0),
        ("\n", 1, "", 0),
      ] {
        let mut dat = input(text);
        dat.pos = pos;
        dat.edit(key, 7).unwrap();
        assert_eq!(dat.text, expected);
        assert_eq!(dat.pos, cursor);
      }
    }
  }

  #[test]
  fn super_modified_characters_do_not_become_draft_text() {
    let mut dat = input("keep 界");
    let saved = dat.clone();
    for mods in [Mods::SUPER, Mods::SUPER | Mods::SHIFT, Mods::HYPER, Mods::META] {
      dat.edit(KeyEvent::new(KeyCode::Char('x'), mods), 80).unwrap();
      assert_eq!(dat, saved);
    }
  }

  #[test]
  fn shared_layout_tracks_unicode_tab_and_soft_boundary_offsets() {
    let text = "界e\u{301}👩‍💻x\n\tend";
    let layout = Layout::input(text, 6);
    assert_eq!(
      layout.rows.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(),
      ["界e\u{301}👩‍💻", "x", "    e", "nd"]
    );
    let boundary = "界e\u{301}👩‍💻".len();
    assert_eq!(layout.cursor(boundary, false), (1, 0));
    assert_eq!(layout.cursor(boundary, true), (0, 5));
    assert_eq!(layout.cursor("界e\u{301}👩‍💻x\n\t".len(), false), (2, 4));
  }

  #[test]
  fn resize_and_tiny_layouts_preserve_source_and_reachable_cursor() {
    let text = "界👩‍💻e\u{301}\t\nend\n";
    let dat = input(text);
    for cols in [119, 40, 8, 3, 2, 1, 80] {
      let layout = Layout::input(&dat.text, cols);
      for (pos, _) in text.grapheme_indices(true).chain([(text.len(), "")]) {
        let (row, col) = layout.cursor(pos, false);
        assert!(row < layout.rows.len());
        assert!(col < cols);
        assert!(layout.rows[row].stops.iter().any(|&(p, _)| p == pos));
      }
      assert!(layout.rows.iter().all(|row| row.text.width() <= usize::from(cols)));
      assert_eq!(dat.text, text);
      assert_eq!(dat.pos, text.len());
    }
    let mut dat = input("abcdefgh");
    dat.pos = 1;
    edit(&mut dat, KeyCode::Down, 7);
    assert_eq!(dat.pos, 7);
    edit(&mut dat, KeyCode::Home, 80);
    assert_eq!(dat.pos, 0);
  }
}
