use std::ops::Range;

use super::input::Input;
use crate::Res;
use crate::agent::model::{Effort, Model, Settings};

pub const VISIBLE: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
  Clear,
  Quit,
  Fast,
  Copy,
  Model(Option<&'static Model>),
  Effort(Option<Effort>),
}

impl Command {
  pub fn parse(text: &str, models: &'static [Model]) -> Option<Self> {
    if text.contains('\n') {
      return None;
    }
    let text = text.trim_end();
    let (name, value) = text.split_once(' ').unwrap_or((text, ""));
    let value = value.trim();
    match name {
      "/clear" if value.is_empty() => Some(Self::Clear),
      "/quit" if value.is_empty() => Some(Self::Quit),
      "/fast" if value.is_empty() => Some(Self::Fast),
      "/copy" if value.is_empty() => Some(Self::Copy),
      "/model" => Some(Self::Model(models.iter().find(|model| model.name == value))),
      "/effort" => Some(Self::Effort(Effort::parse(value))),
      _ => None,
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
  Commands,
  Skills,
  Models,
  Effort,
}

impl Kind {
  pub fn title(self) -> &'static str {
    match self {
      Self::Commands => "Commands",
      Self::Skills => "Skills",
      Self::Models => "Models",
      Self::Effort => "Effort",
    }
  }

  fn items(self, settings: Settings, models: &'static [Model]) -> Vec<Item> {
    match self {
      Self::Commands => COMMANDS.to_vec(),
      Self::Skills => SKILLS.to_vec(),
      Self::Models => models
        .iter()
        .map(|model| Item {
          name: model.name,
          description: model.description,
          source: model.provider,
        })
        .collect(),
      Self::Effort => settings
        .model
        .efforts
        .iter()
        .map(|effort| Item { name: effort.name(), description: effort.description(), source: "" })
        .collect(),
    }
  }
}

#[derive(Clone, Copy)]
pub struct Item {
  pub name: &'static str,
  pub description: &'static str,
  pub source: &'static str,
}

const COMMANDS: &[Item] = &[
  Item { name: "/clear", description: "Start a fresh conversation", source: "General" },
  Item { name: "/quit", description: "Exit clankr", source: "General" },
  Item { name: "/model", description: "Select model", source: "General" },
  Item { name: "/effort", description: "Select reasoning effort", source: "General" },
  Item { name: "/fast", description: "Toggle fast mode", source: "General" },
  Item { name: "/copy", description: "Copy the last response", source: "General" },
];

// UI fixtures only. Choosing one never reads or executes a local SKILL.md.
const SKILLS: &[Item] = &[
  Item {
    name: "agent-intervention",
    description: "Review an agent's reported failure",
    source: "Global",
  },
  Item {
    name: "agent-rules",
    description: "Create or revise project agent instructions",
    source: "Global",
  },
  Item {
    name: "brainstorm",
    description: "Explore ideas and compare approaches",
    source: "Global",
  },
  Item {
    name: "code-style-python",
    description: "Apply the project's Python coding style",
    source: "Project",
  },
  Item {
    name: "code-style-rust",
    description: "Apply the project's Rust coding style",
    source: "Project",
  },
  Item {
    name: "git-changelog",
    description: "Write release notes from Git history",
    source: "Global",
  },
  Item { name: "plan-exec", description: "Execute an implementation plan", source: "Global" },
  Item { name: "planning", description: "Create an implementation plan", source: "Global" },
];

pub struct Menu {
  pub kind: Kind,
  pub items: Vec<Item>,
  pub selected: usize,
  pub offset: usize,
  pub settings: Settings,
  range: Range<usize>,
  query: String,
  models: &'static [Model],
}

impl Menu {
  pub fn open(input: &Input, settings: Settings, models: &'static [Model]) -> Option<Self> {
    let (kind, range, query) = target(input)?;
    let items = search(kind, &query, settings, models);
    let mut menu = Self { kind, items, selected: 0, offset: 0, settings, range, query, models };
    menu.select_current();
    Some(menu)
  }

  pub fn refresh(&mut self, input: &Input, settings: Settings) -> bool {
    let Some((kind, range, query)) = target(input) else { return false };
    if kind != self.kind || range.start != self.range.start {
      return false;
    }
    if query != self.query || settings != self.settings {
      self.items = search(kind, &query, settings, self.models);
      self.query = query;
      self.settings = settings;
      self.select_current();
    }
    self.range = range;
    true
  }

  pub fn current(&self) -> Option<&'static str> {
    match self.kind {
      Kind::Models => Some(self.settings.model.name),
      Kind::Effort => self.settings.effort.map(Effort::name),
      _ => None,
    }
  }

  fn select_current(&mut self) {
    self.selected = if self.query.is_empty() {
      self.items.iter().position(|item| Some(item.name) == self.current()).unwrap_or(0)
    } else {
      0
    };
    self.offset = (self.selected + 1).saturating_sub(VISIBLE);
  }

  pub fn step(&mut self, down: bool) {
    if self.items.is_empty() {
      return;
    }

    if down {
      self.selected = (self.selected + 1) % self.items.len();
    } else if self.selected == 0 {
      self.selected = self.items.len() - 1;
    } else {
      self.selected -= 1;
    }
    self.offset = self.offset.min(self.selected).max((self.selected + 1).saturating_sub(VISIBLE));
  }

  pub fn choose(&self, input: &mut Input, complete: bool) -> Res<Option<Command>> {
    let Some(item) = self.items.get(self.selected) else { return Ok(None) };
    match self.kind {
      Kind::Commands if !complete => Ok(Command::parse(item.name, self.models)),
      Kind::Models if !complete => {
        Ok(Some(Command::Model(self.models.iter().find(|model| model.name == item.name))))
      }
      Kind::Effort if !complete => Ok(Some(Command::Effort(Effort::parse(item.name)))),
      Kind::Commands | Kind::Models | Kind::Effort => {
        input.replace(self.range.clone(), &format!("{} ", item.name))?;
        Ok(None)
      }
      Kind::Skills => {
        input.mention(self.range.clone(), item.name)?;
        Ok(None)
      }
    }
  }
}

fn name_char(c: char) -> bool {
  c.is_alphanumeric() || matches!(c, '-' | '_')
}

fn code(text: &str) -> bool {
  let mut fence = 0;
  let mut chars = text.chars().peekable();
  while let Some(c) = chars.next() {
    if c == '\\' {
      chars.next();
    } else if c == '`' {
      let mut count = 1;
      while chars.peek() == Some(&'`') {
        chars.next();
        count += 1;
      }
      if fence == 0 {
        fence = count;
      } else if fence == count {
        fence = 0;
      }
    }
  }
  fence != 0
}

fn target(input: &Input) -> Option<(Kind, Range<usize>, String)> {
  let before = &input.text[..input.pos];
  for (prefix, kind) in [("/model ", Kind::Models), ("/effort ", Kind::Effort)] {
    if let Some(query) = before.strip_prefix(prefix)
      && !input.text.contains('\n')
    {
      return Some((kind, prefix.len()..input.text.len(), query.trim_start().to_lowercase()));
    }
  }
  if let Some(query) = before.strip_prefix('/')
    && input.text[1..].chars().all(name_char)
  {
    return Some((Kind::Commands, 0..input.text.len(), query.to_lowercase()));
  }
  let start = before
    .rfind(char::is_whitespace)
    .map_or(0, |i| i + before[i..].chars().next().unwrap().len_utf8());
  let query = before[start..].strip_prefix('$')?;
  if !query.chars().all(name_char) || code(&input.text[..start]) {
    return None;
  }
  let end = input.pos
    + input.text[input.pos..]
      .chars()
      .take_while(|&c| name_char(c))
      .map(char::len_utf8)
      .sum::<usize>();
  if input.skills.iter().any(|skill| start < skill.range.end && skill.range.start < end) {
    return None;
  }
  Some((Kind::Skills, start..end, query.to_lowercase()))
}

fn search(kind: Kind, query: &str, settings: Settings, models: &'static [Model]) -> Vec<Item> {
  let mut items: Vec<_> = kind
    .items(settings, models)
    .into_iter()
    .filter_map(|item| {
      let name = item.name.trim_start_matches('/');
      let mut chars = name.chars();
      let score = if name.starts_with(query) {
        0
      } else if name.contains(query) {
        1
      } else if query.chars().all(|c| chars.by_ref().any(|x| x == c)) {
        2
      } else if item.description.to_lowercase().contains(query) {
        3
      } else {
        return None;
      };
      Some((score, item))
    })
    .collect();
  items.sort_by_key(|&(score, _)| score);
  items.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::agent::model::MODELS;

  fn input(text: &str) -> Input {
    let mut input = Input::default();
    input.insert(text).unwrap();
    input
  }

  #[test]
  fn commands_and_skills_are_separate_and_only_the_current_token_is_completed() {
    let commands = Menu::open(&input("/"), Settings::default(), MODELS).unwrap();
    assert_eq!(
      commands.items.iter().map(|x| x.name).collect::<Vec<_>>(),
      ["/clear", "/quit", "/model", "/effort", "/fast", "/copy"]
    );
    let mut draft = input("до $age after");
    draft.pos = "до $age".len();
    let mut menu = Menu::open(&draft, Settings::default(), MODELS).unwrap();
    assert_eq!(menu.kind, Kind::Skills);
    assert!(menu.items.iter().all(|x| !x.name.starts_with('/')));
    menu.step(true);
    menu.choose(&mut draft, false).unwrap();
    assert_eq!(draft.text, "до $agent-rules after");
    assert_eq!(draft.skills[0].name, "agent-rules");
    assert_eq!(&draft.text[draft.skills[0].range.clone()], "$agent-rules");
    assert!(Menu::open(&draft, Settings::default(), MODELS).is_none());
  }

  #[test]
  fn literals_paths_code_and_other_tokens_do_not_open_a_picker() {
    for text in [
      "hello /",
      "/tmp/file",
      "/clear\n",
      "\\$age",
      "x$age",
      "`code $age",
      "```\n$age",
      "$age?",
      "$age text",
    ] {
      assert!(Menu::open(&input(text), Settings::default(), MODELS).is_none(), "{text}");
    }
    assert!(Menu::open(&input("`code` then $age"), Settings::default(), MODELS).is_some());
    assert_eq!(Command::parse("/clear ", MODELS), Some(Command::Clear));
    assert_eq!(Command::parse("/clear\n", MODELS), None);
  }

  #[test]
  fn navigation_wraps_in_both_directions_and_keeps_selection_visible() {
    for text in ["/", "$", "$age", "$rust", "$zzzz", "/model ", "/effort "] {
      let mut menu = Menu::open(&input(text), Settings::default(), MODELS).unwrap();
      let count = menu.items.len();
      let initial = menu.selected;
      for down in [true, false] {
        for step in 1..=2 * count.max(1) {
          menu.step(down);
          let expected = if count == 0 {
            0
          } else if down {
            (initial + step) % count
          } else {
            (initial + count - step % count) % count
          };
          assert_eq!(menu.selected, expected, "{text}, down={down}, step={step}");
          assert!(menu.offset <= menu.selected && menu.selected < menu.offset + VISIBLE);
        }
      }
    }
  }

  #[test]
  fn filtering_scrolls_and_empty_results_do_not_choose_a_command() {
    let fuzzy = Menu::open(&input("$aget"), Settings::default(), MODELS).unwrap();
    assert_eq!(fuzzy.items[0].name, "agent-intervention");
    assert_eq!(fuzzy.items[1].name, "agent-rules");
    let mut draft = input("$");
    let mut menu = Menu::open(&draft, Settings::default(), MODELS).unwrap();
    for _ in 1..SKILLS.len() {
      menu.step(true);
    }
    assert_eq!(menu.selected, SKILLS.len() - 1);
    assert_eq!(menu.offset, SKILLS.len() - VISIBLE);
    draft.insert("rust").unwrap();
    assert!(menu.refresh(&draft, Settings::default()));
    assert_eq!(menu.items.len(), 1);
    assert_eq!(menu.items[0].name, "code-style-rust");
    draft.insert("not-found").unwrap();
    assert!(menu.refresh(&draft, Settings::default()));
    let saved = draft.clone();
    assert!(menu.items.is_empty());
    assert_eq!(menu.choose(&mut draft, true).unwrap(), None);
    assert_eq!(draft, saved);
  }
}
