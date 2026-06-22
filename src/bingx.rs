use crate::types::BingXAccountInfo;

fn calculate_signature(secret: &str, query: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(query.as_bytes());
    let result = mac.finalize();
    hex::encode(result.into_bytes())
}

fn normalize_symbol(symbol: &str) -> String {
    if symbol.contains('-') {
        symbol.to_string()
    } else {
        symbol.replace("USDT", "-USDT")
    }
}

pub async fn test_api_connection(
    client: &reqwest::Client,
    api_key: &str,
    api_secret: &str,
    use_testnet: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let base_url = if use_testnet {
        "https://open-api-vst.bingx.com"
    } else {
        "https://open-api.bingx.com"
    };
    let timestamp = chrono::Utc::now().timestamp_millis();
    let query = format!("timestamp={}", timestamp);
    let signature = calculate_signature(api_secret, &query);
    let url = format!("{}/openApi/swap/v2/user/balance?{}&signature={}", base_url, query, signature);

    let res = client
        .get(&url)
        .header("X-BX-APIKEY", api_key)
        .send()
        .await?;

    if res.status().is_success() {
        let text = res.text().await?;
        let json: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(code) = json.get("code").and_then(|c| c.as_i64()) {
            if code == 0 {
                Ok(())
            } else {
                let msg = json.get("msg").and_then(|m| m.as_str()).unwrap_or("Error desconocido");
                Err(format!("Error BingX (código {}): {}", code, msg).into())
            }
        } else {
            Err("Formato de respuesta de BingX no válido".into())
        }
    } else {
        let err_text = res.text().await?;
        Err(format!("Error HTTP en BingX: {}", err_text).into())
    }
}

pub async fn set_leverage(
    client: &reqwest::Client,
    api_key: &str,
    api_secret: &str,
    symbol: &str,
    leverage: u32,
    position_side: &str, // "LONG" o "SHORT" en modo Hedge
    use_testnet: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let base_url = if use_testnet {
        "https://open-api-vst.bingx.com"
    } else {
        "https://open-api.bingx.com"
    };
    let normalized_symbol = normalize_symbol(symbol);
    let timestamp = chrono::Utc::now().timestamp_millis();
    let query = format!(
        "leverage={}&side={}&symbol={}&timestamp={}",
        leverage, position_side, normalized_symbol, timestamp
    );
    let signature = calculate_signature(api_secret, &query);
    let url = format!("{}/openApi/swap/v2/trade/leverage?{}&signature={}", base_url, query, signature);

    let res = client
        .post(&url)
        .header("X-BX-APIKEY", api_key)
        .send()
        .await?;

    if res.status().is_success() {
        let text = res.text().await?;
        let json: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(code) = json.get("code").and_then(|c| c.as_i64()) {
            if code == 0 {
                Ok(())
            } else {
                let msg = json.get("msg").and_then(|m| m.as_str()).unwrap_or("Error");
                Err(format!("Error BingX (código {}): {}", code, msg).into())
            }
        } else {
            Err("Respuesta no válida".into())
        }
    } else {
        let err_text = res.text().await?;
        Err(format!("Error HTTP: {}", err_text).into())
    }
}

pub async fn get_ticker_price(
    client: &reqwest::Client,
    symbol: &str,
    use_testnet: bool,
) -> Result<f64, Box<dyn std::error::Error>> {
    let base_url = if use_testnet {
        "https://open-api-vst.bingx.com"
    } else {
        "https://open-api.bingx.com"
    };
    let normalized_symbol = normalize_symbol(symbol);
    let url = format!("{}/openApi/swap/v2/quote/price?symbol={}", base_url, normalized_symbol);
    
    let res = client.get(&url).send().await?;
    if res.status().is_success() {
        let text = res.text().await?;
        let json: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(price_str) = json.get("data").and_then(|d| d.get("price")).and_then(|p| p.as_str()) {
            let price: f64 = price_str.parse()?;
            Ok(price)
        } else {
            Err("Formato de precio no encontrado en BingX".into())
        }
    } else {
        let err_text = res.text().await?;
        Err(format!("Error obteniendo precio: {}", err_text).into())
    }
}

pub async fn open_market_order(
    client: &reqwest::Client,
    api_key: &str,
    api_secret: &str,
    symbol: &str,
    side: &str,          // "BUY" o "SELL"
    position_side: &str, // "LONG" o "SHORT" (modo Hedge)
    quantity: f64,
    client_order_id: Option<&str>,
    use_testnet: bool,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let base_url = if use_testnet {
        "https://open-api-vst.bingx.com"
    } else {
        "https://open-api.bingx.com"
    };
    let normalized_symbol = normalize_symbol(symbol);
    let timestamp = chrono::Utc::now().timestamp_millis();
    let qty_str = format!("{:.4}", quantity);
    let qty_val: f64 = qty_str.parse().unwrap_or(0.0);
    if qty_val <= 0.0 {
        return Err("La cantidad calculada es menor que el mínimo permitido por el exchange (0.0001 BTC).".into());
    }
    
    let mut query = format!(
        "positionSide={}&quantity={}&side={}&symbol={}&timestamp={}&type=MARKET",
        position_side, qty_str, side, normalized_symbol, timestamp
    );
    if let Some(cid) = client_order_id {
        query = format!("clientOrderID={}&{}", cid, query);
    }
    
    let signature = calculate_signature(api_secret, &query);
    let url = format!("{}/openApi/swap/v2/trade/order?{}&signature={}", base_url, query, signature);

    let res = client
        .post(&url)
        .header("X-BX-APIKEY", api_key)
        .send()
        .await?;

    let status = res.status();
    let text = res.text().await?;
    if status.is_success() {
        let json: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(code) = json.get("code").and_then(|c| c.as_i64()) {
            if code == 0 {
                Ok(json.get("data").cloned().unwrap_or(json))
            } else {
                let msg = json.get("msg").and_then(|m| m.as_str()).unwrap_or("Error");
                Err(format!("Error BingX al colocar orden (código {}): {}", code, msg).into())
            }
        } else {
            Err("Respuesta no válida de BingX".into())
        }
    } else {
        Err(format!("Error HTTP: {}", text).into())
    }
}

pub async fn get_open_positions(
    client: &reqwest::Client,
    api_key: &str,
    api_secret: &str,
    symbol: &str,
    use_testnet: bool,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
    let base_url = if use_testnet {
        "https://open-api-vst.bingx.com"
    } else {
        "https://open-api.bingx.com"
    };
    let normalized_symbol = normalize_symbol(symbol);
    let timestamp = chrono::Utc::now().timestamp_millis();
    let query = format!("symbol={}&timestamp={}", normalized_symbol, timestamp);
    let signature = calculate_signature(api_secret, &query);
    let url = format!("{}/openApi/swap/v2/user/positions?{}&signature={}", base_url, query, signature);

    let res = client
        .get(&url)
        .header("X-BX-APIKEY", api_key)
        .send()
        .await?;

    if res.status().is_success() {
        let text = res.text().await?;
        let json: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(code) = json.get("code").and_then(|c| c.as_i64()) {
            if code == 0 {
                if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
                    return Ok(data.clone());
                }
                Ok(vec![])
            } else {
                let msg = json.get("msg").and_then(|m| m.as_str()).unwrap_or("Error");
                Err(format!("Error BingX al consultar posiciones (código {}): {}", code, msg).into())
            }
        } else {
            Err("Respuesta no válida de BingX".into())
        }
    } else {
        let err_text = res.text().await?;
        Err(format!("Error HTTP al obtener posiciones: {}", err_text).into())
    }
}

pub async fn get_account_details(
    client: &reqwest::Client,
    api_key: &str,
    api_secret: &str,
    use_testnet: bool,
) -> Result<BingXAccountInfo, Box<dyn std::error::Error>> {
    let base_url = if use_testnet {
        "https://open-api-vst.bingx.com"
    } else {
        "https://open-api.bingx.com"
    };
    let timestamp = chrono::Utc::now().timestamp_millis();
    let query = format!("timestamp={}", timestamp);
    let signature = calculate_signature(api_secret, &query);
    let url = format!("{}/openApi/swap/v2/user/balance?{}&signature={}", base_url, query, signature);

    let res = client
        .get(&url)
        .header("X-BX-APIKEY", api_key)
        .send()
        .await?;

    if res.status().is_success() {
        let text = res.text().await?;
        let json: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(data) = json.get("data").and_then(|d| d.get("balance")) {
            let wallet_balance: f64 = data.get("balance")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .parse()
                .unwrap_or(0.0);
            let available_margin: f64 = data.get("availableMargin")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .parse()
                .unwrap_or(0.0);
            let user_id = data.get("userId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(BingXAccountInfo {
                wallet_balance,
                available_margin,
                user_id,
            })
        } else {
            Err("No se pudo obtener la información de balance/userId en la respuesta de BingX".into())
        }
    } else {
        let err_text = res.text().await?;
        Err(format!("Error HTTP al obtener balances: {}", err_text).into())
    }
}

pub async fn get_stable_balance(
    client: &reqwest::Client,
    api_key: &str,
    api_secret: &str,
    use_testnet: bool,
) -> Result<f64, Box<dyn std::error::Error>> {
    let details = get_account_details(client, api_key, api_secret, use_testnet).await?;
    let available_margin = details.available_margin;

    let mut total_position_margin = 0.0;
    if let Ok(positions) = get_open_positions(client, api_key, api_secret, "BTC-USDT", use_testnet).await {
        for pos in &positions {
            let amt_str = pos.get("positionAmt").and_then(|a| a.as_str()).unwrap_or("0");
            let amt: f64 = amt_str.parse().unwrap_or(0.0);
            if amt.abs() > 0.0 {
                let entry_price_str = pos.get("entryPrice").and_then(|p| p.as_str()).unwrap_or("0");
                let entry_price: f64 = entry_price_str.parse().unwrap_or(0.0);
                let leverage_str = pos.get("leverage").and_then(|l| l.as_str()).unwrap_or("10");
                let leverage: f64 = leverage_str.parse().unwrap_or(10.0);
                if leverage > 0.0 {
                    let initial_margin = (amt.abs() * entry_price) / leverage;
                    total_position_margin += initial_margin;
                }
            }
        }
    }

    Ok(available_margin + total_position_margin)
}
