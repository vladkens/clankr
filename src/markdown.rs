use std::io::Write;
use std::ops::Range;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossterm::queue;
use crossterm::style::{Attribute, SetAttribute};
use mdtext::{Alignment, Event, ListKind, Options, Parser, Tag};
use osc8::Hyperlink;
use unicode_width::UnicodeWidthStr;
use url::Url;

use crate::{Res, input};

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Style(u8);

impl Style {
  const BOLD: u8 = 1;
  const ITALIC: u8 = 2;
  const STRIKE: u8 = 4;

  fn with(self, flag: u8) -> Self {
    Self(self.0 | flag)
  }

  pub fn write(self, out: &mut impl Write) -> Res<()> {
    queue!(out, SetAttribute(Attribute::Reset))?;
    for (flag, attr) in [
      (Self::BOLD, Attribute::Bold),
      (Self::ITALIC, Attribute::Italic),
      (Self::STRIKE, Attribute::CrossedOut),
    ] {
      if self.0 & flag != 0 {
        queue!(out, SetAttribute(attr))?;
      }
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
struct Link {
  uri: String,
  id: u64,
}

impl Link {
  fn new(target: &str) -> Option<Rc<Self>> {
    if target.is_empty() || target.chars().any(input::is_control) {
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
  fn slice(&self, range: Range<usize>) -> Self {
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

  fn parts(&self) -> impl Iterator<Item = (&str, Style, Option<&Rc<Link>>)> {
    self.spans.iter().enumerate().map(|(i, span)| {
      let end = self.spans.get(i + 1).map_or(self.text.len(), |span| span.start);
      (&self.text[span.start..end], span.style, span.link.as_ref())
    })
  }

  fn push(&mut self, text: &str, style: Style, link: Option<&Rc<Link>>) {
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

enum Kind {
  Block,
  Inline,
  Code,
  Quote,
  List(Option<u32>),
  Item,
  Table,
  Row,
  Cell,
}

struct Table {
  // Buffer rows until the table closes: a later cell can widen an earlier column.
  align: Vec<Alignment>,
  rows: Vec<Vec<Text>>,
  cell: Text,
}

struct Open {
  kind: Kind,
  style: Style,
  link: Option<Rc<Link>>,
}

#[derive(Default)]
struct Render {
  text: Text,
  stack: Vec<Open>,
  style: Style,
  link: Option<Rc<Link>>,
  gap: usize,
  table: Option<Table>,
  marker: Option<usize>,
}

impl Render {
  fn space(&mut self, n: usize) {
    self.gap = self.gap.max(n);
  }

  fn push(&mut self, text: &str) {
    let text = input::clean(text, 0, true).0;
    if text.is_empty() {
      return;
    }
    self.marker = None;
    if let Some(table) = &mut self.table {
      table.cell.push(&text.replace('\n', " "), self.style, self.link.as_ref());
      return;
    }
    if !self.text.text.is_empty() {
      let trailing = self.text.text.bytes().rev().take_while(|&c| c == b'\n').count();
      self.text.push(&"\n".repeat(self.gap.saturating_sub(trailing)), Style::default(), None);
    }
    self.gap = 0;
    for part in text.split_inclusive('\n') {
      if self.text.text.is_empty() || self.text.text.ends_with('\n') {
        let quotes = self.stack.iter().filter(|open| matches!(open.kind, Kind::Quote)).count();
        self.text.push(&"> ".repeat(quotes), Style::default(), None);
      }
      self.text.push(part, self.style, self.link.as_ref());
    }
  }

  fn event(&mut self, event: Event<'_>) {
    match event {
      Event::Start(tag) => self.open(tag),
      Event::End => self.close(),
      Event::Text(text)
      | Event::Html(text)
      | Event::InlineMath(text)
      | Event::DisplayMath(text) => {
        self.push(&text);
      }
      Event::Code(text) => {
        self.push(&text);
        if self.stack.last().is_some_and(|open| matches!(open.kind, Kind::Code))
          && !text.is_empty()
          && !text.ends_with('\n')
        {
          self.push("\n");
        }
      }
      Event::SoftBreak | Event::HardBreak => self.push("\n"),
      Event::ThematicBreak => {
        self.space(2);
        self.push("────────");
        self.space(2);
      }
      Event::TaskListMarker(checked) => self.push(if checked { "[x] " } else { "[ ] " }),
    }
  }

  fn table(&mut self, table: Table) {
    let widths: Vec<_> = (0..table.align.len())
      .map(|i| {
        table
          .rows
          .iter()
          .filter_map(|row| row.get(i))
          .map(|cell| cell.text.width())
          .max()
          .unwrap_or(0)
          .max(1)
      })
      .collect();
    let style = self.style;
    let link = self.link.take();
    for (r, row) in table.rows.iter().enumerate() {
      if r > 0 {
        self.space(1);
      }
      for (i, cell) in row.iter().enumerate() {
        self.style = Style::default();
        if i > 0 {
          self.push(" │ ");
        }
        let pad = widths[i].saturating_sub(cell.text.width());
        let left = match table.align[i] {
          Alignment::Right => pad,
          Alignment::Center => pad / 2,
          _ => 0,
        };
        self.push(&" ".repeat(left));
        for (text, style, link) in cell.parts() {
          self.style = style;
          self.link = link.cloned();
          self.push(text);
        }
        self.style = Style::default();
        self.link = None;
        if i + 1 < widths.len() {
          self.push(&" ".repeat(pad - left));
        }
      }
      if r == 0 {
        self.space(1);
        self.push(&widths.iter().map(|&width| "─".repeat(width)).collect::<Vec<_>>().join("─┼─"));
      }
    }
    self.style = style;
    self.link = link;
    self.space(2);
  }

  fn open(&mut self, tag: Tag<'_>) {
    let style = self.style;
    let link = self.link.clone();
    let kind = match tag {
      Tag::Paragraph | Tag::HtmlBlock | Tag::DisplayMath | Tag::Heading(_) => {
        if !self.stack.last().is_some_and(|open| matches!(open.kind, Kind::Item)) {
          self.space(2);
        }
        if matches!(tag, Tag::Heading(_)) {
          self.style = style.with(Style::BOLD);
        }
        Kind::Block
      }
      Tag::CodeBlock(info) => {
        self.space(2);
        let lang = info.as_deref().and_then(|info| info.split_whitespace().next()).unwrap_or("");
        self.push(&if lang.is_empty() {
          "────────".into()
        } else {
          format!("─ {lang} ─────")
        });
        self.push("\n");
        Kind::Code
      }
      Tag::Quote => {
        self.space(2);
        Kind::Quote
      }
      Tag::List(kind) => {
        if self.stack.iter().any(|open| matches!(open.kind, Kind::Item)) {
          self.gap = 1;
        } else {
          self.space(2);
        }
        Kind::List(match kind {
          ListKind::Ordered(n) => Some(n),
          ListKind::Unordered => None,
        })
      }
      Tag::Item => {
        let depth = self.stack.iter().filter(|open| matches!(open.kind, Kind::List(_))).count();
        let list = self.stack.iter_mut().rev().find_map(|open| match &mut open.kind {
          Kind::List(n) => Some(n),
          _ => None,
        });
        let marker = match list {
          Some(Some(n)) => {
            let marker = format!("{n}. ");
            *n = n.saturating_add(1);
            marker
          }
          _ => "- ".into(),
        };
        let start = self.text.text.len();
        self.push(&format!("{}{marker}", "  ".repeat(depth.saturating_sub(1))));
        self.marker = Some(start);
        Kind::Item
      }
      Tag::Link { url, .. } | Tag::Image { url, .. } => {
        if self.link.is_none() {
          self.link = Link::new(&url);
        }
        Kind::Inline
      }
      Tag::CodeSpan => Kind::Inline,
      Tag::Strong | Tag::Emphasis | Tag::Strikethrough => {
        self.style = style.with(match tag {
          Tag::Strong => Style::BOLD,
          Tag::Emphasis => Style::ITALIC,
          _ => Style::STRIKE,
        });
        Kind::Inline
      }
      Tag::Table(align) => {
        self.space(2);
        self.table = Some(Table { align, rows: Vec::new(), cell: Text::default() });
        Kind::Table
      }
      Tag::TableHead => {
        self.style = style.with(Style::BOLD);
        Kind::Inline
      }
      Tag::TableBody => Kind::Inline,
      Tag::TableRow => {
        self.table.as_mut().unwrap().rows.push(Vec::new());
        Kind::Row
      }
      Tag::TableCell => Kind::Cell,
    };
    self.stack.push(Open { kind, style, link });
  }

  fn close(&mut self) {
    let Some(open) = self.stack.pop() else { return };
    self.style = open.style;
    self.link = open.link;
    match open.kind {
      Kind::Block | Kind::Quote | Kind::List(_) => self.space(2),
      // Items are adjacent even when the parser emits paragraph boundaries inside them.
      Kind::Item => {
        self.marker = None;
        self.gap = 1;
      }
      Kind::Code => {
        self.space(1);
        self.push("────────");
        self.space(2);
      }
      Kind::Table => {
        let table = self.table.take().unwrap();
        self.table(table);
      }
      Kind::Cell => {
        let table = self.table.as_mut().unwrap();
        table.rows.last_mut().unwrap().push(std::mem::take(&mut table.cell));
      }
      Kind::Inline | Kind::Row => {}
    }
  }
}

pub struct Markdown {
  parser: Parser,
  read: usize,
  seen: usize,
  render: Render,
  preview: Text,
  preview_start: usize,
  preview_shown: usize,
}

impl Default for Markdown {
  fn default() -> Self {
    Self {
      parser: Parser::with_options(Options::GFM | Options::IMMEDIATE_MODE),
      read: 0,
      seen: 0,
      render: Render::default(),
      preview: Text::default(),
      preview_start: 0,
      preview_shown: 0,
    }
  }
}

impl Markdown {
  pub fn update(&mut self, source: &str, complete: bool) -> &Text {
    // mdtext keeps unresolved syntax; feed only its unread suffix, never reparse history.
    if source.len() > self.seen {
      let mut events = self.parser.feed(&source[self.read..]);
      for event in events.by_ref() {
        self.render.event(event);
      }
      self.read += events.consumed();
      self.seen = source.len();
      drop(events);
      if self.render.stack.is_empty() {
        self.preview_start = self.read;
        self.preview_shown = self.render.text.text.len();
      }
      if !complete {
        self.preview = self.pending(source);
      }
    }
    if complete {
      for event in self.parser.finish_iter(&source[self.read..]) {
        self.render.event(event);
      }
      self.read = source.len();
      self.preview = Text::default();
    }
    &self.render.text
  }

  pub fn text(&self) -> &Text {
    &self.render.text
  }

  pub fn preview(&self) -> &Text {
    &self.preview
  }

  pub fn committed_len(&self) -> usize {
    self.render.marker.unwrap_or(self.render.text.text.len())
  }

  fn pending(&self, source: &str) -> Text {
    let pending = &source[self.preview_start..];
    if pending.len() > 16 * 1024 {
      // Bound preview work for a huge unfinished block. Stable history is never reparsed.
      let pending = if self.render.stack.iter().any(|open| matches!(open.kind, Kind::Code)) {
        source.rsplit('\n').next().unwrap_or("")
      } else {
        &source[self.read..]
      };
      let start = pending.char_indices().rev().nth(4095).map_or(0, |(i, _)| i);
      let mut text = Text::default();
      text.push(&input::clean(&pending[start..], 0, true).0, Style::default(), None);
      return text;
    }
    let mut render = Render::default();
    for event in Parser::parse_str(pending, Options::GFM | Options::IMMEDIATE_MODE) {
      render.event(event);
    }
    let shown =
      self.render.text.text[self.preview_shown..self.committed_len()].trim_start_matches('\n');
    if render.text.text.starts_with(shown) {
      render.text.slice(shown.len()..render.text.text.len())
    } else {
      Text::default()
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn render(source: &str) -> Text {
    let mut markdown = Markdown::default();
    markdown.update(source, true);
    markdown.render.text
  }

  fn parts(text: &Text) -> Vec<(&str, Style, Option<&str>)> {
    text
      .parts()
      .map(|(text, style, link)| (text, style, link.map(|link| link.uri.as_str())))
      .collect()
  }

  #[test]
  fn markdown_formats_blocks_and_nested_styles_without_colours() {
    let source = "# Title\n\n**bold *both***, ~~old~~ and `code`.\n\n\
      - first\n  - nested\n- [x] done\n\n3. third\n4. fourth\n\n\
      > quoted\n> continuation\n\n[site](https://example.org)\n\n\
      ```rust\nfn main() {\n    println!(\"hello\");\n}\n```";
    let text = render(source);
    assert_eq!(
      text.text,
      "Title\n\nbold both, old and code.\n\n\
      - first\n  - nested\n- [x] done\n\n3. third\n4. fourth\n\n\
      > quoted\n> continuation\n\nsite\n\n\
      ─ rust ─────\nfn main() {\n    println!(\"hello\");\n}\n────────"
    );
    let mut out = Vec::new();
    text.write(&mut out, 0..text.text.len()).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("\x1b[1mTitle"));
    assert!(out.contains("\x1b[1m\x1b[3mboth"));
    assert!(out.contains("\x1b[9mold"));
    assert!(out.contains("\x1b[0m and code."));
    assert!(out.ends_with("\x1b[0m"));
  }

  #[test]
  fn streamed_chunks_match_complete_markdown_and_flush_unclosed_syntax() {
    for source in [
      "# Heading\n\n**bold** *italic* ~~strike~~ `code` &amp; \\*literal\\*\n\nnext",
      "Heading\n=======\n\n[site](https://example.org) ![alt](image.png) <https://example.org>",
      "- first\n  - nested\n- [ ] todo\n\n> quote\n> **strong**\n\nlast",
      "| name | value |\n| --- | ---: |\n| **bold** | 42 |\n\nlast",
      "```rust\nfn main() {}\n    e\u{301} 👩‍💻 界\n```\n\nnext",
      "```\n    unfinished code",
      "**unfinished `inline",
      "a\r\nb\r\n\r\nc\n\n&#27;[31m escaped \x1b[2J control",
    ] {
      let expected = render(source);
      let mut markdown = Markdown::default();
      let mut previous = String::new();
      for (i, c) in source.char_indices() {
        let text = markdown.update(&source[..i + c.len_utf8()], false);
        assert!(text.text.starts_with(&previous), "changed emitted text for {source:?}");
        previous.clone_from(&text.text);
      }
      assert_eq!(parts(markdown.update(source, true)), parts(&expected), "{source:?}");
      assert!(!expected.text.contains('\x1b'));
    }
  }

  #[test]
  fn unfinished_blocks_have_a_preview_without_changing_committed_text() {
    let mut markdown = Markdown::default();
    assert!(markdown.update("**hello", false).text.is_empty());
    assert_eq!(markdown.preview().text, "**hello");
    assert!(markdown.update("**hello** world", false).text.is_empty());
    assert_eq!(markdown.preview().text, "hello world");
    let mut out = Vec::new();
    markdown.preview().write(&mut out, 0..11).unwrap();
    assert!(String::from_utf8(out).unwrap().contains("\x1b[1mhello"));
    assert_eq!(markdown.update("**hello** world\n\nnext", false).text, "hello world");
    assert_eq!(markdown.preview().text, "next");
    assert_eq!(markdown.update("**hello** world\n\nnext", true).text, "hello world\n\nnext");
    assert!(markdown.preview().text.is_empty());

    for (source, committed, pending) in [
      ("```rust\nfn", "─ rust ─────\n", "fn\n────────"),
      ("- first\n- sec", "", "- first\n- sec"),
      ("| A | B |\n| - | - |\n| one | tw", "", "A   │ B\n────┼───\none │ tw"),
    ] {
      let mut markdown = Markdown::default();
      for (i, c) in source.char_indices() {
        markdown.update(&source[..i + c.len_utf8()], false);
      }
      assert_eq!(&markdown.text().text[..markdown.committed_len()], committed, "{source:?}");
      assert_eq!(markdown.preview().text, pending, "{source:?}");
    }

    let source = "界".repeat(20000);
    let mut markdown = Markdown::default();
    assert!(markdown.update(&source, false).text.is_empty());
    assert_eq!(markdown.preview().text, "界".repeat(4096));
    assert_eq!(markdown.update(&source, true).text, source);
  }

  #[test]
  fn closed_blocks_and_code_lines_arrive_before_the_stream_finishes() {
    let mut markdown = Markdown::default();
    let first = "# Ready\n\n";
    assert_eq!(markdown.update(first, false).text, "Ready");
    let second = format!("{first}```rust\nfn main() {{}}\n");
    assert!(markdown.update(&second, false).text.contains("fn main() {}\n"));
    let third = format!("{second}    // still streaming\n");
    assert!(markdown.update(&third, false).text.ends_with("    // still streaming\n"));
    assert!(markdown.update(&third, true).text.ends_with("\n────────"));
  }

  #[test]
  fn tables_align_styled_cells_by_display_width_after_the_last_row_arrives() {
    let source = "Before\n\n| Язык | Строк кода |\n| --- | ---: |\n\
      | Rust | 3 304 |\n| Python | 621 |\n| TOML | 26 |\n| Makefile | 19 |\n\
      | **Всего** | **3 970** |\n\nAfter";
    let text = render(source);
    assert_eq!(
      text.text,
      "Before\n\nЯзык     │ Строк кода\n─────────┼───────────\n\
      Rust     │      3 304\nPython   │        621\nTOML     │         26\n\
      Makefile │         19\nВсего    │      3 970\n\nAfter"
    );
    let mut streamed = Markdown::default();
    for (i, c) in source.char_indices() {
      let prefix = &source[..i + c.len_utf8()];
      let output = streamed.update(prefix, false);
      if !prefix.contains("| **3 970** |\n\n") {
        assert!(!output.text.contains("Язык"), "table must wait for its final column widths");
      }
    }
    assert_eq!(streamed.update(source, true), &text);
    let mut out = Vec::new();
    text.write(&mut out, 0..text.text.len()).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("\x1b[1mВсего") && out.contains("\x1b[1m3 970"));

    let text = render(
      "| L | C | R |\n| :--- | :---: | ---: |\n\
      | 界 | **abcdef** | 1234 |\n| 👩‍💻 | e\u{301} | 2 |\n| | | |\n",
    );
    let lines: Vec<_> = text.text.lines().collect();
    assert_eq!(lines[0], "L  │   C    │    R");
    assert_eq!(lines[1], "───┼────────┼─────");
    assert_eq!(lines[2], "界 │ abcdef │ 1234");
    assert_eq!(lines[3], "👩‍💻 │   e\u{301}    │    2");
    for line in lines.iter().filter(|line| line.contains('│')) {
      assert_eq!(line.split('│').map(str::width).collect::<Vec<_>>(), [3, 8, 5]);
    }
  }

  #[test]
  fn links_keep_only_labels_and_close_before_plain_text_and_each_write_boundary() {
    let file = "/Users/user/Code/clankr/src/main.rs";
    let source = format!(
      "[**Markdown** guide](https://www.markdownguide.org/basic-syntax/) tail\n\n[src/main.rs]({file})"
    );
    let text = render(&source);
    assert_eq!(text.text, "Markdown guide tail\n\nsrc/main.rs");
    let links: Vec<_> = text
      .parts()
      .filter_map(|(text, _, link)| link.map(|link| (text, link.uri.as_str())))
      .collect();
    assert_eq!(
      links,
      [
        ("Markdown", "https://www.markdownguide.org/basic-syntax/"),
        (" guide", "https://www.markdownguide.org/basic-syntax/"),
        ("src/main.rs", "file:///Users/user/Code/clankr/src/main.rs"),
      ]
    );
    let mut out = Vec::new();
    text.write(&mut out, 0..text.text.len()).unwrap();
    let out = String::from_utf8(out).unwrap();
    let close = "\x1b]8;;\x1b\\";
    assert_eq!(out.matches("\x1b]8;id=").count(), 2);
    assert!(out.contains(";https://www.markdownguide.org/basic-syntax/\x1b\\"));
    assert!(out.contains(&format!("{close}\x1b[0m tail")));
    assert!(out.ends_with(&format!("{close}\x1b[0m")));
    for range in [0..3, 13..14, text.text.len() - 1..text.text.len()] {
      let mut out = Vec::new();
      text.write(&mut out, range).unwrap();
      let out = String::from_utf8(out).unwrap();
      assert!(out.starts_with("\x1b]8;id=clankr-"));
      assert!(out.ends_with(&format!("{close}\x1b[0m")));
    }

    let text = render("| File |\n| --- |\n| [`main`](src/main.rs) |\n");
    assert_eq!(text.text, "File\n────\nmain");
    let uri = Url::from_file_path(std::env::current_dir().unwrap().join("src/main.rs")).unwrap();
    assert_eq!(parts(&text).last().unwrap().2, Some(uri.as_str()));
    let uri = Link::new("/tmp/Мой файл%23name.rs").unwrap().uri.clone();
    assert_eq!(uri, "file:///tmp/%D0%9C%D0%BE%D0%B9%20%D1%84%D0%B0%D0%B9%D0%BB%23name.rs");
    for target in
      ["", "https://example.org/\x1b]52;c;bad\x07", "/tmp/\nfile", "https://example.org/\u{9c}"]
    {
      assert!(Link::new(target).is_none());
    }
  }
}
