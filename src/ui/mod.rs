mod chat;
mod input;
mod menu;
mod terminal;
mod theme;
mod view;

pub use chat::run;

pub fn report(error: &crate::Error) {
  eprintln!("Error: {}", crate::text::clean(&error.to_string(), 0, true).0);
}
