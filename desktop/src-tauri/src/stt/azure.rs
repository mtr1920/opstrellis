//! Azure AI Speech short-audio REST API (≤ 60 s). Region comes from the provider entry.

use super::{check, send, Request, HTTP};
use crate::audio::to_wav;
use anyhow::{anyhow, Result};
use serde_json::Value;

pub async fn transcribe(key: &str, region: &str, req: &Request<'_>) -> Result<String> {
    // The region becomes part of the host name; anything else could send the key elsewhere.
    anyhow::ensure!(valid_region(region), "azure: invalid region {region:?} (expected e.g. westeurope)");
    let url = format!("https://{region}.stt.speech.microsoft.com/speech/recognition/conversation/cognitiveservices/v1");
    let lang = match req.language {
        Some(l) if l.contains('-') => l.to_string(),
        Some(l) => format!("{l}-{}", default_region(l)),
        None => "en-US".into(),
    };
    let resp = send(
        HTTP.post(url)
            .query(&[("language", lang.as_str()), ("format", "simple")])
            .header("Ocp-Apim-Subscription-Key", key)
            .header("Content-Type", "audio/wav; codecs=audio/pcm; samplerate=16000")
            .body(to_wav(req.audio)),
    )
    .await?;
    let v: Value = check("azure", resp).await?.json().await?;
    match v["RecognitionStatus"].as_str() {
        Some("Success") => Ok(v["DisplayText"].as_str().unwrap_or("").to_string()),
        Some("NoMatch") | Some("InitialSilenceTimeout") => Ok(String::new()),
        other => Err(anyhow!("azure: {}", other.unwrap_or("unexpected response"))),
    }
}

fn valid_region(region: &str) -> bool {
    !region.is_empty() && region.chars().all(|c| c.is_ascii_alphanumeric())
}

fn default_region(lang: &str) -> String {
    match lang {
        "en" => "US".into(),
        "pt" => "BR".into(),
        "zh" => "CN".into(),
        "ja" => "JP".into(),
        "ko" => "KR".into(),
        "hi" => "IN".into(),
        "sv" => "SE".into(),
        "da" => "DK".into(),
        "uk" => "UA".into(),
        "cs" => "CZ".into(),
        "el" => "GR".into(),
        "he" => "IL".into(),
        "ar" => "SA".into(),
        // de-DE, fr-FR, es-ES, it-IT, nl-NL, pl-PL, ru-RU, tr-TR, ...
        other => other.to_uppercase(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn validates_region() {
        assert!(super::valid_region("westeurope"));
        assert!(super::valid_region("eastus2"));
        assert!(super::valid_region("WestEurope"));
        assert!(!super::valid_region(""));
        assert!(!super::valid_region("evil.com/#"));
        assert!(!super::valid_region("evil.com"));
    }
}
