use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use reqwest::header::HeaderMap;
use serde_json::{Value, json};

use super::chat::{HISTORY, Reply, bearer, credentials_file};
use super::model::Settings;
use super::{Event, LIMIT, Phase, Usage};
use crate::{Error, Res};

pub const URL: &str = "https://api.anthropic.com/v1/messages";
const SESSION: Error =
  Error::Provider("Missing or invalid Claude session; sign in with Pi or set CLAUDE_ACCESS_TOKEN");
const INVALID: Error = Error::Provider("Invalid Claude stream");
const PART_LIMIT: usize = 4096;

pub fn body(settings: &Settings, messages: &[Value]) -> Res<Vec<u8>> {
  // Preserve the existing OAuth compatibility identity from the reference transport.
  let mut body = json!({
    "model": settings.model.name, "max_tokens": 128_000, "stream": true,
    "system": [
      {"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."},
      {"type": "text", "text": "Be concise, accurate and practical. Reply in the user's language."}
    ],
    "messages": messages, "thinking": {"type": "adaptive"},
  });
  if let Some(effort) = settings.effort {
    body["output_config"] = json!({"effort": effort.name()});
  }
  if settings.fast {
    body["speed"] = json!("fast");
  }
  let body =
    serde_json::to_vec(&body).map_err(|_| Error::Provider("Cannot encode Claude request"))?;
  if body.len() > LIMIT {
    return Err(HISTORY);
  }
  Ok(body)
}

pub fn credentials() -> Res<HeaderMap> {
  if let Some(token) = std::env::var_os("CLAUDE_ACCESS_TOKEN") {
    return bearer(&token.into_string().map_err(|_| SESSION)?).map_err(|_| SESSION);
  }
  let dir = std::env::var_os("PI_CODING_AGENT_DIR")
    .map(PathBuf::from)
    .or_else(|| std::env::var_os("HOME").map(|dir| PathBuf::from(dir).join(".pi/agent")))
    .ok_or(SESSION)?;
  session(&dir.join("auth.json"))
}

fn session(path: &Path) -> Res<HeaderMap> {
  let data = credentials_file(path).map_err(|_| SESSION)?;
  if data["anthropic"]["type"] != "oauth" {
    return Err(SESSION);
  }
  bearer(data["anthropic"]["access"].as_str().ok_or(SESSION)?).map_err(|_| SESSION)
}

struct Block {
  value: Value,
  sent: usize,
  done: bool,
}

#[derive(Default)]
pub struct Stream {
  blocks: BTreeMap<usize, Block>,
  bytes: usize,
  counts: [Option<u64>; 4],
  usage: Option<Usage>,
  started: bool,
  reason: Option<String>,
}

impl Stream {
  fn reserve(&mut self, bytes: usize) -> Res<()> {
    if bytes > LIMIT.saturating_sub(self.bytes) {
      return Err(Error::ResponseTooLarge);
    }
    self.bytes += bytes;
    Ok(())
  }

  fn block(&mut self, index: usize, value: &Value) -> Res<()> {
    if index >= PART_LIMIT || self.blocks.contains_key(&index) || self.reason.is_some() {
      return Err(INVALID);
    }
    let value = match value["type"].as_str() {
      Some("text") => json!({"type": "text", "text": text(value, "text")?}),
      Some("thinking") => json!({"type": "thinking", "thinking": text(value, "thinking")?,
        "signature": value.get("signature").map(|_| text(value, "signature")).transpose()?.unwrap_or("")}),
      Some("redacted_thinking") => {
        json!({"type": "redacted_thinking", "data": text(value, "data")?})
      }
      _ => {
        return Err(Error::Provider(
          "Claude requested unsupported output; tools are not connected yet",
        ));
      }
    };
    self.reserve(value.to_string().len())?;
    self.blocks.insert(index, Block { value, sent: 0, done: false });
    Ok(())
  }

  fn observe(&mut self, usage: &Value, events: &mut Vec<Event>) -> Res<()> {
    // Input is uncached + cache creation + cache read. Nested cache breakdowns overlap.
    for (i, key) in
      ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens"]
        .iter()
        .enumerate()
    {
      if let Some(value) = usage.get(key).filter(|x| !x.is_null()) {
        self.counts[i] = Some(value.as_u64().ok_or(INVALID)?);
      }
    }
    if let (Some(input), Some(output)) = (self.counts[0], self.counts[3]) {
      let input = input
        .checked_add(self.counts[1].unwrap_or(0))
        .and_then(|n| n.checked_add(self.counts[2].unwrap_or(0)))
        .ok_or(INVALID)?;
      let usage = Usage { input, output };
      if self.usage != Some(usage) {
        self.usage = Some(usage);
        events.push(Event::Usage(usage));
      }
    }
    Ok(())
  }

  pub fn event(&mut self, name: &str, data: &str, events: &mut Vec<Event>) -> Res<bool> {
    let dat: Value = serde_json::from_str(data).map_err(|_| INVALID)?;
    let kind = dat["type"].as_str().unwrap_or(name);
    if kind.is_empty() || (!name.is_empty() && name != kind) {
      return Err(INVALID);
    }
    if !self.started && !matches!(kind, "message_start" | "ping" | "error") {
      return Err(INVALID);
    }
    let mut done = false;
    match kind {
      "message_start" => {
        if self.started
          || dat["message"]["role"] != "assistant"
          || !dat["message"]["stop_reason"].is_null()
        {
          return Err(INVALID);
        }
        let parts = dat["message"]["content"].as_array().ok_or(INVALID)?;
        for (index, part) in parts.iter().enumerate() {
          self.block(index, part)?;
        }
        self.started = true;
        self.observe(&dat["message"]["usage"], events)?;
      }
      "content_block_start" => {
        self.block(index(&dat)?, &dat["content_block"])?;
      }
      "content_block_delta" => {
        if self.reason.is_some() {
          return Err(INVALID);
        }
        let index = index(&dat)?;
        let delta = &dat["delta"];
        let (kind, field) = match text(delta, "type")? {
          "text_delta" => ("text", "text"),
          "thinking_delta" => ("thinking", "thinking"),
          "signature_delta" => ("thinking", "signature"),
          _ => return Err(Error::Provider("Unsupported Claude content delta")),
        };
        let value = text(delta, field)?;
        self.reserve(value.len())?;
        let block = self.blocks.get_mut(&index).ok_or(INVALID)?;
        if block.done || block.value["type"] != kind {
          return Err(INVALID);
        }
        let Value::String(current) = &mut block.value[field] else { return Err(INVALID) };
        current.push_str(value);
        if field == "thinking" {
          events.push(Event::Phase(Phase::Thinking));
        }
      }
      "content_block_stop" => {
        let block = self.blocks.get_mut(&index(&dat)?).ok_or(INVALID)?;
        if block.done
          || (block.value["type"] == "thinking" && text(&block.value, "signature")?.is_empty())
        {
          return Err(INVALID);
        }
        block.done = true;
      }
      "message_delta" => {
        if let Some(reason) = dat["delta"].get("stop_reason").filter(|x| !x.is_null()) {
          let reason = reason.as_str().ok_or(INVALID)?;
          if !matches!(
            reason,
            "end_turn"
              | "stop_sequence"
              | "refusal"
              | "max_tokens"
              | "model_context_window_exceeded"
          ) {
            return Err(Error::Provider("Claude returned an unsupported stop reason"));
          }
          if self.reason.as_deref().is_some_and(|prev| prev != reason) {
            return Err(INVALID);
          }
          self.reason = Some(reason.into());
        }
        self.observe(&dat["usage"], events)?;
      }
      "message_stop" => {
        let reason = self.reason.as_deref().ok_or(INVALID)?;
        for (next, (&index, block)) in self.blocks.iter().enumerate() {
          if index != next || !block.done {
            return Err(INVALID);
          }
        }
        let notice = match reason {
          "max_tokens" => Some("Claude reached the output limit; the partial response was saved."),
          "model_context_window_exceeded" => {
            Some("Claude reached the context limit; the partial response was saved.")
          }
          _ => None,
        };
        if let Some(notice) = notice {
          events.push(Event::Notice(notice));
        }
        done = true;
      }
      "error" => return Err(Error::Provider("Claude stream failed")),
      "ping" => {}
      _ => return Err(INVALID),
    }
    // Later indexed blocks cannot overtake earlier text in the terminal.
    for (next, (&index, block)) in self.blocks.iter_mut().enumerate() {
      if index != next {
        break;
      }
      if block.value["type"] == "text" {
        let text = text(&block.value, "text")?;
        if block.sent < text.len() {
          events.push(Event::Text(text[block.sent..].into()));
          block.sent = text.len();
        }
      }
      if !block.done {
        break;
      }
    }
    Ok(done)
  }

  pub fn reply(self) -> Reply {
    Reply {
      output: self.blocks.into_values().map(|block| block.value).collect(),
      usage: self.usage,
    }
  }
}

fn text<'a>(dat: &'a Value, key: &str) -> Res<&'a str> {
  dat[key].as_str().ok_or(INVALID)
}

fn index(dat: &Value) -> Res<usize> {
  dat["index"].as_u64().filter(|&n| n < PART_LIMIT as u64).map(|n| n as usize).ok_or(INVALID)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::agent::model::{Effort, LIVE_MODELS};
  use crate::agent::stream::Sse;

  fn start() -> Value {
    json!({"type": "message_start", "message": {"role": "assistant", "content": [], "stop_reason": null,
      "usage": {"input_tokens": 7, "cache_creation_input_tokens": 2, "cache_read_input_tokens": 8,
        "cache_creation": {"ephemeral_5m_input_tokens": 2}, "output_tokens": 1}}})
  }

  fn event(stream: &mut Stream, dat: Value, events: &mut Vec<Event>) -> Res<bool> {
    stream.event(dat["type"].as_str().unwrap(), &dat.to_string(), events)
  }

  fn text_start(index: usize, text: &str) -> Value {
    json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": text}})
  }

  fn stop(index: usize) -> Value {
    json!({"type": "content_block_stop", "index": index})
  }

  fn reason(reason: &str) -> Value {
    json!({"type": "message_delta", "delta": {"stop_reason": reason}, "usage": {"output_tokens": 11}})
  }

  fn text(events: &[Event]) -> String {
    events
      .iter()
      .filter_map(|event| match event {
        Event::Text(text) => Some(text.as_str()),
        _ => None,
      })
      .collect()
  }

  #[test]
  fn indexed_stream_preserves_thinking_and_counts_cache_usage_once() {
    let values = [
      start(),
      text_start(2, "界"),
      json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
      json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "private thought"}}),
      json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "signed-"}}),
      json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "thought"}}),
      stop(0),
      json!({"type": "content_block_start", "index": 1, "content_block": {"type": "redacted_thinking", "data": "opaque"}}),
      stop(1),
      json!({"type": "content_block_delta", "index": 2, "delta": {"type": "text_delta", "text": " 🦀"}}),
      stop(2),
      json!({"type": "message_delta", "delta": {}, "usage": {"input_tokens": 10, "cache_read_input_tokens": null}}),
      reason("end_turn"),
      reason("end_turn"),
      json!({"type": "message_stop"}),
    ];
    let wire = values
      .iter()
      .map(|dat| format!("event: {}\r\ndata: {dat}\r\n\r\n", dat["type"].as_str().unwrap()))
      .collect::<String>();
    let (mut sse, mut stream, mut events) = (Sse::default(), Stream::default(), Vec::new());
    let mut done = false;
    for byte in wire.bytes() {
      done |= sse.push(&[byte], |name, data| stream.event(name, data, &mut events)).unwrap();
    }
    assert!(done);
    assert_eq!(text(&events), "界 🦀");
    let usage: Vec<_> = events
      .iter()
      .filter_map(|event| match event {
        Event::Usage(usage) => Some(*usage),
        _ => None,
      })
      .collect();
    assert_eq!(
      usage,
      [
        Usage { input: 17, output: 1 },
        Usage { input: 20, output: 1 },
        Usage { input: 20, output: 11 }
      ]
    );
    let reply = stream.reply();
    assert_eq!(
      reply.output[0],
      json!({"type": "thinking", "thinking": "private thought", "signature": "signed-thought"})
    );
    assert_eq!(reply.output[1], json!({"type": "redacted_thinking", "data": "opaque"}));
    assert_eq!(reply.output[2], json!({"type": "text", "text": "界 🦀"}));
    assert_eq!(reply.usage, Some(Usage { input: 20, output: 11 }));
  }

  #[test]
  fn completed_refusals_and_truncations_keep_text_but_only_truncations_warn() {
    for stop_reason in
      ["end_turn", "refusal", "stop_sequence", "max_tokens", "model_context_window_exceeded"]
    {
      let (mut stream, mut events) = (Stream::default(), Vec::new());
      for dat in [start(), text_start(0, "received"), stop(0), reason(stop_reason)] {
        assert!(!event(&mut stream, dat, &mut events).unwrap());
      }
      assert!(event(&mut stream, json!({"type": "message_stop"}), &mut events).unwrap());
      assert_eq!(text(&events), "received");
      assert_eq!(
        events.iter().any(|event| matches!(event, Event::Notice(_))),
        matches!(stop_reason, "max_tokens" | "model_context_window_exceeded")
      );
      assert_eq!(stream.reply().output[0]["text"], "received");
    }
  }

  #[test]
  fn malformed_or_unfinished_blocks_fail_without_exposing_payloads() {
    let cases = [
      vec![text_start(0, "before start")],
      vec![start(), text_start(0, "hello"), text_start(0, "private conflict")],
      vec![start(), text_start(0, "hello"), reason("end_turn"), json!({"type": "message_stop"})],
      vec![
        start(),
        text_start(1, "missing zero"),
        stop(1),
        reason("end_turn"),
        json!({"type": "message_stop"}),
      ],
      vec![start(), text_start(0, "hello"), stop(0), reason("private unsupported")],
      vec![
        start(),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "private unsigned"}}),
        stop(0),
      ],
      vec![
        start(),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "name": "private tool"}}),
      ],
      vec![start(), json!({"type": "error", "error": {"message": "private details"}})],
    ];
    for values in cases {
      let (mut stream, mut events) = (Stream::default(), Vec::new());
      let result =
        values.into_iter().try_for_each(|dat| event(&mut stream, dat, &mut events).map(|_| ()));
      assert!(!result.unwrap_err().to_string().contains("private"));
    }
    let (mut stream, mut events) = (Stream::default(), Vec::new());
    assert!(stream.event("error", &start().to_string(), &mut events).is_err());
    stream.reserve(LIMIT).unwrap();
    assert!(matches!(stream.reserve(1), Err(Error::ResponseTooLarge)));
    assert!(event(&mut Stream::default(), text_start(PART_LIMIT, ""), &mut events).is_err());
  }

  #[test]
  fn absent_usage_is_not_estimated_and_overflow_is_rejected() {
    let (mut stream, mut events) = (Stream::default(), Vec::new());
    stream.observe(&json!({"output_tokens": 5}), &mut events).unwrap();
    assert!(stream.usage.is_none() && events.is_empty());
    stream.observe(&json!({"input_tokens": 0}), &mut events).unwrap();
    assert_eq!(stream.usage, Some(Usage { input: 0, output: 5 }));
    assert!(
      stream
        .observe(&json!({"input_tokens": u64::MAX, "cache_read_input_tokens": 1}), &mut events)
        .is_err()
    );
  }

  #[test]
  fn request_maps_current_effort_and_fast_without_codex_fields() {
    for model in &LIVE_MODELS[3..] {
      let settings = Settings { model, effort: Some(Effort::High), fast: model.fast };
      let data: Value = serde_json::from_slice(
        &body(&settings, &[json!({"role": "user", "content": "hi"})]).unwrap(),
      )
      .unwrap();
      assert_eq!(data["model"], model.name);
      assert_eq!(data["output_config"]["effort"], "high");
      assert_eq!(data["thinking"]["type"], "adaptive");
      assert_eq!(data["max_tokens"], 128_000);
      assert_eq!(data.get("speed").is_some(), model.fast);
      assert!(data.get("service_tier").is_none());
      assert!(data.get("reasoning").is_none());
    }
  }

  #[test]
  fn session_is_read_only_and_invalid_tokens_are_redacted() {
    let path = std::env::temp_dir().join(format!("clankr-claude-auth-{}.json", fastrand::u64(..)));
    let data = br#"{"anthropic":{"type":"oauth","access":"fixture-token"}}"#;
    std::fs::write(&path, data).unwrap();
    let headers = session(&path).unwrap();
    assert_eq!(headers["Authorization"], "Bearer fixture-token");
    assert!(headers["Authorization"].is_sensitive());
    assert_eq!(std::fs::read(&path).unwrap(), data);
    for data in [
      r#"{"anthropic":{"type":"api_key","access":"private"}}"#,
      r#"{"anthropic":{"type":"oauth","access":"private\n"}}"#,
      "private malformed",
      r#"{"anthropic":{"type":"oauth","access":" "}}"#,
    ] {
      std::fs::write(&path, data).unwrap();
      assert!(!session(&path).unwrap_err().to_string().contains("private"));
    }
    std::fs::remove_file(&path).unwrap();
  }
}
