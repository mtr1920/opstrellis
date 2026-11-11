//! Deepgram: batch `POST /v1/listen` and realtime WebSocket `/v1/listen`.
//! https://developers.deepgram.com/reference/speech-to-text-api/listen

use super::ws::{self, Message, Ws};
use super::{check, pcm_bytes, send, PartialFn, Request, HTTP};
use crate::audio::{to_wav, SAMPLE_RATE};
use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;

fn params(model: &str, language: Option<&str>, vocabulary: &[String], streaming: bool) -> Vec<(String, String)> {
    let mut p = vec![
        ("model".to_string(), model.to_string()),
        ("smart_format".into(), "true".into()),
        ("punctuate".into(), "true".into()),
    ];
    match language {
        Some(l) => p.push(("language".into(), l.into())),
        // Streaming can't auto-detect; nova-3 "multi" handles code-switching across major languages.
        None if streaming => p.push(("language".into(), if model.starts_with("nova-3") { "multi".into() } else { "en".into() })),
        None => p.push(("detect_language".into(), "true".into())),
    }
    // nova-3 uses keyterm prompting; older models take boosted keywords.
    let hint = if model.starts_with("nova-3") { "keyterm" } else { "keywords" };
    p.extend(vocabulary.iter().take(100).map(|w| (hint.to_string(), w.clone())));
    if streaming {
        p.extend([
            ("encoding".into(), "linear16".into()),
            ("sample_rate".into(), SAMPLE_RATE.to_string()),
            ("channels".into(), "1".into()),
            ("interim_results".into(), "true".into()),
        ]);
    }
    p
}

pub async fn transcribe(key: &str, model: &str, req: &Request<'_>) -> Result<String> {
    let resp = send(
        HTTP.post("https://api.deepgram.com/v1/listen")
            .query(&params(model, req.language, req.vocabulary, false))
            .header("Authorization", format!("Token {key}"))
            .header("Content-Type", "audio/wav")
            .body(to_wav(req.audio)),
    )
    .await?;
    let v: Value = check("deepgram", resp).await?.json().await?;
    parse_batch(&v)
}

fn parse_batch(v: &Value) -> Result<String> {
    v.pointer("/results/channels/0/alternatives/0/transcript")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| anyhow!("deepgram: unexpected response"))
}

pub async fn connect(key: &str, model: &str, language: Option<&str>, vocabulary: &[String]) -> Result<Ws> {
    let query: String = params(model, language, vocabulary, true)
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    ws::connect(&format!("wss://api.deepgram.com/v1/listen?{query}"), &[("Authorization", &format!("Token {key}"))]).await
}

pub async fn run(ws: Ws, mut audio: UnboundedReceiver<Vec<i16>>, on_partial: PartialFn) -> Result<String> {
    let (mut sink, mut source) = ws.split();
    let sender = tokio::spawn(async move {
        while let Some(chunk) = audio.recv().await {
            sink.send(Message::Binary(pcm_bytes(&chunk).into())).await?;
        }
        // Flush remaining audio and close; Deepgram sends the last results before closing.
        sink.send(Message::Text(r#"{"type":"CloseStream"}"#.into())).await?;
        anyhow::Ok(sink)
    });
    let mut finals: Vec<String> = Vec::new();
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
            Some("Results") => {
                let text = v.pointer("/channel/alternatives/0/transcript").and_then(Value::as_str).unwrap_or("").trim();
                if v["is_final"].as_bool().unwrap_or(false) {
                    if !text.is_empty() {
                        finals.push(text.to_string());
                    }
                    on_partial(finals.join(" "));
                } else if !text.is_empty() {
                    on_partial(format!("{} {text}", finals.join(" ")).trim().to_string());
                }
            }
            Some("Metadata") => break,
            Some("Error") => return Err(anyhow!("deepgram: {}", v["description"].as_str().unwrap_or("stream error"))),
            _ => {}
        }
    }
    ws::check_sender(sender).await?;
    Ok(finals.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_params_and_parses() {
        let p = params("nova-3", None, &["OpsTrellis".into()], true);
        assert!(p.contains(&("language".into(), "multi".into())));
        assert!(p.contains(&("keyterm".into(), "OpsTrellis".into())));
        let p = params("nova-2", Some("de"), &["OpsTrellis".into()], false);
        assert!(p.contains(&("keywords".into(), "OpsTrellis".into())) && p.contains(&("language".into(), "de".into())));
        let v = serde_json::json!({"results":{"channels":[{"alternatives":[{"transcript":"hello world","confidence":0.99}]}]}});
        assert_eq!(parse_batch(&v).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn streaming_protocol() {
        let script = vec![
            r#"{"type":"Results","is_final":false,"channel":{"alternatives":[{"transcript":"hello"}]}}"#.to_string(),
            r#"{"type":"Results","is_final":true,"channel":{"alternatives":[{"transcript":"Hello world."}]}}"#.to_string(),
            r#"{"type":"Results","is_final":true,"channel":{"alternatives":[{"transcript":"How are you?"}]}}"#.to_string(),
            r#"{"type":"Metadata","duration":1.0}"#.to_string(),
        ];
        let (url, peer) = ws::scripted_peer("CloseStream", script).await;
        let socket = ws::connect(&url, &[("Authorization", "Token k")]).await.unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let partials = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let p2 = partials.clone();
        let task = tokio::spawn(run(socket, rx, std::sync::Arc::new(move |t| p2.lock().unwrap().push(t))));
        tx.send(vec![1i16; 1600]).unwrap();
        tx.send(vec![2i16; 1600]).unwrap();
        drop(tx);
        assert_eq!(task.await.unwrap().unwrap(), "Hello world. How are you?");
        let (bytes, texts) = peer.await.unwrap();
        assert_eq!(bytes, 6400, "all PCM16 audio forwarded");
        assert!(texts.last().unwrap().contains("CloseStream"));
        assert_eq!(partials.lock().unwrap().first().unwrap(), "hello");
    }

    #[tokio::test]
    async fn error_close_is_a_failure_not_a_partial_success() {
        let script = vec![
            r#"{"type":"Results","is_final":true,"channel":{"alternatives":[{"transcript":"First half of the sentence"}]}}"#.to_string(),
            "CLOSE:1011".to_string(),
        ];
        let (url, _peer) = ws::scripted_peer("CloseStream", script).await;
        let socket = ws::connect(&url, &[]).await.unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(run(socket, rx, std::sync::Arc::new(|_| {})));
        tx.send(vec![0i16; 160]).unwrap();
        drop(tx);
        assert!(task.await.unwrap().is_err(), "a 1011 close must trigger the batch fallback");
    }

    /// Live test: DEEPGRAM_API_KEY=... cargo test deepgram_live -- --ignored
    #[tokio::test]
    #[ignore]
    async fn deepgram_live() {
        let key = std::env::var("DEEPGRAM_API_KEY").expect("DEEPGRAM_API_KEY");
        let audio = super::super::sample_audio();
        let req = Request { audio: &audio, language: Some("en"), vocabulary: &[] };
        let text = transcribe(&key, "nova-3", &req).await.unwrap();
        assert!(text.to_lowercase().contains("yellow lamps"), "{text}");
    }
}
