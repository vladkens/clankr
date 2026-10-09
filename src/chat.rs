use std::io::{self, IsTerminal, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{
  self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
  KeyModifiers as Mods, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
  PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{self, EndSynchronizedUpdate, disable_raw_mode, enable_raw_mode};

use crate::agent::{self, Backend, Echo, Phase, Prompt, Usage};
use crate::input::Input;
use crate::menu::{Command, Kind, Menu};
use crate::model::Settings;
use crate::view::{self, Frame, Live};
use crate::{Error, Res};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static ENHANCED: AtomicBool = AtomicBool::new(false);
static HOOK: Once = Once::new();

fn restore() {
  if ACTIVE.swap(false, Ordering::SeqCst) {
    restore_modes(&mut io::stdout(), ENHANCED.swap(false, Ordering::SeqCst), disable_raw_mode);
  }
}

fn restore_modes(out: &mut impl Write, enhanced: bool, disable: impl FnOnce() -> io::Result<()>) {
  if enhanced {
    let _ = execute!(out, PopKeyboardEnhancementFlags);
  }
  let _ = execute!(out, EndSynchronizedUpdate, DisableBracketedPaste);
  let _ = write!(out, "\x1b[0m\r\n");
  let _ = out.flush();
  let _ = disable();
}

struct Keyboard;

impl Keyboard {
  fn enter() -> Res<Self> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
      return Err(Error::NotTerminal);
    }
    HOOK.call_once(|| {
      let old = std::panic::take_hook();
      std::panic::set_hook(Box::new(move |info| {
        restore();
        old(info);
      }));
    });
    if ACTIVE.swap(true, Ordering::SeqCst) {
      return Err(Error::KeyboardActive);
    }
    let guard = Self;
    enable_raw_mode()?;
    // Unsupported terminals ignore the push; no query competes with the event reader.
    ENHANCED.store(true, Ordering::SeqCst);
    execute!(
      io::stdout(),
      PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
      EnableBracketedPaste
    )?;
    Ok(guard)
  }
}

impl Drop for Keyboard {
  fn drop(&mut self) {
    restore();
  }
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
  None,
  Send(Prompt),
  Clear,
  Copy,
  Cancel,
  Exit,
}

struct Request {
  response: String,
  failure: Option<Error>,
  phase: Phase,
  usage: Option<Usage>,
  started: Instant,
}

impl Default for Request {
  fn default() -> Self {
    Self {
      response: String::new(),
      failure: None,
      phase: Phase::Thinking,
      usage: None,
      started: Instant::now(),
    }
  }
}

impl Request {
  fn tokens(&self) -> String {
    self.usage.map_or_else(String::new, |usage| format!(" (↑{} ↓{})", usage.input, usage.output))
  }

  fn activity(&self) -> String {
    let elapsed = self.started.elapsed();
    let dot = if (elapsed.as_millis() / 500).is_multiple_of(2) { '•' } else { ' ' };
    let phase = if self.failure.is_some() {
      "Cancelling"
    } else {
      match self.phase {
        Phase::Thinking => "Thinking",
        Phase::Generating => "Generating",
      }
    };
    format!("{dot} {phase} ({}s){}", elapsed.as_secs(), self.tokens())
  }

  fn summary(&self, error: Option<&Error>) -> String {
    let stats = format!("{}s{}", self.started.elapsed().as_secs(), self.tokens());
    match error {
      Some(Error::Cancelled) => format!("Cancelled · {stats}"),
      Some(error) => format!("Error: {error}\n{stats}"),
      None => stats,
    }
  }
}

struct App {
  input: Input,
  notice: Option<String>,
  backend: Box<dyn Backend>,
  request: Option<Request>,
  has_session: bool,
  exit_deadline: Option<Instant>,
  draft_cleared: bool,
  menu: Option<Menu>,
  settings: Settings,
  last_response: Option<String>,
  messages: Vec<String>,
}

impl Default for App {
  fn default() -> Self {
    Self::new(Box::new(Echo::default()))
  }
}

impl App {
  fn new(backend: Box<dyn Backend>) -> Self {
    Self {
      input: Input::default(),
      notice: None,
      backend,
      request: None,
      has_session: false,
      exit_deadline: None,
      draft_cleared: false,
      menu: None,
      settings: Settings::default(),
      last_response: None,
      messages: Vec::new(),
    }
  }

  fn frame(&self) -> Frame<'_> {
    let response = self.request.as_ref().map_or("", |request| request.response.as_str());
    let activity = self.request.as_ref().map(Request::activity);
    let status = self.backend.status();
    let mut label = format!("{} · {}", status.mode, self.settings.model.name);
    if let Some(effort) = self.settings.effort {
      label.push_str(&format!(" · {}", effort.name()));
    }
    if self.settings.fast {
      label.push_str(" · ⚡︎");
    }
    if let Some((used, limit)) = status.context {
      let compact = |n: u64| {
        if n < 1000 { n.to_string() } else { format!("{}k", n / 1000) }
      };
      let percent = used.saturating_mul(100).checked_div(limit).unwrap_or(0);
      label.push_str(&format!(" · {}/{} {percent}%", compact(used), compact(limit)));
    }
    Frame {
      input: &self.input,
      response,
      activity,
      status: label,
      notice: self.notice.as_deref(),
      menu: self.menu.as_ref(),
      messages: &self.messages,
    }
  }

  fn start(&mut self, prompt: Prompt, live: &mut Live, out: &mut impl Write, cols: u16) -> Res<()> {
    live.prompt(out, &prompt.text, cols)?;
    self.request = Some(Request::default());
    self.has_session = true;
    self.backend.start(prompt);
    Ok(())
  }

  fn clear(&mut self, live: &mut Live, out: &mut impl Write, rows: u16) -> Res<()> {
    self.backend.reset();
    self.request = None;
    self.has_session = false;
    self.last_response = None;
    self.messages.clear();
    self.input = Input::default();
    self.menu = None;
    self.notice = None;
    self.exit_deadline = None;
    self.draft_cleared = false;
    live.restart(out, rows)
  }

  fn command(&mut self, command: Command) -> Res<Action> {
    self.menu = None;
    match command {
      Command::Clear => return Ok(Action::Clear),
      Command::Quit => return Ok(Action::Exit),
      Command::Fast => {
        self.input = Input::default();
        if self.settings.model.fast {
          self.settings.fast = !self.settings.fast;
        } else {
          self.notice = Some(format!("Fast mode is unavailable for {}", self.settings.model.name));
        }
      }
      Command::Copy => {
        self.input = Input::default();
        return Ok(Action::Copy);
      }
      Command::Model(Some(model)) => {
        self.settings.select(model);
        self.input = Input::default();
      }
      Command::Effort(Some(effort)) if self.settings.model.efforts.contains(&effort) => {
        self.settings.effort = Some(effort);
        self.input = Input::default();
      }
      Command::Model(_) | Command::Effort(_) => {
        let prefix = if matches!(command, Command::Model(_)) { "/model " } else { "/effort " };
        if !self.input.text.starts_with(prefix) {
          self.input = Input::default();
          self.input.insert(prefix)?;
        }
        self.menu = Menu::open(&self.input, self.settings);
      }
    }
    Ok(Action::None)
  }

  fn copy(&mut self, copy: impl FnOnce(&str) -> Res<()>) {
    let text = self
      .request
      .as_ref()
      .map(|request| request.response.as_str())
      .filter(|text| !text.is_empty())
      .or(self.last_response.as_deref());
    let message = match text {
      Some(text) => match copy(text) {
        Ok(()) => "Copied to clipboard.".into(),
        Err(error) => error.to_string(),
      },
      None => "No assistant reply to copy.".into(),
    };
    self.messages.push(message);
  }

  fn flush_messages(&mut self, live: &mut Live, out: &mut impl Write, cols: u16) -> Res<bool> {
    if self.request.is_some() || self.messages.is_empty() {
      return Ok(false);
    }

    for message in &self.messages {
      live.message(out, message, cols)?;
    }
    self.messages.clear();
    Ok(true)
  }

  fn cancel(&mut self) {
    if let Some(request) = &mut self.request
      && request.failure.is_none()
    {
      request.failure = Some(Error::Cancelled);
      self.backend.cancel();
    }
  }

  fn poll(&mut self, live: &mut Live, out: &mut impl Write, cols: u16) -> Res<bool> {
    let mut changed = false;
    // Bound each batch so a fast producer cannot starve keyboard input.
    for _ in 0..64 {
      let Some(event) = self.backend.poll() else { break };
      let Some(request) = &mut self.request else { continue };
      changed = true;
      match event {
        agent::Event::Phase(phase) => request.phase = phase,
        agent::Event::Usage(usage) => request.usage = Some(usage),
        agent::Event::Text(text) if request.failure.is_none() => {
          if text.len() > agent::LIMIT.saturating_sub(request.response.len()) {
            request.failure = Some(Error::ResponseTooLarge);
            self.backend.cancel();
          } else {
            request.response.push_str(&text);
            request.phase = Phase::Generating;
          }
        }
        agent::Event::Text(_) => {}
        agent::Event::Finished(result) => {
          let mut request = self.request.take().unwrap();
          let error = request.failure.take().or_else(|| result.err());
          live.finish(out, &request.response, Some(&request.summary(error.as_ref())), cols)?;
          if !request.response.is_empty() {
            self.last_response = Some(request.response);
          }
        }
      }
    }
    Ok(self.flush_messages(live, out, cols)? || changed)
  }

  fn stop(&mut self, live: &mut Live, out: &mut impl Write, cols: u16) -> Res<()> {
    self.cancel();
    if let Some(request) = self.request.take() {
      let summary = request.summary(request.failure.as_ref());
      live.finish(out, &request.response, Some(&summary), cols)?;
    }
    self.flush_messages(live, out, cols)?;
    Ok(())
  }

  fn event(&mut self, event: Event, cols: u16) -> Res<Action> {
    if matches!(event, Event::Resize(..))
      || matches!(event, Event::Key(key) if key.kind == KeyEventKind::Release)
    {
      return Ok(Action::None);
    }
    if let Event::Key(key) = event
      && key.modifiers.contains(Mods::CONTROL)
      && key.code == KeyCode::Char('c')
    {
      self.menu = None;
      if self.request.is_some() {
        self.notice = None;
        self.exit_deadline = None;
        self.draft_cleared = false;
        return Ok(Action::Cancel);
      }
      if !self.input.text.is_empty() {
        self.input = Input::default();
        self.notice = Some("press ctrl+c again to exit".into());
        self.exit_deadline = None;
        self.draft_cleared = true;
        return Ok(Action::None);
      }
      let now = Instant::now();
      if !self.has_session
        || self.draft_cleared
        || self.exit_deadline.is_some_and(|deadline| now < deadline)
      {
        return Ok(Action::Exit);
      }
      self.exit_deadline = Some(now + Duration::from_secs(3));
      self.notice = Some("press ctrl+c again to exit".into());
      return Ok(Action::None);
    }
    self.exit_deadline = None;
    self.draft_cleared = false;
    self.notice = None;
    match event {
      Event::Key(key) => {
        if key.modifiers == Mods::CONTROL && matches!(key.code, KeyCode::Char(',' | '.')) {
          self.settings.step(key.code == KeyCode::Char('.'));
          if let Some(menu) = &mut self.menu
            && menu.kind == Kind::Effort
          {
            menu.refresh(&self.input, self.settings);
          }
          return Ok(Action::None);
        }
        if key.modifiers.contains(Mods::CONTROL)
          && key.code == KeyCode::Char('d')
          && self.input.text.is_empty()
        {
          return Ok(Action::Exit);
        }
        if let Some(menu) = &mut self.menu
          && !key.modifiers.intersects(Mods::SHIFT | Mods::ALT | Mods::CONTROL | Mods::SUPER)
        {
          match key.code {
            KeyCode::Up | KeyCode::Down => {
              menu.step(key.code == KeyCode::Down);
              return Ok(Action::None);
            }
            KeyCode::Esc => {
              self.menu = None;
              return Ok(Action::None);
            }
            KeyCode::Enter | KeyCode::Tab | KeyCode::Right => {
              if menu.items.is_empty() {
                return Ok(Action::None);
              }
              let kind = menu.kind;
              let command = menu.choose(&mut self.input, key.code != KeyCode::Enter)?;
              self.menu = None;
              if let Some(command) = command {
                return self.command(command);
              }
              if kind == Kind::Commands {
                self.menu = Menu::open(&self.input, self.settings);
              }
              return Ok(Action::None);
            }
            _ => {}
          }
        }
        if key.code == KeyCode::Enter && !key.modifiers.intersects(Mods::SHIFT | Mods::ALT) {
          if let Some(command) = Command::parse(&self.input.text) {
            return self.command(command);
          }
          if self.request.is_none() && !self.input.text.trim().is_empty() {
            let input = std::mem::take(&mut self.input);
            let mut skills = Vec::new();
            for skill in input.skills {
              if !skills.contains(&skill.name) {
                skills.push(skill.name);
              }
            }
            self.menu = None;
            return Ok(Action::Send(Prompt { text: input.text, skills, settings: self.settings }));
          }
        } else if key.code == KeyCode::Esc && self.request.is_some() {
          return Ok(Action::Cancel);
        } else {
          self.input.edit(key, cols)?;
          if let Some(menu) = &mut self.menu {
            if !menu.refresh(&self.input, self.settings) {
              self.menu = Menu::open(&self.input, self.settings);
            }
          } else if matches!(key.code, KeyCode::Char('/' | '$' | ' '))
            && !key
              .modifiers
              .intersects(Mods::CONTROL | Mods::ALT | Mods::SUPER | Mods::HYPER | Mods::META)
          {
            self.menu = Menu::open(&self.input, self.settings);
          }
        }
      }
      Event::Paste(text) => {
        self.input.insert(&text)?;
        self.menu = None;
      }
      _ => {}
    }
    Ok(Action::None)
  }
}

pub fn run() -> Res<()> {
  let _keyboard = Keyboard::enter()?;
  let mut out = io::stdout();
  let mut app = App::default();
  let mut clipboard = None;
  let mut live = Live::terminal();
  view::header(&mut out)?;
  let mut size = (0, 0);
  let result = (|| -> Res<()> {
    let mut dirty = true;
    let mut tick = None;
    loop {
      if app.exit_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        app.exit_deadline = None;
        app.notice = None;
        dirty = true;
      }
      let (cols, rows) = terminal::size()?;
      let next = (cols.max(1), rows.max(1));
      if next != size {
        size = next;
        live.resize();
        // Relocate the live tail before completion can freeze its remaining text.
        live.draw(&mut out, &app.frame(), size.0, size.1)?;
        dirty = true;
      }
      let changed = app.poll(&mut live, &mut out, size.0)?;
      let next_tick =
        app.request.as_ref().map(|request| request.started.elapsed().as_millis() / 500);
      dirty |= tick != next_tick;
      tick = next_tick;
      if dirty || changed {
        live.draw(&mut out, &app.frame(), size.0, size.1)?;
      }
      dirty = false;
      if !event::poll(Duration::from_millis(25))? {
        continue;
      }
      let event = event::read()?;
      // Read current dimensions even if a resize notification and a key were coalesced.
      let (next_cols, next_rows) = terminal::size()?;
      let next = (next_cols.max(1), next_rows.max(1));
      if next != size || matches!(event, Event::Resize(..)) {
        size = next;
        live.resize();
        live.draw(&mut out, &app.frame(), size.0, size.1)?;
      }
      match app.event(event, size.0) {
        Ok(Action::Exit) => return Ok(()),
        Ok(Action::Send(text)) => app.start(text, &mut live, &mut out, size.0)?,
        Ok(Action::Cancel) => app.cancel(),
        Ok(Action::Clear) => app.clear(&mut live, &mut out, size.1)?,
        Ok(Action::Copy) => app.copy(|text| {
          let clipboard = match &mut clipboard {
            Some(clipboard) => clipboard,
            None => clipboard.insert(arboard::Clipboard::new()?),
          };
          clipboard.set_text(text)?;
          Ok(())
        }),
        Ok(Action::None) => {}
        Err(error @ Error::InputTooLarge) => app.notice = Some(error.to_string()),
        Err(error) => return Err(error),
      }
      dirty = true;
    }
  })();
  let stopped = app.stop(&mut live, &mut out, size.0.max(1));
  let cleared = live.clear(&mut out);
  result.and(stopped).and(cleared)?;
  view::separator(&mut out, size.0.max(1))
}

#[cfg(test)]
mod tests {
  use std::cell::RefCell;
  use std::collections::VecDeque;
  use std::rc::Rc;

  use crossterm::event::KeyEvent;

  use super::*;

  fn key(code: KeyCode, modifiers: Mods) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
  }

  fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
      app.event(key(KeyCode::Char(c), Mods::NONE), 80).unwrap();
    }
  }

  #[test]
  fn fast_toggles_the_status_and_next_request_without_changing_an_active_request() {
    let peer = Rc::new(RefCell::new(Peer::default()));
    let mut app = App::new(Box::new(Script(peer.clone())));
    app.start("active".into(), &mut Live::default(), &mut Vec::new(), 80).unwrap();
    for fast in [false, true] {
      type_text(&mut app, "/fast");
      app.event(key(KeyCode::Right, Mods::NONE), 80).unwrap();
      assert_eq!(app.settings.fast, !fast);
      assert_eq!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::None);
      assert_eq!(app.settings.fast, fast);
      assert_eq!(app.frame().status.contains('⚡'), fast);
      assert!(app.input.text.is_empty() && app.menu.is_none());
      assert!(peer.borrow().prompts[0].settings.fast);
    }
    type_text(&mut app, "/model sonnet");
    app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap();
    type_text(&mut app, "/fast");
    app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap();
    assert!(!app.settings.fast);
    assert_eq!(app.notice.as_deref(), Some("Fast mode is unavailable for sonnet"));
    assert!(app.request.is_some());
  }

  #[test]
  fn copy_uses_the_latest_response_text_handles_failure_and_resets_on_clear() {
    let peer = Rc::new(RefCell::new(Peer::default()));
    let mut app = App::new(Box::new(Script(peer.clone())));
    let mut live = Live::default();
    let mut out = Vec::new();
    app.copy(|_| panic!("Empty conversation must not touch the clipboard"));
    assert_eq!(app.messages, ["No assistant reply to copy."]);
    assert!(app.notice.is_none());
    app.poll(&mut live, &mut out, 80).unwrap();
    assert!(app.messages.is_empty());

    app.start("question".into(), &mut live, &mut out, 80).unwrap();
    let answer = "# Ответ\n\n```rust\nlet x = 1;\n```\n界 👩‍💻";
    peer.borrow_mut().events.extend([
      agent::Event::Text(answer.into()),
      agent::Event::Usage(Usage { input: 3, output: 20 }),
      agent::Event::Finished(Ok(())),
    ]);
    app.poll(&mut live, &mut out, 80).unwrap();
    type_text(&mut app, "/copy");
    app.event(key(KeyCode::Tab, Mods::NONE), 80).unwrap();
    assert_eq!(app.input.text, "/copy ");
    assert_eq!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::Copy);
    app.copy(|text| {
      assert_eq!(text, answer);
      Ok(())
    });
    assert_eq!(app.messages, ["Copied to clipboard."]);
    assert!(app.notice.is_none());
    app.poll(&mut live, &mut out, 80).unwrap();
    assert!(app.messages.is_empty());
    assert!(app.input.text.is_empty());

    app.start("next question".into(), &mut live, &mut out, 80).unwrap();
    app.copy(|text| {
      assert_eq!(text, answer);
      Ok(())
    });
    peer.borrow_mut().events.push_back(agent::Event::Text("new partial answer".into()));
    app.poll(&mut live, &mut out, 80).unwrap();
    app.copy(|text| {
      assert_eq!(text, "new partial answer");
      Err(arboard::Error::ClipboardNotSupported.into())
    });
    assert_eq!(app.messages.last().unwrap(), &arboard::Error::ClipboardNotSupported.to_string());
    assert!(app.notice.is_none());
    assert!(app.request.is_some());
    peer.borrow_mut().events.push_back(agent::Event::Finished(Err(Error::Cancelled)));
    app.poll(&mut live, &mut out, 80).unwrap();
    app.copy(|text| {
      assert_eq!(text, "new partial answer");
      Ok(())
    });
    assert_eq!(app.messages, ["Copied to clipboard."]);
    assert!(app.notice.is_none());
    app.clear(&mut live, &mut out, 24).unwrap();
    app.copy(|_| panic!("Cleared conversation must not copy old output"));
    assert_eq!(app.messages, ["No assistant reply to copy."]);
    assert!(app.notice.is_none());
  }

  #[test]
  fn model_and_effort_pickers_apply_independently_and_never_send_commands_as_prompts() {
    use crate::model::Effort;

    let mut app = App::default();
    app.event(key(KeyCode::Char('.'), Mods::CONTROL), 80).unwrap();
    type_text(&mut app, "/model");
    app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap();
    assert_eq!(app.menu.as_ref().unwrap().kind, Kind::Models);
    assert_eq!(app.menu.as_ref().unwrap().current(), Some("echo"));
    type_text(&mut app, "gpt-6.1-sol");
    assert_eq!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::None);
    assert_eq!(app.settings.model.name, "gpt-6.1-sol");
    assert_eq!(app.settings.effort, Some(Effort::High));
    assert!(app.settings.fast && app.menu.is_none() && app.request.is_none());
    assert!(app.input.text.is_empty());
    assert!(app.frame().status.contains("gpt-6.1-sol · high · ⚡"));

    type_text(&mut app, "/effort");
    app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap();
    let menu = app.menu.as_ref().unwrap();
    assert_eq!(menu.kind, Kind::Effort);
    assert_eq!(menu.items[menu.selected].name, "high");
    app.event(key(KeyCode::Down, Mods::NONE), 80).unwrap();
    assert_eq!(app.settings.effort, Some(Effort::High));
    app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap();
    assert_eq!(app.settings.effort, Some(Effort::Xhigh));
    assert_eq!(app.settings.model.name, "gpt-6.1-sol");
    assert!(app.menu.is_none() && app.input.text.is_empty());

    type_text(&mut app, "/model sonnet");
    app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap();
    assert_eq!(app.settings.effort, Some(Effort::Medium));
    assert!(!app.settings.fast);
    type_text(&mut app, "/model haiku");
    app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap();
    assert_eq!(app.settings.effort, None);
    assert_eq!(app.frame().status, "auto · haiku");
    type_text(&mut app, "/effort");
    app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap();
    assert!(app.menu.as_ref().unwrap().items.is_empty());
    app.event(key(KeyCode::Char('.'), Mods::CONTROL), 80).unwrap();
    assert_eq!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::None);
    assert_eq!(app.settings.effort, None);
    app.event(key(KeyCode::Esc, Mods::NONE), 80).unwrap();
    app.input = Input::default();

    for text in ["/model unknown", "/effort unknown", "/effort high"] {
      app.input = Input::default();
      app.event(Event::Paste(text.into()), 80).unwrap();
      assert_eq!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::None);
      assert!(app.menu.as_ref().unwrap().items.is_empty());
      assert_eq!(app.input.text, text);
    }
    assert!(!app.has_session);
  }

  #[test]
  fn setting_completion_and_escape_do_not_apply_the_highlighted_option() {
    for (command, query) in [("/model", "sonnet"), ("/effort", "low")] {
      for completion in [KeyCode::Tab, KeyCode::Right] {
        let mut app = App::default();
        let original = app.settings;
        type_text(&mut app, command);
        app.event(key(completion, Mods::NONE), 80).unwrap();
        type_text(&mut app, query);
        app.event(key(completion, Mods::NONE), 80).unwrap();
        assert!(app.menu.is_none());
        assert_eq!(app.input.text, format!("{command} {query} "));
        assert_eq!(app.settings, original);
        assert_eq!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::None);
        assert_ne!(app.settings, original);
      }
      let mut app = App::default();
      let original = app.settings;
      type_text(&mut app, &format!("{command} {query}"));
      assert_eq!(app.event(key(KeyCode::Esc, Mods::NONE), 80).unwrap(), Action::None);
      assert!(app.menu.is_none());
      assert_eq!(app.settings, original);
      assert_eq!(app.input.text, format!("{command} {query}"));
    }
  }

  #[test]
  fn hotkeys_preserve_the_editor_and_new_settings_do_not_change_an_active_request() {
    use crate::model::Effort;

    let peer = Rc::new(RefCell::new(Peer::default()));
    let mut app = App::new(Box::new(Script(peer.clone())));
    let mut live = Live::default();
    let mut out = Vec::new();
    type_text(&mut app, "first");
    let Action::Send(prompt) = app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap() else {
      panic!()
    };
    app.start(prompt, &mut live, &mut out, 80).unwrap();
    type_text(&mut app, "next $age");
    app.event(key(KeyCode::Down, Mods::NONE), 80).unwrap();
    app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap();
    app.event(key(KeyCode::Left, Mods::NONE), 80).unwrap();
    let draft = app.input.clone();
    app.event(key(KeyCode::Char('.'), Mods::CONTROL), 80).unwrap();
    assert_eq!(app.settings.effort, Some(Effort::High));
    assert_eq!(app.input, draft);
    let mut release = KeyEvent::new(KeyCode::Char('.'), Mods::CONTROL);
    release.kind = KeyEventKind::Release;
    app.event(Event::Key(release), 80).unwrap();
    assert_eq!(app.settings.effort, Some(Effort::High));
    app.event(key(KeyCode::Char(','), Mods::CONTROL), 80).unwrap();
    assert_eq!(app.settings.effort, Some(Effort::Medium));
    assert_eq!(app.input, draft);

    app.input = Input::default();
    type_text(&mut app, "/model opus");
    app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap();
    type_text(&mut app, "/effort ");
    app.event(key(KeyCode::Char('.'), Mods::CONTROL), 80).unwrap();
    let menu = app.menu.as_ref().unwrap();
    assert_eq!(menu.current(), Some("high"));
    assert_eq!(menu.items[menu.selected].name, "high");
    assert_eq!(peer.borrow().prompts[0].settings, Settings::default());
    assert!(app.request.is_some());
    peer.borrow_mut().events.push_back(agent::Event::Finished(Ok(())));
    app.poll(&mut live, &mut out, 80).unwrap();
    app.input = draft;
    app.menu = None;
    let Action::Send(prompt) = app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap() else {
      panic!()
    };
    assert_eq!(prompt.settings.model.name, "opus");
    assert_eq!(prompt.settings.effort, Some(Effort::High));
    assert_eq!(prompt.skills, ["agent-rules"]);
    app.start(prompt, &mut live, &mut out, 80).unwrap();
    let settings = app.settings;
    app.clear(&mut live, &mut out, 24).unwrap();
    assert_eq!(app.settings, settings);
    assert_eq!(peer.borrow().prompts[1].settings, settings);
  }

  #[test]
  fn menus_complete_without_sending_and_capture_only_picked_skills() {
    let mut app = App::default();
    type_text(&mut app, "/cl");
    assert_eq!(app.menu.as_ref().unwrap().items[0].name, "/clear");
    assert_eq!(app.event(key(KeyCode::Tab, Mods::NONE), 80).unwrap(), Action::None);
    assert_eq!(app.input.text, "/clear ");
    assert!(app.menu.is_none());
    assert_eq!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::Clear);

    app.input = Input::default();
    type_text(&mut app, "huj! $age");
    app.event(key(KeyCode::Down, Mods::NONE), 80).unwrap();
    assert_eq!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::None);
    assert_eq!(app.input.text, "huj! $agent-rules ");
    assert!(app.menu.is_none() && app.request.is_none());
    type_text(&mut app, "yes");
    let Action::Send(prompt) = app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap() else {
      panic!()
    };
    assert_eq!(prompt.text, "huj! $agent-rules yes");
    assert_eq!(prompt.skills, ["agent-rules"]);
    let peer = Rc::new(RefCell::new(Peer::default()));
    app.backend = Box::new(Script(peer.clone()));
    app.start(prompt, &mut Live::default(), &mut Vec::new(), 80).unwrap();
    app.event(Event::Paste("copy paste $agent-rules? 1221e".into()), 80).unwrap();
    assert!(app.menu.is_none() && app.input.skills.is_empty());
    assert_eq!(peer.borrow().prompts[0].skills, ["agent-rules"]);
    assert_eq!(peer.borrow().prompts[0].text, "huj! $agent-rules yes");
  }

  #[test]
  fn right_completes_the_selected_menu_item_and_left_moves_the_cursor() {
    for (draft, completed) in [("/q", "/quit "), ("text $age", "text $agent-rules ")] {
      let mut app = App::default();
      type_text(&mut app, draft);
      assert_eq!(app.event(key(KeyCode::Left, Mods::NONE), 80).unwrap(), Action::None);
      assert_eq!(app.input.text, draft);
      assert_eq!(app.input.pos, draft.len() - 1);
      assert!(app.menu.is_some());
      app.event(key(KeyCode::Down, Mods::NONE), 80).unwrap();
      assert_eq!(app.event(key(KeyCode::Right, Mods::NONE), 80).unwrap(), Action::None);
      assert_eq!(app.input.text, completed);
      assert_eq!(app.input.pos, completed.len());
      assert!(app.menu.is_none() && app.request.is_none());
      if draft.contains('$') {
        assert_eq!(app.input.skills[0].name, "agent-rules");
      }

      assert_eq!(app.event(key(KeyCode::Left, Mods::NONE), 80).unwrap(), Action::None);
      assert_eq!(app.input.pos, completed.len() - 1);
      assert_eq!(app.input.text, completed);
      assert_eq!(app.event(key(KeyCode::Right, Mods::NONE), 80).unwrap(), Action::None);
      assert_eq!(app.input.pos, completed.len());
      assert_eq!(app.input.text, completed);
    }
  }

  #[test]
  fn esc_closes_only_the_menu_and_ctrl_c_cancels_without_an_exit_confirmation() {
    let mut app = App::default();
    let mut live = Live::default();
    let mut out = Vec::new();
    app.start("active".into(), &mut live, &mut out, 80).unwrap();
    type_text(&mut app, "$age");
    assert_eq!(app.event(key(KeyCode::Esc, Mods::NONE), 80).unwrap(), Action::None);
    assert!(app.menu.is_none());
    assert_eq!(app.input.text, "$age");
    assert_eq!(app.event(key(KeyCode::Esc, Mods::NONE), 80).unwrap(), Action::Cancel);
    for _ in 0..2 {
      assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 80).unwrap(), Action::Cancel);
      app.cancel();
      assert!(app.exit_deadline.is_none() && app.notice.is_none());
    }
    app.poll(&mut live, &mut out, 80).unwrap();
    assert!(app.request.is_none());
    assert_eq!(app.input.text, "$age");
    assert!(!String::from_utf8(out).unwrap().contains("exit"));
    assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 80).unwrap(), Action::None);
    assert_eq!(app.input, Input::default());
    assert_eq!(app.notice.as_deref(), Some("press ctrl+c again to exit"));
    assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 80).unwrap(), Action::Exit);
  }

  #[test]
  fn ctrl_c_exits_immediately_before_the_first_prompt() {
    let mut app = App::default();
    assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 80).unwrap(), Action::Exit);
    assert!(app.notice.is_none() && app.exit_deadline.is_none());
  }

  #[test]
  fn ctrl_c_clears_the_whole_draft_before_exiting() {
    for has_session in [false, true] {
      for draft in ["unsent draft", " \n ", "first 界\ne\u{301} 👩‍💻", "/", "$"] {
        let mut app = App { has_session, ..App::default() };
        app.input.insert(draft).unwrap();
        app.menu = Menu::open(&app.input, app.settings);
        assert_eq!(app.menu.is_some(), matches!(draft, "/" | "$"));
        assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 80).unwrap(), Action::None);
        assert_eq!(app.input, Input::default());
        assert!(app.menu.is_none() && app.exit_deadline.is_none());
        assert_eq!(app.notice.as_deref(), Some("press ctrl+c again to exit"));

        app.event(Event::Resize(40, 12), 40).unwrap();
        let mut release = KeyEvent::new(KeyCode::Char('c'), Mods::CONTROL);
        release.kind = KeyEventKind::Release;
        assert_eq!(app.event(Event::Key(release), 40).unwrap(), Action::None);
        assert_eq!(app.notice.as_deref(), Some("press ctrl+c again to exit"));
        assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 40).unwrap(), Action::Exit);
      }
    }

    let mut app = App::default();
    type_text(&mut app, "$age");
    app.event(key(KeyCode::Right, Mods::NONE), 80).unwrap();
    assert!(!app.input.skills.is_empty());
    assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 80).unwrap(), Action::None);
    assert_eq!(app.input, Input::default());
    type_text(&mut app, "new draft");
    assert!(app.notice.is_none());
    assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 80).unwrap(), Action::None);
    assert_eq!(app.input, Input::default());
    assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 80).unwrap(), Action::Exit);
  }

  #[test]
  fn clearing_cancels_old_events_and_resets_the_draft_and_mock_context() {
    let peer = Rc::new(RefCell::new(Peer::default()));
    let mut app = App::new(Box::new(Script(peer.clone())));
    let mut live = Live::default();
    let mut out = Vec::new();
    app.start("old turn".into(), &mut live, &mut out, 80).unwrap();
    peer
      .borrow_mut()
      .events
      .extend([agent::Event::Text("late old answer".into()), agent::Event::Finished(Ok(()))]);
    type_text(&mut app, "/cl");
    assert_eq!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::Clear);
    app.clear(&mut live, &mut out, 24).unwrap();
    assert!(!app.poll(&mut live, &mut out, 80).unwrap());
    assert!(app.request.is_none() && app.menu.is_none());
    assert_eq!(app.input, Input::default());
    assert_eq!(peer.borrow().cancels, 1);
    assert!(!String::from_utf8(out).unwrap().contains("late old answer"));
    assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 80).unwrap(), Action::Exit);
    assert!(app.notice.is_none() && app.exit_deadline.is_none());
  }

  #[derive(Default)]
  struct Peer {
    prompts: Vec<Prompt>,
    events: VecDeque<agent::Event>,
    cancels: usize,
  }

  struct Script(Rc<RefCell<Peer>>);

  impl Backend for Script {
    fn status(&self) -> agent::Status<'_> {
      agent::Status { mode: "auto", context: None }
    }
    fn start(&mut self, prompt: Prompt) {
      self.0.borrow_mut().prompts.push(prompt);
    }
    fn poll(&mut self) -> Option<agent::Event> {
      self.0.borrow_mut().events.pop_front()
    }
    fn cancel(&mut self) {
      self.0.borrow_mut().cancels += 1;
    }
    fn reset(&mut self) {
      let mut peer = self.0.borrow_mut();
      peer.cancels += 1;
      peer.events.clear();
    }
  }

  #[test]
  fn streaming_and_failure_keep_the_next_draft_and_the_last_queued_text() {
    let peer = Rc::new(RefCell::new(Peer::default()));
    let mut app = App::new(Box::new(Script(peer.clone())));
    let mut live = Live::default();
    let mut out = Vec::new();
    app.input.insert("original\nquestion").unwrap();
    let Action::Send(text) = app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap() else {
      panic!()
    };
    app.start(text, &mut live, &mut out, 80).unwrap();
    assert_eq!(app.frame().activity.as_deref(), Some("• Thinking (0s)"));
    live.draw(&mut out, &app.frame(), 80, 24).unwrap();
    app.event(Event::Paste("next 界\ndraft".into()), 80).unwrap();
    app.event(key(KeyCode::Left, Mods::NONE), 80).unwrap();
    let draft = app.input.clone();
    assert_eq!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::None);
    assert_eq!(peer.borrow().prompts, [Prompt::from("original\nquestion")]);
    peer.borrow_mut().events.extend([
      agent::Event::Phase(Phase::Generating),
      agent::Event::Usage(Usage { input: 13, output: 1 }),
      agent::Event::Text("first ".into()),
    ]);
    app.poll(&mut live, &mut out, 80).unwrap();
    live.draw(&mut out, &app.frame(), 80, 24).unwrap();
    assert_eq!(app.frame().response, "first ");
    assert_eq!(app.frame().activity.as_deref(), Some("• Generating (0s) (↑13 ↓1)"));
    peer.borrow_mut().events.extend([
      agent::Event::Text("last".into()),
      agent::Event::Usage(Usage { input: 13, output: 2 }),
      agent::Event::Finished(Err(
        io::Error::new(io::ErrorKind::ConnectionReset, "connection lost").into(),
      )),
    ]);
    out.clear();
    app.poll(&mut live, &mut out, 80).unwrap();
    live.draw(&mut out, &app.frame(), 80, 24).unwrap();
    let output = String::from_utf8(out).unwrap();
    assert_eq!(app.last_response.as_deref(), Some("first last"));
    assert!(output.contains(" last") && !output.contains("first"));
    assert!(output.contains("Error: connection lost"));
    assert!(output.contains("(↑13 ↓2)"));
    assert!(app.request.is_none());
    assert_eq!(app.input, draft);
    assert!(
      matches!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::Send(prompt) if prompt.text == draft.text)
    );
  }

  #[test]
  fn cancellation_waits_for_completion_without_sending_or_losing_the_next_draft() {
    let peer = Rc::new(RefCell::new(Peer::default()));
    let mut app = App::new(Box::new(Script(peer.clone())));
    let mut live = Live::default();
    let mut out = Vec::new();
    app.start("question".into(), &mut live, &mut out, 80).unwrap();
    peer.borrow_mut().events.push_back(agent::Event::Text("kept".into()));
    app.poll(&mut live, &mut out, 80).unwrap();
    live.draw(&mut out, &app.frame(), 80, 24).unwrap();
    app.input.insert("next").unwrap();
    assert_eq!(app.event(key(KeyCode::Esc, Mods::NONE), 80).unwrap(), Action::Cancel);
    app.cancel();
    live.draw(&mut out, &app.frame(), 80, 24).unwrap();
    assert_eq!(app.frame().activity.as_deref(), Some("• Cancelling (0s)"));
    assert_eq!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::None);
    assert_eq!(peer.borrow().cancels, 1);
    peer.borrow_mut().events.extend([
      agent::Event::Text("discarded after cancel".into()),
      agent::Event::Finished(Ok(())),
    ]);
    app.poll(&mut live, &mut out, 80).unwrap();
    let output = String::from_utf8(out).unwrap();
    assert_eq!(app.last_response.as_deref(), Some("kept"));
    assert!(output.contains("Cancelled"));
    assert!(!output.contains("discarded"));
    assert!(app.request.is_none());
    assert_eq!(app.input.text, "next");
    let mut out = Vec::new();
    app.start("another".into(), &mut live, &mut out, 80).unwrap();
    app.stop(&mut live, &mut out, 80).unwrap();
    assert!(app.request.is_none());
    assert_eq!(peer.borrow().cancels, 2);
  }

  #[test]
  fn event_batches_allow_editing_and_oversized_responses_stop_without_losing_the_draft() {
    let peer = Rc::new(RefCell::new(Peer::default()));
    let mut app = App::new(Box::new(Script(peer.clone())));
    let mut live = Live::default();
    let mut out = Vec::new();
    app.start("question".into(), &mut live, &mut out, 80).unwrap();
    peer.borrow_mut().events.extend((0..1000).map(|_| agent::Event::Text("x".into())));
    app.poll(&mut live, &mut out, 80).unwrap();
    assert!(!peer.borrow().events.is_empty());
    app.event(Event::Paste("draft".into()), 80).unwrap();
    peer.borrow_mut().events = VecDeque::from([
      agent::Event::Text("x".repeat(agent::LIMIT + 1)),
      agent::Event::Finished(Ok(())),
    ]);
    app.poll(&mut live, &mut out, 80).unwrap();
    assert!(app.request.is_none());
    assert_eq!(app.input.text, "draft");
    assert_eq!(peer.borrow().cancels, 1);
    assert!(String::from_utf8(out).unwrap().contains("Response exceeds 16 MiB"));
  }

  #[test]
  fn sending_captures_the_exact_message_and_starts_a_new_draft() {
    let mut app = App::default();
    assert_eq!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::None);
    let text = " first 界\ne\u{301} 👩‍💻\n";
    app.event(Event::Paste(text.into()), 80).unwrap();
    assert_eq!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::Send(text.into()));
    assert_eq!(app.input, Input::default());
    app.event(Event::Paste("next".into()), 80).unwrap();
    for modifier in [Mods::SHIFT, Mods::ALT] {
      assert_eq!(app.event(key(KeyCode::Enter, modifier), 80).unwrap(), Action::None);
    }
    assert_eq!(app.input.text, "next\n\n");
  }

  #[test]
  fn release_events_and_ctrl_d_do_not_submit_or_destroy_a_draft() {
    let peer = Rc::new(RefCell::new(Peer::default()));
    let mut app = App::new(Box::new(Script(peer.clone())));
    let mut live = Live::default();
    let mut out = Vec::new();
    app.start("previous turn".into(), &mut live, &mut out, 80).unwrap();
    peer.borrow_mut().events.push_back(agent::Event::Finished(Ok(())));
    app.poll(&mut live, &mut out, 80).unwrap();
    app.input.insert("keep").unwrap();
    assert_eq!(app.event(key(KeyCode::Char('d'), Mods::CONTROL), 80).unwrap(), Action::None);
    let mut release = KeyEvent::new(KeyCode::Enter, Mods::NONE);
    release.kind = KeyEventKind::Release;
    assert_eq!(app.event(Event::Key(release), 80).unwrap(), Action::None);
    assert_eq!(app.input.text, "keep");
    assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 80).unwrap(), Action::None);
    assert_eq!(app.notice.as_deref(), Some("press ctrl+c again to exit"));
    assert_eq!(app.input, Input::default());
    app.event(key(KeyCode::Left, Mods::NONE), 80).unwrap();
    assert!(app.notice.is_none() && app.exit_deadline.is_none());
    assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 80).unwrap(), Action::None);
    assert_eq!(app.notice.as_deref(), Some("press ctrl+c again to exit"));
    assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 80).unwrap(), Action::Exit);
    app.input = Input::default();
    assert_eq!(app.event(key(KeyCode::Char('d'), Mods::CONTROL), 80).unwrap(), Action::Exit);
  }

  #[test]
  fn cleanup_attempts_raw_mode_restoration_even_if_output_is_broken() {
    struct Broken;
    impl Write for Broken {
      fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
      }
      fn flush(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::BrokenPipe.into())
      }
    }
    let mut restored = false;
    restore_modes(&mut Broken, true, || {
      restored = true;
      Ok(())
    });
    assert!(restored);
  }
}
