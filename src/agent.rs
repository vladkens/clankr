use std::time::{Duration, Instant};

use crate::model::Settings;
use crate::{Error, Res};

pub const LIMIT: usize = 16 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct Prompt {
  pub text: String,
  pub skills: Vec<String>,
  pub settings: Settings,
}

impl From<String> for Prompt {
  fn from(text: String) -> Self {
    Self { text, skills: Vec::new(), settings: Settings::default() }
  }
}

impl From<&str> for Prompt {
  fn from(text: &str) -> Self {
    text.to_owned().into()
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
  pub input: u64,
  pub output: u64,
}

#[derive(Clone, Copy, Default)]
pub enum Phase {
  #[default]
  Thinking,
  Generating,
}

pub struct Status<'a> {
  pub mode: &'a str,
  pub context: Option<(u64, u64)>,
}

pub enum Event {
  Phase(Phase),
  Text(String),
  // Cumulative counts for this request, not deltas. Exact counts come from the provider.
  Usage(Usage),
  Finished(Res<()>),
}

// Methods must not block terminal input. A request emits Text events followed by
// exactly one Finished, including after cancellation. The backend owns its work
// and must stop it when dropped; it never writes to the terminal.
pub trait Backend {
  fn status(&self) -> Status<'_>;
  fn start(&mut self, prompt: Prompt);
  fn poll(&mut self) -> Option<Event>;
  fn cancel(&mut self);
  // Stop the current request, discard its events, and reset conversation context.
  fn reset(&mut self);
}

#[derive(Default)]
pub struct Echo {
  reply: Option<Reply>,
  usage: Option<Usage>,
  cancelled: bool,
  context: u64,
}

struct Reply {
  prompt: Prompt,
  usage: Usage,
  phase: Phase,
  sent: usize,
  next: Instant,
  finish: Instant,
}

impl Echo {
  fn start_at(&mut self, prompt: Prompt, now: Instant) {
    // Mock token counts and latency exercise the same UI events as a real backend.
    let total = prompt.text.chars().count().div_ceil(4) as u64;
    let input =
      total + prompt.skills.iter().map(|name| name.chars().count().div_ceil(4) as u64).sum::<u64>();
    let usage = Usage { input, output: 0 };
    self.reply = Some(Reply {
      prompt,
      usage,
      phase: Phase::Thinking,
      sent: 0,
      next: now + Duration::from_millis(1200),
      finish: now + Duration::from_millis(2000),
    });
    self.usage = Some(usage);
    self.cancelled = false;
  }

  fn poll_at(&mut self, now: Instant) -> Option<Event> {
    if std::mem::take(&mut self.cancelled) {
      return Some(Event::Finished(Err(Error::Cancelled)));
    }
    if let Some(usage) = self.usage.take() {
      return Some(Event::Usage(usage));
    }
    let reply = self.reply.as_mut()?;
    if now < reply.next {
      return None;
    }
    if matches!(reply.phase, Phase::Thinking) {
      reply.phase = Phase::Generating;
      reply.next = now + Duration::from_millis(50);
      return Some(Event::Phase(Phase::Generating));
    }
    if reply.sent < reply.prompt.text.len() {
      let chunk = reply.prompt.text.len().div_ceil(12);
      let end = reply.prompt.text[reply.sent..]
        .char_indices()
        .map(|(i, c)| reply.sent + i + c.len_utf8())
        .find(|&end| end - reply.sent >= chunk)
        .unwrap_or(reply.prompt.text.len());
      let text = reply.prompt.text[reply.sent..end].to_owned();
      reply.sent = end;
      reply.usage.output = reply.prompt.text[..end].chars().count().div_ceil(4) as u64;
      self.usage = Some(reply.usage);
      reply.next = now + Duration::from_millis(50);
      if end == reply.prompt.text.len() {
        reply.next = reply.next.max(reply.finish);
      }
      return Some(Event::Text(text));
    }
    self.context += reply.usage.input + reply.usage.output;
    self.reply = None;
    Some(Event::Finished(Ok(())))
  }
}

impl Backend for Echo {
  fn status(&self) -> Status<'_> {
    Status { mode: "auto", context: (self.context > 0).then_some((self.context, 272_000)) }
  }

  fn start(&mut self, prompt: Prompt) {
    self.start_at(prompt, Instant::now());
  }

  fn poll(&mut self) -> Option<Event> {
    self.poll_at(Instant::now())
  }

  fn cancel(&mut self) {
    if self.reply.take().is_some() {
      self.usage = None;
      self.cancelled = true;
    }
  }

  fn reset(&mut self) {
    *self = Self::default();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn echo_exposes_waiting_generation_usage_and_completion_without_blocking() {
    let now = Instant::now();
    for prompt in ["a", "界e\u{301} 👩‍💻\nnext", &"long 界\n".repeat(100)] {
      let mut echo = Echo::default();
      echo.start_at(prompt.into(), now);
      assert!(matches!(echo.poll_at(now), Some(Event::Usage(Usage { output: 0, .. }))));
      assert!(echo.poll_at(now + Duration::from_millis(1199)).is_none());
      let mut response = String::new();
      let mut chunks = 0;
      let mut generating = false;
      let mut finished = false;
      let mut usage = None;
      for ms in (1200..3000).step_by(50) {
        while let Some(event) = echo.poll_at(now + Duration::from_millis(ms)) {
          match event {
            Event::Phase(Phase::Generating) => generating = true,
            Event::Phase(Phase::Thinking) => panic!(),
            Event::Text(text) => {
              assert!(generating && ms < 2000);
              chunks += 1;
              response.push_str(&text);
            }
            Event::Usage(counts) => usage = Some(counts),
            Event::Finished(result) => {
              result.unwrap();
              assert!(ms >= 2000);
              assert!(!finished);
              finished = true;
            }
          }
        }
      }
      assert!(finished && generating);
      assert_eq!(response, prompt);
      assert!(chunks > 0 && (prompt.len() == 1 || chunks > 1));
      let count = prompt.chars().count().div_ceil(4) as u64;
      assert_eq!(usage, Some(Usage { input: count, output: count }));
      assert_eq!(echo.status().context, Some((count * 2, 272_000)));
    }
  }

  #[test]
  fn cancellation_stops_waiting_or_streaming_and_allows_the_next_request() {
    let now = Instant::now();
    for ms in [0, 1200] {
      let mut echo = Echo::default();
      echo.start_at("hello".into(), now);
      echo.poll_at(now);
      echo.poll_at(now + Duration::from_millis(ms));
      echo.cancel();
      echo.cancel();
      assert!(matches!(echo.poll_at(now), Some(Event::Finished(Err(Error::Cancelled)))));
      assert!(echo.poll_at(now + Duration::from_secs(5)).is_none());
      echo.start_at("next".into(), now);
      assert!(matches!(echo.poll_at(now), Some(Event::Usage(_))));
      echo.reset();
      assert!(echo.poll_at(now + Duration::from_secs(5)).is_none());
      assert!(echo.status().context.is_none());
    }
  }
}
