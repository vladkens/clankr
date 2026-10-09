use std::cell::RefCell;
use std::ops::Range;
use std::sync::LazyLock;

use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

const SCOPES: &[&str] = &[
  "comment",
  "keyword",
  "string",
  "character",
  "number",
  "float",
  "boolean",
  "constant",
  "function",
  "type",
  "constructor",
  "namespace",
  "diff.plus",
  "diff.minus",
  "scope",
  "comment.open",
  "string.open",
];

thread_local! {
  static HIGHLIGHTER: RefCell<Highlighter> = RefCell::new(Highlighter::new());
}

// Only the open code block is retained. Preview clones cannot change committed state.
#[derive(Clone)]
pub struct Code {
  config: &'static HighlightConfiguration,
  source: String,
}

impl Code {
  pub fn new(lang: &str) -> Option<Self> {
    Some(Self { config: config(lang)?, source: String::new() })
  }

  pub fn line(&mut self, line: &str) -> Option<Vec<(&'static str, Range<usize>)>> {
    let offset = self.source.len();
    self.source.push_str(line);
    HIGHLIGHTER.with_borrow_mut(|highlighter| {
      // The closure coerces static configurations to the highlighter's shorter borrow.
      #[allow(clippy::redundant_closure)]
      let events = highlighter
        .highlight(self.config, self.source.as_bytes(), None, None, |name| config(name))
        .ok()?;
      let mut scopes = Vec::new();
      let mut parts = Vec::new();
      let mut open = None;
      for event in events {
        match event.ok()? {
          HighlightEvent::HighlightStart(highlight) => {
            let scope = SCOPES[highlight.0];
            // A parser-recognized opening delimiter colours the unfinished token up to
            // its language boundary, including when that language is injected into Markdown.
            if open.is_none()
              && let Some(scope) = scope.strip_suffix(".open")
              && let Some(depth) = scopes.iter().rposition(|&name| name == "scope")
            {
              open = Some((scope, depth));
            }
            scopes.push(scope);
          }
          HighlightEvent::HighlightEnd => {
            scopes.pop();
            if open.is_some_and(|(_, depth)| scopes.len() <= depth) {
              open = None;
            }
          }
          HighlightEvent::Source { start, end } if end > offset => {
            parts.push((
              open.map(|(scope, _)| scope).or_else(|| scopes.last().copied()).unwrap_or(""),
              start.saturating_sub(offset)..end - offset,
            ));
          }
          _ => {}
        }
      }
      Some(parts)
    })
  }
}

fn config(lang: &str) -> Option<&'static HighlightConfiguration> {
  // Each grammar's queries are compiled once, on first use, including injected languages.
  macro_rules! grammar {
    ($name:literal, $language:expr, $highlights:expr) => {
      grammar!($name, $language, $highlights, "", "")
    };
    ($name:literal, $language:expr, $highlights:expr, $injections:expr, $locals:expr) => {{
      static CONFIG: LazyLock<Option<HighlightConfiguration>> = LazyLock::new(|| {
        let mut config = HighlightConfiguration::new(
          $language.into(),
          $name,
          &$highlights,
          &$injections,
          &$locals,
        )
        .ok()?;
        config.configure(SCOPES);
        Some(config)
      });
      CONFIG.as_ref()
    }};
  }

  match lang.to_ascii_lowercase().as_str() {
    "rust" | "rs" => grammar!(
      "rust",
      tree_sitter_rust::LANGUAGE,
      format!(
        "{}\n{}",
        tree_sitter_rust::HIGHLIGHTS_QUERY,
        r#"
        (source_file) @scope
        (ERROR "fn" . (identifier) @function)
        (ERROR "/*" @comment.open)
        (ERROR "\"" @string.open)
      "#
      ),
      tree_sitter_rust::INJECTIONS_QUERY,
      ""
    ),
    "python" | "py" => {
      grammar!(
        "python",
        tree_sitter_python::LANGUAGE,
        format!(
          "{}\n{}",
          tree_sitter_python::HIGHLIGHTS_QUERY,
          "(module) @scope\n(ERROR (string_start) @string.open)"
        )
      )
    }
    "javascript" | "js" | "jsx" => grammar!(
      "javascript",
      tree_sitter_javascript::LANGUAGE,
      format!(
        "{}\n{}",
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
      ),
      tree_sitter_javascript::INJECTIONS_QUERY,
      tree_sitter_javascript::LOCALS_QUERY
    ),
    "typescript" | "ts" => grammar!(
      "typescript",
      tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
      format!(
        "{}\n{}",
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_typescript::HIGHLIGHTS_QUERY
      ),
      tree_sitter_javascript::INJECTIONS_QUERY,
      tree_sitter_typescript::LOCALS_QUERY
    ),
    "tsx" => grammar!(
      "tsx",
      tree_sitter_typescript::LANGUAGE_TSX,
      format!(
        "{}\n{}\n{}",
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
        tree_sitter_typescript::HIGHLIGHTS_QUERY
      ),
      tree_sitter_javascript::INJECTIONS_QUERY,
      tree_sitter_typescript::LOCALS_QUERY
    ),
    "bash" | "sh" | "shell" => {
      grammar!("bash", tree_sitter_bash::LANGUAGE, tree_sitter_bash::HIGHLIGHT_QUERY)
    }
    "json" => grammar!("json", tree_sitter_json::LANGUAGE, tree_sitter_json::HIGHLIGHTS_QUERY),
    "sql" => grammar!("sql", tree_sitter_sequel::LANGUAGE, tree_sitter_sequel::HIGHLIGHTS_QUERY),
    "diff" | "patch" => grammar!(
      "diff",
      tree_sitter_diff::LANGUAGE,
      tree_sitter_diff::HIGHLIGHTS_QUERY
        .replace("@string", "@diff.plus")
        .replace("@keyword", "@diff.minus")
    ),
    "markdown" | "md" => grammar!(
      "markdown",
      tree_sitter_md::LANGUAGE,
      tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
      // Include children so injected code retains all its text ranges during streaming.
      "((fenced_code_block (info_string (language) @injection.language)
        (code_fence_content) @injection.content) (#set! injection.include-children))
       ((inline) @injection.content (#set! injection.language \"markdown_inline\")
        (#set! injection.include-children))",
      ""
    ),
    "markdown_inline" => grammar!(
      "markdown_inline",
      tree_sitter_md::INLINE_LANGUAGE,
      tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
      tree_sitter_md::INJECTION_QUERY_INLINE,
      ""
    ),
    "toml" => {
      grammar!("toml", tree_sitter_toml_ng::LANGUAGE, tree_sitter_toml_ng::HIGHLIGHTS_QUERY)
    }
    "yaml" | "yml" => {
      grammar!("yaml", tree_sitter_yaml::LANGUAGE, tree_sitter_yaml::HIGHLIGHTS_QUERY)
    }
    "html" => grammar!(
      "html",
      tree_sitter_html::LANGUAGE,
      tree_sitter_html::HIGHLIGHTS_QUERY,
      tree_sitter_html::INJECTIONS_QUERY,
      ""
    ),
    "css" => grammar!("css", tree_sitter_css::LANGUAGE, tree_sitter_css::HIGHLIGHTS_QUERY),
    "c" => grammar!("c", tree_sitter_c::LANGUAGE, tree_sitter_c::HIGHLIGHT_QUERY),
    "cpp" | "c++" => grammar!(
      "cpp",
      tree_sitter_cpp::LANGUAGE,
      format!("{}\n{}", tree_sitter_c::HIGHLIGHT_QUERY, tree_sitter_cpp::HIGHLIGHT_QUERY)
    ),
    "go" | "golang" => grammar!("go", tree_sitter_go::LANGUAGE, tree_sitter_go::HIGHLIGHTS_QUERY),
    "java" => grammar!("java", tree_sitter_java::LANGUAGE, tree_sitter_java::HIGHLIGHTS_QUERY),
    "ruby" | "rb" => grammar!(
      "ruby",
      tree_sitter_ruby::LANGUAGE,
      tree_sitter_ruby::HIGHLIGHTS_QUERY,
      "",
      tree_sitter_ruby::LOCALS_QUERY
    ),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn bundled_queries_compile_and_aliases_share_configs() {
    for lang in [
      "rust",
      "python",
      "javascript",
      "typescript",
      "tsx",
      "bash",
      "json",
      "sql",
      "diff",
      "markdown",
      "markdown_inline",
      "toml",
      "yaml",
      "html",
      "css",
      "c",
      "cpp",
      "go",
      "java",
      "ruby",
    ] {
      assert!(config(lang).is_some(), "{lang}");
    }
    for (alias, name) in [
      ("rs", "rust"),
      ("py", "python"),
      ("JS", "javascript"),
      ("ts", "typescript"),
      ("sh", "bash"),
      ("yml", "yaml"),
      ("md", "markdown"),
    ] {
      assert!(std::ptr::eq(config(alias).unwrap(), config(name).unwrap()));
    }
    assert!(config("robotlang").is_none());
  }
}
