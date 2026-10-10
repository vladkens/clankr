use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue};
use rustix::fs::{Mode, OFlags};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::model::{LIVE_MODELS, Model, Settings};
use super::stream::{Sse, Stream as CodexStream};
use super::{Backend, Event, LIMIT, Prompt, Status, Usage, claude, codex};
use crate::{Error, Res};

type Credentials = fn() -> Res<HeaderMap>;

pub const SESSION_LIMIT: u64 = 1024 * 1024;
pub const HISTORY: Error =
  Error::Provider("Conversation exceeds 16 MiB; use /clear to start again");

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Provider {
  #[default]
  Codex,
  Claude,
}

impl Provider {
  fn name(self) -> &'static str {
    match self {
      Self::Codex => "codex",
      Self::Claude => "claude",
    }
  }
}

#[derive(Clone)]
struct Turn {
  user: String,
  provider: Provider,
  output: Vec<Value>,
}

impl Turn {
  fn text(&self) -> String {
    self
      .output
      .iter()
      .flat_map(|item| {
        if self.provider == Provider::Codex {
          item["content"].as_array().map(Vec::as_slice).unwrap_or_default()
        } else {
          std::slice::from_ref(item)
        }
      })
      .filter_map(|part| match part["type"].as_str()? {
        "text" | "output_text" => part["text"].as_str(),
        "refusal" => part["refusal"].as_str(),
        _ => None,
      })
      .collect()
  }

  fn size(&self) -> usize {
    json!({"user": self.user, "provider": self.provider.name(), "output": self.output})
      .to_string()
      .len()
      + 1
  }
}

pub struct Chat {
  history: Arc<Vec<Turn>>,
  usage: Option<Usage>,
  running: Option<Running>,
  error: Option<Error>,
  provider: Provider,
  #[cfg(test)]
  endpoint: Option<(String, Credentials)>,
}

struct Running {
  events: mpsc::Receiver<Event>,
  worker: JoinHandle<Res<Completed>>,
  cancel: Option<oneshot::Sender<()>>,
  cancelled: bool,
}

struct Completed {
  history: Arc<Vec<Turn>>,
  usage: Option<Usage>,
}

pub struct Reply {
  pub output: Vec<Value>,
  pub usage: Option<Usage>,
}

impl Default for Chat {
  fn default() -> Self {
    Self {
      history: Arc::default(),
      usage: None,
      running: None,
      error: None,
      provider: Provider::Codex,
      #[cfg(test)]
      endpoint: None,
    }
  }
}

impl Chat {
  fn submit(&mut self, prompt: Prompt) -> Res<()> {
    let provider = match prompt.settings.model.provider {
      "OpenAI" => Provider::Codex,
      "Anthropic" => Provider::Claude,
      _ => return Err(Error::Provider("Choose a connected model with /model")),
    };
    if !self.models().contains(prompt.settings.model) {
      return Err(Error::Provider("Choose a connected model with /model"));
    }
    if !prompt.skills.is_empty() {
      return Err(Error::Provider("Skills are not connected yet"));
    }
    let history = Arc::clone(&self.history);
    let (sender, events) = mpsc::channel(64);
    let (cancel, cancelled) = oneshot::channel();
    let (url, auth): (_, Credentials) = match provider {
      Provider::Codex => (codex::URL.to_owned(), codex::credentials),
      Provider::Claude => (claude::URL.to_owned(), claude::credentials),
    };
    #[cfg(test)]
    let (url, auth) = self.endpoint.clone().unwrap_or((url, auth));
    let worker = thread::Builder::new().name(provider.name().into()).spawn(move || {
      let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
      let result = runtime.block_on(async {
        tokio::select! {
          biased;
          _ = cancelled => Err(Error::Cancelled),
          reply = async {
            let mut input = Self::input(&history, provider);
            input.push(user_message(provider, &prompt.text));
            let body = match provider {
              Provider::Codex => codex::body(&prompt.settings, &input)?,
              Provider::Claude => claude::body(&prompt.settings, &input)?,
            };
            drop(input);
            let reply = request(provider, &url, body, auth, prompt.settings.fast, sender).await?;
            let turn = Turn { user: prompt.text, provider, output: reply.output };
            if history.iter().map(Turn::size).sum::<usize>() + turn.size() + 2 > LIMIT {
              return Err(HISTORY);
            }
            let mut history = history.as_ref().clone();
            history.push(turn);
            Ok(Completed { history: Arc::new(history), usage: reply.usage })
          } => reply,
        }
      });
      // Dropping the request closes its socket. Do not wait for an OS DNS lookup.
      runtime.shutdown_background();
      result
    })?;
    if self.provider != provider {
      self.usage = None;
    }
    self.provider = provider;
    self.running = Some(Running { events, worker, cancel: Some(cancel), cancelled: false });
    Ok(())
  }

  fn input(history: &[Turn], provider: Provider) -> Vec<Value> {
    let mut input = Vec::new();
    for turn in history {
      input.push(user_message(provider, &turn.user));
      match provider {
        Provider::Codex if turn.provider == provider => input.extend(turn.output.clone()),
        Provider::Codex => {
          let text = turn.text();
          if !text.is_empty() {
            input.push(
              json!({"role": "assistant", "content": [{"type": "output_text", "text": text}]}),
            );
          }
        }
        Provider::Claude => {
          let content: Vec<_> = if turn.provider == provider {
            turn
              .output
              .iter()
              .filter(|part| part["type"] != "text" || part["text"] != "")
              .cloned()
              .collect()
          } else {
            let text = turn.text();
            if text.is_empty() { Vec::new() } else { vec![json!({"type": "text", "text": text})] }
          };
          if !content.is_empty() {
            input.push(json!({"role": "assistant", "content": content}));
          }
        }
      }
    }
    input
  }
}

impl Backend for Chat {
  fn models(&self) -> &'static [Model] {
    LIVE_MODELS
  }

  fn settings(&self) -> Settings {
    let model = &self.models()[0];
    Settings { model, effort: model.default_effort, fast: false }
  }

  fn status(&self, model: &Model) -> Status<'_> {
    let provider = if model.provider == "Anthropic" { Provider::Claude } else { Provider::Codex };
    Status {
      mode: provider.name(),
      context: self.usage.filter(|_| self.provider == provider).map(|usage| {
        (
          usage.input.saturating_add(usage.output),
          match provider {
            Provider::Codex => 272_000,
            Provider::Claude => 1_000_000,
          },
        )
      }),
    }
  }

  fn start(&mut self, prompt: Prompt) {
    debug_assert!(self.running.is_none());
    self.error = self.submit(prompt).err();
  }

  fn poll(&mut self) -> Option<Event> {
    if let Some(error) = self.error.take() {
      return Some(Event::Finished(Err(error)));
    }
    let running = self.running.as_mut()?;
    match running.events.try_recv() {
      Ok(event) => return Some(event),
      Err(mpsc::error::TryRecvError::Empty) => return None,
      Err(mpsc::error::TryRecvError::Disconnected) => {}
    }
    if !running.worker.is_finished() {
      return None;
    }
    let running = self.running.take().unwrap();
    let reply =
      running.worker.join().unwrap_or(Err(Error::Provider("Provider worker stopped unexpectedly")));
    let result = if running.cancelled {
      Err(Error::Cancelled)
    } else {
      reply.map(|reply| {
        self.history = reply.history;
        self.usage = reply.usage;
      })
    };
    Some(Event::Finished(result))
  }

  fn cancel(&mut self) {
    if let Some(running) = &mut self.running {
      running.cancelled = true;
      if let Some(cancel) = running.cancel.take() {
        let _ = cancel.send(());
      }
    }
  }

  fn reset(&mut self) {
    self.cancel();
    if let Some(running) = self.running.take() {
      let _ = running.worker.join();
    }
    self.history = Arc::default();
    self.usage = None;
    self.error = None;
  }
}

impl Drop for Chat {
  fn drop(&mut self) {
    self.reset();
  }
}

fn user_message(provider: Provider, text: &str) -> Value {
  let kind = if provider == Provider::Codex { "input_text" } else { "text" };
  json!({"role": "user", "content": [{"type": kind, "text": text}]})
}

async fn request(
  provider: Provider,
  url: &str,
  body: Vec<u8>,
  auth: Credentials,
  fast: bool,
  events: mpsc::Sender<Event>,
) -> Res<Reply> {
  let _ = rustls::crypto::ring::default_provider().install_default();
  let client = reqwest::Client::builder()
    .redirect(reqwest::redirect::Policy::none())
    .connect_timeout(Duration::from_secs(30))
    .read_timeout(Duration::from_secs(120))
    .build()
    .map_err(|_| Error::Provider("Cannot initialize provider connection"))?;
  let req = client.post(url).headers(auth()?);
  let req = match provider {
    Provider::Codex => {
      req.header("OpenAI-Beta", "responses=experimental").header("originator", "clankr")
    }
    Provider::Claude => req
      .header("anthropic-version", "2023-06-01")
      .header(
        "anthropic-beta",
        if fast {
          "claude-code-20250219,oauth-2025-04-20,fast-mode-2026-02-01"
        } else {
          "claude-code-20250219,oauth-2025-04-20"
        },
      )
      .header("x-app", "cli")
      .header("User-Agent", "claude-cli/2.1.280"),
  };
  let mut rep = req
    .header("Accept", "text/event-stream")
    .header("Content-Type", "application/json")
    .body(body)
    .send()
    .await
    .map_err(|_| Error::Provider("Provider request failed"))?;
  // Provider error bodies and transport errors can contain secrets. Never display them.
  match rep.status().as_u16() {
    401 => {
      return Err(Error::Provider(match provider {
        Provider::Codex => "Codex HTTP 401; renew the session in Codex",
        Provider::Claude => "Claude HTTP 401; renew the session in Pi",
      }));
    }
    429 => return Err(Error::Provider("Provider HTTP 429; rate limit reached, try again later")),
    status if !rep.status().is_success() => return Err(Error::Http(status)),
    _ => {}
  }
  let mut sse = Sse::default();
  let mut codex = CodexStream::default();
  let mut claude = claude::Stream::default();
  while let Some(chunk) =
    rep.chunk().await.map_err(|_| Error::Provider("Provider stream read failed"))?
  {
    let mut batch = Vec::new();
    let result = sse.push(&chunk, |name, data| match provider {
      Provider::Codex => codex.event(name, data, &mut batch),
      Provider::Claude => claude.event(name, data, &mut batch),
    });
    // Deliver received text even if a later event in this same read is malformed.
    for event in batch {
      events.send(event).await.map_err(|_| Error::Cancelled)?;
    }
    if result? {
      return Ok(match provider {
        Provider::Codex => Reply { usage: codex.usage, output: codex.output() },
        Provider::Claude => claude.reply(),
      });
    }
  }
  Err(Error::Provider("Provider stream ended before a completed response"))
}

pub fn credentials_file(path: &Path) -> Res<Value> {
  let file =
    rustix::fs::open(path, OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC, Mode::empty())
      .map(File::from)
      .map_err(std::io::Error::from)?;
  let meta = file.metadata()?;
  if !meta.is_file() || meta.len() > SESSION_LIMIT {
    return Err(Error::Provider("Invalid credentials file"));
  }
  let mut data = Vec::new();
  file.take(SESSION_LIMIT + 1).read_to_end(&mut data)?;
  if data.len() as u64 > SESSION_LIMIT {
    return Err(Error::Provider("Invalid credentials file"));
  }
  serde_json::from_slice(&data).map_err(|_| Error::Provider("Invalid credentials file"))
}

pub fn sensitive(text: &str) -> Res<HeaderValue> {
  if text.trim().is_empty() || text.bytes().any(|byte| byte.is_ascii_control()) {
    return Err(Error::Provider("Invalid credentials"));
  }
  let mut value =
    HeaderValue::from_str(text).map_err(|_| Error::Provider("Invalid credentials"))?;
  value.set_sensitive(true);
  Ok(value)
}

pub fn bearer(token: &str) -> Res<HeaderMap> {
  sensitive(token)?;
  let mut headers = HeaderMap::new();
  headers.insert("Authorization", sensitive(&format!("Bearer {token}"))?);
  Ok(headers)
}

#[cfg(test)]
mod tests {
  use std::io::Write;
  use std::net::{TcpListener, TcpStream};
  use std::sync::mpsc as sync;
  use std::time::Instant;

  use super::*;

  fn auth() -> Res<HeaderMap> {
    bearer("fixture-token")
  }

  fn client(url: String) -> Chat {
    let mut chat = Chat::default();
    chat.endpoint = Some((url, auth));
    chat
  }

  fn read(socket: &mut TcpStream) -> Value {
    socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut data = Vec::new();
    while !data.ends_with(b"\r\n\r\n") {
      let mut byte = [0];
      socket.read_exact(&mut byte).unwrap();
      data.push(byte[0]);
      assert!(data.len() < 8192);
    }
    let head = std::str::from_utf8(&data).unwrap();
    assert!(head.contains("Bearer fixture-token"));
    let size: usize = head
      .lines()
      .find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
      })
      .unwrap();
    assert!(size <= LIMIT);
    let mut body = vec![0; size];
    socket.read_exact(&mut body).unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    if body["model"].as_str().unwrap().starts_with("claude-") {
      assert!(head.contains("anthropic-version: 2023-06-01"));
      assert!(head.contains("anthropic-beta: claude-code-20250219,oauth-2025-04-20"));
      assert!(head.contains("x-app: cli"));
      assert!(head.contains("user-agent: claude-cli/2.1.280"));
      assert_eq!(head.contains("fast-mode-2026-02-01"), body["speed"] == "fast");
    }
    body
  }

  fn event(dat: Value) -> String {
    format!("data: {dat}\n\n")
  }

  fn delta(text: &str) -> String {
    event(
      json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "item_id": "msg_0", "delta": text}),
    )
  }

  fn answer(text: &str) -> String {
    delta(text)
      + &event(json!({"type": "response.completed", "response": {
        "status": "completed", "output": [{"type": "message", "id": "msg_0", "role": "assistant", "phase": "final_answer", "content": [{"type": "output_text", "text": text}]}],
        "usage": {"input_tokens": 42, "output_tokens": 3}
      }}))
  }

  fn server(replies: Vec<(u16, String)>) -> (Chat, sync::Receiver<Value>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (send, requests) = sync::channel();
    let worker = thread::spawn(move || {
      for (status, body) in replies {
        let (mut socket, _) = listener.accept().unwrap();
        send.send(read(&mut socket)).unwrap();
        write!(socket, "HTTP/1.1 {status} Test\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
      }
    });
    (client(url), requests, worker)
  }

  fn send(codex: &mut Chat, text: &str) {
    let settings = codex.settings();
    codex.start(Prompt { text: text.into(), skills: Vec::new(), settings });
  }

  fn finish(codex: &mut Chat) -> (String, Option<Usage>, Res<()>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let (mut text, mut usage) = (String::new(), None);
    loop {
      match codex.poll() {
        Some(Event::Text(value)) => text.push_str(&value),
        Some(Event::Usage(value)) => usage = Some(value),
        Some(Event::Finished(result)) => {
          assert!(codex.poll().is_none());
          return (text, usage, result);
        }
        _ => {
          assert!(Instant::now() < deadline, "backend did not finish");
          thread::sleep(Duration::from_millis(1));
        }
      }
    }
  }

  #[test]
  fn two_turns_replay_completed_items_and_clear_resets_context() {
    let (mut codex, requests, server) =
      server(vec![(200, answer("界 hello")), (200, answer("next"))]);
    send(&mut codex, "first");
    let (text, usage, result) = finish(&mut codex);
    result.unwrap();
    assert_eq!(text, "界 hello");
    assert_eq!(usage, Some(Usage { input: 42, output: 3 }));
    assert_eq!(codex.status(&LIVE_MODELS[0]).context, Some((45, 272_000)));
    assert_eq!(codex.status(&LIVE_MODELS[3]).mode, "claude");
    assert!(codex.status(&LIVE_MODELS[3]).context.is_none());
    let first = requests.recv().unwrap();
    assert_eq!(first["model"], "gpt-6.1-sol");
    assert_eq!(first["reasoning"]["effort"], "low");
    assert_eq!(first["service_tier"], "default");
    assert_eq!(first["store"], false);
    send(&mut codex, "second");
    finish(&mut codex).2.unwrap();
    let second = requests.recv().unwrap();
    assert_eq!(second["input"][0], first["input"][0]);
    assert_eq!(second["input"][1]["phase"], "final_answer");
    assert_eq!(second["input"][1]["content"][0]["text"], "界 hello");
    assert_eq!(second["input"][2]["content"][0]["text"], "second");
    codex.reset();
    assert!(codex.history.is_empty() && codex.status(&LIVE_MODELS[0]).context.is_none());
    server.join().unwrap();
  }

  #[test]
  fn failures_keep_the_last_chunk_redact_details_and_do_not_commit_history() {
    let failed = delta("last received chunk")
      + &event(json!({"type": "response.failed", "error": "private-provider-details"}));
    let replies = vec![
      (200, failed),
      (200, delta("partial EOF")),
      (401, "private-token".into()),
      (429, "private-token".into()),
      (503, "private-token".into()),
      (200, answer("recovered")),
    ];
    let (mut codex, requests, server) = server(replies);
    codex.history =
      Arc::new(vec![Turn { user: "saved".into(), provider: Provider::Codex, output: Vec::new() }]);
    codex.usage = Some(Usage { input: 10, output: 2 });
    let history = Arc::clone(&codex.history);
    for expected in ["last received chunk", "partial EOF", "", "", ""] {
      send(&mut codex, "failed question");
      let (text, _, result) = finish(&mut codex);
      assert_eq!(text, expected);
      assert!(!result.unwrap_err().to_string().contains("private"));
      assert!(Arc::ptr_eq(&codex.history, &history));
      assert_eq!(codex.usage, Some(Usage { input: 10, output: 2 }));
      requests.recv().unwrap();
    }
    send(&mut codex, "new question");
    finish(&mut codex).2.unwrap();
    assert_eq!(requests.recv().unwrap()["input"].as_array().unwrap().len(), 2);
    server.join().unwrap();
  }

  #[test]
  fn history_limits_and_cancellation_after_completion_preserve_the_previous_turn() {
    for provider in [Provider::Codex, Provider::Claude] {
      for oversized in [false, true] {
        let response = if oversized { "x".repeat(2048) } else { "next".into() };
        let (wire, send): (_, fn(&mut Chat, &str)) = match provider {
          Provider::Codex => (answer(&response), send),
          Provider::Claude => (claude_answer(&response, "end_turn"), send_claude),
        };
        let (mut chat, requests, server) = server(vec![(200, wire)]);
        let text = "x".repeat(if oversized { LIMIT - 1024 } else { 1 });
        chat.history = Arc::new(vec![Turn { user: text, provider, output: Vec::new() }]);
        chat.provider = provider;
        chat.usage = Some(Usage { input: 10, output: 2 });
        let history = Arc::clone(&chat.history);
        send(&mut chat, "next");
        if !oversized {
          let deadline = Instant::now() + Duration::from_secs(5);
          while !chat.running.as_ref().unwrap().worker.is_finished() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
          }
          chat.cancel();
        }
        let error = finish(&mut chat).2.unwrap_err();
        assert_eq!(
          error.to_string(),
          if oversized { HISTORY } else { Error::Cancelled }.to_string()
        );
        assert!(Arc::ptr_eq(&chat.history, &history));
        assert_eq!(chat.usage, Some(Usage { input: 10, output: 2 }));
        assert!(requests.recv().is_ok());
        server.join().unwrap();

        send(&mut chat, &"x".repeat(LIMIT));
        assert_eq!(finish(&mut chat).2.unwrap_err().to_string(), HISTORY.to_string());
        assert!(Arc::ptr_eq(&chat.history, &history));
        assert_eq!(chat.usage, Some(Usage { input: 10, output: 2 }));
      }
    }
  }

  #[test]
  fn a_credentials_fifo_is_rejected_without_waiting_for_a_writer() {
    let path = std::env::temp_dir().join(format!("clankr-auth-{}.fifo", fastrand::u64(..)));
    assert!(std::process::Command::new("mkfifo").arg(&path).status().unwrap().success());
    let file = path.clone();
    let (send, received) = sync::channel();
    let worker = thread::spawn(move || send.send(credentials_file(&file).is_err()).unwrap());
    let rejected = received.recv_timeout(Duration::from_secs(1));
    // Unblock a regressed blocking open before joining, so a failure cannot hang the suite.
    let writer = rejected.is_err().then(|| {
      rustix::fs::open(&path, OFlags::RDWR | OFlags::NONBLOCK | OFlags::CLOEXEC, Mode::empty())
        .unwrap()
    });
    worker.join().unwrap();
    drop(writer);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(rejected, Ok(true));
  }

  #[test]
  fn cancelling_or_dropping_closes_a_stalled_stream_and_stops_its_worker() {
    for (claude, drop) in [(false, false), (false, true), (true, false), (true, true)] {
      let listener = TcpListener::bind("127.0.0.1:0").unwrap();
      let url = format!("http://{}", listener.local_addr().unwrap());
      let (ready, received) = sync::channel();
      let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        read(&mut socket);
        write!(
          socket,
          "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{}",
          if claude { claude_partial("partial") } else { delta("partial") }
        )
        .unwrap();
        ready.send(()).unwrap();
        // Cancellation must close the socket without waiting for another provider event.
        let mut buf = [0];
        match socket.read(&mut buf) {
          Ok(0) => {}
          Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
          other => panic!("connection stayed open: {other:?}"),
        }
      });
      let mut codex = client(url);
      if claude {
        send_claude(&mut codex, "question")
      } else {
        send(&mut codex, "question")
      }
      received.recv_timeout(Duration::from_secs(5)).unwrap();
      if drop {
        std::mem::drop(codex);
      } else {
        codex.cancel();
        codex.cancel();
        assert!(matches!(finish(&mut codex).2, Err(Error::Cancelled)));
        assert!(codex.history.is_empty());
      }
      server.join().unwrap();
    }
  }

  #[test]
  fn cancellation_unblocks_a_full_event_queue() {
    let wire = delta("x").repeat(128);
    let (mut codex, requests, server) = server(vec![(200, wire)]);
    send(&mut codex, "question");
    requests.recv_timeout(Duration::from_secs(5)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while codex.running.as_ref().unwrap().events.len() < 64 {
      assert!(Instant::now() < deadline);
      thread::sleep(Duration::from_millis(1));
    }
    codex.cancel();
    let (text, _, result) = finish(&mut codex);
    assert_eq!(text, "x".repeat(64));
    assert!(matches!(result, Err(Error::Cancelled)));
    assert!(codex.history.is_empty());
    server.join().unwrap();
  }

  fn claude_partial(text: &str) -> String {
    event(
      json!({"type": "message_start", "message": {"role": "assistant", "content": [], "usage": {"input_tokens": 12, "cache_read_input_tokens": 4, "output_tokens": 1}}}),
    ) + &event(
      json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": text}}),
    )
  }

  fn claude_answer(text: &str, reason: &str) -> String {
    claude_partial(text)
      + &event(json!({"type": "content_block_stop", "index": 0}))
      + &event(
        json!({"type": "message_delta", "delta": {"stop_reason": reason}, "usage": {"output_tokens": 7}}),
      )
      + &event(json!({"type": "message_stop"}))
  }

  fn send_claude(chat: &mut Chat, text: &str) {
    let model = &LIVE_MODELS[3];
    chat.start(Prompt {
      text: text.into(),
      skills: Vec::new(),
      settings: Settings { model, effort: model.default_effort, fast: true },
    });
  }

  #[test]
  fn switching_providers_replays_text_and_restores_native_reasoning() {
    let codex_output = json!([
      {"type": "reasoning", "id": "r0", "summary": [], "encrypted_content": "codex-opaque"},
      {"type": "message", "id": "m0", "role": "assistant", "phase": "final_answer", "content": [{"type": "refusal", "refusal": "first reply"}]}
    ]);
    let codex = event(
      json!({"type": "response.completed", "response": {"status": "completed", "output": codex_output}}),
    );
    let claude = event(
      json!({"type": "message_start", "message": {"role": "assistant", "content": []}}),
    ) + &event(
      json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "thought", "signature": "claude-signature"}}),
    ) + &event(json!({"type": "content_block_stop", "index": 0}))
      + &event(
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": "second reply"}}),
      )
      + &event(json!({"type": "content_block_stop", "index": 1}))
      + &event(json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}))
      + &event(json!({"type": "message_stop"}));
    let (mut chat, requests, server) = server(vec![
      (200, codex),
      (200, claude),
      (200, answer("third reply")),
      (200, claude_answer("fourth reply", "end_turn")),
    ]);
    send(&mut chat, "first question");
    assert_eq!(finish(&mut chat).0, "first reply");
    let first = requests.recv().unwrap();
    send_claude(&mut chat, "second question");
    assert_eq!(finish(&mut chat).0, "second reply");
    let second = requests.recv().unwrap();
    assert_eq!(second["messages"][0]["content"][0]["text"], "first question");
    assert_eq!(second["messages"][1]["content"], json!([{"type": "text", "text": "first reply"}]));
    assert!(!second.to_string().contains("codex-opaque"));
    send(&mut chat, "third question");
    finish(&mut chat).2.unwrap();
    let third = requests.recv().unwrap();
    assert_eq!(third["input"][0], first["input"][0]);
    assert_eq!(third["input"][1], codex_output[0]);
    assert_eq!(third["input"][2], codex_output[1]);
    assert_eq!(third["input"][4]["content"][0]["text"], "second reply");
    assert!(!third.to_string().contains("claude-signature"));
    send_claude(&mut chat, "fourth question");
    let (text, usage, result) = finish(&mut chat);
    result.unwrap();
    assert_eq!(text, "fourth reply");
    assert_eq!(usage, Some(Usage { input: 16, output: 7 }));
    assert_eq!(chat.status(&LIVE_MODELS[3]).context, Some((23, 1_000_000)));
    let fourth = requests.recv().unwrap();
    assert_eq!(fourth["messages"][0], second["messages"][0]);
    assert_eq!(fourth["messages"][1], second["messages"][1]);
    assert_eq!(fourth["messages"][3]["content"][0]["signature"], "claude-signature");
    assert_eq!(fourth["messages"][3]["content"][1]["text"], "second reply");
    assert_eq!(chat.history.len(), 4);
    server.join().unwrap();
  }

  #[test]
  fn claude_failures_keep_received_text_and_do_not_commit_failed_turns() {
    let failed =
      claude_partial("last chunk") + &event(json!({"type": "error", "error": "private details"}));
    let (mut chat, requests, server) = server(vec![
      (200, failed),
      (200, claude_partial("partial EOF")),
      (401, "private token".into()),
      (429, "private token".into()),
      (503, "private token".into()),
      (200, claude_answer("truncated", "max_tokens")),
      (200, claude_answer("refusal", "refusal")),
    ]);
    chat.history =
      Arc::new(vec![Turn { user: "saved".into(), provider: Provider::Claude, output: Vec::new() }]);
    chat.provider = Provider::Claude;
    chat.usage = Some(Usage { input: 10, output: 2 });
    let history = Arc::clone(&chat.history);
    for expected in ["last chunk", "partial EOF", "", "", ""] {
      send_claude(&mut chat, "failed question");
      let (text, _, result) = finish(&mut chat);
      assert_eq!(text, expected);
      assert!(!result.unwrap_err().to_string().contains("private"));
      assert!(Arc::ptr_eq(&chat.history, &history));
      assert_eq!(chat.usage, Some(Usage { input: 10, output: 2 }));
      requests.recv().unwrap();
    }
    send_claude(&mut chat, "new question");
    finish(&mut chat).2.unwrap();
    assert_eq!(requests.recv().unwrap()["messages"].as_array().unwrap().len(), 2);
    send_claude(&mut chat, "another question");
    finish(&mut chat).2.unwrap();
    assert_eq!(requests.recv().unwrap()["messages"][2]["content"][0]["text"], "truncated");
    assert_eq!(chat.history.len(), 3);
    server.join().unwrap();
  }

  #[test]
  fn empty_completed_turns_do_not_create_invalid_claude_messages() {
    let reasoning =
      json!({"type": "reasoning", "id": "r0", "summary": [], "encrypted_content": "opaque"});
    let empty_claude =
      event(json!({"type": "message_start", "message": {"role": "assistant", "content": []}}))
        + &event(json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}))
        + &event(json!({"type": "message_stop"}));
    let replies = [
      (
        false,
        event(
          json!({"type": "response.completed", "response": {"status": "completed", "output": []}}),
        ),
      ),
      (
        false,
        event(
          json!({"type": "response.completed", "response": {"status": "completed", "output": [reasoning]}}),
        ),
      ),
      (true, empty_claude),
      (true, claude_answer("", "end_turn")),
    ];
    for (claude, reply) in replies {
      let (mut chat, requests, server) =
        server(vec![(200, reply), (200, claude_answer("next", "end_turn"))]);
      if claude {
        send_claude(&mut chat, "first")
      } else {
        send(&mut chat, "first")
      }
      let (text, _, result) = finish(&mut chat);
      result.unwrap();
      assert!(text.is_empty());
      requests.recv().unwrap();
      send_claude(&mut chat, "second");
      finish(&mut chat).2.unwrap();
      let next = requests.recv().unwrap();
      assert_eq!(
        next["messages"],
        json!([
          {"role": "user", "content": [{"type": "text", "text": "first"}]},
          {"role": "user", "content": [{"type": "text", "text": "second"}]},
        ])
      );
      assert_eq!(chat.history.len(), 2);
      server.join().unwrap();
    }
    let thinking = json!({"type": "thinking", "thinking": "", "signature": "signed"});
    let mut chat = Chat::default();
    chat.history = Arc::new(vec![Turn {
      user: "question".into(),
      provider: Provider::Claude,
      output: vec![thinking.clone(), json!({"type": "text", "text": ""})],
    }]);
    assert_eq!(Chat::input(&chat.history, Provider::Claude)[1]["content"], json!([thinking]));
    assert_eq!(Chat::input(&chat.history, Provider::Codex).len(), 1);
    assert_eq!(chat.history[0].output.len(), 2);
  }
}
