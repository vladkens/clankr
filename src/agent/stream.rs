use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{Event, LIMIT, Usage};
use crate::{Error, Res};

const EVENT_LIMIT: usize = 1024 * 1024;
const PART_LIMIT: usize = 4096;
const INVALID: Error = Error::Provider("Invalid Codex stream");
const CONFLICT: Error = Error::Provider("Codex snapshot conflicts with streamed output");

#[derive(Default)]
pub struct Sse {
  buf: Vec<u8>,
  bytes: usize,
  cr: bool,
  started: bool,
}

impl Sse {
  pub fn push(
    &mut self,
    chunk: &[u8],
    mut event: impl FnMut(&str, &str) -> Res<bool>,
  ) -> Res<bool> {
    for &byte in chunk {
      let skip = std::mem::take(&mut self.cr) && byte == b'\n';
      self.bytes += usize::from(!skip || self.bytes > 0);
      if self.bytes > EVENT_LIMIT {
        return Err(Error::Provider("SSE event exceeds 1 MiB"));
      }
      if skip {
        continue;
      }
      self.cr = byte == b'\r';
      let byte = if self.cr { b'\n' } else { byte };
      if byte != b'\n' || (!self.buf.is_empty() && !self.buf.ends_with(b"\n")) {
        self.buf.push(byte);
        continue;
      }
      // Decode frames, not network chunks: any UTF-8 character can span reads.
      let frame = std::str::from_utf8(&self.buf).map_err(|_| INVALID)?;
      let frame = if self.started { frame } else { frame.trim_start_matches('\u{feff}') };
      self.started = true;
      let (mut name, mut data) = ("", String::new());
      for line in frame.lines() {
        let (key, val) = line.split_once(':').unwrap_or((line, ""));
        let val = val.strip_prefix(' ').unwrap_or(val);
        match key {
          "event" => name = val,
          "data" => {
            data.push_str(val);
            data.push('\n');
          }
          _ => {}
        }
      }
      let done = !data.is_empty() && event(name, &data[..data.len() - 1])?;
      self.buf.clear();
      self.bytes = 0;
      if done {
        return Ok(true);
      }
    }
    Ok(false)
  }
}

#[derive(Default)]
struct Block {
  kind: &'static str,
  text: String,
  deltas: usize,
  sent: usize,
  done: bool,
}

impl Block {
  fn update(&mut self, text: &str, delta: bool, done: bool) -> Res<()> {
    if delta {
      if self.done {
        return Err(CONFLICT);
      }
      let rest = &self.text[self.deltas..];
      let shared = rest.len().min(text.len());
      if rest.as_bytes()[..shared] != text.as_bytes()[..shared] {
        return Err(CONFLICT);
      }
      if text.len() > rest.len() {
        self.text.push_str(&text[rest.len()..]);
      }
      self.deltas += text.len();
    } else {
      let compatible = (self.text == text)
        || (!self.done && text.starts_with(&self.text))
        || (!done && self.text.starts_with(text));
      if !compatible {
        return Err(CONFLICT);
      }
      if text.len() > self.text.len() {
        self.text.push_str(&text[self.text.len()..]);
      }
      self.done |= done;
    }
    Ok(())
  }
}

#[derive(Default)]
struct Item {
  id: String,
  kind: String,
  phase: Option<String>,
  blocks: BTreeMap<usize, Block>,
  reasoning: Option<Value>,
  done: bool,
}

#[derive(Default)]
pub struct Stream {
  items: BTreeMap<usize, Item>,
  bytes: usize,
  blocks: usize,
  input: Option<u64>,
  output: Option<u64>,
  pub usage: Option<Usage>,
}

impl Stream {
  fn item(&mut self, i: usize, id: &str, kind: &str) -> Res<()> {
    if id.is_empty() || self.items.iter().any(|(&index, item)| index != i && item.id == id) {
      return Err(INVALID);
    }
    if !self.items.contains_key(&i) {
      self.reserve(id.len())?;
    }
    let item = self.items.entry(i).or_insert_with(|| Item {
      id: id.into(),
      kind: kind.into(),
      ..Item::default()
    });
    if item.id != id || item.kind != kind {
      return Err(CONFLICT);
    }
    Ok(())
  }

  fn reserve(&mut self, bytes: usize) -> Res<()> {
    if bytes > LIMIT.saturating_sub(self.bytes) {
      return Err(Error::ResponseTooLarge);
    }
    self.bytes += bytes;
    Ok(())
  }

  fn block(
    &mut self,
    i: usize,
    j: usize,
    kind: &'static str,
    text: &str,
    delta: bool,
    done: bool,
  ) -> Res<()> {
    let item = self.items.get(&i).ok_or(INVALID)?;
    let prev = item.blocks.get(&j);
    if item.kind != "message" || (item.done && prev.is_none()) {
      return Err(CONFLICT);
    }
    if prev.is_none() {
      self.blocks += 1;
      if self.blocks > PART_LIMIT {
        return Err(INVALID);
      }
    }
    let old = prev.map_or(0, |block| block.text.len());
    let size = if delta { prev.map_or(0, |block| block.deltas) + text.len() } else { text.len() };
    self.reserve(size.saturating_sub(old))?;
    let block = self.items.get_mut(&i).unwrap().blocks.entry(j).or_default();
    if !block.kind.is_empty() && block.kind != kind {
      return Err(CONFLICT);
    }
    block.kind = kind;
    block.update(text, delta, done)
  }

  fn snapshot(&mut self, i: usize, dat: &Value, done: bool) -> Res<()> {
    let kind = text(dat, "type")?;
    if !matches!(kind, "message" | "reasoning") {
      return Err(Error::Provider(
        "Codex requested unsupported output; tools are not connected yet",
      ));
    }
    self.item(i, text(dat, "id")?, kind)?;
    if kind == "reasoning" {
      // Added events contain provisional encrypted data. Only replay final reasoning.
      if done {
        let mut value = json!({"id": dat["id"], "type": "reasoning"});
        for key in ["summary", "encrypted_content"] {
          if let Some(part) = dat.get(key) {
            value[key] = part.clone();
          }
        }
        let prev = self.items[&i].reasoning.as_ref();
        if prev.is_some_and(|prev| prev != &value) {
          return Err(CONFLICT);
        }
        if prev.is_none() {
          self.reserve(value.to_string().len())?;
          self.items.get_mut(&i).unwrap().reasoning = Some(value);
        }
      }
    } else {
      if dat.get("role").is_some_and(|role| role != "assistant") {
        return Err(INVALID);
      }
      if let Some(phase) = dat.get("phase").filter(|phase| !phase.is_null()) {
        let phase = phase.as_str().ok_or(INVALID)?;
        let prev = self.items[&i].phase.as_deref();
        if prev.is_some_and(|prev| prev != phase) {
          return Err(CONFLICT);
        }
        if prev.is_none() {
          self.reserve(phase.len())?;
          self.items.get_mut(&i).unwrap().phase = Some(phase.into());
        }
      }
      let parts = dat["content"].as_array().ok_or(INVALID)?;
      if parts.len() > PART_LIMIT
        || (done && self.items[&i].blocks.keys().any(|&j| j >= parts.len()))
      {
        return Err(CONFLICT);
      }
      for (j, part) in parts.iter().enumerate() {
        let (kind, value) = content(part)?;
        self.block(i, j, kind, value, false, done)?;
      }
    }
    self.items.get_mut(&i).unwrap().done |= done;
    Ok(())
  }

  pub fn event(&mut self, name: &str, data: &str, events: &mut Vec<Event>) -> Res<bool> {
    let dat: Value = serde_json::from_str(data).map_err(|_| INVALID)?;
    let kind = dat["type"].as_str().unwrap_or(name);
    if kind.is_empty() || (!name.is_empty() && name != "message" && name != kind) {
      return Err(INVALID);
    }
    let mut done = false;
    match kind {
      "response.output_text.delta"
      | "response.output_text.done"
      | "response.refusal.delta"
      | "response.refusal.done"
      | "response.content_part.added"
      | "response.content_part.done" => {
        let i = index(&dat, "output_index")?;
        let j = index(&dat, "content_index")?;
        self.item(i, text(&dat, "item_id")?, "message")?;
        let delta = kind.ends_with("delta");
        let (ty, value) = if kind.starts_with("response.content_part.") {
          content(&dat["part"])?
        } else {
          let refusal = kind.starts_with("response.refusal.");
          (
            if refusal { "refusal" } else { "output_text" },
            text(
              &dat,
              if delta {
                "delta"
              } else if refusal {
                "refusal"
              } else {
                "text"
              },
            )?,
          )
        };
        self.block(i, j, ty, value, delta, kind.ends_with("done"))?;
      }
      "response.output_item.added" | "response.output_item.done" => {
        self.snapshot(index(&dat, "output_index")?, &dat["item"], kind.ends_with("done"))?;
      }
      "response.completed" => {
        let rep = &dat["response"];
        if rep["status"] != "completed" || !rep["error"].is_null() {
          return Err(Error::Provider("Codex response did not complete"));
        }
        let items = rep["output"].as_array().ok_or(INVALID)?;
        if items.is_empty() {
          // Codex can finish with usage only after sending complete output_item.done snapshots.
          if self.items.iter().enumerate().any(|(next, (&i, item))| i != next || !item.done) {
            return Err(CONFLICT);
          }
        } else {
          if items.len() > PART_LIMIT || self.items.keys().any(|&i| i >= items.len()) {
            return Err(CONFLICT);
          }
          for (i, item) in items.iter().enumerate() {
            self.snapshot(i, item, true)?;
          }
        }
        done = true;
      }
      "error" | "response.error" | "response.failed" | "response.incomplete" => {
        return Err(Error::Provider("Codex stream failed or returned an incomplete response"));
      }
      x if x.starts_with("response.") => {}
      _ => return Err(INVALID),
    }
    // Emit only the contiguous prefix, even when later items arrive first.
    'items: for (next, (&i, item)) in self.items.iter_mut().enumerate() {
      if i != next {
        break;
      }
      for (next, (&j, block)) in item.blocks.iter_mut().enumerate() {
        if j != next {
          break 'items;
        }
        if block.sent < block.text.len() {
          events.push(Event::Text(block.text[block.sent..].into()));
          block.sent = block.text.len();
        }
        if !block.done {
          break 'items;
        }
      }
      if !item.done {
        break;
      }
    }
    let counts = &dat["response"]["usage"];
    for (value, key) in [(&mut self.input, "input_tokens"), (&mut self.output, "output_tokens")] {
      if let Some(count) = counts.get(key) {
        *value = Some(count.as_u64().ok_or(INVALID)?);
      }
    }
    if let (Some(input), Some(output)) = (self.input, self.output) {
      let usage = Usage { input, output };
      if self.usage != Some(usage) {
        self.usage = Some(usage);
        events.push(Event::Usage(usage));
      }
    }
    Ok(done)
  }

  pub fn output(self) -> Vec<Value> {
    self
      .items
      .into_values()
      .map(|item| {
        if let Some(reasoning) = item.reasoning {
          return reasoning;
        }
        let parts: Vec<_> = item
          .blocks
          .into_values()
          .map(|block| {
            if block.kind == "refusal" {
              json!({"type": "refusal", "refusal": block.text})
            } else {
              json!({"type": "output_text", "text": block.text})
            }
          })
          .collect();
        let mut value =
          json!({"type": "message", "id": item.id, "role": "assistant", "content": parts});
        if let Some(phase) = item.phase {
          value["phase"] = json!(phase);
        }
        value
      })
      .collect()
  }
}

fn text<'a>(dat: &'a Value, key: &str) -> Res<&'a str> {
  dat[key].as_str().ok_or(INVALID)
}

fn index(dat: &Value, key: &str) -> Res<usize> {
  dat[key].as_u64().filter(|&n| n < PART_LIMIT as u64).map(|n| n as usize).ok_or(INVALID)
}

fn content(dat: &Value) -> Res<(&'static str, &str)> {
  match text(dat, "type")? {
    "output_text" => Ok(("output_text", text(dat, "text")?)),
    "refusal" => Ok(("refusal", text(dat, "refusal")?)),
    _ => Err(INVALID),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  pub fn message(id: &str, parts: &[&str]) -> Value {
    json!({"type": "message", "id": id, "role": "assistant", "content": parts.iter()
      .map(|text| json!({"type": "output_text", "text": text})).collect::<Vec<_>>()})
  }

  fn delta(i: usize, j: usize, text: &str) -> Value {
    json!({"type": "response.output_text.delta", "item_id": format!("msg_{i}"),
      "output_index": i, "content_index": j, "delta": text})
  }

  fn complete(items: Vec<Value>) -> Value {
    json!({"type": "response.completed", "response": {"status": "completed", "output": items,
      "usage": {"input_tokens": 12, "output_tokens": 8}}})
  }

  #[test]
  fn interleaved_snapshots_reconcile_and_only_final_reasoning_is_replayed() {
    // Regression from old:src/providers.rs, including a stale added snapshot after done.
    let pending =
      json!({"type": "reasoning", "id": "rs_0", "summary": [], "encrypted_content": "partial"});
    let reasoning =
      json!({"type": "reasoning", "id": "rs_0", "summary": [], "encrypted_content": "final"});
    let first = message("msg_1", &["界 hello", "🦀"]);
    let last = message("msg_2", &[" later", " end"]);
    let events = vec![
      delta(2, 1, " end"),
      json!({"type": "response.output_item.added", "output_index": 0, "item": pending}),
      json!({"type": "response.output_item.added", "output_index": 1, "item": message("msg_1", &[])}),
      delta(1, 1, "🦀"),
      delta(1, 0, "界 "),
      delta(2, 0, " later"),
      json!({"type": "response.output_item.added", "output_index": 1, "item": first}),
      delta(1, 0, "hello"),
      json!({"type": "response.output_text.done", "output_index": 1, "content_index": 0, "item_id": "msg_1", "text": "界 hello"}),
      json!({"type": "response.output_item.done", "output_index": 1, "item": first}),
      json!({"type": "response.output_item.done", "output_index": 0, "item": reasoning}),
      json!({"type": "response.output_item.done", "output_index": 2, "item": last}),
      json!({"type": "response.output_item.added", "output_index": 1, "item": message("msg_1", &["界"])}),
      json!({"type": "response.output_item.added", "output_index": 0, "item": pending}),
      complete(vec![reasoning.clone(), first.clone(), last.clone()]),
    ];
    let wire = events.iter().map(|event| format!("data: {event}\r\n\r\n")).collect::<String>();
    for width in [1, 2, 7, wire.len()] {
      let mut sse = Sse::default();
      let mut stream = Stream::default();
      let mut out = Vec::new();
      let mut done = false;
      for chunk in wire.as_bytes().chunks(width) {
        done |= sse.push(chunk, |name, data| stream.event(name, data, &mut out)).unwrap();
      }
      assert!(done);
      let text: String = out
        .iter()
        .filter_map(|event| match event {
          Event::Text(text) => Some(text.as_str()),
          _ => None,
        })
        .collect();
      assert_eq!(text, "界 hello🦀 later end");
      assert_eq!(stream.usage, Some(Usage { input: 12, output: 8 }));
      assert_eq!(stream.output(), [reasoning.clone(), first.clone(), last.clone()]);
    }
  }

  #[test]
  fn usage_only_completion_preserves_finished_items_without_repeating_text() {
    // Real Codex event order: the terminal response can have an empty output array.
    for reasoning in [false, true] {
      let i = usize::from(reasoning);
      let id = format!("msg_{i}");
      let mut reply = message(&id, &["BIRCH-EMBER"]);
      reply["phase"] = json!("final_answer");
      let mut wire = Vec::new();
      let mut expected = Vec::new();
      if reasoning {
        let item =
          json!({"type": "reasoning", "id": "rs_0", "summary": [], "encrypted_content": "final"});
        wire.push(json!({"type": "response.output_item.done", "output_index": 0, "item": item}));
        expected.push(item);
      }
      expected.push(reply.clone());
      wire.extend([
        json!({"type": "response.output_item.added", "output_index": i, "item": message(&id, &[])}),
        json!({"type": "response.content_part.added", "output_index": i, "content_index": 0, "item_id": id, "part": {"type": "output_text", "text": ""}}),
        delta(i, 0, "BIR"),
        delta(i, 0, "CH-EMBER"),
        json!({"type": "response.output_text.done", "output_index": i, "content_index": 0, "item_id": id, "text": "BIRCH-EMBER"}),
        json!({"type": "response.content_part.done", "output_index": i, "content_index": 0, "item_id": id, "part": reply["content"][0]}),
        json!({"type": "response.output_item.done", "output_index": i, "item": reply}),
        complete(vec![]),
      ]);
      let mut stream = Stream::default();
      let mut events = Vec::new();
      for (i, event) in wire.iter().enumerate() {
        assert_eq!(stream.event("", &event.to_string(), &mut events).unwrap(), i + 1 == wire.len());
      }
      let text: String = events
        .iter()
        .filter_map(|event| match event {
          Event::Text(text) => Some(text.as_str()),
          _ => None,
        })
        .collect();
      assert_eq!(text, "BIRCH-EMBER");
      assert_eq!(stream.usage, Some(Usage { input: 12, output: 8 }));
      assert_eq!(stream.output(), expected);
    }
  }

  #[test]
  fn usage_only_completion_rejects_unfinished_or_missing_items() {
    for (i, done) in [(0, false), (1, true)] {
      let mut stream = Stream::default();
      let mut events = Vec::new();
      let kind = if done { "response.output_item.done" } else { "response.output_item.added" };
      let event = json!({"type": kind, "output_index": i, "item": message("msg", &["text"])});
      stream.event("", &event.to_string(), &mut events).unwrap();
      assert!(stream.event("", &complete(vec![]).to_string(), &mut events).is_err());
      assert!(stream.usage.is_none());
    }
  }

  #[test]
  fn malformed_conflicting_and_oversized_events_fail_without_exposing_payloads() {
    for bad in [
      complete(vec![message("msg_0", &["private-conflicting-text"])]),
      complete(vec![]),
      delta(0, PART_LIMIT, "private-text"),
      json!({"type": "response.failed", "error": "private-provider-detail"}),
      json!({"type": "response.output_item.done", "output_index": 0, "item": {"id": "msg_0", "type": "function_call", "arguments": "private-arguments"}}),
    ] {
      let mut stream = Stream::default();
      let mut events = Vec::new();
      stream.event("", &delta(0, 0, "kept").to_string(), &mut events).unwrap();
      let error = stream.event("", &bad.to_string(), &mut events).unwrap_err();
      assert!(!error.to_string().contains("private"));
      assert!(matches!(&events[0], Event::Text(text) if text == "kept"));
    }
    let mut sse = Sse::default();
    assert!(sse.push(b"data: \xff\n\n", |_, _| panic!()).is_err());
    assert!(Sse::default().push(&vec![b'x'; EVENT_LIMIT + 1], |_, _| panic!()).is_err());
    let mut stream = Stream { bytes: LIMIT, ..Stream::default() };
    assert!(stream.event("", &delta(0, 0, "x").to_string(), &mut Vec::new()).is_err());
  }

  #[test]
  fn sse_handles_bom_comments_multiline_data_and_all_line_endings() {
    for eol in ["\n", "\r", "\r\n"] {
      let wire = format!("\u{feff}: comment{eol}event: test{eol}data: 界{eol}data: two{eol}{eol}");
      let mut sse = Sse::default();
      let mut received = Vec::new();
      for byte in wire.as_bytes() {
        sse
          .push(&[*byte], |name, data| {
            received.push((name.to_owned(), data.to_owned()));
            Ok(false)
          })
          .unwrap();
      }
      assert_eq!(received, [("test".into(), "界\ntwo".into())]);
    }
  }
}
