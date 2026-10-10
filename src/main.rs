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
  #[error("{0}")]
  Provider(&'static str),
  #[error("Provider HTTP {0}")]
  Http(u16),
}

fn main() {
  let args: Vec<_> = std::env::args_os().skip(1).collect();
  let backend: Box<dyn agent::Backend> = if args.is_empty() {
    Box::new(agent::Chat::default())
  } else if args.len() == 1 && args[0] == "--mock" {
    Box::new(agent::Echo::default())
  } else {
    eprintln!("Usage: clankr [--mock]");
    std::process::exit(2);
  };
  if let Err(error) = ui::run(backend) {
    ui::report(&error);
    std::process::exit(1);
  }
}
