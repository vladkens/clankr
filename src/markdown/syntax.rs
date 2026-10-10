use std::ops::Range;
use std::sync::LazyLock;

use syntect::highlighting::{
  Color, FontStyle, HighlightState, Highlighter, RangedHighlightIterator, StyleModifier, Theme,
  ThemeItem,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};

const RED: u8 = 31;
const GREEN: u8 = 32;
const YELLOW: u8 = 33;
const BLUE: u8 = 34;
const MAGENTA: u8 = 35;
const CYAN: u8 = 36;

// Loaded on the first fenced block with a language; bat's set adds TypeScript, TOML and others.
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

// A theme only selects scopes: red carries the SGR colour code, 0 keeps the default foreground.
// Italic is reserved for comments, which render faint and italic.
static THEME: LazyLock<Theme> = LazyLock::new(|| {
  let item = |scope: &str, code: u8, font_style| ThemeItem {
    scope: scope.parse().unwrap(),
    style: StyleModifier {
      foreground: Some(Color { r: code, g: 0, b: 0, a: 0 }),
      background: None,
      font_style,
    },
  };
  let mut scopes = vec![item("comment", 0, Some(FontStyle::ITALIC))];
  scopes.extend(
    [
      ("keyword, storage, keyword.operator.word", MAGENTA),
      ("keyword.operator", 0),
      ("string, markup.inserted", GREEN),
      ("markup.deleted", RED),
      ("constant.numeric, constant.language", YELLOW),
      ("entity.name.function, support.function, support.macro, variable.function", BLUE),
      (
        "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, \
        entity.name.trait, entity.name.interface, entity.other.inherited-class, support.type, \
        support.class",
        CYAN,
      ),
    ]
    .map(|(scope, code)| item(scope, code, None)),
  );
  Theme { scopes, ..Theme::default() }
});

static HIGHLIGHTER: LazyLock<Highlighter<'static>> = LazyLock::new(|| Highlighter::new(&THEME));

// Parser state carries multi-line comments and strings from one code line to the next.
#[derive(Clone)]
pub struct Code {
  parse: ParseState,
  highlight: HighlightState,
}

impl Code {
  pub fn new(lang: &str) -> Option<Self> {
    let lang = lang.to_ascii_lowercase();
    let lang = match lang.as_str() {
      "shell" => "sh",
      "golang" => "go",
      _ => &lang,
    };
    let syntax = SYNTAXES.find_syntax_by_token(lang)?;
    Some(Self {
      parse: ParseState::new(syntax),
      highlight: HighlightState::new(&HIGHLIGHTER, ScopeStack::new()),
    })
  }

  pub fn line(&mut self, line: &str) -> Option<Vec<(&'static str, Range<usize>)>> {
    let ops = self.parse.parse_line(line, &SYNTAXES).ok()?;
    let parts = RangedHighlightIterator::new(&mut self.highlight, &ops, line, &HIGHLIGHTER);
    Some(
      parts
        .map(|(style, _, range)| {
          let scope = if style.font_style.contains(FontStyle::ITALIC) {
            "comment"
          } else {
            match style.foreground.r {
              RED => "diff.minus",
              GREEN => "string",
              YELLOW => "number",
              BLUE => "function",
              MAGENTA => "keyword",
              CYAN => "type",
              _ => "",
            }
          };
          (scope, range)
        })
        .collect(),
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn bundled_languages_and_existing_aliases_are_available() {
    for lang in [
      "rust",
      "rs",
      "python",
      "py",
      "javascript",
      "js",
      "JS",
      "jsx",
      "typescript",
      "ts",
      "tsx",
      "bash",
      "sh",
      "shell",
      "json",
      "sql",
      "diff",
      "patch",
      "markdown",
      "md",
      "toml",
      "yaml",
      "yml",
      "html",
      "css",
      "c",
      "cpp",
      "c++",
      "go",
      "golang",
      "java",
      "ruby",
      "rb",
      "kotlin",
      "swift",
      "lua",
      "cs",
      "php",
    ] {
      assert!(Code::new(lang).is_some(), "{lang}");
    }
    assert!(Code::new("robotlang").is_none());
  }
}
