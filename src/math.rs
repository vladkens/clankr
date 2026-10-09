use pulldown_latex::event::{
  Content, EnvironmentFlow as Flow, Grouping, ScriptType, StateChange, Visual,
};
use pulldown_latex::{Event, Parser, Storage};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub fn render(source: &str, block: bool) -> String {
  let source = source.trim();
  let convert = || {
    if source.len() > 8192 {
      return None;
    }
    let storage = Storage::new();
    let events = Parser::new(source, &storage).collect::<Result<Vec<_>, _>>().ok()?;
    let mut reader = Render { events: &events, block };
    let mut out = Block::text("");
    while !reader.events.is_empty() {
      out.append(reader.next(0)?);
    }
    Some(out.lines.iter().map(|line| line.trim_end()).collect::<Vec<_>>().join("\n"))
  };
  // Unsupported notation stays intact instead of silently changing the formula.
  convert().unwrap_or_else(|| source.to_owned())
}

// Each expression keeps its baseline, so a fraction can sit beside text or another fraction.
struct Block {
  lines: Vec<String>,
  base: usize,
}

impl Block {
  fn text(text: impl Into<String>) -> Self {
    Self { lines: vec![text.into()], base: 0 }
  }

  fn width(&self) -> usize {
    self.lines.iter().map(|line| line.width()).max().unwrap_or(0)
  }

  fn append(&mut self, right: Self) {
    let width = self.width();
    let base = self.base.max(right.base);
    let height = base + (self.lines.len() - self.base).max(right.lines.len() - right.base);
    let line = |block: &Self, row: usize| {
      row
        .checked_sub(base - block.base)
        .and_then(|i| block.lines.get(i))
        .cloned()
        .unwrap_or_default()
    };
    self.lines = (0..height)
      .map(|row| {
        let left = line(self, row);
        format!("{left}{}{}", " ".repeat(width - left.width()), line(&right, row))
      })
      .collect();
    self.base = base;
  }

  fn trim(mut self) -> Self {
    let margin = self
      .lines
      .iter()
      .filter(|line| !line.trim().is_empty())
      .map(|line| line.bytes().take_while(|c| *c == b' ').count())
      .min()
      .unwrap_or(0);
    for line in &mut self.lines {
      *line = line.get(margin..).unwrap_or("").trim_end().to_owned();
    }
    if self.lines.len() == 1
      && let Some(rest) = self.lines[0].strip_prefix("− ")
    {
      self.lines[0] = format!("−{rest}");
    }
    self
  }

  fn enclose(mut self, left: Option<char>, right: Option<char>) -> Self {
    let width = self.width();
    let height = self.lines.len();
    let delimiter = |c: Option<char>, row| match (c, height) {
      (None, _) => String::new(),
      (Some(c), 1) => c.to_string(),
      (Some(c), _) => {
        let (top, middle, bottom) = match c {
          '(' => ('⎛', '⎜', '⎝'),
          ')' => ('⎞', '⎟', '⎠'),
          '[' => ('⎡', '⎢', '⎣'),
          ']' => ('⎤', '⎥', '⎦'),
          '{' => ('⎧', '⎨', '⎩'),
          '}' => ('⎫', '⎬', '⎭'),
          c => (c, c, c),
        };
        if row == 0 {
          top
        } else if row + 1 == height {
          bottom
        } else {
          middle
        }
        .to_string()
      }
    };
    for (row, line) in self.lines.iter_mut().enumerate() {
      *line = format!(
        "{}{line}{}{}",
        delimiter(left, row),
        " ".repeat(width - line.width()),
        delimiter(right, row)
      );
    }
    self
  }

  fn operand(self) -> Self {
    let text = self.trim();
    if text.lines.len() == 1 && text.lines[0].graphemes(true).count() == 1 {
      text
    } else {
      text.enclose(Some('('), Some(')'))
    }
  }

  fn fraction(top: Self, bottom: Self) -> Self {
    let (top, bottom) = (top.trim(), bottom.trim());
    let width = top.width().max(bottom.width()).max(1);
    let base = top.lines.len();
    let center = |block: Self| {
      let pad = " ".repeat((width - block.width()) / 2);
      block.lines.into_iter().map(move |line| format!("{pad}{line}"))
    };
    let mut lines: Vec<_> = center(top).collect();
    lines.push("─".repeat(width));
    lines.extend(center(bottom));
    Self { lines, base }
  }
}

struct Render<'a, 'b> {
  events: &'b [Event<'a>],
  block: bool,
}

impl Render<'_, '_> {
  fn next(&mut self, depth: usize) -> Option<Block> {
    if depth >= 64 {
      return None;
    }
    let (event, rest) = self.events.split_first()?;
    self.events = rest;
    let next = |this: &mut Self| this.next(depth + 1);
    Some(match event {
      Event::Content(content) => Block::text(match content {
        Content::Text(text) | Content::Number(text) => (*text).into(),
        Content::Function(text) => format!("{text} "),
        Content::LargeOp { content, .. } => format!("{content} "),
        Content::Ordinary { content, .. } | Content::Delimiter { content, .. } => {
          content.to_string()
        }
        Content::BinaryOp { content, .. } => format!(" {content} "),
        Content::Relation { content, .. } => {
          let mut bytes = [0; 8];
          let text = std::str::from_utf8(content.encode_utf8_to_buf(&mut bytes)).ok()?;
          format!(" {text} ")
        }
        Content::Punctuation(c) => format!("{c} "),
      }),
      Event::Begin(group) => {
        let matrix = matches!(group, Grouping::Matrix { .. } | Grouping::Array(..));
        let mut rows = vec![vec![Block::text("")]];
        loop {
          match self.events.first()? {
            Event::End => {
              self.events = &self.events[1..];
              break;
            }
            Event::EnvironmentFlow(Flow::Alignment) => {
              self.events = &self.events[1..];
              rows.last_mut()?.push(Block::text(""));
            }
            Event::EnvironmentFlow(Flow::NewLine { horizontal_lines, .. })
              if horizontal_lines.is_empty() =>
            {
              self.events = &self.events[1..];
              rows.push(vec![Block::text("")]);
            }
            _ => rows.last_mut()?.last_mut()?.append(next(self)?),
          }
        }
        let text = if rows.len() == 1 && rows[0].len() == 1 {
          rows.pop()?.pop()?.trim()
        } else if self.block {
          grid(rows, matrix)
        } else {
          Block::text(
            rows
              .into_iter()
              .map(|row| {
                row
                  .into_iter()
                  .map(|cell| cell.trim().lines.remove(0))
                  .collect::<Vec<_>>()
                  .join(if matrix { ", " } else { " " })
              })
              .collect::<Vec<_>>()
              .join("; "),
          )
        };
        match group {
          Grouping::LeftRight(left, right) => text.enclose(*left, *right),
          Grouping::Cases { left: true } => text.enclose(Some('{'), None),
          Grouping::Cases { left: false } => text.enclose(None, Some('}')),
          _ => text,
        }
      }
      Event::Visual(Visual::Fraction(None)) => {
        let numerator = next(self)?;
        let denominator = next(self)?;
        if self.block {
          Block::fraction(numerator, denominator)
        } else {
          let mut text = numerator.operand();
          text.append(Block::text("/"));
          text.append(denominator.operand());
          text
        }
      }
      Event::Visual(Visual::SquareRoot) => {
        let mut text = Block::text("√");
        text.append(next(self)?.operand());
        text
      }
      Event::Visual(Visual::Root) => {
        let radicand = next(self)?;
        let index = self.inline(depth + 1)?;
        let mut text = Block::text(format!("{}√", script(&index, true)));
        text.append(radicand.operand());
        text
      }
      Event::Script { ty, .. } => {
        let base = next(self)?;
        let gap = base.lines[base.base].ends_with(' ');
        let mut text = base.operand();
        let mut suffix = String::new();
        if matches!(ty, ScriptType::Subscript | ScriptType::SubSuperscript) {
          suffix.push_str(&script(&self.inline(depth + 1)?, false));
        }
        if matches!(ty, ScriptType::Superscript | ScriptType::SubSuperscript) {
          suffix.push_str(&script(&self.inline(depth + 1)?, true));
        }
        if gap {
          suffix.push(' ');
        }
        text.append(Block::text(suffix));
        text
      }
      Event::Space { width, .. } => Block::text(if width.is_some() { " " } else { "" }),
      Event::StateChange(StateChange::Style(_)) => Block::text(""),
      _ => return None,
    })
  }

  fn inline(&mut self, depth: usize) -> Option<String> {
    let block = std::mem::replace(&mut self.block, false);
    let text = self.next(depth);
    self.block = block;
    text.map(|mut text| text.lines.remove(0))
  }
}

fn grid(rows: Vec<Vec<Block>>, matrix: bool) -> Block {
  let rows: Vec<Vec<_>> =
    rows.into_iter().map(|row| row.into_iter().map(Block::trim).collect()).collect();
  let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
  let widths: Vec<_> = (0..cols)
    .map(|i| rows.iter().filter_map(|row| row.get(i)).map(Block::width).max().unwrap_or(0))
    .collect();
  let mut lines = Vec::new();
  for row in rows {
    let mut out = Block::text("");
    for (i, cell) in row.into_iter().enumerate() {
      let pad = widths[i] - cell.width();
      let left = if matrix {
        pad / 2
      } else if i % 2 == 0 {
        pad
      } else {
        0
      };
      out.append(Block::text(" ".repeat(left)));
      out.append(cell);
      if i + 1 < cols {
        out.append(Block::text(" ".repeat(pad - left + 2)));
      }
    }
    lines.extend(out.lines);
  }
  let base = lines.len().saturating_sub(1) / 2;
  Block { lines, base }
}

fn script(text: &str, upper: bool) -> String {
  let (plain, raised) = if upper {
    ("0123456789+-=()abcdefghijklmnoprstuvwxyz", "⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ᵃᵇᶜᵈᵉᶠᵍʰⁱʲᵏˡᵐⁿᵒᵖʳˢᵗᵘᵛʷˣʸᶻ")
  } else {
    ("0123456789+-=()aehijklmnoprstuvx", "₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎ₐₑₕᵢⱼₖₗₘₙₒₚᵣₛₜᵤᵥₓ")
  };
  let mapped: Option<String> = text
    .chars()
    .filter(|c| !c.is_whitespace())
    .map(|c| plain.chars().zip(raised.chars()).find_map(|(a, b)| (a == c).then_some(b)))
    .collect();
  mapped.unwrap_or_else(|| format!("{}({})", if upper { '^' } else { '_' }, text.trim()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn fractions_align_surrounding_expressions_and_nested_operands() {
    let source = r"x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}";
    assert_eq!(render(source, true), "    −b ± √(b² − 4ac)\nx = ────────────────\n           2a");
    assert!(!render(source, false).contains('\n'));
    assert_eq!(render(r"x = \frac{\frac{a}{b}}{c}", true), "    a\n    ─\n    b\nx = ─\n    c");
    assert_eq!(render(r"a_i^2 + b_i^2 = c_i^2", false), "aᵢ² + bᵢ² = cᵢ²");
  }

  #[test]
  fn matrices_keep_rows_columns_and_surrounding_terms() {
    assert_eq!(
      render(r"A = \begin{pmatrix}1 & 22 \\ 333 & 4\end{pmatrix} + B", true),
      "A = ⎛ 1   22⎞ + B\n    ⎝333  4 ⎠"
    );
  }

  #[test]
  fn unsupported_and_incomplete_notation_stays_intact() {
    for source in [r"\unknown{x}", r"\frac{a}", r"\frac{a}{b", r"\color{red}{x}"] {
      assert_eq!(render(source, true), source);
    }
  }
}
