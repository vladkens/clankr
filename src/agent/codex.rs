use std::path::PathBuf;

use reqwest::header::HeaderMap;
use serde_json::{Value, json};

use super::LIMIT;
use super::chat::{HISTORY, bearer, credentials_file};
use super::model::Settings;
use crate::{Error, Res};

pub const URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const SESSION: Error =
  Error::Provider("Missing or invalid Codex session; sign in with Codex or set CODEX_ACCESS_TOKEN");

pub fn body(settings: &Settings, input: &[Value]) -> Res<Vec<u8>> {
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

pub fn credentials() -> Res<HeaderMap> {
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

fn session(path: &std::path::Path, id: Option<&str>) -> Res<HeaderMap> {
  let data = credentials_file(path).map_err(|_| SESSION)?;
  let token = data["tokens"]["access_token"].as_str().ok_or(SESSION)?;
  headers(token, id.or_else(|| data["tokens"]["account_id"].as_str()))
}

fn headers(token: &str, id: Option<&str>) -> Res<HeaderMap> {
  let mut headers = bearer(token).map_err(|_| SESSION)?;
  if let Some(id) = id {
    headers.insert("ChatGPT-Account-Id", super::chat::sensitive(id).map_err(|_| SESSION)?);
  }
  Ok(headers)
}

#[cfg(test)]
mod tests {
  use std::fs::File;

  use super::super::chat::SESSION_LIMIT;
  use super::*;

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
}
