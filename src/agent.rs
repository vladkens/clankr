use std::time::{Duration, Instant};

use crate::{Error, Res};

pub const LIMIT: usize = 16 * 1024 * 1024;

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
  pub model: &'a str,
  pub effort: &'a str,
  pub fast: bool,
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
  fn start(&mut self, prompt: String);
  fn poll(&mut self) -> Option<Event>;
  fn cancel(&mut self);
}

#[derive(Default)]
pub struct Echo {
  reply: Option<Reply>,
  usage: Option<Usage>,
  cancelled: bool,
  context: u64,
}

struct Reply {
  text: String,
  usage: Usage,
  phase: Phase,
  sent: bool,
  next: Instant,
  finish: Instant,
}

impl Echo {
  fn start_at(&mut self, prompt: String, now: Instant) {
    // Mock token counts and latency exercise the same UI events as a real backend.
    let input = prompt.chars().count().div_ceil(4) as u64;
    let usage = Usage { input, output: 0 };
    self.reply = Some(Reply {
      text: prompt,
      usage,
      phase: Phase::Thinking,
      sent: false,
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
    if reply.usage.output < reply.usage.input {
      reply.usage.output =
        (reply.usage.output + reply.usage.input.div_ceil(12)).min(reply.usage.input);
      reply.next = now + Duration::from_millis(50);
      if reply.usage.output == reply.usage.input {
        reply.next = reply.next.max(reply.finish);
      }
      return Some(Event::Usage(reply.usage));
    }
    if !reply.sent {
      reply.sent = true;
      return Some(Event::Text(std::mem::take(&mut reply.text)));
    }
    self.context += reply.usage.input + reply.usage.output;
    self.reply = None;
    Some(Event::Finished(Ok(())))
  }
}

impl Backend for Echo {
  fn status(&self) -> Status<'_> {
    Status {
      mode: "auto",
      model: "echo",
      effort: "medium",
      fast: true,
      context: (self.context > 0).then_some((self.context, 272_000)),
    }
  }

  fn start(&mut self, prompt: String) {
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
      let mut generating = false;
      let mut finished = false;
      let mut usage = None;
      for ms in (1200..3000).step_by(50) {
        while let Some(event) = echo.poll_at(now + Duration::from_millis(ms)) {
          match event {
            Event::Phase(Phase::Generating) => generating = true,
            Event::Phase(Phase::Thinking) => panic!(),
            Event::Text(text) => {
              assert!(generating && ms >= 2000);
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
    }
  }
}
