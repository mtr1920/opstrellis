//! AssemblyAI: batch (upload + transcript + poll) and Universal-Streaming v3 WebSocket.
//! https://www.assemblyai.com/docs/api-reference/streaming-api/streaming-api

use super::ws::{self, Message, Ws};
use super::{check, pcm_bytes, send, PartialFn, Request, HTTP};
use crate::audio::{to_wav, SAMPLE_RATE};
use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

const API: &str = "https://api.assemblyai.com/v2";

pub async fn transcribe(key: &str, model: &str, req: &Request<'_>) -> Result<String> {
    let up: Value = check("assemblyai", send(HTTP.post(format!("{API}/upload")).header("authorization", key).body(to_wav(req.audio))).await?)
        .await?
        .json()
        .await?;
    let upload_url = up["upload_url"].as_str().ok_or_else(|| anyhow!("assemblyai: upload failed"))?;
    let mut body = json!({ "audio_url": upload_url, "punctuate": true, "format_text": true });
    // Streaming model names ("universal-streaming…") aren't valid for batch; let the API pick its default.
    if !model.is_empty() && !model.contains("streaming") {
        body["speech_model"] = json!(model);
    }
    match req.language {
        Some(l) => body["language_code"] = json!(l),
        None => body["language_detection"] = json!(true),
    }
    if !req.vocabulary.is_empty() {
        body["keyterms_prompt"] = json!(req.vocabulary.iter().take(100).collect::<Vec<_>>());
    }
    let t: Value = check("assemblyai", send(HTTP.post(format!("{API}/transcript")).header("authorization", key).json(&body)).await?)
        .await?
        .json()
        .await?;
    let id = t["id"].as_str().ok_or_else(|| anyhow!("assemblyai: no transcript id"))?;
    for _ in 0..240 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let t: Value = check("assemblyai", send(HTTP.get(format!("{API}/transcript/{id}")).header("authorization", key)).await?)
            .await?
            .json()
            .await?;
        match t["status"].as_str() {
            Some("completed") => return Ok(t["text"].as_str().unwrap_or("").to_string()),
            Some("error") => return Err(anyhow!("assemblyai: {}", t["error"].as_str().unwrap_or("transcription failed"))),
            _ => {}
        }
    }
    Err(anyhow!("assemblyai: timed out"))
}

pub async fn connect(key: &str, model: &str, vocabulary: &[String]) -> Result<Ws> {
    let mut url = format!("wss://streaming.assemblyai.com/v3/ws?sample_rate={SAMPLE_RATE}&encoding=pcm_s16le&format_turns=true");
    if model.starts_with("universal-streaming") {
        url.push_str(&format!("&speech_model={model}"));
    }
    if !vocabulary.is_empty() {
        url.push_str(&format!("&keyterms_prompt={}", urlencoding::encode(&serde_json::to_string(&vocabulary)?)));
    }
    ws::connect(&url, &[("Authorization", key)]).await
}

pub async fn run(ws: Ws, mut audio: UnboundedReceiver<Vec<i16>>, on_partial: PartialFn) -> Result<String> {
    let (mut sink, mut source) = ws.split();
    let sender = tokio::spawn(async move {
        while let Some(chunk) = audio.recv().await {
            sink.send(Message::Binary(pcm_bytes(&chunk).into())).await?;
        }
        sink.send(Message::Text(r#"{"type":"Terminate"}"#.into())).await?;
        anyhow::Ok(sink)
    });
    // turn_order -> best transcript so far (formatted version replaces the raw one).
    let mut turns: BTreeMap<u64, String> = BTreeMap::new();
    while let Some(msg) = source.next().await {
        let msg = msg?;
        let Message::Text(t) = msg else {
            if let Message::Close(frame) = msg {
                ws::check_close(&frame)?;
                break;
            }
            continue;
        };
        let v: Value = serde_json::from_str(&t)?;
        match v["type"].as_str() {
            Some("Turn") => {
                let order = v["turn_order"].as_u64().unwrap_or(0);
                turns.insert(order, v["transcript"].as_str().unwrap_or("").trim().to_string());
                on_partial(join(&turns));
            }
            Some("Termination") => break,
            _ if v.get("error").is_some() => {
                return Err(anyhow!("assemblyai: {}", v["error"].as_str().unwrap_or("stream error")))
            }
            _ => {}
        }
    }
    ws::check_sender(sender).await?;
    Ok(join(&turns))
}

fn join(turns: &BTreeMap<u64, String>) -> String {
    turns.values().filter(|t| !t.is_empty()).cloned().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn streaming_protocol() {
        let script = vec![
            r#"{"type":"Begin","id":"x"}"#.to_string(),
            r#"{"type":"Turn","turn_order":0,"end_of_turn":false,"transcript":"hello wor"}"#.to_string(),
            r#"{"type":"Turn","turn_order":0,"end_of_turn":true,"transcript":"hello world"}"#.to_string(),
            r#"{"type":"Turn","turn_order":0,"end_of_turn":true,"turn_is_formatted":true,"transcript":"Hello world."}"#.to_string(),
            r#"{"type":"Turn","turn_order":1,"end_of_turn":true,"turn_is_formatted":true,"transcript":"Bye."}"#.to_string(),
            r#"{"type":"Termination"}"#.to_string(),
        ];
        let (url, peer) = ws::scripted_peer("Terminate", script).await;
        let socket = ws::connect(&url, &[("Authorization", "k")]).await.unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(run(socket, rx, std::sync::Arc::new(|_| {})));
        tx.send(vec![0i16; 800]).unwrap();
        drop(tx);
        assert_eq!(task.await.unwrap().unwrap(), "Hello world. Bye.");
        assert_eq!(peer.await.unwrap().0, 1600);
    }

    #[tokio::test]
    #[ignore]
    async fn assemblyai_live() {
        let key = std::env::var("ASSEMBLYAI_API_KEY").expect("ASSEMBLYAI_API_KEY");
        let audio = super::super::sample_audio();
        let text = transcribe(&key, "universal", &Request { audio: &audio, language: None, vocabulary: &["OpsTrellis".into()] }).await.unwrap();
        assert!(text.to_lowercase().contains("yellow lamps"), "{text}");
    }
}
