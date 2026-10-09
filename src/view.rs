//! Inline editor and append-only transcript on the terminal's main screen.
//! Ghostty 1.3.1 can clear marked prompt rows before reflow. Unknown terminals
//! retain the previous footer on resize rather than guessing and erasing history.

use std::io::{self, Write};
use std::ops::Range;
use std::sync::LazyLock;

use crossterm::cursor::{MoveDown, MoveToColumn, MoveUp, RestorePosition, SavePosition};
use crossterm::queue;
use crossterm::style::{Attribute, SetAttribute, Stylize};
use crossterm::terminal::{BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::Res;
use crate::input::{self, Input, Layout};
use crate::markdown::{Markdown, Text};
use crate::menu::{Item, Kind, Menu, VISIBLE};

pub struct Frame<'a> {
  pub input: &'a Input,
  pub response: &'a str,
  pub activity: Option<String>,
  pub status: String,
  pub notice: Option<&'a str>,
  pub menu: Option<&'a Menu>,
  pub messages: &'a [String],
  pub queued: Vec<&'a str>,
  pub paused: bool,
}

#[derive(Default)]
pub struct Live {
  markdown: Markdown,
  // Text already written to native scrollback; only the footer is repainted.
  shown: String,
  // Current output block, restarted only when resize cannot preserve its anchor.
  start: usize,
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
  Bold,
  Faint,
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
  text: String,
  style: Style,
  origin: Origin,
  marks: Vec<Range<usize>>,
  ansi: Vec<u8>,
}

impl Line {
  fn same(&self, other: &Self) -> bool {
    self.text == other.text
      && self.style == other.style
      && self.marks == other.marks
      && self.ansi == other.ansi
  }

  fn write(&self, out: &mut impl Write) -> Res<()> {
    if !self.ansi.is_empty() {
      out.write_all(&self.ansi)?;
      return Ok(());
    }
    queue!(out, SetAttribute(Attribute::Reset))?;
    if !self.marks.is_empty() {
      let mut col = 0;
      let mut bold = false;
      for g in self.text.graphemes(true) {
        let marked = self.marks.iter().any(|range| range.contains(&col));
        if marked != bold {
          queue!(out, SetAttribute(if marked { Attribute::Bold } else { Attribute::Reset }))?;
          bold = marked;
        }
        out.write_all(g.as_bytes())?;
        col += g.width();
      }
      queue!(out, SetAttribute(Attribute::Reset))?;
      return Ok(());
    }
    match self.style {
      Style::Plain => {}
      Style::Bold => queue!(out, SetAttribute(Attribute::Bold))?,
      Style::Faint => queue!(out, SetAttribute(Attribute::Dim))?,
    }
    write!(out, "{}{}", self.text, SetAttribute(Attribute::Reset))?;
    Ok(())
  }
}

// One cell short of the edge, like the editor, so drawing it never triggers a wrap.
fn rule(cols: u16) -> Line {
  Line {
    text: "─".repeat(usize::from(cols.saturating_sub(1))),
    style: Style::Faint,
    ..Line::default()
  }
}

fn clean(text: &str, cursor: usize) -> (String, usize) {
  input::clean(text, cursor, true)
}

fn wrap(text: &str, cols: u16, first: &str, rest: &str, style: Style) -> Vec<Line> {
  let layout = Layout::new(text, cols, first, rest);
  layout.rows.into_iter().map(|row| Line { text: row.text, style, ..Line::default() }).collect()
}

// Summaries and notices are meta, not copy targets: faint and indented.
fn meta(text: &str, cols: u16) -> Vec<Line> {
  if text.is_empty() { Vec::new() } else { wrap(text, cols, "  ", "  ", Style::Faint) }
}

fn preview_lines(text: &Text, cols: u16) -> Res<Vec<Line>> {
  if text.text.is_empty() {
    return Ok(Vec::new());
  }
  Layout::new(&text.text, cols, "", "")
    .rows
    .into_iter()
    .map(|row| {
      let range = row.stops.first().unwrap().0..row.stops.last().unwrap().0;
      let mut ansi = Vec::new();
      // Tiny layouts can replace a wide grapheme; retain that safe display instead.
      if row.text == text.text[range.clone()] {
        text.write(&mut ansi, range)?;
      }
      Ok(Line { text: row.text, ansi, ..Line::default() })
    })
    .collect()
}

// Native wrap uses every cell; unlike the editor it adds no decorative prefix.
fn edge(text: &str, cols: u16) -> u16 {
  let width = usize::from(cols.max(1));
  let mut used = 0;
  let mut edge = 0;
  for g in text.graphemes(true) {
    let size = g.width();
    if used + size > width {
      used = 0;
    }
    edge = used as u16;
    used += size;
  }
  edge
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
    let mut begin = Vec::new();
    queue!(begin, BeginSynchronizedUpdate)?;
    bytes.splice(..0, begin);
    queue!(bytes, EndSynchronizedUpdate)?;
  }
  if let Err(e) = out.write_all(&bytes).and_then(|()| out.flush()) {
    // A short write may have delivered the begin marker but not the end marker.
    if sync {
      let _ = queue!(out, EndSynchronizedUpdate, SetAttribute(Attribute::Reset));
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

const SPLASHES: &[&str] = &[
  "roger roger.",
  "no disassemble. just refactor.",
  "do androids lint electric sheep?",
  "i'm sorry, dave. use sudo.",
  "your clothes. your boots. your dotfiles.",
  "bite my shiny metal diff.",
  "these aren't the bugs you're looking for.",
  "some assembly required.",
  "sentience sold separately.",
  "three laws. zero tests.",
  "your move, meatbag.",
  "the cake is a dependency.",
];

static SPLASH: LazyLock<&str> = LazyLock::new(|| SPLASHES[fastrand::usize(..SPLASHES.len())]);

pub fn header(out: &mut impl Write) -> Res<()> {
  write!(
    out,
    "\r{}:: {} v{} · {}\r\n\r\n",
    SetAttribute(Attribute::Reset),
    "clankr".bold(),
    env!("CARGO_PKG_VERSION"),
    *SPLASH
  )?;
  out.flush()?;
  Ok(())
}

pub fn separator(out: &mut impl Write, cols: u16) -> Res<()> {
  out.write_all(b"\r")?;
  rule(cols).write(out)?;
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
      // Only UI rows are erased here. Response rows retain their native soft-wrap edges.
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
    position(out, &mut pos, cursor)?;
    Ok(changed)
  }

  pub fn resize(&mut self) {
    self.invalidated = true;
  }

  // Re-establish the footer below the terminal-owned response cursor after reflow.
  fn relocated(&mut self, out: &mut impl Write, rows: u16) -> Res<()> {
    if self.lines.is_empty() {
      return Ok(());
    }
    let clipped = self.gap() > 0 && usize::from(rows) <= self.cursor.0 + self.gap();
    if self.redraw && !clipped {
      queue!(out, RestorePosition)?;
      if self.gap() > 0 {
        Origin::Transcript.write(out)?;
        self.last(out)?;
        write!(out, "\r\n")?;
      }
      // Also handle a resize notification coalesced back to the previous dimensions.
      queue!(out, SavePosition)?;
      for i in 0..self.lines.len().min(usize::from(rows)) {
        if i > 0 {
          queue!(out, MoveDown(1))?;
        }
        queue!(out, Clear(ClearType::UntilNewLine))?;
      }
      queue!(out, RestorePosition)?;
    } else {
      // Unknown reflow or an anchor pushed outside the screen cannot be recovered.
      // Preserve the text and continue a new block rather than overwrite history.
      queue!(out, MoveDown(u16::MAX))?;
      write!(out, "{}\r\n", SetAttribute(Attribute::Reset))?;
      self.start = self.shown.len();
    }
    self.lines.clear();
    self.cursor = (0, 0);
    self.anchor(out)?;
    Ok(())
  }

  fn gap(&self) -> usize {
    usize::from(self.shown.len() > self.start && !self.shown.ends_with('\n'))
  }

  // Save AFTER footer allocation, since native scrolling moves the response upwards.
  // Anchor a real cell: reflow can discard the blank insertion cell after the text.
  fn anchor(&self, out: &mut impl Write) -> Res<()> {
    let mut pos = self.cursor;
    position(out, &mut pos, (0, 0))?;
    if self.gap() > 0 {
      queue!(out, MoveUp(1))?;
      let text = self.shown[self.start..].rsplit('\n').next().unwrap_or("");
      let col = edge(text, self.cols);
      queue!(out, MoveToColumn(col))?;
    }
    queue!(out, SavePosition)?;
    if self.gap() > 0 {
      queue!(out, MoveDown(1), MoveToColumn(0))?;
    }
    pos = (0, 0);
    if self.redraw {
      out.write_all(b"\x1b]133;B\x07")?;
    }
    position(out, &mut pos, self.cursor)?;
    Ok(())
  }

  fn last(&self, out: &mut impl Write) -> Res<()> {
    let start = self.shown.grapheme_indices(true).next_back().unwrap().0;
    self.markdown.text().write(out, start..self.shown.len())
  }

  fn append(&mut self, out: &mut impl Write, complete: bool) -> Res<bool> {
    let response = &self.markdown.text().text;
    if !response.starts_with(&self.shown) {
      return Err(invalid("Response changed an already written prefix").into());
    }
    if !complete && self.rows < 2 {
      return Ok(false);
    }
    // The streaming parser emits complete blocks or code lines, including grapheme boundaries.
    let end = if complete { response.len() } else { self.markdown.committed_len() };
    if end <= self.shown.len() {
      return Ok(false);
    }
    self.patch(out, &[], (0, 0))?;
    if !self.lines.is_empty() || self.shown.len() > self.start {
      queue!(out, RestorePosition)?;
    }
    if self.redraw {
      Origin::Transcript.write(out)?;
    }
    if self.gap() > 0 {
      // Restore the last grapheme as well as the cursor, recreating pending-wrap
      // when it occupied the rightmost cell before we moved down to the editor.
      self.last(out)?;
    }
    self.markdown.text().write(out, self.shown.len()..end)?;
    self.shown = response[..end].into();
    if self.gap() > 0 {
      out.write_all(b"\r\n")?;
    }
    self.lines.clear();
    self.cursor = (0, 0);
    Ok(true)
  }

  pub fn clear(&mut self, out: &mut impl Write) -> Res<()> {
    let mut bytes = Vec::new();
    let changed = self.patch(&mut bytes, &[], (0, 0))?;
    update(out, bytes, changed && self.sync)?;
    self.lines.clear();
    self.cursor = (0, 0);
    Ok(())
  }

  pub fn restart(&mut self, out: &mut impl Write, rows: u16) -> Res<()> {
    let mut bytes = Vec::new();
    self.patch(&mut bytes, &[], (0, 0))?;
    // Scroll completed output into native history, then start on the fresh screen.
    // Never erase the user's terminal scrollback.
    write!(bytes, "{}", "\r\n".repeat(usize::from(rows.max(1))))?;
    if rows > 1 {
      queue!(bytes, MoveUp(rows - 1))?;
    }
    queue!(bytes, MoveToColumn(0))?;
    if self.redraw {
      Origin::Transcript.write(&mut bytes)?;
    }
    header(&mut bytes)?;
    update(out, bytes, self.sync)?;
    *self = Self { sync: self.sync, redraw: self.redraw, ..Self::default() };
    Ok(())
  }

  // Bold marks the user's role without a colour. The terminal wraps it, so a selection copies only
  // the user's own line breaks; no prefix or frame enters the clipboard.
  pub fn prompt(&mut self, out: &mut impl Write, text: &str) -> Res<()> {
    let mut bytes = Vec::new();
    self.patch(&mut bytes, &[], (0, 0))?;
    queue!(bytes, SetAttribute(Attribute::Reset), SetAttribute(Attribute::Bold))?;
    write!(bytes, "{}", clean(text, 0).0.replace('\n', "\r\n"))?;
    queue!(bytes, SetAttribute(Attribute::Reset))?;
    write!(bytes, "\r\n\r\n")?;
    update(out, bytes, self.sync)?;
    self.lines.clear();
    self.cursor = (0, 0);
    self.start = 0;
    self.shown.clear();
    self.markdown = Markdown::default();
    Ok(())
  }

  pub fn message(&mut self, out: &mut impl Write, text: &str, cols: u16) -> Res<()> {
    let mut bytes = Vec::new();
    self.patch(&mut bytes, &[], (0, 0))?;
    for line in meta(&clean(text, 0).0, cols) {
      line.write(&mut bytes)?;
      write!(bytes, "\r\n")?;
    }
    write!(bytes, "\r\n")?;
    update(out, bytes, self.sync)?;
    self.lines.clear();
    self.cursor = (0, 0);
    Ok(())
  }

  pub fn draw(&mut self, out: &mut impl Write, frame: &Frame<'_>, cols: u16, rows: u16) -> Res<()> {
    if cols == 0 || rows == 0 {
      return Err(invalid("Terminal dimensions must be nonzero").into());
    }
    self.markdown.update(frame.response, false, cols);
    let resizing =
      !self.lines.is_empty() && (self.invalidated || cols != self.cols || rows != self.rows);
    let mut bytes = Vec::new();
    self.cols = cols;
    if resizing {
      self.relocated(&mut bytes, rows)?;
    }
    self.rows = rows;
    self.invalidated = false;

    let appended = self.append(&mut bytes, false)?;
    let thinking = nonempty(frame.activity.as_deref());
    let gap = usize::from(!self.shown.is_empty());
    let reserve = self.gap().min(usize::from(rows.saturating_sub(1)));
    let budget = usize::from(rows) - reserve;
    let mut preview = preview_lines(self.markdown.preview(), cols)?;
    let limit = budget.saturating_sub(7).min(budget / 2);
    if preview.len() > limit {
      preview.drain(..preview.len() - limit);
    }
    let pending = preview.len() + usize::from(!preview.is_empty());
    let extra = (gap + pending + usize::from(thinking.is_some()) * 2).min(budget.saturating_sub(1));
    let (footer, cursor) = footer(frame, cols, budget - extra);
    let mut lines = Vec::new();
    if gap > 0 && extra > 0 {
      lines.push(Line::default());
    }
    if !preview.is_empty() {
      lines.append(&mut preview);
      lines.push(Line::default());
    }
    if let Some(text) = thinking {
      let mut activity = wrap(&text, cols, "", "", Style::Plain);
      activity.truncate(extra.saturating_sub(lines.len() + 1));
      if !activity.is_empty() {
        lines.extend(activity);
        lines.push(Line::default());
      }
    }
    let cursor = (lines.len() + cursor.0, cursor.1);
    lines.extend(footer);
    if self.redraw {
      for (i, line) in lines.iter_mut().enumerate() {
        line.origin = if i == 0 { Origin::Prompt } else { Origin::Continuation };
      }
    }
    let changed = self.patch(&mut bytes, &lines, cursor)?;
    self.lines = lines;
    self.cursor = cursor;
    if changed || resizing || appended {
      self.anchor(&mut bytes)?;
    }
    update(out, bytes, (changed || resizing || appended) && self.sync)?;
    Ok(())
  }

  /// Flush pending Markdown and the summary, leaving the next editor below native history.
  pub fn finish(
    &mut self,
    out: &mut impl Write,
    response: &str,
    summary: Option<&str>,
    cols: u16,
  ) -> Res<()> {
    self.markdown.update(response, true, cols);
    let resizing = !self.lines.is_empty() && (self.invalidated || cols != self.cols);
    let mut bytes = Vec::new();
    self.cols = cols;
    if resizing {
      self.relocated(&mut bytes, self.rows)?;
    }
    let appended = self.append(&mut bytes, true)?;
    let mut lines = Vec::new();
    if !response.is_empty() {
      lines.push(Line::default());
    }
    if let Some(summary) = nonempty(summary) {
      lines.extend(meta(&summary, cols));
      lines.push(Line::default());
    }
    lines.push(Line::default());
    let changed = self.patch(&mut bytes, &lines, (lines.len() - 1, 0))?;
    update(out, bytes, (changed || resizing || appended) && self.sync)?;
    self.lines.clear();
    self.cursor = (0, 0);
    self.start = 0;
    self.shown.clear();
    self.markdown = Markdown::default();
    self.invalidated = false;
    Ok(())
  }
}

fn footer(frame: &Frame<'_>, cols: u16, budget: usize) -> (Vec<Line>, (usize, u16)) {
  let input = frame.input;
  let layout = Layout::input(&input.text, cols);
  let cursor = layout.cursor(input.pos, input.trailing);
  let label = frame.notice.unwrap_or(&frame.status);
  let label = clean(label, 0).0;
  let menu = frame.menu.filter(|_| frame.notice.is_none());
  // Rules frame the editor once the input and status fit; the lower one also tops the menu.
  let ruled = budget >= 4;
  let rules = 2 * usize::from(ruled);
  let mut status = if let Some(menu) = menu {
    menu_lines(menu, cols, budget - 1 - rules)
  } else {
    let mut status = wrap(&label, cols, "", "", Style::Plain);
    status.truncate(usize::from(budget >= 2));
    status
  };
  let available = budget - status.len() - rules;
  let limit = available.saturating_sub(2);
  let mut queued = Vec::new();
  if !frame.queued.is_empty() && limit > 0 {
    let title = if frame.paused { "Queue paused" } else { "Queued" };
    let resume = if frame.paused { " · enter resume" } else { "" };
    let width = usize::from(cols.saturating_sub(3));
    let title = format!("{title}: {}{resume} · ↑ edit last", frame.queued.len());
    queued.extend(meta(&clip(&title, width), cols));
    let count = limit.saturating_sub(1).min(3);
    for (i, text) in frame.queued.iter().enumerate().skip(frame.queued.len().saturating_sub(count))
    {
      let text = clean(&clip(text, width), 0).0.replace('\n', " ↵ ");
      queued.extend(meta(&clip(&format!("{}. {text}", i + 1), width), cols));
    }
  }
  let mut lines: Vec<_> =
    frame.messages.iter().flat_map(|message| meta(&clean(message, 0).0, cols)).collect();
  let remaining = limit.saturating_sub(queued.len());
  if lines.len() > remaining {
    lines.drain(..lines.len() - remaining);
  }
  lines.extend(queued);
  if !lines.is_empty() {
    lines.push(Line::default());
  }
  let count = layout.rows.len().min(available - lines.len());
  if ruled {
    lines.push(rule(cols));
  }
  let lead = lines.len();
  let start = cursor.0.saturating_sub(count - 1).min(layout.rows.len() - count);
  lines.extend(layout.rows.into_iter().skip(start).take(count).map(|row| {
    let mut marks = Vec::new();
    for skill in &input.skills {
      let start = row.stops.iter().find(|&&(pos, _)| pos >= skill.range.start);
      let end =
        row.stops.iter().find(|&&(pos, _)| pos >= skill.range.end).or_else(|| row.stops.last());
      if let (Some(&(pos, start)), Some(&(_, end))) = (start, end)
        && pos < skill.range.end
        && start < end
      {
        marks.push(usize::from(start)..usize::from(end));
      }
    }
    Line { text: row.text, marks, ..Line::default() }
  }));
  if ruled {
    lines.push(rule(cols));
  }
  lines.append(&mut status);
  (lines, (lead + cursor.0 - start, cursor.1))
}

fn clip(text: &str, width: usize) -> String {
  if text.width() <= width {
    return text.into();
  }
  if width == 0 {
    return String::new();
  }
  let mut used = 0;
  let mut out = String::new();
  for g in text.graphemes(true) {
    if used + g.width() >= width {
      break;
    }
    used += g.width();
    out.push_str(g);
  }
  out.push('…');
  out
}

fn menu_lines(menu: &Menu, cols: u16, budget: usize) -> Vec<Line> {
  if budget == 0 {
    return Vec::new();
  }
  let width = usize::from(cols.saturating_sub(1).max(1));
  // A full menu sits below the editor's lower rule, which serves as its top border.
  let full = budget >= 5;
  let visible = (if full { budget - 4 } else { budget }).min(VISIBLE).min(menu.items.len().max(1));
  let start = menu.offset.min(menu.selected).max((menu.selected + 1).saturating_sub(visible));
  let end = (start + visible).min(menu.items.len());
  let line = |text: &str, selected| Line {
    text: clip(text, width),
    style: if selected { Style::Bold } else { Style::Plain },
    ..Line::default()
  };
  let mut lines = Vec::new();
  if full {
    let mut title = format!("{} {}", menu.kind.title(), menu.items.len());
    if menu.kind == Kind::Effort {
      title.push_str(&format!(" · {}", menu.settings.model.name));
    }
    if menu.items.len() > visible {
      let range = format!("{}–{end}", start + 1);
      let gap = width.saturating_sub(title.width() + range.width());
      if gap > 0 {
        title.push_str(&format!("{}{range}", " ".repeat(gap)));
      }
    }
    lines.push(line(&title, false));
    lines.push(Line::default());
  }
  if menu.items.is_empty() {
    let text = if menu.kind == Kind::Effort && menu.settings.model.efforts.is_empty() {
      format!("{} has no reasoning effort.", menu.settings.model.name)
    } else {
      format!("No {} found.", menu.kind.title().to_lowercase())
    };
    lines.push(line(&text, false));
  } else {
    let name = |item: &Item| {
      let mark = if Some(item.name) == menu.current() { " ✓" } else { "" };
      format!("{}{mark}", item.name)
    };
    let name_width =
      menu.items[start..end].iter().map(|item| name(item).width()).max().unwrap_or(0);
    for (i, item) in menu.items.iter().enumerate().take(end).skip(start) {
      let name = name(item);
      let text = if width < 40 {
        format!("  {name}")
      } else {
        let name_width = name_width.min(width / 3);
        let name = clip(&name, name_width);
        let source = if width >= 70 { item.source } else { "" };
        let desc_width = width.saturating_sub(2 + name_width + 3 + source.width() + 3);
        let desc = clip(item.description, desc_width);
        format!(
          "  {name}{}   {desc}{}{source}",
          " ".repeat(name_width - name.width()),
          " ".repeat(width.saturating_sub(2 + name_width + 3 + desc.width() + source.width()))
        )
      };
      lines.push(line(&text, i == menu.selected));
    }
  }
  if full {
    lines.push(rule(cols));
    let hint = if matches!(menu.kind, Kind::Models | Kind::Effort) {
      "↑↓ navigate     →/tab complete     enter apply     esc close"
    } else {
      "↑↓ navigate     →/tab complete     enter use     esc close"
    };
    lines.push(line(hint, false));
  }
  lines
}

#[cfg(test)]
mod tests {
  use unicode_segmentation::UnicodeSegmentation;

  use super::*;

  fn frame(input: &Input) -> Frame<'_> {
    Frame {
      input,
      response: "",
      activity: None,
      status: "echo".into(),
      notice: None,
      menu: None,
      messages: &[],
      queued: Vec::new(),
      paused: false,
    }
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
    width: usize,
    pending: bool,
    sync: bool,
    snapshots: Vec<String>,
    erased: Vec<usize>,
    prompt: bool,
    coloured: bool,
    saved: Option<(usize, usize, bool)>,
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
        width: 80,
        pending: false,
        sync: false,
        snapshots: Vec::new(),
        erased: Vec::new(),
        prompt: false,
        coloured: false,
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
                self.saved = Some((self.row, self.col, self.pending));
                continue;
              }
              '8' => {
                (self.row, self.col, self.pending) = self.saved.unwrap();
                continue;
              }
              ']' => {
                let mut osc = String::new();
                while let Some(c) = chars.next() {
                  match c {
                    '\x07' => break,
                    '\x1b' => {
                      assert_eq!(chars.next(), Some('\\'));
                      break;
                    }
                    c => osc.push(c),
                  }
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
                  "8;;" => {}
                  link if link.starts_with("8;id=clankr-") => {}
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
              // Markdown may use classic palette foregrounds; never 256/RGB or backgrounds.
              assert!(
                matches!(
                  arg.as_str(),
                  "0" | "1" | "2" | "3" | "4" | "9" | "31" | "32" | "33" | "34" | "35" | "36"
                ),
                "unexpected SGR {arg}"
              );
              if arg == "0" || arg.len() == 2 {
                self.coloured = arg != "0";
              }
              continue;
            }
            assert!(!self.coloured, "colour leaked into the UI at {arg}{cmd}");
            let n: usize = if arg.is_empty() { 0 } else { arg.parse().unwrap() };
            self.pending = false;
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
          '\r' => {
            self.col = 0;
            self.pending = false;
          }
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
            let mut text = c.to_string();
            while chars.peek().is_some_and(|c| !c.is_control()) {
              text.push(chars.next().unwrap());
            }
            for g in text.graphemes(true) {
              let width = g.width();
              if self.pending || self.col + width > self.width {
                self.wraps.resize(self.row + 1, false);
                self.wraps[self.row] = true;
                self.row += 1;
                self.top = self.top.max((self.row + 1).saturating_sub(self.height));
                self.col = 0;
                self.pending = false;
              }
              self.lines.resize_with(self.lines.len().max(self.row + 1), String::new);
              let line = &mut self.lines[self.row];
              let mut used = 0;
              let end = line.grapheme_indices(true).find_map(|(i, g)| {
                if used >= self.col {
                  return Some(i);
                }
                used += g.width();
                None
              });
              if let Some(end) = end {
                line.truncate(end);
              }
              line.push_str(&" ".repeat(self.col.saturating_sub(line.width())));
              line.push_str(g);
              self.col += width;
              if self.col >= self.width {
                self.col = self.width - 1;
                self.pending = true;
              }
            }
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
        if self.saved.is_some_and(|(row, _, _)| row == i) {
          saved = Some((lines.len(), line.width(), false));
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
          if let Some((row, col, pending)) = self.saved
            && row == i
            && used <= col
            && col < used + g.width()
          {
            saved = Some((lines.len(), line.width() + col - used, pending));
          }
          line.push_str(g);
          used += g.width();
        }
        if let Some((row, col, pending)) = self.saved
          && row == i
          && col >= used
        {
          saved = Some((lines.len(), line.width(), pending));
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
      self.width = usize::from(cols);
      self.pending = false;
      self.top = (self.row + 1).saturating_sub(rows);
      // Model a terminal which keeps the cursor visible and clips rows below the new screen.
      self.lines.truncate(self.top + self.height);
      self.origins.truncate(self.lines.len());
      self.wraps.truncate(self.lines.len());
      self.saved = saved.map(|(row, col, pending)| {
        if row < self.top || row >= self.top + self.height {
          (self.top, 0, false)
        } else if pending && col != self.width - 1 {
          (row, col + 1, false)
        } else {
          (row, col, pending)
        }
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
  fn picker_resize_preserves_history_and_selected_skill_styles_use_default_colours() {
    let mut live = Live { redraw: true, sync: true, ..Live::default() };
    let mut screen = Screen::new(24);
    apply(&mut screen, |out| {
      live.prompt(out, "HISTORY").unwrap();
      live.finish(out, "reply", None, 80).unwrap();
    });
    let mut input = Input::default();
    input.insert("before $").unwrap();
    let mut menu = Menu::open(&input, crate::model::Settings::default()).unwrap();
    for _ in 0..7 {
      menu.step(true);
    }
    for (cols, rows) in [(80, 24), (40, 12), (8, 5), (2, 2), (1, 1), (119, 53)] {
      screen.resize(cols, rows);
      live.resize();
      let frame = Frame { menu: Some(&menu), ..frame(&input) };
      let delta = apply(&mut screen, |out| live.draw(out, &frame, cols, rows as u16).unwrap());
      assert!(!delta.contains("HISTORY"));
      assert!(screen.col < usize::from(cols));
      assert!(screen.row >= screen.top && screen.row < screen.top + rows);
    }
    menu.choose(&mut input, false).unwrap();
    let delta = apply(&mut screen, |out| live.draw(out, &frame(&input), 119, 53).unwrap());
    assert!(delta.contains("\x1b[1m$planning\x1b[0m"));
    assert!(!screen.text().contains("Skills 8"));
    assert!(screen.text().contains("before $planning"));
    apply(&mut screen, |out| live.restart(out, 53).unwrap());
    input = Input::default();
    apply(&mut screen, |out| live.draw(out, &frame(&input), 119, 53).unwrap());
    assert_eq!(screen.text().matches("HISTORY").count(), 1);
    assert_eq!(screen.text().matches("reply").count(), 1);
    assert_eq!(screen.lines[screen.top], format!(":: clankr v0.1.0 · {}", *SPLASH));
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
        live.prompt(out, &input.text).unwrap();
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
    assert_eq!(delta, "\x1b[4G");
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
      live.prompt(out, "HISTORY").unwrap();
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
          let (lines, cursor) = footer(&frame(&input), cols, rows);
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
  fn queue_stays_above_the_editor_and_yields_space_in_small_windows() {
    let mut input = Input::default();
    input.insert("next draft").unwrap();
    let messages = ["Copied to clipboard.".into()];
    let frame = Frame {
      queued: vec!["one", "two\n界\tthree", "four"],
      paused: true,
      messages: &messages,
      ..frame(&input)
    };
    let (lines, cursor) = footer(&frame, 80, 24);
    let queue = lines.iter().position(|line| line.text.contains("Queue paused: 3")).unwrap();
    assert!(queue < cursor.0);
    assert_eq!(lines[queue + 2].text, "  2. two ↵ 界    three");
    assert_eq!(lines[cursor.0].text, "next draft");
    for cols in [1, 2, 3, 8, 40, 119] {
      for rows in 1..16 {
        let (lines, cursor) = footer(&frame, cols, rows);
        assert!(lines.len() <= rows);
        assert!(cursor.0 < lines.len() && cursor.1 < cols);
        assert!(lines.iter().all(|line| line.text.width() < usize::from(cols.max(2))));
      }
    }
  }

  #[test]
  fn rules_frame_the_editor_and_its_lower_rule_tops_the_menu() {
    let mut input = Input::default();
    input.insert("draft").unwrap();
    let texts = |lines: Vec<Line>| lines.into_iter().map(|line| line.text).collect::<Vec<_>>();
    let rule = "─".repeat(39);
    let (lines, cursor) = footer(&frame(&input), 40, 12);
    assert_eq!(texts(lines), [&rule, "draft", &rule, "echo"]);
    assert_eq!(cursor, (1, 5));
    assert_eq!(texts(footer(&frame(&input), 40, 3).0), ["draft", "echo"]);

    input = Input::default();
    input.insert("$").unwrap();
    let menu = Menu::open(&input, crate::model::Settings::default()).unwrap();
    let lines = texts(footer(&Frame { menu: Some(&menu), ..frame(&input) }, 40, 12).0);
    assert_eq!(lines[..3], [&rule, "$", &rule]);
    assert!(lines[3].starts_with("Skills 8") && lines[3].ends_with("1–5"));
    assert_eq!(lines.iter().filter(|line| **line == rule).count(), 3);
  }

  #[test]
  fn submitted_messages_are_bold_and_copy_their_own_lines_after_resize() {
    let paragraph = "Всего учтено 13 файлов, 4 584 строки и 395 пустых строк. ".repeat(3);
    for text in [
      format!("{paragraph}\n\n    **literal** 界e\u{301} 👩‍💻\nlast  "),
      format!("{}\n{}\nlast", "x".repeat(80), "界".repeat(40)),
    ] {
      let mut live = Live { redraw: true, ..Live::default() };
      let mut screen = Screen::new(24);
      let output = apply(&mut screen, |out| live.prompt(out, &text).unwrap());
      assert!(output.starts_with("\x1b[0m\x1b[1m") && output.ends_with("\x1b[0m\r\n\r\n"));
      for cols in [80, 40, 120] {
        screen.resize(cols, 24);
        let mut selected = String::new();
        for (i, line) in screen.lines.iter().enumerate() {
          selected.push_str(line);
          if !screen.wraps[i] {
            selected.push('\n');
          }
        }
        assert_eq!(selected.trim_end_matches('\n'), text, "width {cols}");
      }
    }
  }

  #[test]
  fn summaries_and_notices_are_faint_and_indented() {
    let mut live = Live::default();
    let mut out = Vec::new();
    live.finish(&mut out, "reply", Some("2s (↑1 ↓1)"), 80).unwrap();
    live.message(&mut out, "Copied to clipboard.", 80).unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("\x1b[0m\x1b[2m  2s (↑1 ↓1)\x1b[0m"));
    assert!(output.contains("\x1b[0m\x1b[2m  Copied to clipboard.\x1b[0m"));

    let messages = ["Copied to clipboard.".to_owned()];
    let (lines, _) = footer(&Frame { messages: &messages, ..frame(&Input::default()) }, 40, 12);
    assert_eq!(lines[0].text, "  Copied to clipboard.");
    assert!(lines[0].style == Style::Faint);
  }

  #[test]
  fn unsupported_resize_retains_history_without_reprinting_completed_turns() {
    let mut live = Live::default();
    let mut input = Input::default();
    let mut out = Vec::new();
    live.prompt(&mut out, "kept").unwrap();
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
  fn clipped_response_anchor_keeps_text_and_defers_streaming_in_one_row() {
    let mut live = Live { redraw: true, ..Live::default() };
    let mut screen = Screen::new(12);
    let mut input = Input::default();
    input.insert(&"draft\n".repeat(8)).unwrap();
    let mut response = "# retained before resize.\n\n".to_owned();
    apply(&mut screen, |out| {
      live.draw(out, &Frame { response: &response, ..frame(&input) }, 80, 12).unwrap()
    });
    screen.resize(40, 1);
    live.resize();
    response.push_str("New portion.\n\n");
    apply(&mut screen, |out| {
      live.draw(out, &Frame { response: &response, ..frame(&input) }, 40, 1).unwrap()
    });
    assert!(!screen.text().contains("New portion"));
    screen.resize(80, 12);
    live.resize();
    apply(&mut screen, |out| {
      live.draw(out, &Frame { response: &response, ..frame(&input) }, 80, 12).unwrap()
    });
    apply(&mut screen, |out| live.finish(out, &response, None, 80).unwrap());
    assert_eq!(screen.text().replace('\n', ""), "retained before resize.New portion.");
  }

  #[test]
  fn native_wrap_keeps_streamed_paragraphs_and_source_newlines_copyable() {
    let mut live = Live { redraw: true, sync: true, ..Live::default() };
    let mut screen = Screen::new(12);
    let mut input = Input::default();
    let paragraph = "paragraph-界-e\u{301}-👩‍💻 ".repeat(20).trim_end().to_owned();
    let code = format!("    let s = \"{}\";\n", "code-界-e\u{301}-👩‍💻 ".repeat(20)).repeat(8);
    let text =
      format!("# Title\n\n[**{paragraph}**](https://example.org)\n\n```rust\n{code}```\n\nnext");
    let mut response = String::new();
    let (mut cols, mut rows) = (80, 12);
    let mut output = apply(&mut screen, |out| live.prompt(out, "question").unwrap());
    for (i, c) in text.chars().enumerate() {
      response.push(c);
      if i % 100 == 0 {
        input = Input::default();
        input.insert(if i % 200 == 0 { "editing\na draft" } else { "next" }).unwrap();
      }
      if i == 600 || i == 1200 {
        (cols, rows) = if i == 600 { (40, 12) } else { (80, 24) };
        screen.resize(cols, usize::from(rows));
        live.resize();
      }
      output += &apply(&mut screen, |out| {
        live.draw(out, &Frame { response: &response, ..frame(&input) }, cols, rows).unwrap()
      });
    }
    output += &apply(&mut screen, |out| live.finish(out, &response, Some("1s"), cols).unwrap());
    // Highlighted strings wrap natively; colours are classic SGR and reset before the summary.
    assert!(["\x1b[34mTitle", "\x1b[35mlet", "\x1b[32m\"code"].iter().all(|s| output.contains(s)));
    assert!(!output.contains("38;") && !output.contains("48;"));
    let mut selected = String::new();
    for (i, line) in screen.lines.iter().enumerate() {
      selected.push_str(line);
      if !screen.wraps[i] {
        selected.push('\n');
      }
    }
    assert_eq!(
      selected.trim_end(),
      format!("question\n\nTitle\n\n{paragraph}\n\n```rust\n{code}```\n\nnext\n\n  1s")
    );
  }

  #[test]
  fn streaming_response_survives_resize_and_completion_without_duplicate_history() {
    let mut live = Live { redraw: true, sync: true, ..Live::default() };
    let mut screen = Screen::new(12);
    let mut input = Input::default();
    input.insert("next draft\nwith another line").unwrap();
    let mut response = String::new();
    let messages = ["Copied to clipboard.".to_owned()];
    let (mut cols, mut rows) = (80, 12);
    apply(&mut screen, |out| live.prompt(out, "original question").unwrap());
    let waiting = Frame { activity: Some("• Thinking".into()), ..frame(&input) };
    apply(&mut screen, |out| live.draw(out, &waiting, cols, rows).unwrap());
    assert_eq!(screen.text().lines().nth(2), Some("• Thinking"));
    for index in 0..100 {
      response.push_str(&format!("line-{index:03} 界\n\n"));
      if index == 30 || index == 60 {
        (cols, rows) = if index == 30 { (40, 12) } else { (80, 24) };
        screen.resize(cols, usize::from(rows));
        live.resize();
      }
      let frame = Frame {
        response: &response,
        messages: if index >= 20 { &messages } else { &[] },
        ..frame(&input)
      };
      apply(&mut screen, |out| live.draw(out, &frame, cols, rows).unwrap());
      if index >= 20 {
        assert!(screen.text().contains("  Copied to clipboard."));
        assert!(screen.text().ends_with("echo"));
      }
    }
    apply(&mut screen, |out| live.finish(out, &response, None, cols).unwrap());
    apply(&mut screen, |out| live.message(out, &messages[0], cols).unwrap());
    apply(&mut screen, |out| live.draw(out, &frame(&input), cols, rows).unwrap());
    let text = screen.text();
    assert_eq!(text.matches("original question").count(), 1);
    for index in 0..100 {
      assert_eq!(text.matches(&format!("line-{index:03}")).count(), 1);
    }
    assert_eq!(text.matches("next draft").count(), 1);
    assert_eq!(text.matches("  Copied to clipboard.").count(), 1);
    assert!(text.find("line-099").unwrap() < text.find("  Copied to clipboard.").unwrap());
    assert!(text.find("  Copied to clipboard.").unwrap() < text.find("next draft").unwrap());
    assert_eq!(input.text, "next draft\nwith another line");
  }
}
