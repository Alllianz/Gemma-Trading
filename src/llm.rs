use crate::types::GemmaResponse;

pub fn parse_gemma_response(text: &str) -> Option<GemmaResponse> {
    let cleaned = text.trim();
    let cleaned = if cleaned.starts_with("```json") {
        cleaned.strip_prefix("```json").unwrap_or(cleaned)
    } else if cleaned.starts_with("```") {
        cleaned.strip_prefix("```").unwrap_or(cleaned)
    } else {
        cleaned
    };
    let cleaned = if cleaned.ends_with("```") {
        cleaned.strip_suffix("```").unwrap_or(cleaned)
    } else {
        cleaned
    };
    let cleaned = cleaned.trim();
    serde_json::from_str(cleaned).ok()
}

pub async fn call_gemma(
    client: &reqwest::Client,
    api_url: &str,
    api_token: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut normalized_url = api_url.trim().to_string();
    if !normalized_url.ends_with("/v1/chat/completions") {
        if normalized_url.ends_with('/') {
            normalized_url.push_str("v1/chat/completions");
        } else {
            normalized_url.push_str("/v1/chat/completions");
        }
    }

    let body = serde_json::json!({
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_prompt }
        ],
        "temperature": 0.2
    });

    let resp = client.post(&normalized_url)
        .bearer_auth(api_token)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let err_text = resp.text().await?;
        return Err(format!("Error en respuesta de LM Studio ({}): {}", normalized_url, err_text).into());
    }

    let json_resp: serde_json::Value = resp.json().await?;
    let content = match json_resp["choices"][0]["message"]["content"].as_str() {
        Some(c) => c.to_string(),
        None => {
            return Err(format!(
                "No se encontró el contenido del mensaje. Respuesta completa de LM Studio: {}",
                json_resp
            ).into());
        }
    };

    Ok(content)
}
