use std::io::{self, IsTerminal, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{
  DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
  PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::style::{Attribute, SetAttribute};
use crossterm::terminal::{EndSynchronizedUpdate, disable_raw_mode, enable_raw_mode};
use osc8::Hyperlink;

use super::theme;
use crate::{Error, Res};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static ENHANCED: AtomicBool = AtomicBool::new(false);
static HOOK: Once = Once::new();

fn restore() {
  if ACTIVE.swap(false, Ordering::SeqCst) {
    let mut out = io::stdout();
    theme::restore(&mut out);
    restore_modes(&mut out, ENHANCED.swap(false, Ordering::SeqCst), disable_raw_mode);
  }
}

fn restore_modes(out: &mut impl Write, enhanced: bool, disable: impl FnOnce() -> io::Result<()>) {
  if enhanced {
    let _ = execute!(out, PopKeyboardEnhancementFlags);
  }
  let _ = execute!(out, EndSynchronizedUpdate, DisableBracketedPaste);
  let _ = write!(out, "{}{}\r\n", Hyperlink::END, SetAttribute(Attribute::Reset));
  let _ = out.flush();
  let _ = disable();
}

pub(super) struct Terminal;

impl Terminal {
  pub(super) fn enter() -> Res<Self> {
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

impl Drop for Terminal {
  fn drop(&mut self) {
    restore();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

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
