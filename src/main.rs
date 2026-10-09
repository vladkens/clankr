mod agent;
mod chat;
mod input;
mod view;

type Res<T> = Result<T, Error>;

#[derive(Debug, thiserror::Error)]
enum Error {
  #[error(transparent)]
  Io(#[from] std::io::Error),
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
  if let Err(error) = chat::run() {
    eprintln!("Error: {}", input::clean(&error.to_string(), 0, true).0);
    std::process::exit(1);
  }
}
