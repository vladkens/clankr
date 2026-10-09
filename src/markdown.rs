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

use crate::syntax::Code;
use crate::{Res, input};

// Classic SGR foregrounds use the terminal's own palette, so printed history follows theme changes.
// Black, white and bright colours are avoided: some themes render them like the background.
const RED: u8 = 31;
const GREEN: u8 = 32;
const YELLOW: u8 = 33;
const BLUE: u8 = 34;
const MAGENTA: u8 = 35;
const CYAN: u8 = 36;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Style {
  flags: u8,
  colour: Option<u8>,
}

impl Style {
  const BOLD: u8 = 1;
  const ITALIC: u8 = 2;
  const STRIKE: u8 = 4;
  const FAINT: u8 = 8;

  fn with(self, flag: u8) -> Self {
    Self { flags: self.flags | flag, ..self }
  }

  fn colour(self, colour: u8) -> Self {
    Self { colour: Some(colour), ..self }
  }

  fn syntax(self, scope: &str) -> Self {
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
  Math(String),
  Quote,
  List(Option<u32>),
  // Marker width and whether a blank line separates blocks inside the item.
  Item(usize, bool),
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
  // Quotes and items whose prefixes the current line already has.
  prefixed: usize,
  code: Option<Code>,
  // Preview only: committed highlighting state and the text offset where it applies.
  seed: Option<(usize, Code)>,
}

impl Render {
  fn space(&mut self, n: usize) {
    self.gap = self.gap.max(n);
  }

  // Code fences, rules and quote markers are drawn faint so they recede behind the content.
  fn decor(&mut self, text: &str) {
    let style = std::mem::replace(&mut self.style, Style::default().with(Style::FAINT));
    self.push(text);
    self.style = style;
  }

  // mdtext emits fenced code one complete line at a time; only the final line may lack '\n'.
  fn code(&mut self, text: &str) {
    let mut text = input::clean(text, 0, true).0;
    if text.is_empty() {
      return;
    }
    if !text.ends_with('\n') {
      text.push('\n');
    }
    let style = self.style;
    for line in text.split_inclusive('\n') {
      // The preview slices off committed lines, so it resumes their state instead of
      // re-highlighting.
      let len = self.text.text.len();
      if let Some((start, code)) = self.seed.take_if(|(start, _)| len >= *start)
        && start == len
      {
        self.code = Some(code);
      }
      if self.seed.is_some() {
        self.push(line);
        continue;
      }
      match self.code.as_mut().and_then(|code| code.line(line)) {
        Some(parts) => {
          for (part, range) in parts {
            self.style = style.syntax(part);
            self.push(&line[range]);
          }
          self.style = style;
        }
        // Unknown language, or a parser error that leaves the rest of the block plain.
        None => {
          self.code = None;
          self.push(line);
        }
      }
    }
  }

  // A block opens after a blank line, or on the marker line when it starts an item.
  fn block(&mut self) {
    if self.marker.is_none() {
      self.space(2);
    }
  }

  fn containers(&self) -> usize {
    self.stack.iter().filter(|open| matches!(open.kind, Kind::Quote | Kind::Item(..))).count()
  }

  // Lines repeat their containers in nesting order: a faint marker for each quote and spaces
  // under each item marker. A blank line keeps only the quote markers, without trailing spaces.
  fn prefix(&mut self, range: Range<usize>, blank: bool) {
    let mut parts: Vec<_> = self
      .stack
      .iter()
      .filter_map(|open| match open.kind {
        Kind::Quote => Some("> ".to_owned()),
        Kind::Item(width, _) => Some(" ".repeat(width)),
        _ => None,
      })
      .take(range.end)
      .skip(range.start)
      .collect();
    if blank {
      parts.truncate(parts.iter().rposition(|part| part.starts_with('>')).map_or(0, |i| i + 1));
      if let Some(quote) = parts.last_mut() {
        quote.truncate(1);
      }
    }
    for part in parts {
      let style =
        if part.starts_with('>') { Style::default().with(Style::FAINT) } else { Style::default() };
      self.text.push(&part, style, None);
    }
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
      for line in trailing..self.gap {
        if line > 0 {
          // A blank line belongs to the containers of the line above and loosens their items.
          self.prefix(0..self.prefixed, true);
          for open in &mut self.stack {
            if let Kind::Item(_, loose) = &mut open.kind {
              *loose = true;
            }
          }
        }
        self.text.push("\n", Style::default(), None);
      }
    }
    self.gap = 0;
    for part in text.split_inclusive('\n') {
      let count = self.containers();
      if self.text.text.is_empty() || self.text.text.ends_with('\n') {
        self.prefix(0..count, part == "\n");
      } else {
        // A block that starts an item still adds its own prefix after the marker.
        self.prefix(self.prefixed..count, false);
      }
      self.prefixed = count;
      self.text.push(part, self.style, self.link.as_ref());
    }
  }

  fn event(&mut self, event: Event<'_>) {
    match event {
      Event::Start(tag) => self.open(tag),
      Event::End => self.close(),
      Event::Text(text) | Event::Html(text) => {
        self.push(&text);
      }
      Event::InlineMath(text) => self.push(&crate::math::render(&text, false)),
      Event::DisplayMath(text) => {
        if let Some(Open { kind: Kind::Math(source), .. }) = self.stack.last_mut() {
          source.push_str(&text);
        }
      }
      Event::Code(text) => {
        if self.stack.last().is_some_and(|open| matches!(open.kind, Kind::Code)) {
          self.code(&text);
        } else {
          self.push(&text);
        }
      }
      Event::SoftBreak | Event::HardBreak => self.push("\n"),
      Event::ThematicBreak => {
        self.space(2);
        self.decor("────────");
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
          self.decor(" │ ");
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
      if r == 0 || r + 1 < table.rows.len() {
        self.space(1);
        self.decor(&widths.iter().map(|&width| "─".repeat(width)).collect::<Vec<_>>().join("─┼─"));
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
      Tag::Paragraph | Tag::HtmlBlock | Tag::Heading(_) => {
        if !self.stack.last().is_some_and(|open| matches!(open.kind, Kind::Item(..))) {
          self.block();
        }
        // A colour accent keeps headings apart from bold user messages.
        if matches!(tag, Tag::Heading(_)) {
          self.style = style.with(Style::BOLD).colour(BLUE);
        }
        Kind::Block
      }
      Tag::DisplayMath => {
        self.block();
        Kind::Math(String::new())
      }
      Tag::CodeBlock(info) => {
        self.block();
        let lang = info.as_deref().and_then(|info| info.split_whitespace().next()).unwrap_or("");
        // Fences on their own lines keep a selected body pure code and a copied reply valid
        // Markdown. mdtext hides the source fence, so a body line of ``` ends the copy early.
        self.decor(&format!("```{lang}\n"));
        self.code = if lang.is_empty() { None } else { Code::new(lang) };
        Kind::Code
      }
      Tag::Quote => {
        self.block();
        Kind::Quote
      }
      Tag::List(kind) => {
        let loose = self.stack.iter().rev().find_map(|open| match open.kind {
          Kind::Item(_, loose) => Some(loose),
          _ => None,
        });
        // A nested list follows the previous block of a tight item directly.
        if loose == Some(false) && self.marker.is_none() {
          self.gap = 1;
        } else {
          self.block();
        }
        Kind::List(match kind {
          ListKind::Ordered(n) => Some(n),
          ListKind::Unordered => None,
        })
      }
      Tag::Item => {
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
        self.push(&marker);
        self.marker = Some(start);
        // The marker stands in for the item's own prefix on its first line.
        self.prefixed += 1;
        Kind::Item(marker.len(), false)
      }
      Tag::Link { url, .. } | Tag::Image { url, .. } => {
        if self.link.is_none() {
          self.link = Link::new(&url);
        }
        Kind::Inline
      }
      Tag::CodeSpan => {
        self.style = style.colour(CYAN);
        Kind::Inline
      }
      Tag::Strong | Tag::Emphasis | Tag::Strikethrough => {
        self.style = style.with(match tag {
          Tag::Strong => Style::BOLD,
          Tag::Emphasis => Style::ITALIC,
          _ => Style::STRIKE,
        });
        Kind::Inline
      }
      Tag::Table(align) => {
        self.block();
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
    self.prefixed = self.prefixed.min(self.containers());
    match open.kind {
      Kind::Block | Kind::Quote | Kind::List(_) => self.space(2),
      Kind::Math(source) => {
        self.push(&crate::math::render(&source, true));
        self.space(2);
      }
      // Tight items are adjacent even though the parser emits paragraph boundaries inside them.
      Kind::Item(_, loose) => {
        self.marker = None;
        if !loose {
          self.gap = 1;
        }
      }
      Kind::Code => {
        self.code = None;
        self.space(1);
        self.decor("```");
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
      parser: Parser::with_options(Options::GFM | Options::IMMEDIATE_MODE | Options::MATH),
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
    let shown =
      self.render.text.text[self.preview_shown..self.committed_len()].trim_start_matches('\n');
    let mut render = Render {
      seed: self.render.code.clone().map(|code| (shown.len(), code)),
      ..Render::default()
    };
    for event in Parser::parse_str(pending, Options::GFM | Options::IMMEDIATE_MODE | Options::MATH)
    {
      render.event(event);
    }
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

  fn ansi(text: &Text) -> String {
    let mut out = Vec::new();
    text.write(&mut out, 0..text.text.len()).unwrap();
    String::from_utf8(out).unwrap()
  }

  fn styled<'a>(text: &'a Text, word: &str) -> Vec<(&'a str, Style)> {
    text
      .parts()
      .filter(|(part, ..)| part.contains(word))
      .map(|(part, style, _)| (part, style))
      .collect()
  }

  // The nested blocks section of mocks/markdown.md.
  const NESTED: &str = "1. Первый пункт с несколькими абзацами.\n\n   \
    Второй абзац всё ещё относится к первому пункту.\n\n   \
    - Вложенный пункт.\n     - Ещё один уровень с **выделением**.\n     \
    - Соседний пункт того же уровня.\n\n   \
    > Цитата внутри первого пункта.\n   >\n   > Второй абзац цитаты с [ссылкой](src/main.rs).\n\n   \
    ```rust\n   let robot = \"clankr\";\n   println!(\"roger roger, {robot}\");\n   ```\n\n\
    2. Второй пункт снова на верхнем уровне.\n3. Третий пункт после вложенных блоков.\n\n\
    > Внешняя цитата.\n>\n> > Вложенная цитата с *курсивом*.\n>\n> Возвращение во внешнюю цитату.";

  const WIDE: &str = "9. nine\n10. ten\n    - nested\n\n    | A | B |\n    | - | - |\n    \
    | 1 | 2 |\n11. eleven\n\n- > quote\n  >\n  > > inner";

  #[test]
  fn nested_blocks_repeat_container_prefixes_on_every_line() {
    let text = render(NESTED);
    assert_eq!(
      text.text,
      "1. Первый пункт с несколькими абзацами.\n\n   \
      Второй абзац всё ещё относится к первому пункту.\n\n   \
      - Вложенный пункт.\n     - Ещё один уровень с выделением.\n     \
      - Соседний пункт того же уровня.\n\n   \
      > Цитата внутри первого пункта.\n   >\n   > Второй абзац цитаты с ссылкой.\n\n   \
      ```rust\n   let robot = \"clankr\";\n   println!(\"roger roger, {robot}\");\n   \
      ```\n\n\
      2. Второй пункт снова на верхнем уровне.\n3. Третий пункт после вложенных блоков.\n\n\
      > Внешняя цитата.\n>\n> > Вложенная цитата с курсивом.\n>\n> Возвращение во внешнюю цитату."
    );
    // Indents keep the default style, quote markers stay faint and code keeps its colours.
    let out = ansi(&text);
    assert!(out.contains("\r\n   \x1b[0m\x1b[2m> \x1b[0mЦитата"));
    assert!(out.contains("\x1b[2m>\x1b[0m\r\n   \x1b[0m\x1b[2m> \x1b[0mВторой абзац цитаты"));
    assert!(out.contains("```rust\r\n\x1b[0m   \x1b[0m\x1b[35mlet"));
    assert_eq!(styled(&text, "let"), [("let", Style::default().colour(MAGENTA))]);
    assert_eq!(styled(&text, "> >"), [("> > ", Style::default().with(Style::FAINT))]);

    assert_eq!(
      render(WIDE).text,
      "9. nine\n10. ten\n    - nested\n\n    A │ B\n    ──┼──\n    1 │ 2\n\n11. eleven\n\n\
      - > quote\n  >\n  > > inner"
    );
  }

  #[test]
  fn markdown_formats_blocks_and_nested_styles_with_palette_colours() {
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
      ```rust\nfn main() {\n    println!(\"hello\");\n}\n```"
    );
    let out = ansi(&text);
    assert!(out.contains("\x1b[0m\x1b[1m\x1b[34mTitle\x1b[0m"));
    assert!(out.contains("\x1b[1m\x1b[3mboth"));
    assert!(out.contains("\x1b[9mold"));
    assert!(out.contains("\x1b[0m and \x1b[0m\x1b[36mcode\x1b[0m."));
    assert!(out.contains("\x1b[0m\x1b[2m> \x1b[0mquoted"));
    assert!(out.contains("\x1b[0m\x1b[2m```rust\r\n\x1b[0m\x1b[35mfn"));
    assert!(out.ends_with("\x1b[0m\x1b[2m```\x1b[0m"));
    // Only classic palette colours: no 256-colour, truecolour or background sequences.
    assert!(!out.contains("38;") && !out.contains("48;"));
  }

  #[test]
  fn headings_inline_code_and_decorations_use_accents_and_faint_rules() {
    let bold = Style::default().with(Style::BOLD);
    let faint = Style::default().with(Style::FAINT);
    let text = render(
      "## Head `code`\n\nplain `span` [`link`](https://example.org)\n\n\
      | A | B |\n| - | - |\n| 1 | 2 |\n\n---\n\n> quote",
    );
    assert_eq!(styled(&text, "Head"), [("Head ", bold.colour(BLUE))]);
    assert_eq!(styled(&text, "code"), [("code", bold.colour(BLUE).colour(CYAN))]);
    assert_eq!(styled(&text, "span"), [("span", Style::default().colour(CYAN))]);
    let link = text.parts().find(|(part, ..)| *part == "link").unwrap();
    assert_eq!((link.1, link.2.is_some()), (Style::default().colour(CYAN), true));
    assert_eq!(styled(&text, "│"), [(" │ ", faint), (" │ ", faint)]);
    assert_eq!(styled(&text, "┼"), [("──┼──", faint)]);
    assert_eq!(styled(&text, "────────"), [("────────", faint)]);
    assert_eq!(styled(&text, ">"), [("> ", faint)]);
    assert_eq!(styled(&text, "quote"), [("quote", Style::default())]);
    assert_eq!(ansi(&text).matches("\x1b[4m").count(), 1, "only the link is underlined");
  }

  #[test]
  fn fenced_code_highlights_known_languages_and_leaves_others_plain() {
    let comment = Style::default().with(Style::FAINT | Style::ITALIC);
    let colour = |code| Style::default().colour(code);
    let text = render(
      "```rust\nfn main() {\n  // note\n  let s = \"hi\"; let n = 42; let b = true;\n}\n```",
    );
    assert_eq!(styled(&text, "fn"), [("fn", colour(MAGENTA))]);
    assert_eq!(styled(&text, "main"), [("main", colour(BLUE))]);
    assert_eq!(styled(&text, "// note"), [("// note", comment)]);
    assert_eq!(styled(&text, "hi"), [("\"hi\"", colour(GREEN))]);
    assert_eq!(styled(&text, "42"), [("42", colour(YELLOW))]);
    assert_eq!(styled(&text, "true"), [("true", colour(YELLOW))]);
    // Operators, punctuation and variables keep the default foreground.
    assert!(styled(&text, "=").iter().all(|(_, style)| *style == Style::default()));
    assert!(styled(&text, "{").iter().all(|(_, style)| *style == Style::default()));

    let text = render(
      "```python\ndef greet(robot: Robot) -> str:\n    # note\n    return f\"hi {robot}\" or 42 or None\n```",
    );
    assert_eq!(styled(&text, "def"), [("def", colour(MAGENTA))]);
    assert_eq!(styled(&text, "greet"), [("greet", colour(BLUE))]);
    assert_eq!(styled(&text, "str"), [("str", colour(CYAN))]);
    assert_eq!(styled(&text, "# note"), [("# note", comment)]);
    assert_eq!(styled(&text, "return"), [("return", colour(MAGENTA))]);
    assert_eq!(styled(&text, "42"), [("42", colour(YELLOW))]);
    assert_eq!(styled(&text, "None"), [("None", colour(YELLOW))]);

    for lang in ["typescript", "ts", "bash", "json", "sql", "diff"] {
      let text = render(&format!("```{lang}\nx\n```"));
      assert!(Code::new(lang).is_some(), "{lang}");
      assert_eq!(text.text, format!("```{lang}\nx\n```"));
    }
    let text = render("```ts\nconst x: number = 42;\n```");
    assert_eq!(styled(&text, "const"), [("const", colour(MAGENTA))]);
    assert_eq!(styled(&text, "number"), [("number", colour(CYAN))]);
    let text = render("```diff\n-old\n+new\n```");
    assert_eq!(styled(&text, "old"), [("-old", colour(RED))]);
    assert_eq!(styled(&text, "new"), [("+new", colour(GREEN))]);

    for source in ["```robotlang\nfn \"x\" 42\n```", "```\nfn \"x\" 42\n```", "    fn \"x\" 42"] {
      let text = render(source);
      assert_eq!(styled(&text, "fn"), [("fn \"x\" 42\n", Style::default())], "{source:?}");
    }
  }

  // Shaped like the nested fence section of mocks/markdown.md.
  const FENCED: &str = "````markdown\n# Head\n\n```rust\nprintln!(\"x\");\n```\n````\n\nafter";

  #[test]
  fn code_blocks_render_faint_markdown_fences_around_highlighted_code() {
    let faint = Style::default().with(Style::FAINT);
    let magenta = Style::default().colour(MAGENTA);
    let plain = Style::default();
    for (source, expected) in [
      ("```rust\nlet x = 1;\n```", "```rust\nlet x = 1;\n```"),
      ("~~~rust title\nlet x = 1;\n~~~", "```rust\nlet x = 1;\n```"),
      ("```\nlet x = 1;\n```", "```\nlet x = 1;\n```"),
      ("    let x = 1;", "```\nlet x = 1;\n```"),
      ("- item\n\n  ```rust\n  let x = 1;\n  ```", "- item\n\n  ```rust\n  let x = 1;\n  ```"),
      ("> ```rust\n> let x = 1;\n> ```", "> ```rust\n> let x = 1;\n> ```"),
    ] {
      let text = render(source);
      assert_eq!(text.text, expected, "{source:?}");
      let fences = styled(&text, "`");
      assert_eq!(fences.len(), 2, "{source:?}");
      assert!(fences.iter().all(|(_, style)| *style == faint), "{source:?}");
      let code = if source.contains("rust") { ("let", magenta) } else { ("let x = 1;\n", plain) };
      assert_eq!(styled(&text, "let"), [code], "{source:?}");
    }
    // mdtext does not expose the source fence, so a nested fence reuses three backticks.
    let text = render(FENCED);
    assert_eq!(text.text, "```markdown\n# Head\n\n```rust\nprintln!(\"x\");\n```\n```\n\nafter");
    assert_eq!(parts(&text)[0], ("```markdown\n", faint, None));
    assert!(styled(&text, "after").iter().all(|(_, style)| *style == plain));
  }

  #[test]
  fn multiline_comments_and_strings_keep_their_style_on_following_lines() {
    let comment = Style::default().with(Style::FAINT | Style::ITALIC);
    let green = Style::default().colour(GREEN);
    let text = render("```rust\n/* one\ntwo */ let s = \"a\nb\";\n```\n\nafter");
    assert_eq!(styled(&text, "one"), [("/* one\ntwo */", comment)]);
    assert_eq!(styled(&text, "let"), [("let", Style::default().colour(MAGENTA))]);
    assert_eq!(styled(&text, "a\nb"), [("\"a\nb\"", green)]);
    assert_eq!(styled(&text, "after"), [("\n\nafter", Style::default())]);
    let text = render("```python\nx = \"\"\"doc\nstring\"\"\"\ny = 1\n```");
    assert_eq!(styled(&text, "doc"), [("\"\"\"doc\nstring\"\"\"", green)]);
    assert_eq!(styled(&text, "1"), [("1", Style::default().colour(YELLOW))]);
  }

  #[test]
  fn unfinished_tokens_keep_their_style_without_leaking_out_of_injected_code() {
    let text =
      render("```rust\nfn main() {\n  /* open\n  still */ let s = \"/* literal */\";\n}\n```");
    let comment = Style::default().with(Style::FAINT | Style::ITALIC);
    assert_eq!(styled(&text, "main"), [("main", Style::default().colour(BLUE))]);
    assert_eq!(styled(&text, "open"), [("/* open\n  still */", comment)]);
    assert_eq!(styled(&text, "literal"), [("\"/* literal */\"", Style::default().colour(GREEN))]);

    let text = render("````markdown\n```rust\n/* unfinished\n```\n\nafter code\n````");
    assert!(styled(&text, "unfinished").iter().all(|(_, style)| *style == comment));
    let after = styled(&text, "after code");
    assert!(!after.is_empty() && after.iter().all(|(_, style)| *style == Style::default()));
  }

  #[test]
  fn streamed_chunks_match_complete_markdown_and_flush_unclosed_syntax() {
    for source in [
      "# Heading\n\n**bold** *italic* ~~strike~~ `code` &amp; \\*literal\\*\n\nnext",
      "Heading\n=======\n\n[site](https://example.org) ![alt](image.png) <https://example.org>",
      "- first\n  - nested\n- [ ] todo\n\n> quote\n> **strong**\n\nlast",
      "| name | value |\n| --- | ---: |\n| **bold** | 42 |\n\nlast",
      "```rust\nfn main() {}\n    e\u{301} 👩‍💻 界\n```\n\nnext",
      "```rust\n/* multi\n   line */ let s = \"str\ning\"; // tail\n```\n\n**after**",
      "> ```python\n> \"\"\"doc\n> string\"\"\"\n> ```\n\n1. ```ts\n   const x: number = 42;\n   ```",
      "```\n    unfinished code",
      "```rust\nlet unfinished = \"str",
      "**unfinished `inline",
      "a\r\nb\r\n\r\nc\n\n&#27;[31m escaped \x1b[2J control",
      "    indented\n\n~~~\ntilde\n~~~",
      "$E = mc^2$ and $a_i^2$\n\n$$\nx = \\frac{1}{2}\n$$\n\nafter",
      "\\(\\alpha + \\beta\\)\n\n\\[\\frac{a}{b}\\]\n\n```math\nx^2\n```",
      "$$\n\\begin{pmatrix}1 & 2 \\\\ 3 & 4\\end{pmatrix}\n$$\n\nafter",
      NESTED,
      WIDE,
      FENCED,
      "\\(",
      "\\[",
    ] {
      let expected = render(source);
      let mut markdown = Markdown::default();
      let mut previous = Vec::new();
      // Every prefix splits a line or a token somewhere; committed spans and styles never change.
      for (i, c) in source.char_indices() {
        let text = markdown.update(&source[..i + c.len_utf8()], false);
        let len = previous.iter().map(|(part, _): &(String, Style)| part.len()).sum::<usize>();
        let committed: Vec<_> =
          text.slice(0..len).parts().map(|(part, style, _)| (part.to_owned(), style)).collect();
        assert_eq!(committed, previous, "changed emitted text for {source:?}");
        previous = text.parts().map(|(part, style, _)| (part.to_owned(), style)).collect();
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
      ("```rust\nfn", "```rust\n", "fn\n```"),
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

    // The preview re-renders the open block from its fence with the same highlighting state.
    let mut markdown = Markdown::default();
    let source = "```rust\n/* open\nstill */ fn";
    for (i, c) in source.char_indices() {
      markdown.update(&source[..i + c.len_utf8()], false);
    }
    let comment = Style::default().with(Style::FAINT | Style::ITALIC);
    assert_eq!(styled(markdown.text(), "open"), [("/* open\n", comment)]);
    assert_eq!(styled(markdown.preview(), "still"), [("still */", comment)]);
    assert_eq!(styled(markdown.preview(), "fn"), [("fn", Style::default().colour(MAGENTA))]);
    assert!(ansi(markdown.preview()).ends_with("\x1b[0m\x1b[2m```\x1b[0m"));

    // Committed text plus preview renders like the finished prefix, whether the preview resumes
    // the committed state mid-block or one update spans several blocks.
    let source =
      "```rust\n/* a\n b */ let s = \"x\ny\"; // z\nfn f() {}\n```\n\n```py\nx = 1\ny = \"\"\"q\nw";
    let joined = |markdown: &Markdown| {
      let mut text = markdown.text().slice(0..markdown.committed_len());
      // The view separates a closed block from the next block's preview with a blank line.
      if !text.text.is_empty() && !text.text.ends_with('\n') && !markdown.preview().text.is_empty()
      {
        text.push("\n\n", Style::default(), None);
      }
      for (part, style, link) in markdown.preview().parts() {
        text.push(part, style, link);
      }
      text
    };
    let mut markdown = Markdown::default();
    for (i, c) in source.char_indices() {
      let prefix = &source[..i + c.len_utf8()];
      markdown.update(prefix, false);
      assert_eq!(parts(&joined(&markdown)), parts(&render(prefix)), "{prefix:?}");
    }
    let mut markdown = Markdown::default();
    markdown.update(source, false);
    assert_eq!(parts(&joined(&markdown)), parts(&render(source)));

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
    assert!(markdown.update(&third, true).text.ends_with("\n```"));
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
      Rust     │      3 304\n─────────┼───────────\n\
      Python   │        621\n─────────┼───────────\n\
      TOML     │         26\n─────────┼───────────\n\
      Makefile │         19\n─────────┼───────────\nВсего    │      3 970\n\nAfter"
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
    assert_eq!(lines[3], lines[1]);
    assert_eq!(lines[4], "👩‍💻 │   e\u{301}    │    2");
    assert_eq!(lines[5], lines[1]);
    for line in lines.iter().filter(|line| line.contains('│')) {
      assert_eq!(line.split('│').map(str::width).collect::<Vec<_>>(), [3, 8, 5]);
    }
  }

  #[test]
  fn math_formats_formulas_but_leaves_currency_and_code_literal() {
    assert_eq!(
      render("$E = mc^2$ and \\(a_i^2\\)\n\n$$\nx = \\frac{1}{2}\n$$\n\nafter").text,
      "E = mc² and aᵢ²\n\n    1\nx = ─\n    2\n\nafter"
    );
    assert_eq!(render("\\[\\frac{a}{b}\\]").text, "a\n─\nb");
    assert_eq!(render("```math\nx^2\n```").text, "x²");
    assert_eq!(
      render("$5 for coffee, $10 for oil and `a_b`.").text,
      "$5 for coffee, $10 for oil and a_b."
    );
    assert_eq!(render("```text\n$x^2$\n```").text, "```text\n$x^2$\n```");
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
