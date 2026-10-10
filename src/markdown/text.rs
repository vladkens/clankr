use std::io::Write;
use std::ops::Range;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossterm::queue;
use crossterm::style::{Attribute, SetAttribute};
use osc8::Hyperlink;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use url::Url;

use crate::Res;
use crate::text::is_control;

// Classic SGR foregrounds use the terminal's own palette, so printed history follows theme changes.
// Black, white and bright colours are avoided: some themes render them like the background.
pub(super) const RED: u8 = 31;
pub(super) const GREEN: u8 = 32;
pub(super) const YELLOW: u8 = 33;
pub(super) const BLUE: u8 = 34;
pub(super) const MAGENTA: u8 = 35;
pub(super) const CYAN: u8 = 36;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Style {
  flags: u8,
  colour: Option<u8>,
}

impl Style {
  pub(super) const BOLD: u8 = 1;
  pub(super) const ITALIC: u8 = 2;
  pub(super) const STRIKE: u8 = 4;
  pub(super) const FAINT: u8 = 8;

  pub(super) fn with(self, flag: u8) -> Self {
    Self { flags: self.flags | flag, ..self }
  }

  pub(super) fn colour(self, colour: u8) -> Self {
    Self { colour: Some(colour), ..self }
  }

  pub(super) fn syntax(self, scope: &str) -> Self {
    match scope {
      "comment" => self.with(Self::FAINT | Self::ITALIC),
      "keyword" => self.colour(MAGENTA),
      "string" | "character" | "diff.plus" => self.colour(GREEN),
      "diff.minus" => self.colour(RED),
      "number" | "float" | "boolean" | "constant" => self.colour(YELLOW),
      "function" => self.colour(BLUE),
      "type" | "constructor" | "namespace" => self.colour(CYAN),
      _ => self,
    }
  }

  pub fn write(self, out: &mut impl Write) -> Res<()> {
    queue!(out, SetAttribute(Attribute::Reset))?;
    for (flag, attr) in [
      (Self::BOLD, Attribute::Bold),
      (Self::FAINT, Attribute::Dim),
      (Self::ITALIC, Attribute::Italic),
      (Self::STRIKE, Attribute::CrossedOut),
    ] {
      if self.flags & flag != 0 {
        queue!(out, SetAttribute(attr))?;
      }
    }
    // crossterm would encode named colours as 38;5;n.
    if let Some(colour) = self.colour {
      write!(out, "\x1b[{colour}m")?;
    }
    Ok(())
  }
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Text {
  pub text: String,
  spans: Vec<Span>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Link {
  pub(super) uri: String,
  id: u64,
}

impl Link {
  pub(super) fn new(target: &str) -> Option<Rc<Self>> {
    if target.is_empty() || target.chars().any(is_control) {
      return None;
    }
    let uri = Url::parse(target).ok().or_else(|| {
      let dir = std::env::current_dir().ok()?;
      Url::from_directory_path(dir).ok()?.join(target).ok()
    })?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    Some(Rc::new(Self { uri: uri.into(), id: NEXT.fetch_add(1, Ordering::Relaxed) }))
  }
}

#[derive(Debug, PartialEq, Eq)]
struct Span {
  start: usize,
  style: Style,
  link: Option<Rc<Link>>,
}

impl Text {
  pub(super) fn wrap(&self, width: usize) -> Vec<Self> {
    let mut lines = Vec::new();
    let (mut start, mut used, mut space) = (0, 0, 0);
    for (pos, g) in self.text.grapheme_indices(true) {
      if used + g.width() > width && pos > start {
        let end = if space > start { space } else { pos };
        lines.push(self.slice(start..end));
        start = end;
        used = self.text[start..pos].width();
      }
      used += g.width();
      if g.chars().all(char::is_whitespace) {
        space = pos + g.len();
      }
    }
    lines.push(self.slice(start..self.text.len()));
    lines
  }

  pub(super) fn slice(&self, range: Range<usize>) -> Self {
    let mut out = Self::default();
    let mut pos = 0;
    for (text, style, link) in self.parts() {
      let start = range.start.saturating_sub(pos).min(text.len());
      let end = range.end.saturating_sub(pos).min(text.len());
      if start < end {
        out.push(&text[start..end], style, link);
      }
      pos += text.len();
    }
    out
  }

  pub(super) fn parts(&self) -> impl Iterator<Item = (&str, Style, Option<&Rc<Link>>)> {
    self.spans.iter().enumerate().map(|(i, span)| {
      let end = self.spans.get(i + 1).map_or(self.text.len(), |span| span.start);
      (&self.text[span.start..end], span.style, span.link.as_ref())
    })
  }

  pub(super) fn push(&mut self, text: &str, style: Style, link: Option<&Rc<Link>>) {
    if text.is_empty() {
      return;
    }
    if self.spans.last().is_none_or(|last| last.style != style || last.link.as_ref() != link) {
      self.spans.push(Span { start: self.text.len(), style, link: link.cloned() });
    }
    self.text.push_str(text);
  }

  pub fn write(&self, out: &mut impl Write, range: Range<usize>) -> Res<()> {
    let mut pos = range.start;
    let mut index = self.spans.partition_point(|span| span.start <= pos).saturating_sub(1);
    let mut link = None;
    while pos < range.end {
      let span = &self.spans[index];
      let end = self.spans.get(index + 1).map_or(range.end, |span| span.start.min(range.end));
      if link != span.link.as_deref() {
        link = span.link.as_deref();
        // OSC 8 is independent of SGR; close it before writing the footer or moving the cursor.
        match link {
          Some(link) => {
            let id = format!("clankr-{}-{}", std::process::id(), link.id);
            write!(out, "{}", Hyperlink::new(&link.uri).with_id(&id))?;
          }
          None => write!(out, "{}", Hyperlink::END)?,
        }
      }
      span.style.write(out)?;
      if link.is_some() {
        queue!(out, SetAttribute(Attribute::Underlined))?;
      }
      for part in self.text[pos..end].split_inclusive('\n') {
        out.write_all(part.trim_end_matches('\n').as_bytes())?;
        if part.ends_with('\n') {
          out.write_all(b"\r\n")?;
        }
      }
      pos = end;
      index += 1;
    }
    if link.is_some() {
      write!(out, "{}", Hyperlink::END)?;
    }
    queue!(out, SetAttribute(Attribute::Reset))?;
    Ok(())
  }
}
