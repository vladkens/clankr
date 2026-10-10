mod agent;
mod markdown;
mod text;
mod ui;

type Res<T> = Result<T, Error>;

#[derive(Debug, thiserror::Error)]
enum Error {
  #[error(transparent)]
  Io(#[from] std::io::Error),
  #[error(transparent)]
  Clipboard(#[from] arboard::Error),
  #[error("clankr needs a terminal")]
  NotTerminal,
  #[error("Keyboard is already active")]
  KeyboardActive,
  #[error("Input exceeds 1 MiB")]
  InputTooLarge,
  #[error("Response exceeds 16 MiB")]
  ResponseTooLarge,
  #[error("Cancelled")]
  Cancelled,
}

fn main() {
  if let Err(error) = ui::run(Box::new(agent::Echo::default())) {
    ui::report(&error);
    std::process::exit(1);
  }
}
