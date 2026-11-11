//! Google Cloud Speech-to-Text v1 `speech:recognize` (API key auth, synchronous, ≤ 60 s audio).

use super::{check, pcm_bytes, send, Request, HTTP};
use crate::audio::SAMPLE_RATE;
use anyhow::Result;
use base64::Engine;
use serde_json::{json, Value};

pub async fn transcribe(key: &str, model: &str, req: &Request<'_>) -> Result<String> {
    let mut config = json!({
        "encoding": "LINEAR16",
        "sampleRateHertz": SAMPLE_RATE,
        "languageCode": req.language.unwrap_or("en-US"),
        "enableAutomaticPunctuation": true,
    });
    if !model.is_empty() && model != "default" {
        config["model"] = json!(model);
    }
    if !req.vocabulary.is_empty() {
        config["speechContexts"] = json!([{ "phrases": req.vocabulary }]);
    }
    let body = json!({
        "config": config,
        "audio": { "content": base64::engine::general_purpose::STANDARD.encode(pcm_bytes(req.audio)) }
    });
    let resp = send(HTTP.post("https://speech.googleapis.com/v1/speech:recognize").header("x-goog-api-key", key).json(&body)).await?;
    let v: Value = check("google", resp).await?.json().await?;
    Ok(parse(&v))
}

fn parse(v: &Value) -> String {
    v["results"]
        .as_array()
        .map(|rs| {
            rs.iter()
                .filter_map(|r| r.pointer("/alternatives/0/transcript").and_then(Value::as_str))
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_results() {
        let v = serde_json::json!({"results":[{"alternatives":[{"transcript":"hello"}]},{"alternatives":[{"transcript":" world"}]}]});
        assert_eq!(super::parse(&v), "hello world");
        assert_eq!(super::parse(&serde_json::json!({})), "");
    }
}
