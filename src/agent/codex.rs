use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::model::{MODELS, Model, Settings};
use super::stream::{Sse, Stream};
use super::{Backend, Event, LIMIT, Prompt, Status, Usage};
use crate::{Error, Res};

const URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const SESSION_LIMIT: u64 = 1024 * 1024;
const SESSION: Error =
  Error::Provider("Missing or invalid Codex session; sign in with Codex or set CODEX_ACCESS_TOKEN");
const HISTORY: Error = Error::Provider("Conversation exceeds 16 MiB; use /clear to start again");

pub struct Codex {
  history: Vec<Value>,
  usage: Option<Usage>,
  running: Option<Running>,
  error: Option<Error>,
  url: String,
  auth: fn() -> Res<HeaderMap>,
}

struct Running {
  events: mpsc::Receiver<Event>,
  worker: JoinHandle<Res<Reply>>,
  cancel: Option<oneshot::Sender<()>>,
  prompt: Value,
  cancelled: bool,
}

struct Reply {
  output: Vec<Value>,
  usage: Option<Usage>,
}

impl Default for Codex {
  fn default() -> Self {
    Self {
      history: Vec::new(),
      usage: None,
      running: None,
      error: None,
      url: URL.into(),
      auth: credentials,
    }
  }
}

impl Codex {
  fn submit(&mut self, prompt: Prompt) -> Res<()> {
    if prompt.settings.model.provider != "OpenAI" {
      return Err(Error::Provider("Choose a Codex model with /model"));
    }
    if !prompt.skills.is_empty() {
      return Err(Error::Provider("Skills are not connected yet"));
    }
    let message = json!({"role": "user", "content": [{"type": "input_text", "text": prompt.text}]});
    let mut input = self.history.clone();
    input.push(message.clone());
    let body = body(&prompt.settings, input)?;
    let (sender, events) = mpsc::channel(64);
    let (cancel, cancelled) = oneshot::channel();
    let url = self.url.clone();
    let auth = self.auth;
    let worker = thread::Builder::new().name("codex".into()).spawn(move || {
      let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
      let result = runtime.block_on(async {
        tokio::select! {
          biased;
          _ = cancelled => Err(Error::Cancelled),
          reply = request(&url, body, auth, sender) => reply,
        }
      });
      // Dropping the request closes its socket. Do not wait for an OS DNS lookup.
      runtime.shutdown_background();
      result
    })?;
    self.running =
      Some(Running { events, worker, cancel: Some(cancel), prompt: message, cancelled: false });
    Ok(())
  }
}

impl Backend for Codex {
  fn models(&self) -> &'static [Model] {
    &MODELS[1..4]
  }

  fn settings(&self) -> Settings {
    let model = &self.models()[0];
    Settings { model, effort: model.default_effort, fast: false }
  }

  fn status(&self) -> Status<'_> {
    Status {
      mode: "codex",
      context: self.usage.map(|usage| (usage.input.saturating_add(usage.output), 272_000)),
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
      running.worker.join().unwrap_or(Err(Error::Provider("Codex worker stopped unexpectedly")));
    let result = if running.cancelled {
      Err(Error::Cancelled)
    } else {
      reply.and_then(|reply| {
        let mut history = self.history.clone();
        history.push(running.prompt);
        history.extend(reply.output);
        if serde_json::to_vec(&history).map_err(|_| HISTORY)?.len() > LIMIT {
          return Err(HISTORY);
        }
        self.history = history;
        self.usage = reply.usage;
        Ok(())
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
    self.history.clear();
    self.usage = None;
    self.error = None;
  }
}

impl Drop for Codex {
  fn drop(&mut self) {
    self.reset();
  }
}

fn body(settings: &Settings, input: Vec<Value>) -> Res<Vec<u8>> {
  let mut body = json!({
    "model": settings.model.name,
    "instructions": "Be concise, accurate and practical. Reply in the user's language.",
    "input": input, "stream": true, "store": false,
    "include": ["reasoning.encrypted_content"],
    "service_tier": if settings.fast { "priority" } else { "default" },
  });
  if let Some(effort) = settings.effort {
    body["reasoning"] = json!({"effort": effort.name()});
  }
  let body =
    serde_json::to_vec(&body).map_err(|_| Error::Provider("Cannot encode Codex request"))?;
  if body.len() > LIMIT {
    return Err(HISTORY);
  }
  Ok(body)
}

async fn request(
  url: &str,
  body: Vec<u8>,
  auth: fn() -> Res<HeaderMap>,
  events: mpsc::Sender<Event>,
) -> Res<Reply> {
  let _ = rustls::crypto::ring::default_provider().install_default();
  let client = reqwest::Client::builder()
    .redirect(reqwest::redirect::Policy::none())
    .connect_timeout(Duration::from_secs(30))
    .read_timeout(Duration::from_secs(120))
    .build()
    .map_err(|_| Error::Provider("Cannot initialize Codex connection"))?;
  let mut rep = client
    .post(url)
    .headers(auth()?)
    .header("OpenAI-Beta", "responses=experimental")
    .header("originator", "clankr")
    .header("Accept", "text/event-stream")
    .header("Content-Type", "application/json")
    .body(body)
    .send()
    .await
    .map_err(|_| Error::Provider("Codex request failed"))?;
  // Provider error bodies and transport errors can contain secrets. Never display them.
  match rep.status().as_u16() {
    401 => return Err(Error::Provider("Codex HTTP 401; renew the session in Codex")),
    429 => return Err(Error::Provider("Codex HTTP 429; rate limit reached, try again later")),
    status if !rep.status().is_success() => return Err(Error::Http(status)),
    _ => {}
  }
  let mut sse = Sse::default();
  let mut stream = Stream::default();
  while let Some(chunk) =
    rep.chunk().await.map_err(|_| Error::Provider("Codex stream read failed"))?
  {
    let mut batch = Vec::new();
    let result = sse.push(&chunk, |name, data| stream.event(name, data, &mut batch));
    // Deliver received text even if a later event in this same read is malformed.
    for event in batch {
      events.send(event).await.map_err(|_| Error::Cancelled)?;
    }
    if result? {
      return Ok(Reply { usage: stream.usage, output: stream.output() });
    }
  }
  Err(Error::Provider("Codex stream ended before a completed response"))
}

fn credentials() -> Res<HeaderMap> {
  let id = std::env::var_os("CODEX_ACCOUNT_ID")
    .map(|x| x.into_string().map_err(|_| SESSION))
    .transpose()?;
  if let Some(token) = std::env::var_os("CODEX_ACCESS_TOKEN") {
    return headers(&token.into_string().map_err(|_| SESSION)?, id.as_deref());
  }
  let dir = std::env::var_os("CODEX_HOME")
    .map(PathBuf::from)
    .or_else(|| std::env::var_os("HOME").map(|dir| PathBuf::from(dir).join(".codex")))
    .ok_or(SESSION)?;
  session(&dir.join("auth.json"), id.as_deref())
}

fn session(path: &Path, id: Option<&str>) -> Res<HeaderMap> {
  let file = File::open(path).map_err(|_| SESSION)?;
  let meta = file.metadata().map_err(|_| SESSION)?;
  if !meta.is_file() || meta.len() > SESSION_LIMIT {
    return Err(SESSION);
  }
  let mut data = Vec::new();
  file.take(SESSION_LIMIT + 1).read_to_end(&mut data).map_err(|_| SESSION)?;
  if data.len() as u64 > SESSION_LIMIT {
    return Err(SESSION);
  }
  let data: Value = serde_json::from_slice(&data).map_err(|_| SESSION)?;
  let token = data["tokens"]["access_token"].as_str().ok_or(SESSION)?;
  headers(token, id.or_else(|| data["tokens"]["account_id"].as_str()))
}

fn headers(token: &str, id: Option<&str>) -> Res<HeaderMap> {
  fn value(text: &str) -> Res<HeaderValue> {
    if text.trim().is_empty() || text.bytes().any(|byte| byte.is_ascii_control()) {
      return Err(SESSION);
    }
    let mut value = HeaderValue::from_str(text).map_err(|_| SESSION)?;
    value.set_sensitive(true);
    Ok(value)
  }
  let mut headers = HeaderMap::new();
  value(token)?;
  headers.insert("Authorization", value(&format!("Bearer {token}"))?);
  if let Some(id) = id {
    headers.insert("ChatGPT-Account-Id", value(id)?);
  }
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
    headers("fixture-token", Some("fixture-account"))
  }

  fn client(url: String) -> Codex {
    Codex { url, auth, history: Vec::new(), usage: None, running: None, error: None }
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
    serde_json::from_slice(&body).unwrap()
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

  fn server(replies: Vec<(u16, String)>) -> (Codex, sync::Receiver<Value>, JoinHandle<()>) {
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

  fn send(codex: &mut Codex, text: &str) {
    let settings = codex.settings();
    codex.start(Prompt { text: text.into(), skills: Vec::new(), settings });
  }

  fn finish(codex: &mut Codex) -> (String, Option<Usage>, Res<()>) {
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
    assert_eq!(codex.status().context, Some((45, 272_000)));
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
    assert!(codex.history.is_empty() && codex.status().context.is_none());
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
    for expected in ["last received chunk", "partial EOF", "", "", ""] {
      send(&mut codex, "failed question");
      let (text, _, result) = finish(&mut codex);
      assert_eq!(text, expected);
      assert!(!result.unwrap_err().to_string().contains("private"));
      assert!(codex.history.is_empty());
      requests.recv().unwrap();
    }
    send(&mut codex, "new question");
    finish(&mut codex).2.unwrap();
    assert_eq!(requests.recv().unwrap()["input"].as_array().unwrap().len(), 1);
    server.join().unwrap();
  }

  #[test]
  fn cancelling_or_dropping_closes_a_stalled_stream_and_stops_its_worker() {
    for drop in [false, true] {
      let listener = TcpListener::bind("127.0.0.1:0").unwrap();
      let url = format!("http://{}", listener.local_addr().unwrap());
      let (ready, received) = sync::channel();
      let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        read(&mut socket);
        write!(
          socket,
          "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{}",
          delta("partial")
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
      send(&mut codex, "question");
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
  fn session_is_read_only_bounded_and_sensitive() {
    let path = std::env::temp_dir().join(format!("clankr-auth-{}.json", fastrand::u64(..)));
    let data = br#"{"tokens":{"access_token":"fixture-token","account_id":"fixture-account"}}"#;
    std::fs::write(&path, data).unwrap();
    let values = session(&path, None).unwrap();
    assert!(values["Authorization"].is_sensitive());
    assert!(values["ChatGPT-Account-Id"].is_sensitive());
    assert_eq!(std::fs::read(&path).unwrap(), data);
    assert_eq!(session(&path, Some("override")).unwrap()["ChatGPT-Account-Id"], "override");
    for token in ["", " ", "private\r\ninjected: value"] {
      assert!(!headers(token, None).unwrap_err().to_string().contains("private"));
    }
    std::fs::write(&path, b"private-malformed-json").unwrap();
    assert!(session(&path, None).is_err());
    File::options().write(true).open(&path).unwrap().set_len(SESSION_LIMIT + 1).unwrap();
    assert!(session(&path, None).is_err());
    std::fs::remove_file(&path).unwrap();
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
}
