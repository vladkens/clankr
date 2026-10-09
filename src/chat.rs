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

use crate::agent::{self, Backend, Echo, Phase, Usage};
use crate::input::Input;
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
  Send(String),
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
  exit_deadline: Option<Instant>,
}

impl Default for App {
  fn default() -> Self {
    Self::new(Box::new(Echo::default()))
  }
}

impl App {
  fn new(backend: Box<dyn Backend>) -> Self {
    Self { input: Input::default(), notice: None, backend, request: None, exit_deadline: None }
  }

  fn frame(&self) -> Frame<'_> {
    let response = self.request.as_ref().map_or("", |request| request.response.as_str());
    let activity = self.request.as_ref().map(Request::activity);
    let status = self.backend.status();
    let mut label = format!("{} · {} · {}", status.mode, status.model, status.effort);
    if status.fast {
      label.push_str(" · ⚡︎");
    }
    if let Some((used, limit)) = status.context {
      let compact = |n: u64| {
        if n < 1000 { n.to_string() } else { format!("{}k", n / 1000) }
      };
      let percent = used.saturating_mul(100).checked_div(limit).unwrap_or(0);
      label.push_str(&format!(" · {}/{} {percent}%", compact(used), compact(limit)));
    }
    Frame { input: &self.input, response, activity, status: label, notice: self.notice.as_deref() }
  }

  fn start(&mut self, text: String, live: &mut Live, out: &mut impl Write, cols: u16) -> Res<()> {
    live.prompt(out, &text, cols)?;
    self.request = Some(Request::default());
    self.backend.start(text);
    Ok(())
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
        }
      }
    }
    Ok(changed)
  }

  fn stop(&mut self, live: &mut Live, out: &mut impl Write, cols: u16) -> Res<()> {
    self.cancel();
    if let Some(request) = self.request.take() {
      let summary = request.summary(request.failure.as_ref());
      live.finish(out, &request.response, Some(&summary), cols)?;
    }
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
      let now = Instant::now();
      if self.exit_deadline.is_some_and(|deadline| now < deadline) {
        return Ok(Action::Exit);
      }
      self.exit_deadline = Some(now + Duration::from_secs(3));
      self.notice = Some("press ctrl+c again to exit".into());
      return Ok(Action::None);
    }
    self.exit_deadline = None;
    self.notice = None;
    match event {
      Event::Key(key) => {
        if key.modifiers.contains(Mods::CONTROL)
          && key.code == KeyCode::Char('d')
          && self.input.text.is_empty()
        {
          return Ok(Action::Exit);
        }
        if key.code == KeyCode::Enter && !key.modifiers.intersects(Mods::SHIFT | Mods::ALT) {
          if self.request.is_none() && !self.input.text.trim().is_empty() {
            return Ok(Action::Send(std::mem::take(&mut self.input).text));
          }
        } else if key.code == KeyCode::Esc && self.request.is_some() {
          return Ok(Action::Cancel);
        } else {
          self.input.edit(key, cols)?;
        }
      }
      Event::Paste(text) => self.input.insert(&text)?,
      _ => {}
    }
    Ok(Action::None)
  }
}

pub fn run() -> Res<()> {
  let _keyboard = Keyboard::enter()?;
  let mut out = io::stdout();
  let mut app = App::default();
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
        Ok(Action::None) => {}
        Err(error @ Error::InputTooLarge) => app.notice = Some(error.to_string()),
        Err(error) => return Err(error),
      }
      dirty = true;
    }
  })();
  let stopped = app.stop(&mut live, &mut out, size.0.max(1));
  let cleared = live.clear(&mut out);
  result.and(stopped).and(cleared)
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

  #[derive(Default)]
  struct Peer {
    prompts: Vec<String>,
    events: VecDeque<agent::Event>,
    cancels: usize,
  }

  struct Script(Rc<RefCell<Peer>>);

  impl Backend for Script {
    fn status(&self) -> agent::Status<'_> {
      agent::Status {
        mode: "auto",
        model: "test agent",
        effort: "medium",
        fast: false,
        context: None,
      }
    }
    fn start(&mut self, prompt: String) {
      self.0.borrow_mut().prompts.push(prompt);
    }
    fn poll(&mut self) -> Option<agent::Event> {
      self.0.borrow_mut().events.pop_front()
    }
    fn cancel(&mut self) {
      self.0.borrow_mut().cancels += 1;
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
    assert_eq!(peer.borrow().prompts, ["original\nquestion"]);
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
    assert!(output.contains("first last"));
    assert!(output.contains("Error: connection lost"));
    assert!(output.contains("(↑13 ↓2)"));
    assert!(app.request.is_none());
    assert_eq!(app.input, draft);
    assert!(
      matches!(app.event(key(KeyCode::Enter, Mods::NONE), 80).unwrap(), Action::Send(text) if text == draft.text)
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
    assert!(output.contains("kept") && output.contains("Cancelled"));
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
    let mut app = App::default();
    app.input.insert("keep").unwrap();
    assert_eq!(app.event(key(KeyCode::Char('d'), Mods::CONTROL), 80).unwrap(), Action::None);
    let mut release = KeyEvent::new(KeyCode::Enter, Mods::NONE);
    release.kind = KeyEventKind::Release;
    assert_eq!(app.event(Event::Key(release), 80).unwrap(), Action::None);
    assert_eq!(app.input.text, "keep");
    assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 80).unwrap(), Action::None);
    assert_eq!(app.notice.as_deref(), Some("press ctrl+c again to exit"));
    assert_eq!(app.input.text, "keep");
    app.event(key(KeyCode::Left, Mods::NONE), 80).unwrap();
    assert!(app.notice.is_none() && app.exit_deadline.is_none());
    assert_eq!(app.event(key(KeyCode::Char('c'), Mods::CONTROL), 80).unwrap(), Action::None);
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
