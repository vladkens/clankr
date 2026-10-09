//! Inline editor and append-only transcript on the terminal's main screen.
//! Ghostty 1.3.1 can clear marked prompt rows before reflow. Unknown terminals
//! retain the previous footer on resize rather than guessing and erasing history.

use std::io::{self, Write};

use crossterm::cursor::{MoveDown, MoveToColumn, MoveUp, RestorePosition, SavePosition};
use crossterm::queue;
use crossterm::terminal::{Clear, ClearType};
use unicode_width::UnicodeWidthStr;

use crate::Res;
use crate::input::{self, Input, Layout};

pub struct Frame<'a> {
  pub input: &'a Input,
  pub response: &'a str,
  pub activity: Option<String>,
  pub status: String,
  pub notice: Option<&'a str>,
}

#[derive(Default)]
pub struct Live {
  frozen: String,
  shown: String,
  lines: Vec<Line>,
  cursor: (usize, u16),
  cols: u16,
  rows: u16,
  sync: bool,
  redraw: bool,
  invalidated: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Style {
  #[default]
  Plain,
  Sent,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Origin {
  #[default]
  Transcript,
  Prompt,
  Continuation,
}

impl Origin {
  fn write(self, out: &mut impl Write) -> Res<()> {
    out.write_all(match self {
      Self::Transcript => b"\x1b]133;C\x07",
      Self::Prompt => b"\x1b]133;A;redraw=1\x07",
      Self::Continuation => b"\x1b]133;P;k=s\x07",
    })?;
    Ok(())
  }
}

#[derive(Default)]
struct Line {
  end: usize,
  text: String,
  style: Style,
  origin: Origin,
}

impl Line {
  fn same(&self, other: &Self) -> bool {
    self.text == other.text && self.style == other.style
  }

  fn write(&self, out: &mut impl Write) -> Res<()> {
    out.write_all(b"\x1b[0m")?;
    let mut text = self.text.as_str();
    if self.style == Style::Sent
      && let Some(rest) = text.strip_prefix('┃')
    {
      out.write_all("┃".as_bytes())?;
      text = rest;
    }
    let attr = match self.style {
      Style::Plain => "",
      Style::Sent => "\x1b[1m",
    };
    write!(out, "{attr}{text}\x1b[0m")?;
    Ok(())
  }
}

fn clean(text: &str, cursor: usize) -> (String, usize) {
  input::clean(text, cursor, true)
}

fn wrap(text: &str, cols: u16, first: &str, rest: &str, style: Style) -> Vec<Line> {
  let layout = Layout::new(text, cols, first, rest);
  layout
    .rows
    .into_iter()
    .map(|row| Line { text: row.text, end: row.end, style, origin: Origin::Transcript })
    .collect()
}

fn block(text: &str, cols: u16, style: Style) -> Vec<Line> {
  if text.is_empty() { Vec::new() } else { wrap(text, cols, "  ", "  ", style) }
}

fn nonempty(value: Option<&str>) -> Option<String> {
  value.map(|value| clean(value, 0).0).filter(|value| !value.is_empty())
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn supports_sync(term: &str, program: &str, multiplexed: bool) -> bool {
  !multiplexed
    && (matches!(term, "xterm-ghostty" | "xterm-kitty") || matches!(program, "ghostty" | "WezTerm"))
}

// Screen.resize in this release clears OSC 133 prompts before reflow and tracks DECSC pins:
// https://github.com/ghostty-org/ghostty/blob/v1.3.1/src/terminal/Screen.zig
// Do not infer this behavior from TERM alone: later development versions changed its ordering.
fn supports_redraw(term: &str, program: &str, version: &str, multiplexed: bool) -> bool {
  !multiplexed && (term == "xterm-ghostty" || program == "ghostty") && version == "1.3.1"
}

// Never leave DEC mode 2026 enabled across writes, event reads, or provider waits. Unknown
// terminals and multiplexers use the same single buffered write without private mode changes.
fn update(out: &mut impl Write, mut bytes: Vec<u8>, sync: bool) -> Res<()> {
  if bytes.is_empty() {
    return Ok(());
  }

  if sync {
    bytes.splice(..0, b"\x1b[?2026h".iter().copied());
    bytes.extend_from_slice(b"\x1b[?2026l");
  }
  if let Err(e) = out.write_all(&bytes).and_then(|()| out.flush()) {
    // A short write may have delivered the begin marker but not the end marker.
    if sync {
      let _ = out.write_all(b"\x1b[?2026l\x1b[0m");
      let _ = out.flush();
    }
    return Err(e.into());
  }
  Ok(())
}

fn position(out: &mut impl Write, from: &mut (usize, u16), to: (usize, u16)) -> Res<()> {
  if to.0 < from.0 {
    queue!(out, MoveUp((from.0 - to.0) as u16))?;
  } else if to.0 > from.0 {
    queue!(out, MoveDown((to.0 - from.0) as u16))?;
  }
  if to.1 != from.1 {
    queue!(out, MoveToColumn(to.1))?;
  }
  *from = to;
  Ok(())
}

pub fn header(out: &mut impl Write) -> Res<()> {
  write!(out, "\r\x1b[0m\x1b[1mclankr\x1b[0m v{}\x1b[0m\r\n\r\n", env!("CARGO_PKG_VERSION"))?;
  out.flush()?;
  Ok(())
}

impl Live {
  pub fn terminal() -> Self {
    let term = std::env::var("TERM").unwrap_or_default();
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let version = std::env::var("TERM_PROGRAM_VERSION").unwrap_or_default();
    let multiplexed = std::env::var_os("TMUX").is_some()
      || std::env::var_os("STY").is_some()
      || term.starts_with("screen")
      || term.starts_with("tmux");
    Self {
      sync: supports_sync(&term, &program, multiplexed),
      redraw: supports_redraw(&term, &program, &version, multiplexed),
      ..Self::default()
    }
  }

  // Appending CRLF at the old bottom scrolls natively; relative movement alone
  // must never stand in for allocating a new terminal row.
  fn patch(&self, out: &mut impl Write, lines: &[Line], cursor: (usize, u16)) -> Res<bool> {
    let mut pos = self.cursor;
    let mut changed = false;
    for i in 0..self.lines.len() {
      let same = lines.get(i).is_some_and(|line| line.same(&self.lines[i]));
      let origin = lines.get(i).map_or(Origin::Transcript, |line| line.origin);
      let marked = self.redraw && origin != self.lines[i].origin;
      if same && !marked {
        continue;
      }
      changed = true;
      position(out, &mut pos, (i, 0))?;
      if self.redraw {
        origin.write(out)?;
      }
      if same {
        continue;
      }
      // EL from column zero clears the row AND its soft-wrap edge in Ghostty; EL 2 leaves
      // the wrap edge intact. Every row written by this renderer has a hard CRLF boundary.
      queue!(out, Clear(ClearType::UntilNewLine))?;
      if let Some(line) = lines.get(i) {
        line.write(out)?;
        pos.1 = line.text.width() as u16;
      }
    }
    if lines.len() > self.lines.len() {
      changed = true;
      let start = self.lines.len();
      position(out, &mut pos, (start.saturating_sub(1), 0))?;
      for (i, line) in lines.iter().enumerate().skip(start) {
        if i > 0 {
          write!(out, "\r\n")?;
        }
        if self.redraw {
          queue!(out, Clear(ClearType::UntilNewLine))?;
          line.origin.write(out)?;
        }
        line.write(out)?;
        pos = (i, line.text.width() as u16);
      }
    }
    if self.redraw && changed {
      if let Some(start) = lines.iter().position(|line| line.origin == Origin::Prompt) {
        // Save after all CRLF scrolling. Ghostty remaps this anchor through resize and clamps
        // it to the top when the cleared owned rows no longer fit in the active screen.
        position(out, &mut pos, (start, 0))?;
        queue!(out, SavePosition)?;
        out.write_all(b"\x1b]133;B\x07")?;
      } else {
        Origin::Transcript.write(out)?;
      }
    }
    position(out, &mut pos, cursor)?;
    Ok(changed)
  }

  pub fn resize(&mut self) {
    self.invalidated = true;
  }

  fn relocated(&mut self, out: &mut impl Write, rows: u16) -> Res<()> {
    if self.lines.is_empty() {
      return Ok(());
    }
    if self.redraw {
      // The terminal erased only the marked prompt before reflow, including any rows which
      // would otherwise escape into scrollback. The saved origin is terminal-owned: neither
      // old wrap counts nor the editor cursor are used as an absolute screen position.
      queue!(out, RestorePosition, MoveToColumn(0))?;
      // A coalesced resize can report our old dimensions again. Clear the owned suffix at
      // its remapped origin even if the terminal did not actually change size this time.
      for i in 0..self.lines.len().min(usize::from(rows)) {
        if i > 0 {
          queue!(out, MoveDown(1))?;
        }
        queue!(out, Clear(ClearType::UntilNewLine))?;
      }
      queue!(out, RestorePosition, MoveToColumn(0))?;
      self.lines.clear();
      self.cursor = (0, 0);
      self.shown.clone_from(&self.frozen);
      return Ok(());
    }
    // Unknown reflow is not recoverable from the written row widths. Move to the physical
    // bottom without guessing wraps, then append; this fallback can retain a stale footer.
    queue!(out, MoveDown(u16::MAX))?;
    write!(out, "\x1b[0m\r\n")?;
    self.frozen.clone_from(&self.shown);
    self.lines.clear();
    self.cursor = (0, 0);
    Ok(())
  }

  pub fn clear(&mut self, out: &mut impl Write) -> Res<()> {
    let mut bytes = Vec::new();
    let changed = self.patch(&mut bytes, &[], (0, 0))?;
    update(out, bytes, changed && self.sync)?;
    self.lines.clear();
    self.cursor = (0, 0);
    Ok(())
  }

  pub fn prompt(&mut self, out: &mut impl Write, text: &str, cols: u16) -> Res<()> {
    let mut bytes = Vec::new();
    self.patch(&mut bytes, &[], (0, 0))?;
    for line in wrap(&clean(text, 0).0, cols, "┃ ", "┃ ", Style::Sent) {
      line.write(&mut bytes)?;
      write!(bytes, "\r\n")?;
    }
    write!(bytes, "\r\n")?;
    update(out, bytes, self.sync)?;
    self.lines.clear();
    self.cursor = (0, 0);
    self.frozen.clear();
    self.shown.clear();
    Ok(())
  }

  fn check_prefix(&self, response: &str, resizing: bool) -> Res<()> {
    let prefix = if resizing && !self.redraw { &self.shown } else { &self.frozen };
    if !response.starts_with(prefix) {
      return Err(
        invalid("Response changed an already retained prefix; finish each stream first").into(),
      );
    }
    Ok(())
  }

  pub fn draw(&mut self, out: &mut impl Write, frame: &Frame<'_>, cols: u16, rows: u16) -> Res<()> {
    if cols == 0 || rows == 0 {
      return Err(invalid("Terminal dimensions must be nonzero").into());
    }
    let response = clean(frame.response, 0).0;
    let resizing =
      !self.lines.is_empty() && (self.invalidated || cols != self.cols || rows != self.rows);
    self.check_prefix(&response, resizing)?;
    let mut bytes = Vec::new();
    if resizing {
      self.relocated(&mut bytes, rows)?;
    }
    self.cols = cols;
    self.rows = rows;
    self.invalidated = false;

    let thinking = nonempty(frame.activity.as_deref());
    let active = !response.is_empty() || thinking.is_some();
    let reserve = if active && rows >= 5 { 2 } else { 0 };
    let (footer, cursor) =
      footer(frame.input, frame.notice, &frame.status, cols, usize::from(rows) - reserve);
    let budget = usize::from(rows) - footer.len();
    let mut body = Vec::new();
    let mut prefix = Vec::new();
    let offset = self.frozen.len();
    let mut shown = offset;

    if !response.is_empty() {
      let mut tail = block(&response[offset..], cols, Style::Plain);
      let mut activity =
        thinking.as_deref().map(|text| wrap(text, cols, "", "", Style::Plain)).unwrap_or_default();
      activity.truncate(budget.saturating_sub(3));
      let space =
        budget.saturating_sub(1 + if activity.is_empty() { 0 } else { activity.len() + 1 });
      // Keep the last row mutable: later deltas can extend its last grapheme or change wrapping.
      let count = tail.len().saturating_sub(space.max(1));
      for line in tail.drain(..count) {
        shown = offset + line.end;
        prefix.push(line);
      }
      self.frozen = response[..shown].to_owned();
      tail.truncate(space);
      if let Some(last) = tail.last() {
        shown = offset + last.end;
      }
      body.extend(tail);
      if !body.is_empty() {
        body.push(Line::default());
      }
      if !activity.is_empty() {
        body.extend(activity);
        body.push(Line::default());
      }
    } else if let Some(text) = thinking {
      body = wrap(&text, cols, "", "", Style::Plain);
      body.truncate(budget.saturating_sub(1));
      if !body.is_empty() {
        body.push(Line::default());
      }
    }
    self.shown = response[..shown].to_owned();
    let cursor = (body.len() + cursor.0, cursor.1);
    body.extend(footer);
    let count = prefix.len();
    prefix.extend(body);
    if self.redraw {
      for (i, line) in prefix.iter_mut().enumerate().skip(count) {
        line.origin = if i == count { Origin::Prompt } else { Origin::Continuation };
      }
    }
    let changed = self.patch(&mut bytes, &prefix, (count + cursor.0, cursor.1))?;
    update(out, bytes, (changed || resizing) && self.sync)?;
    self.lines = prefix.drain(count..).collect();
    self.cursor = cursor;
    Ok(())
  }

  /// Freeze only the not-yet-committed suffix of the full accumulated answer, then its summary.
  /// Leaves one blank row before the next prompt; no draft or model footer is frozen normally.
  pub fn finish(
    &mut self,
    out: &mut impl Write,
    response: &str,
    summary: Option<&str>,
    cols: u16,
  ) -> Res<()> {
    let response = clean(response, 0).0;
    let resizing = !self.lines.is_empty() && (self.invalidated || cols != self.cols);
    self.check_prefix(&response, resizing)?;
    let mut bytes = Vec::new();
    if resizing {
      self.relocated(&mut bytes, self.rows)?;
    }
    let mut lines = block(&response[self.frozen.len()..], cols, Style::Plain);
    if !response.is_empty() {
      lines.push(Line::default());
    }
    if let Some(summary) = nonempty(summary) {
      lines.extend(block(&summary, cols, Style::Plain));
      lines.push(Line::default());
    }
    lines.push(Line::default());
    let changed = self.patch(&mut bytes, &lines, (lines.len() - 1, 0))?;
    update(out, bytes, (changed || resizing) && self.sync)?;
    self.lines.clear();
    self.cursor = (0, 0);
    self.frozen.clear();
    self.shown.clear();
    self.invalidated = false;
    Ok(())
  }
}

fn footer(
  input: &Input,
  notice: Option<&str>,
  status: &str,
  cols: u16,
  budget: usize,
) -> (Vec<Line>, (usize, u16)) {
  let layout = Layout::input(&input.text, cols);
  let cursor = layout.cursor(input.pos, input.trailing);
  let label = notice.unwrap_or(status);
  let label = clean(label, 0).0;
  let mut status = wrap(&label, cols, "", "", Style::Plain);
  status.truncate(usize::from(budget >= 3));
  let gap = usize::from(!status.is_empty());
  let count = layout.rows.len().min(budget - status.len() - gap);
  let start = cursor.0.saturating_sub(count - 1).min(layout.rows.len() - count);
  let mut lines: Vec<_> = layout
    .rows
    .into_iter()
    .skip(start)
    .take(count)
    .map(|row| Line { text: row.text, ..Line::default() })
    .collect();
  if gap > 0 {
    lines.push(Line::default());
  }
  lines.extend(status);
  (lines, (cursor.0 - start, cursor.1))
}

#[cfg(test)]
mod tests {
  use unicode_segmentation::UnicodeSegmentation;

  use super::*;

  fn frame(input: &Input) -> Frame<'_> {
    Frame { input, response: "", activity: None, status: "echo".into(), notice: None }
  }

  // Small main-screen interpreter: retains scrolled rows, clamps cursor movement to the screen,
  // and rejects every ANSI command except cursor movement, line erase, SGR and sync updates.
  struct Screen {
    lines: Vec<String>,
    origins: Vec<Origin>,
    wraps: Vec<bool>,
    row: usize,
    col: usize,
    top: usize,
    height: usize,
    sync: bool,
    snapshots: Vec<String>,
    erased: Vec<usize>,
    prompt: bool,
    saved: Option<(usize, usize)>,
  }

  impl Screen {
    fn new(height: usize) -> Self {
      Self {
        lines: vec![String::new()],
        origins: vec![Origin::Transcript],
        wraps: vec![false],
        row: 0,
        col: 0,
        top: 0,
        height,
        sync: false,
        snapshots: Vec::new(),
        erased: Vec::new(),
        prompt: false,
        saved: None,
      }
    }

    fn feed(&mut self, bytes: &[u8]) {
      let text = std::str::from_utf8(bytes).unwrap();
      let mut chars = text.chars().peekable();
      while let Some(c) = chars.next() {
        match c {
          '\x1b' => {
            match chars.next().unwrap() {
              '7' => {
                self.saved = Some((self.row, self.col));
                continue;
              }
              '8' => {
                (self.row, self.col) = self.saved.unwrap();
                continue;
              }
              ']' => {
                let mut osc = String::new();
                for c in chars.by_ref() {
                  if c == '\x07' {
                    break;
                  }
                  osc.push(c);
                }
                self.origins.resize(self.origins.len().max(self.row + 1), Origin::Transcript);
                match osc.as_str() {
                  "133;A;redraw=1" => {
                    assert_eq!(self.col, 0, "prompt marker must not insert a fresh line");
                    self.origins[self.row] = Origin::Prompt;
                    self.prompt = true;
                  }
                  "133;P;k=s" => {
                    self.origins[self.row] = Origin::Continuation;
                    self.prompt = true;
                  }
                  "133;B" => self.prompt = true,
                  "133;C" => {
                    self.prompt = false;
                    if self.col == 0 {
                      self.origins[self.row] = Origin::Transcript;
                    }
                  }
                  _ => panic!("forbidden OSC: {osc}"),
                }
                continue;
              }
              '[' => {}
              c => panic!("forbidden ESC: {c}"),
            }
            let mut arg = String::new();
            let cmd = loop {
              let c = chars.next().unwrap();
              if c.is_ascii_alphabetic() {
                break c;
              }
              arg.push(c);
            };
            if arg == "?2026" {
              match cmd {
                'h' => {
                  assert!(!self.sync, "nested synchronized frame");
                  self.sync = true;
                }
                'l' => {
                  self.sync = false;
                  self.snapshots.push(self.text());
                }
                _ => panic!("forbidden terminal mode: {arg}{cmd}"),
              }
              continue;
            }
            if cmd == 'm' {
              assert!(matches!(arg.as_str(), "0" | "1"), "use terminal defaults and bold only");
              continue;
            }
            let n: usize = if arg.is_empty() { 0 } else { arg.parse().unwrap() };
            match cmd {
              'A' => self.row = self.row.saturating_sub(n).max(self.top),
              'B' => self.row = (self.row + n).min(self.top + self.height - 1),
              'G' => self.col = n - 1,
              'K' => {
                assert_eq!(n, 0, "only erase to end of current line is allowed");
                assert_eq!(self.col, 0, "owned rows must be erased from column zero");
                self.lines.resize_with(self.lines.len().max(self.row + 1), String::new);
                self.lines[self.row].clear();
                self.wraps.resize(self.lines.len(), false);
                self.wraps[self.row] = false;
                self.erased.push(self.row);
              }
              _ => panic!("forbidden terminal command: {arg}{cmd}"),
            }
          }
          '\r' => self.col = 0,
          '\n' => {
            self.row += 1;
            self.top = self.top.max((self.row + 1).saturating_sub(self.height));
            self.origins.resize(self.origins.len().max(self.row + 1), Origin::Transcript);
            if self.prompt {
              self.origins[self.row] = Origin::Continuation;
            }
          }
          c => {
            assert!(!c.is_control());
            self.lines.resize_with(self.lines.len().max(self.row + 1), String::new);
            let line = &mut self.lines[self.row];
            // Writes start on a new/cleared row; cursor-only positioning never inserts text.
            assert!(line.width() <= self.col);
            line.push_str(&" ".repeat(self.col - line.width()));
            line.push(c);
            self.col = line.width();
          }
        }
      }
      self.lines.resize_with(self.lines.len().max(self.row + 1), String::new);
      self.origins.resize(self.lines.len(), Origin::Transcript);
      self.wraps.resize(self.lines.len(), false);
    }

    fn text(&self) -> String {
      self.lines.iter().map(|s| s.trim_end()).collect::<Vec<_>>().join("\n").trim_end().into()
    }

    fn resize(&mut self, cols: u16, rows: usize) {
      if self.prompt {
        let start = self.origins[..=self.row].iter().rposition(|&x| x == Origin::Prompt).unwrap();
        for line in &mut self.lines[start..] {
          line.clear();
        }
      }
      let mut lines = Vec::new();
      let mut origins = Vec::new();
      let mut wraps = Vec::new();
      let mut cursor = (0, 0);
      let mut saved = None;
      let mut line = String::new();
      for (i, text) in self.lines.iter().enumerate() {
        if self.saved.is_some_and(|(row, _)| row == i) {
          saved = Some((lines.len(), line.width()));
        }
        let mut used = 0;
        for g in text.graphemes(true) {
          if line.width() + g.width() > usize::from(cols) {
            lines.push(std::mem::take(&mut line));
            origins.push(self.origins[i]);
            wraps.push(true);
          }
          if i == self.row && used <= self.col {
            cursor = (lines.len(), line.width());
          }
          line.push_str(g);
          used += g.width();
        }
        if i == self.row && self.col >= used {
          cursor = (lines.len(), line.width());
        }
        if !self.wraps[i] || i + 1 == self.lines.len() {
          lines.push(std::mem::take(&mut line));
          origins.push(self.origins[i]);
          wraps.push(false);
        }
      }
      self.lines = lines;
      self.origins = origins;
      self.wraps = wraps;
      (self.row, self.col) = cursor;
      self.height = rows;
      self.top = (self.row + 1).saturating_sub(rows);
      // Model a terminal which keeps the cursor visible and clips rows below the new screen.
      self.lines.truncate(self.top + self.height);
      self.origins.truncate(self.lines.len());
      self.wraps.truncate(self.lines.len());
      self.saved = saved.map(|(row, col)| {
        if row < self.top || row >= self.top + self.height { (self.top, 0) } else { (row, col) }
      });
    }
  }

  fn apply(screen: &mut Screen, f: impl FnOnce(&mut Vec<u8>)) -> String {
    let mut out = Vec::new();
    f(&mut out);
    screen.feed(&out);
    String::from_utf8(out).unwrap()
  }

  #[test]
  fn completed_turns_survive_scrolling_and_cursor_moves_do_not_redraw_text() {
    let mut live = Live { redraw: true, sync: true, ..Live::default() };
    let mut screen = Screen::new(8);
    let mut input = Input::default();
    for index in 0..20 {
      input.insert(&format!("message-{index:02}\nsecond line")).unwrap();
      apply(&mut screen, |out| live.draw(out, &frame(&input), 80, 8).unwrap());
      apply(&mut screen, |out| {
        live.prompt(out, &input.text, 80).unwrap();
        live.finish(out, &input.text, None, 80).unwrap();
      });
      input = Input::default();
      apply(&mut screen, |out| live.draw(out, &frame(&input), 80, 8).unwrap());
    }
    for index in 0..20 {
      assert_eq!(screen.text().matches(&format!("message-{index:02}")).count(), 2);
    }
    input.insert("next").unwrap();
    apply(&mut screen, |out| live.draw(out, &frame(&input), 80, 8).unwrap());
    input.pos -= 1;
    let delta = apply(&mut screen, |out| live.draw(out, &frame(&input), 80, 8).unwrap());
    assert_eq!(delta, "\x1b[6G");
    assert!(apply(&mut screen, |out| live.draw(out, &frame(&input), 80, 8).unwrap()).is_empty());
    apply(&mut screen, |out| live.clear(out).unwrap());
    assert!(!screen.text().contains("next"));
    assert_eq!(screen.text().matches("message-19").count(), 2);
  }

  #[test]
  fn supported_prompt_reflow_preserves_history_and_the_editable_cursor() {
    let mut live = Live { redraw: true, sync: true, ..Live::default() };
    let mut screen = Screen::new(53);
    apply(&mut screen, |out| {
      live.prompt(out, "HISTORY", 119).unwrap();
      live.finish(out, "HISTORY", None, 119).unwrap();
    });
    let mut input = Input::default();
    input.insert(&"line 界 e\u{301}\n".repeat(30)).unwrap();
    apply(&mut screen, |out| live.draw(out, &frame(&input), 119, 53).unwrap());
    for (cols, rows) in [(40, 12), (80, 24), (119, 53), (119, 12), (2, 2), (1, 1), (80, 24)] {
      screen.resize(cols, rows);
      live.resize();
      let delta =
        apply(&mut screen, |out| live.draw(out, &frame(&input), cols, rows as u16).unwrap());
      assert!(!delta.contains("HISTORY"));
      assert!(screen.row >= screen.top && screen.row < screen.top + rows);
      assert!(screen.col < usize::from(cols));
      assert_eq!(input.text, "line 界 e\u{301}\n".repeat(30));
    }
  }

  #[test]
  fn viewport_keeps_cursor_visible_at_both_ends_in_tiny_windows() {
    let mut input = Input::default();
    input.insert(&"界👩‍💻e\u{301}\t\n".repeat(20)).unwrap();
    for pos in [0, "界".len(), input.text.len()] {
      input.pos = pos;
      for cols in [1, 2, 3, 8, 40, 119] {
        for rows in [1, 2, 3, 12, 53] {
          let (lines, cursor) = footer(&input, None, "echo", cols, rows);
          assert!(lines.len() <= rows);
          assert!(cursor.0 < lines.len() && cursor.1 < cols);
          assert!(lines.iter().all(|line| line.text.width() <= usize::from(cols)));
          let layout = Layout::input(&input.text, cols);
          let source = layout.cursor(input.pos, input.trailing);
          assert_eq!(lines[cursor.0].text, layout.rows[source.0].text);
        }
      }
    }
  }

  #[test]
  fn unsupported_resize_retains_history_without_reprinting_completed_turns() {
    let mut live = Live::default();
    let mut input = Input::default();
    let mut out = Vec::new();
    live.prompt(&mut out, "kept", 80).unwrap();
    live.finish(&mut out, "kept", None, 80).unwrap();
    input.insert("draft").unwrap();
    live.draw(&mut out, &frame(&input), 80, 24).unwrap();
    out.clear();
    live.resize();
    live.draw(&mut out, &frame(&input), 40, 12).unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("\x1b[65535B"));
    assert!(!output.contains("kept"));
    assert!(!output.contains("\x1b[2J") && !output.contains("\x1b[3J"));
    assert!(!supports_redraw("xterm-ghostty", "ghostty", "unknown", false));
    assert!(!supports_redraw("xterm-ghostty", "ghostty", "1.3.1", true));
  }

  #[test]
  fn streaming_response_survives_resize_and_completion_without_duplicate_history() {
    let mut live = Live { redraw: true, sync: true, ..Live::default() };
    let mut screen = Screen::new(12);
    let mut input = Input::default();
    input.insert("next draft\nwith another line").unwrap();
    let mut response = String::new();
    let (mut cols, mut rows) = (80, 12);
    apply(&mut screen, |out| live.prompt(out, "original question", cols).unwrap());
    let waiting = Frame { activity: Some("• Thinking".into()), ..frame(&input) };
    apply(&mut screen, |out| live.draw(out, &waiting, cols, rows).unwrap());
    assert_eq!(screen.text().lines().nth(2), Some("• Thinking"));
    for index in 0..100 {
      response.push_str(&format!("line-{index:03} 界\n"));
      if index == 30 || index == 60 {
        (cols, rows) = if index == 30 { (40, 12) } else { (80, 24) };
        screen.resize(cols, usize::from(rows));
        live.resize();
      }
      let frame = Frame { response: &response, ..frame(&input) };
      apply(&mut screen, |out| live.draw(out, &frame, cols, rows).unwrap());
    }
    apply(&mut screen, |out| live.finish(out, &response, None, cols).unwrap());
    apply(&mut screen, |out| live.draw(out, &frame(&input), cols, rows).unwrap());
    let text = screen.text();
    assert_eq!(text.matches("original question").count(), 1);
    for index in 0..100 {
      assert_eq!(text.matches(&format!("line-{index:03}")).count(), 1);
    }
    assert_eq!(text.matches("next draft").count(), 1);
    assert_eq!(input.text, "next draft\nwith another line");
  }
}
