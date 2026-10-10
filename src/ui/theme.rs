use std::fmt;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers as Mods};

use crate::Res;

// Keep the ANSI colours used by Markdown untouched. OSC 4 updates indexed cells in scrollback.
// https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h2-Operating-System-Commands
pub const SLOT: u8 = 16;
const QUERY: &str = "\x1b]4;16;?\x1b\\\x1b]10;?\x1b\\\x1b]11;?\x1b\\";
static SAVED: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rgb([u16; 3]);

impl Rgb {
  fn parse(text: &str) -> Option<Self> {
    let mut parts = text.strip_prefix("rgb:")?.split('/');
    let mut rgb = [0; 3];
    for channel in &mut rgb {
      let part = parts.next()?;
      if part.is_empty() || part.len() > 4 || !part.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
      }
      let value = u32::from_str_radix(part, 16).ok()?;
      *channel = (value * 65535 / ((1 << (part.len() * 4)) - 1)) as u16;
    }
    parts.next().is_none().then_some(Self(rgb))
  }
}

struct Palette(Rgb);

impl fmt::Display for Palette {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let Rgb([r, g, b]) = self.0;
    write!(f, "\x1b]4;{SLOT};rgb:{r:04x}/{g:04x}/{b:04x}\x1b\\")
  }
}

pub fn restore(out: &mut impl Write) {
  let saved = SAVED.swap(0, Ordering::SeqCst);
  if saved > 0 {
    let rgb = saved - 1;
    let _ = write!(out, "{}", Palette(Rgb([(rgb >> 32) as u16, (rgb >> 16) as u16, rgb as u16])));
  }
}

#[derive(Default)]
pub struct Theme {
  next: Option<Instant>,
  colours: [Option<Rgb>; 3],
  applied: Option<Rgb>,
  confirmed: bool,
  reply: String,
  held: Vec<Event>,
  since: Option<Instant>,
}

impl Theme {
  pub fn active(&self) -> bool {
    self.confirmed
  }

  pub fn tick(&mut self, out: &mut impl Write) -> Res<Vec<Event>> {
    let now = Instant::now();
    if self.next.is_none_or(|next| now >= next)
      && !crossterm::style::Colored::ansi_color_disabled_memoized()
    {
      self.colours = [None; 3];
      write!(out, "{QUERY}")?;
      out.flush()?;
      self.next = Some(now + Duration::from_secs(1));
    }
    if self.since.is_some_and(|since| now.duration_since(since) > Duration::from_millis(250)) {
      return Ok(self.release());
    }
    Ok(Vec::new())
  }

  fn release(&mut self) -> Vec<Event> {
    let events = if self.is_reply() { Vec::new() } else { std::mem::take(&mut self.held) };
    self.held.clear();
    self.reply.clear();
    self.since = None;
    events
  }

  fn is_reply(&self) -> bool {
    ["4;16;rgb:", "10;rgb:", "11;rgb:"].iter().any(|prefix| self.reply.starts_with(prefix))
  }

  // Crossterm 0.29 exposes OSC replies as key events. Decode only colour replies here,
  // in the existing input owner; never open a competing stdin reader. Paste stays literal.
  pub fn filter(&mut self, event: Event, out: &mut impl Write) -> Res<Vec<Event>> {
    if let Event::Key(key) = event
      && key.kind != KeyEventKind::Release
    {
      if self.held.is_empty() && key.code == KeyCode::Char(']') && key.modifiers == Mods::ALT {
        self.held.push(event);
        self.since = Some(Instant::now());
        return Ok(Vec::new());
      }
      if !self.held.is_empty() {
        let end = (key.code == KeyCode::Char('g') && key.modifiers == Mods::CONTROL)
          || (key.code == KeyCode::Char('\\') && key.modifiers == Mods::ALT);
        if end && self.is_reply() {
          let reply = self.reply.clone();
          self.release();
          self.colour(&reply, out)?;
          return Ok(Vec::new());
        }
        if let KeyCode::Char(c) = key.code
          && !key.modifiers.intersects(Mods::CONTROL | Mods::ALT | Mods::SUPER | Mods::META)
          && self.reply.len() < 64
        {
          self.reply.push(c);
          let prefixes = ["4;16;rgb:", "10;rgb:", "11;rgb:"];
          if prefixes
            .iter()
            .any(|prefix| prefix.starts_with(&self.reply) || self.reply.starts_with(prefix))
          {
            self.held.push(event);
            return Ok(Vec::new());
          }
        }
      }
    }
    let mut events = self.release();
    events.push(event);
    Ok(events)
  }

  fn colour(&mut self, reply: &str, out: &mut impl Write) -> Res<()> {
    let (index, text) = if let Some(text) = reply.strip_prefix("4;16;") {
      (0, text)
    } else if let Some(text) = reply.strip_prefix("10;") {
      (1, text)
    } else if let Some(text) = reply.strip_prefix("11;") {
      (2, text)
    } else {
      return Ok(());
    };
    self.colours[index] = Rgb::parse(text);
    if index == 0 && self.applied.is_some() && self.colours[0] == self.applied {
      self.confirmed = true;
    }
    if let [Some(slot), Some(fg), Some(bg)] = self.colours {
      let colour = Rgb(std::array::from_fn(|i| {
        // Quantize to the 8-bit channels most terminals actually store.
        ((u32::from(bg.0[i]) * 9 + u32::from(fg.0[i])) / 10 / 257 * 257) as u16
      }));
      if Some(slot) != self.applied {
        let Rgb([r, g, b]) = slot;
        SAVED.store(
          ((u64::from(r) << 32) | (u64::from(g) << 16) | u64::from(b)) + 1,
          Ordering::SeqCst,
        );
      }
      if slot != colour || self.applied != Some(colour) {
        write!(out, "{}\x1b]4;{SLOT};?\x1b\\", Palette(colour))?;
        out.flush()?;
      }
      self.applied = Some(colour);
      self.colours = [None; 3];
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use crossterm::event::KeyEvent;

  use super::*;

  fn key(c: char, mods: Mods) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), mods))
  }

  fn reply(theme: &mut Theme, text: &str, bell: bool, out: &mut Vec<u8>) {
    let events = [key(']', Mods::ALT)]
      .into_iter()
      .chain(text.chars().map(|c| key(c, Mods::NONE)))
      .chain([if bell { key('g', Mods::CONTROL) } else { key('\\', Mods::ALT) }]);
    for event in events {
      assert!(theme.filter(event, out).unwrap().is_empty());
    }
  }

  #[test]
  fn rgb_accepts_xterm_channel_widths_and_rejects_invalid_replies() {
    for value in ["rgb:f/0/8", "rgb:ff/00/88", "rgb:fff/000/888", "rgb:ffff/0000/8888"] {
      assert_eq!(Rgb::parse(value), Some(Rgb([65535, 0, 34952])));
    }
    for value in ["red", "rgb:/0/0", "rgb:10000/0/0", "rgb:+1/2/3", "rgb:1/2", "rgb:1/2/3/4"] {
      assert_eq!(Rgb::parse(value), None);
    }
  }

  #[test]
  fn colour_replies_do_not_consume_paste_keys_or_leave_partial_protocol_in_the_editor() {
    let mut theme = Theme::default();
    let mut out = Vec::new();
    let paste = Event::Paste("\x1b]11;rgb:ff/ff/ff\x07".into());
    assert_eq!(theme.filter(paste.clone(), &mut out).unwrap(), [paste]);
    assert_eq!(theme.filter(key('x', Mods::NONE), &mut out).unwrap(), [key('x', Mods::NONE)]);
    assert!(theme.filter(key(']', Mods::ALT), &mut out).unwrap().is_empty());
    assert_eq!(
      theme.filter(key('x', Mods::NONE), &mut out).unwrap(),
      [key(']', Mods::ALT), key('x', Mods::NONE)]
    );
    reply(&mut theme, "11;rgb:zz/zz/zz", true, &mut out);
    assert!(!theme.active());
    for event in
      [key(']', Mods::ALT)].into_iter().chain("11;rgb:ab".chars().map(|c| key(c, Mods::NONE)))
    {
      assert!(theme.filter(event, &mut out).unwrap().is_empty());
    }
    theme.since = Some(Instant::now() - Duration::from_secs(1));
    assert!(theme.tick(&mut out).unwrap().is_empty());
    assert_eq!(theme.filter(key('c', Mods::CONTROL), &mut out).unwrap(), [key('c', Mods::CONTROL)]);
  }

  #[test]
  fn palette_follows_light_and_dark_themes_and_restores_the_original_slot() {
    let mut theme = Theme::default();
    let mut out = Vec::new();
    let original = "4;16;rgb:0101/0202/0303";
    reply(&mut theme, original, false, &mut out);
    reply(&mut theme, "10;rgb:c0/c0/c0", true, &mut out);
    reply(&mut theme, "11;rgb:20/20/20", false, &mut out);
    assert!(!theme.active(), "wait for palette write confirmation");
    assert!(String::from_utf8_lossy(&out).contains("4;16;rgb:3030/3030/3030"));
    reply(&mut theme, "4;16;rgb:3030/3030/3030", true, &mut out);
    assert!(theme.active());
    out.clear();
    reply(&mut theme, "10;rgb:22/22/22", false, &mut out);
    reply(&mut theme, "11;rgb:ee/ee/ee", true, &mut out);
    assert!(String::from_utf8_lossy(&out).contains("4;16;rgb:d9d9/d9d9/d9d9"));
    out.clear();
    restore(&mut out);
    assert_eq!(String::from_utf8_lossy(&out), format!("\x1b]{original}\x1b\\"));
    out.clear();
    restore(&mut out);
    assert!(out.is_empty());
  }
}
