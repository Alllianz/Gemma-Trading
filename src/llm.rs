use crate::types::GemmaResponse;

pub fn parse_gemma_response(text: &str) -> Option<GemmaResponse> {
    let start_idx = text.find('{')?;
    let end_idx = text.rfind('}')?;
    if end_idx <= start_idx {
        return None;
    }
    let json_substring = &text[start_idx..=end_idx];
    
    // Primero intentamos la deserialización directa
    if let Ok(parsed) = serde_json::from_str::<GemmaResponse>(json_substring) {
        return Some(parsed);
    }
    
    // Si la directa falla, aplicamos parseo robusto campo por campo
    let v: serde_json::Value = serde_json::from_str(json_substring).ok()?;
    
    let accion = v.get("accion")
        .and_then(|a| a.as_str())
        .map(|s| s.to_string())?; // Requerido

    let analisis = v.get("analisis")
        .and_then(|a| a.as_str())
        .map(|s| s.to_string());

    let confianza = v.get("confianza")
        .and_then(|c| c.as_u64())
        .map(|n| n as u32);

    let apalancamiento = v.get("apalancamiento")
        .and_then(|a| a.as_f64());

    let riesgo = v.get("riesgo")
        .and_then(|r| r.as_f64());

    let cerrar_posiciones = v.get("cerrar_posiciones").and_then(|cp| {
        if let Some(arr) = cp.as_array() {
            let mut indices = Vec::new();
            for item in arr {
                if let Some(n) = item.as_u64() {
                    indices.push(n as usize);
                } else if let Some(s) = item.as_str() {
                    let clean_s: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
                    if let Ok(n) = clean_s.parse::<usize>() {
                        indices.push(n);
                    }
                }
            }
            Some(indices)
        } else {
            None
        }
    });

    let stop_losses = v.get("stop_losses").and_then(|sl| {
        if let Some(arr) = sl.as_array() {
            let mut prices = Vec::new();
            for item in arr {
                if item.is_null() {
                    prices.push(None);
                } else if let Some(n) = item.as_f64() {
                    prices.push(Some(n));
                } else if let Some(obj) = item.as_object() {
                    let price = obj.get("precio")
                        .or_else(|| obj.get("price"))
                        .or_else(|| obj.get("val"))
                        .or_else(|| obj.get("value"))
                        .or_else(|| obj.get("stop_loss"))
                        .or_else(|| obj.get("take_profit"))
                        .and_then(|p| p.as_f64());
                    prices.push(price);
                } else {
                    prices.push(None);
                }
            }
            Some(prices)
        } else {
            None
        }
    });

    let take_profits = v.get("take_profits").and_then(|tp| {
        if let Some(arr) = tp.as_array() {
            let mut prices = Vec::new();
            for item in arr {
                if item.is_null() {
                    prices.push(None);
                } else if let Some(n) = item.as_f64() {
                    prices.push(Some(n));
                } else if let Some(obj) = item.as_object() {
                    let price = obj.get("precio")
                        .or_else(|| obj.get("price"))
                        .or_else(|| obj.get("val"))
                        .or_else(|| obj.get("value"))
                        .or_else(|| obj.get("take_profit"))
                        .or_else(|| obj.get("stop_loss"))
                        .and_then(|p| p.as_f64());
                    prices.push(price);
                } else {
                    prices.push(None);
                }
            }
            Some(prices)
        } else {
            None
        }
    });

    Some(GemmaResponse {
        analisis,
        accion,
        cerrar_posiciones,
        stop_losses,
        take_profits,
        confianza,
        apalancamiento,
        riesgo,
    })
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
        "model": "local-model",
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_prompt }
        ],
        "temperature": 0.2,
        "max_tokens": 2048
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
            println!("⚠️ LM Studio devolvió una estructura sin contenido. Respuesta completa: {:?}", json_resp);
            return Err(format!(
                "No se encontró el contenido del mensaje. Respuesta completa de LM Studio: {}",
                json_resp
            ).into());
        }
    };

    if content.trim().is_empty() {
        println!("⚠️ LM Studio devolvió un texto vacío. Respuesta completa de la API: {:?}", json_resp);
    }

    Ok(content)
}
