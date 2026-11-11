//! ElevenLabs Scribe batch speech-to-text.

use super::{check, send, Request, HTTP};
use crate::audio::to_wav;
use anyhow::{anyhow, Result};
use serde_json::Value;

pub async fn transcribe(key: &str, model: &str, req: &Request<'_>) -> Result<String> {
    let file = reqwest::multipart::Part::bytes(to_wav(req.audio)).file_name("audio.wav").mime_str("audio/wav")?;
    let mut form = reqwest::multipart::Form::new()
        .part("file", file)
        .text("model_id", if model.is_empty() { "scribe_v1".to_string() } else { model.to_string() })
        .text("tag_audio_events", "false");
    if let Some(l) = req.language {
        form = form.text("language_code", l.to_string());
    }
    let resp = send(HTTP.post("https://api.elevenlabs.io/v1/speech-to-text").header("xi-api-key", key).multipart(form)).await?;
    let v: Value = check("elevenlabs", resp).await?.json().await?;
    v["text"].as_str().map(String::from).ok_or_else(|| anyhow!("elevenlabs: response has no text"))
}
