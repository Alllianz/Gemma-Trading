use crate::types::{GemmaResponse, BoxAction};

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
    
    // Si la directa falla, aplicamos parseo robusto caja por caja
    let v: serde_json::Value = serde_json::from_str(json_substring).ok()?;
    
    let analisis = v.get("analisis")
        .and_then(|a| a.as_str())
        .map(|s| s.to_string());

    let parse_box = |box_key: &str| -> BoxAction {
        let box_val = v.get(box_key)
            .or_else(|| v.get(&box_key.to_uppercase()))
            .or_else(|| v.get(&box_key.replace("_", "")));
        
        let mut accion = "FLAT".to_string();
        let mut cerrar = false;
        let mut apalancamiento = None;
        let mut stop_loss = None;

        if let Some(b) = box_val {
            if let Some(act) = b.get("accion").and_then(|a| a.as_str()) {
                accion = act.to_string();
            }
            if let Some(c) = b.get("cerrar") {
                if let Some(c_bool) = c.as_bool() {
                    cerrar = c_bool;
                } else if let Some(c_str) = c.as_str() {
                    cerrar = c_str.eq_ignore_ascii_case("true") || c_str == "1";
                } else if let Some(c_num) = c.as_i64() {
                    cerrar = c_num == 1;
                }
            }
            apalancamiento = b.get("apalancamiento").and_then(|a| a.as_f64());
            stop_loss = b.get("stop_loss").and_then(|s| {
                if s.is_null() {
                    None
                } else {
                    s.as_f64()
                }
            });
        }

        BoxAction {
            accion,
            cerrar,
            apalancamiento,
            stop_loss,
        }
    };

    let lt_box = parse_box("lt_box");
    let st_box = parse_box("st_box");

    Some(GemmaResponse {
        analisis,
        lt_box,
        st_box,
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
        "temperature": 0.1,
        "max_tokens": 1000,
        "frequency_penalty": 0.5,
        "presence_penalty": 0.5,
        "thinking_budget": 150,
        "stop": ["<think>", "\n*"]
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
