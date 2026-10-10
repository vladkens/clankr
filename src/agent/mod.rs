mod codex;
mod echo;
pub mod model;
mod stream;

pub use codex::Codex;
pub use echo::Echo;
use model::{MODELS, Model, Settings};

use crate::Res;

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
  fn models(&self) -> &'static [Model] {
    MODELS
  }
  fn settings(&self) -> Settings {
    Settings::default()
  }
  fn status(&self) -> Status<'_>;
  fn start(&mut self, prompt: Prompt);
  fn poll(&mut self) -> Option<Event>;
  fn cancel(&mut self);
  // Stop the current request, discard its events, and reset conversation context.
  fn reset(&mut self);
}
