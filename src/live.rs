use std::io::{self, Write};
use crate::types::{PositionType};
use crate::db::{
    get_latest_candles, get_api_config, save_api_config, delete_api_config,
    get_llm_config, download_candles
};
use crate::llm::{call_gemma, parse_gemma_response};
use crate::backtest::get_liquidation_percentage;
use crate::bingx::{
    test_api_connection, get_stable_balance, get_account_details, get_open_positions,
    get_ticker_price, set_leverage, open_market_order
};
use crate::indicators::{calculate_indicators, calculate_ema};

pub async fn run_live_gemma_step(
    db_path: &str,
    timeframe: &str,
    client: &reqwest::Client,
    api_key: &str,
    api_secret: &str,
    leverage: u32,
    use_testnet: bool,
    _confidence_threshold: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Fetch latest 500 candles from DB (to pre-warm indicators)
    let candles = get_latest_candles(db_path, timeframe, 500)?;
    if candles.len() < 10 {
        return Err("No hay suficientes velas en la base de datos (se requieren al menos 10).".into());
    }
    
    // 2. Fetch current ticker price
    let precio_actual = get_ticker_price(client, "BTC-USDT", use_testnet).await?;
    
    // 3. Fetch BingX account details (wallet balance, available margin)
    let account = get_account_details(client, api_key, api_secret, use_testnet).await?;
    
    // 4. Fetch open positions from BingX
    let positions = get_open_positions(client, api_key, api_secret, "BTC-USDT", use_testnet).await?;
    
    // 5. Parse current position
    let mut position_type = PositionType::None;
    let mut position_margin = 0.0;
    let mut position_size_btc = 0.0;
    let mut precio_entrada = 0.0;
    let mut floating_pnl = 0.0;
    
    for pos in &positions {
        let amt_str = pos.get("positionAmt").and_then(|a| a.as_str()).unwrap_or("0");
        let amt: f64 = amt_str.parse().unwrap_or(0.0);
        if amt.abs() > 0.0 {
            let side = pos.get("positionSide").and_then(|s| s.as_str()).unwrap_or("LONG");
            position_type = if side == "LONG" { PositionType::Long } else { PositionType::Short };
            position_size_btc = amt.abs();
            precio_entrada = pos.get("entryPrice").and_then(|p| p.as_str()).unwrap_or("0").parse().unwrap_or(0.0);
            floating_pnl = pos.get("unrealizedProfit").and_then(|u| u.as_str()).unwrap_or("0").parse().unwrap_or(0.0);
            if leverage > 0 {
                position_margin = (position_size_btc * precio_entrada) / leverage as f64;
            }
            break;
        }
    }
    
    // Available margin is what we can use to open new positions
    let saldo_usdt = account.available_margin;
    let equity = account.available_margin + position_margin + floating_pnl;
    
    let liq_percent = get_liquidation_percentage(leverage as f64);
    let liquidation_price = match position_type {
        PositionType::Long => precio_entrada * (1.0 - liq_percent / 100.0),
        PositionType::Short => precio_entrada * (1.0 + liq_percent / 100.0),
        PositionType::None => 0.0,
    };
    
    // Format candle history for Gemma user prompt (only the last 20 candles)
    let mut history_str = String::new();
    let start_idx = candles.len().saturating_sub(20);
    let actual_history_len = candles.len() - start_idx;
    for (idx, prev_candle) in candles[start_idx..].iter().enumerate() {
        let is_current = idx == actual_history_len - 1;
        let label = if is_current { " (Current)" } else { "" };
        let close_val = if is_current { precio_actual } else { prev_candle.close };
        let high_val = if is_current { prev_candle.high.max(precio_actual) } else { prev_candle.high };
        let low_val = if is_current { prev_candle.low.min(precio_actual) } else { prev_candle.low };
        history_str.push_str(&format!(
            "- t-{}: O:{:.1}, H:{:.1}, L:{:.1}, C:{:.1}, V:{:.0}{}\n",
            actual_history_len - 1 - idx, prev_candle.open, high_val, low_val, close_val, prev_candle.volume, label
        ));
    }
    
    let system_prompt = format!(
        "CRITICAL: DO NOT use any <think> tags. You are strictly FORBIDDEN from reasoning, explaining, or writing thoughts. You must immediately output raw JSON. Your response MUST start with the character '{{' and end with '}}'.

INSTRUCTIONS:

Strategy & Capital Allocation (Base Leverage: {}X):
- Two boxes: 100 percent Long-Term (LT) and 100 percent Short-Term (ST) of the total account equity. This proportion represents the max margin limit of the boxes, not the volume/size.
- Box Independence: The LT and ST boxes are independent trading modules. You can, and should, hold positions in BOTH boxes simultaneously if conditions allow. Do not wait for one box to close or be empty before trading in the other.
- Leverage: Select between 5.0 and 10.0 for any position (include \"apalancamiento\": X in the box JSON).
- Add position per Box: You are authorized to open your first position in any box freely. You are authorized to open an ADDITIONAL/SECOND position in the same box ONLY if the existing position in that box has a profit of >= 200 percent ROI (measured relative to its initial MARGIN). Additional positions in a box will always have the exact same size/margin as the first position.

Trend Priority & Guidelines: 
- Long-Term (LT) Box: It is highly suggested to trade in the direction of the long-term trend (EMA100 and EMA200). Long-term buys/sells are suggested when EMA100 is above/below EMA200, though this is a guidance and not a strict blocker.
- Short-Term (ST) Box (Mid-Term operational mode): Actively trade mid-term trends guided by EMA20 and EMA40.

Position Actions & Stop Loss Rules per Box:
- To open a new trade: set \"accion\" to \"LONG\" or \"SHORT\" and \"cerrar\" to false.
- To maintain an active trade without changes: set \"accion\" to \"HOLD\" and \"cerrar\" to false.
- To close an active trade completely: set \"accion\" to \"FLAT\" and \"cerrar\" to true.
- If a box has no active position and you do not want to open one: set \"accion\" to \"HOLD\", \"cerrar\" to false, and \"stop_loss\" to null.
- Stop Loss (SL) Rules:
  * LT Box (Long-Term): Set a wider stop loss below/above EMA200, or use EMA100 as a trailing stop to protect long-term trends.
  * ST Box (Mid-Term): Set a stop loss below/above EMA40, or use EMA20 as a trailing stop.
- Trailing Stop: ONLY when you have guaranteed profit (position is strictly in profit compared to the entry price), set the \"stop_loss\" as a Trailing Stop and update it dynamically to the current EMA100/EMA200 (for LT) or EMA20/EMA40 (for ST/Mid-Term) to lock in profits. Do not start trailing or moving the Stop Loss if the position is not in profit.

CRITICAL EXECUTION RULES:
1. DO NOT use any <think> tags. Do not think, do not reason, do not explain, and do not write any prose. 
2. Go directly from the market data to the raw JSON output.
3. Output: Respond ONLY with a raw JSON matching the structure below. No markdown (```json), no extra fields.

Example:
{{
  \"lt_box\": {{
    \"accion\": \"HOLD\",
    \"cerrar\": false,
    \"apalancamiento\": 5.0,
    \"stop_loss\": null
  }},
  \"st_box\": {{
    \"accion\": \"HOLD\",
    \"cerrar\": false,
    \"apalancamiento\": 5.0,
    \"stop_loss\": null
  }}
}}",
        leverage
    );
    
    // Calcular la progresión de los indicadores técnicos de las últimas 20 velas (para ver aceleración/desaceleración)
    // Pasamos el array de 500 velas para precalentar adecuadamente las métricas (ej. EMA200)
    let mut indicators_str = String::new();
    for (idx, _) in candles[start_idx..].iter().enumerate() {
        let global_idx = start_idx + idx;
        let is_current = global_idx == candles.len() - 1;
        let label = if is_current { " (Current)" } else { "" };
        let offset = actual_history_len - 1 - idx;
        let price_for_indicator = if is_current { precio_actual } else { candles[global_idx].close };
        let ind_val = calculate_indicators(&candles, global_idx, price_for_indicator);
        indicators_str.push_str(&format!(
            "- t-{}{}: {}\n",
            offset, label, ind_val
        ));
    }

    let user_prompt = format!(
        "DATA (Recent)
Current BTC Price (Close): {:.2} USDT
Last 20 candles history:
{}

TECHNICAL INDICATORS
{}

ACCOUNT STATUS
Free balance (not in margin): {:.2} USDT
Total Equity: {:.2} USDT
Leverage: {:.1}x
Risk parameters: Max % risk per trade: 10%
Active position: {:?}
Position Margin: {:.2} USDT (Isolated Mode)
Position size: {:.6} BTC (${:.2})
Entry price: {:.2} USDT
Liquidation price: {:.2} USDT (If it moves {:.3}% against)
Floating PnL: {:.2} USDT
What action do you take? Respond strictly in JSON format",
        precio_actual, history_str, indicators_str, saldo_usdt, equity, leverage as f64, position_type, position_margin,
        position_size_btc, position_size_btc * precio_actual, precio_entrada, liquidation_price, liq_percent,
        floating_pnl
    );
    
    println!("\n=== [Paso de Trading en Vivo] {} | Precio Actual: {:.2} USDT ===", 
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), precio_actual
    );
    println!("💼 Estado: Saldo Disponible: {:.2} USDT | Margen: {:.2} USDT ({:?}) | Entrada: {:.2} USDT | PnL Flotante: {:.2} USDT | Equity: {:.2} USDT",
        saldo_usdt, position_margin, position_type, precio_entrada, floating_pnl, equity
    );
    
    // Call Gemma API
    let (api_url, api_token) = get_llm_config(db_path).unwrap_or((
        "http://127.0.0.1:5508/v1/chat/completions".to_string(),
        "lm-studio".to_string()
    ));
    let mut gemma_analisis = "Error al obtener respuesta".to_string();
    let mut retries = 3;
    
    while retries > 0 {
        println!("\n=== [ENVÍO A GEMMA] ===");
        println!("System Prompt:\n{}", system_prompt);
        println!("User Prompt:\n{}", user_prompt);
        println!("=======================");
        match call_gemma(client, &api_url, &api_token, &system_prompt, &user_prompt).await {
            Ok(content) => {
                println!("\n=== [RESPUESTA DE GEMMA] ===");
                println!("{}", content.trim());
                println!("============================");
                if let Some(parsed) = parse_gemma_response(&content) {
                    gemma_analisis = parsed.analisis.unwrap_or_else(|| "Sin análisis".to_string());

                    // Execute box actions in live trading
                    let trend_direction_long = {
                        let last_idx = candles.len() - 1;
                        let ema100 = calculate_ema(&candles, last_idx, 100);
                        let ema200 = calculate_ema(&candles, last_idx, 200);
                        if ema100 >= ema200 { PositionType::Long } else { PositionType::Short }
                    };

                    let get_position_by_type = |p_type: PositionType| -> Option<f64> {
                        for pos in &positions {
                            let amt_str = pos.get("positionAmt").and_then(|a| a.as_str()).unwrap_or("0");
                            let amt: f64 = amt_str.parse().unwrap_or(0.0);
                            if amt.abs() > 0.0 {
                                let side = pos.get("positionSide").and_then(|s| s.as_str()).unwrap_or("LONG");
                                let side_type = if side == "LONG" { PositionType::Long } else { PositionType::Short };
                                if side_type == p_type {
                                    return Some(amt.abs());
                                }
                            }
                        }
                        None
                    };

                    // --- 1. LT BOX ---
                    if parsed.lt_box.cerrar {
                        if let Some(qty) = get_position_by_type(trend_direction_long) {
                            let exit_side = if trend_direction_long == PositionType::Long { "SELL" } else { "BUY" };
                            let pos_side_str = if trend_direction_long == PositionType::Long { "LONG" } else { "SHORT" };
                            println!("💰 [LT Box] Cerrando posición {:?} de {:.4} BTC...", trend_direction_long, qty);
                            match open_market_order(client, api_key, api_secret, "BTC-USDT", exit_side, pos_side_str, qty, None, use_testnet).await {
                                Ok(_) => println!("✅ [LT Box] Posición {:?} cerrada exitosamente.", trend_direction_long),
                                Err(e) => println!("❌ [LT Box] Error al cerrar posición: {}", e),
                            }
                        }
                    }

                    let lt_action_upper = parsed.lt_box.accion.to_uppercase();
                    if lt_action_upper == "LONG" || lt_action_upper == "SHORT" {
                        let desired_type = if lt_action_upper == "LONG" { PositionType::Long } else { PositionType::Short };
                        if desired_type != trend_direction_long {
                            println!("⚠️ [LT Box Warning] Acción {:?} no coincide con la tendencia macro calculada por EMAs ({:?}). Procediendo de todos modos por sugerencia.", desired_type, trend_direction_long);
                        }
                        if get_position_by_type(desired_type).is_some() {
                            println!("⏳ [LT Box] Acción {:?} recibida, pero ya existe una posición activa.", desired_type);
                        } else {
                            let lt_leverage = parsed.lt_box.apalancamiento.unwrap_or(leverage as f64) as u32;
                            let margin = equity;
                            let size_usdt = margin * lt_leverage as f64;
                            match get_ticker_price(client, "BTC-USDT", use_testnet).await {
                                Ok(price) => {
                                    let qty = size_usdt / price;
                                    println!("🛒 [LT Box] Abriendo {:?}... Margen: {:.2} USDT | Tamaño: {:.4} BTC | Apalancamiento: {}x", desired_type, margin, qty, lt_leverage);
                                    let pos_side_str = if desired_type == PositionType::Long { "LONG" } else { "SHORT" };
                                    let side_str = if desired_type == PositionType::Long { "BUY" } else { "SELL" };

                                    if let Err(e) = set_leverage(client, api_key, api_secret, "BTC-USDT", lt_leverage, pos_side_str, use_testnet).await {
                                        println!("⚠️ [LT Box] Error configurando apalancamiento: {}", e);
                                    }
                                    
                                    match open_market_order(client, api_key, api_secret, "BTC-USDT", side_str, pos_side_str, qty, None, use_testnet).await {
                                        Ok(res) => {
                                            println!("✅ [LT Box] {:?} ABIERTO EXITOSAMENTE EN BINGX.", desired_type);
                                            if let Some(avg_price) = res.get("avgPrice").and_then(|v| v.as_str()) {
                                                println!("- Precio promedio: {} USDT", avg_price);
                                            }
                                        }
                                        Err(e) => println!("❌ [LT Box] Error abriendo {:?}: {}", desired_type, e),
                                    }
                                }
                                Err(e) => println!("❌ [LT Box] Error al consultar precio ticker: {}", e),
                              }
                          }
                      }

                      // --- 2. ST BOX ---
                      let trend_direction_short = if trend_direction_long == PositionType::Long { PositionType::Short } else { PositionType::Long };

                      if parsed.st_box.cerrar {
                          let target_st_type = trend_direction_short;
                          if let Some(qty) = get_position_by_type(target_st_type) {
                              let exit_side = if target_st_type == PositionType::Long { "SELL" } else { "BUY" };
                              let pos_side_str = if target_st_type == PositionType::Long { "LONG" } else { "SHORT" };
                              println!("💰 [ST Box] Cerrando posición {:?} de {:.4} BTC...", target_st_type, qty);
                              match open_market_order(client, api_key, api_secret, "BTC-USDT", exit_side, pos_side_str, qty, None, use_testnet).await {
                                  Ok(_) => println!("✅ [ST Box] Posición {:?} cerrada exitosamente.", target_st_type),
                                  Err(e) => println!("❌ [ST Box] Error al cerrar posición: {}", e),
                              }
                          }
                      }

                      let st_action_upper = parsed.st_box.accion.to_uppercase();
                      if st_action_upper == "LONG" || st_action_upper == "SHORT" {
                          let desired_type = if st_action_upper == "LONG" { PositionType::Long } else { PositionType::Short };
                          if get_position_by_type(desired_type).is_some() {
                              println!("⏳ [ST Box] Acción {:?} recibida, pero ya existe una posición activa.", desired_type);
                          } else {
                              let st_leverage = parsed.st_box.apalancamiento.unwrap_or(leverage as f64) as u32;
                              let margin = equity;
                              let size_usdt = margin * st_leverage as f64;
                              match get_ticker_price(client, "BTC-USDT", use_testnet).await {
                                  Ok(price) => {
                                      let qty = size_usdt / price;
                                      println!("🛒 [ST Box] Abriendo {:?}... Margen: {:.2} USDT | Tamaño: {:.4} BTC | Apalancamiento: {}x", desired_type, margin, qty, st_leverage);
                                      let pos_side_str = if desired_type == PositionType::Long { "LONG" } else { "SHORT" };
                                      let side_str = if desired_type == PositionType::Long { "BUY" } else { "SELL" };

                                      if let Err(e) = set_leverage(client, api_key, api_secret, "BTC-USDT", st_leverage, pos_side_str, use_testnet).await {
                                          println!("⚠️ [ST Box] Error configurando apalancamiento: {}", e);
                                      }
                                      
                                      match open_market_order(client, api_key, api_secret, "BTC-USDT", side_str, pos_side_str, qty, None, use_testnet).await {
                                          Ok(res) => {
                                              println!("✅ [ST Box] {:?} ABIERTO EXITOSAMENTE EN BINGX.", desired_type);
                                              if let Some(avg_price) = res.get("avgPrice").and_then(|v| v.as_str()) {
                                                  println!("- Precio promedio: {} USDT", avg_price);
                                              }
                                          }
                                          Err(e) => println!("❌ [ST Box] Error abriendo {:?}: {}", desired_type, e),
                                      }
                                  }
                                  Err(e) => println!("❌ [ST Box] Error al consultar precio ticker: {}", e),
                              }
                          }
                      }

                      break;
                  } else {
                      println!("⚠️ No se pudo parsear el JSON de Gemma. Reintentando... (Respuesta recibida: {})", content.trim());
                  }
              }
              Err(e) => {
                  println!("⚠️ Error en petición a Gemma: {}. Reintentando...", e);
              }
          }
          retries -= 1;
          tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
      }
      
      if gemma_analisis != "Sin análisis" && gemma_analisis != "Error al obtener respuesta" && !gemma_analisis.trim().is_empty() {
          println!("🤖 Gemma dice: {}", gemma_analisis);
      }
    
    Ok(())
}

pub async fn trading_en_vivo_menu(db_path: &str, client: &reqwest::Client) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        println!("\n                  Gemma Trading en Vivo                  \n");
        println!("1. Configurar API y Apalancamiento");
        println!("2. Eliminar credenciales de DB");
        println!("3. Test de API y Saldo");
        println!("4. Prueba de Ordenes (Manual)");
        println!("5. Trading en Vivo con Gemma (Automatizado)");
        println!("6. Volver al menú principal");
        print!("Selecciona una opción: ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let choice = input.trim();

        match choice {
            "1" => {
                println!("\n[Configurar API y Apalancamiento]");
                print!("Ingrese API Key de BingX: ");
                io::stdout().flush()?;
                let mut api_key = String::new();
                io::stdin().read_line(&mut api_key)?;
                let api_key = api_key.trim().to_string();

                print!("Ingrese API Secret de BingX: ");
                io::stdout().flush()?;
                let mut api_secret = String::new();
                io::stdin().read_line(&mut api_secret)?;
                let api_secret = api_secret.trim().to_string();

                print!("Ingrese Apalancamiento deseado (ej. 10 para 10x): ");
                io::stdout().flush()?;
                let mut lev_input = String::new();
                io::stdin().read_line(&mut lev_input)?;
                let leverage: u32 = lev_input.trim().parse().unwrap_or(10);

                print!("¿Desea utilizar la Testnet (VST Demo)? (s/N): ");
                io::stdout().flush()?;
                let mut testnet_input = String::new();
                io::stdin().read_line(&mut testnet_input)?;
                let use_testnet = matches!(testnet_input.trim().to_lowercase().as_str(), "s" | "si" | "y" | "yes");

                if !api_key.is_empty() && !api_secret.is_empty() {
                    save_api_config(db_path, &api_key, &api_secret, leverage, use_testnet)?;
                    println!("Configuración guardada. Validando conexión...");
                    match test_api_connection(client, &api_key, &api_secret, use_testnet).await {
                        Ok(_) => println!("✅ ¡Conexión con BingX exitosa!"),
                        Err(e) => println!("❌ Error de conexión: {}", e),
                    }
                } else {
                    println!("La API Key y API Secret no pueden estar vacías.");
                }
            }
            "2" => {
                println!("\n[Eliminar credenciales de DB]");
                print!("¿Está seguro de que desea eliminar todas las credenciales de BingX de la DB? (s/N): ");
                io::stdout().flush()?;
                let mut confirm = String::new();
                io::stdin().read_line(&mut confirm)?;
                if matches!(confirm.trim().to_lowercase().as_str(), "s" | "si" | "y" | "yes") {
                    delete_api_config(db_path)?;
                    println!("¡Credenciales eliminadas!");
                } else {
                    println!("Operación cancelada.");
                }
            }
            "3" => {
                println!("\n[Test de API y Saldo]");
                match get_api_config(db_path)? {
                    Some((api_key, api_secret, leverage, _, use_testnet)) => {
                        println!("Validando conexión...");
                        match get_stable_balance(client, &api_key, &api_secret, use_testnet).await {
                            Ok(balance) => {
                                println!("✅ Conexión Exitosa.");
                                println!("- Cuenta: {}", if use_testnet { "VST Demo" } else { "Real" });
                                println!("- Apalancamiento configurado: {}x", leverage);
                                println!("- Capital Estable (Disponible + Margen): ${:.2} USDT", balance);
                            }
                            Err(e) => println!("❌ Error de Conexión: {}", e),
                        }
                    }
                    None => println!("No hay credenciales configuradas en la DB. Utilice la opción 1."),
                }
            }
            "4" => {
                println!("\n[Prueba de Órdenes Manuales]");
                match get_api_config(db_path)? {
                    Some((api_key, api_secret, leverage, _, use_testnet)) => {
                        let symbol = "BTC-USDT";
                        println!("\nConsultando posiciones actuales en BingX para {}...", symbol);
                        match get_open_positions(client, &api_key, &api_secret, symbol, use_testnet).await {
                            Ok(positions) => {
                                println!("\n--- Posiciones Abiertas para {} ---", symbol);
                                let mut found = false;
                                for pos in &positions {
                                    let amt_str = pos.get("positionAmt").and_then(|a| a.as_str()).unwrap_or("0");
                                    let amt: f64 = amt_str.parse().unwrap_or(0.0);
                                    if amt.abs() > 0.0 {
                                        found = true;
                                        let entry_price = pos.get("entryPrice").and_then(|p| p.as_str()).unwrap_or("0");
                                        let unrealized_pnl = pos.get("unrealizedProfit").and_then(|u| u.as_str()).unwrap_or("0");
                                        let side = pos.get("positionSide").and_then(|s| s.as_str()).unwrap_or("LONG");
                                        let leverage = pos.get("leverage").and_then(|l| l.as_str()).unwrap_or("10");
                                        println!(
                                            "• Lado: {} | Cantidad: {:.4} | Entrada: ${} | PnL No Realizado: ${} | Apalancamiento: {}x",
                                            side, amt, entry_price, unrealized_pnl, leverage
                                        );
                                    }
                                }
                                if !found {
                                    println!("- No hay posiciones abiertas actualmente para {}.", symbol);
                                }
                            }
                            Err(e) => println!("⚠️ Error al obtener posiciones: {}", e),
                        }

                        println!("\n--- Menú de Órdenes ({}) ---", symbol);
                        println!("1) Abrir posición (Manual)");
                        println!("2) Cerrar posición (Manual)");
                        println!("3) Volver");
                        print!("Selecciona una opción: ");
                        io::stdout().flush()?;

                        let mut sub_input = String::new();
                        io::stdin().read_line(&mut sub_input)?;
                        match sub_input.trim() {
                            "1" => {
                                println!("\n[Abrir Posición Manualmente]");
                                println!("Seleccione Dirección:");
                                println!("1) LONG (BUY)");
                                println!("2) SHORT (SELL)");
                                print!("Selecciona: ");
                                io::stdout().flush()?;
                                let mut side_choice = String::new();
                                io::stdin().read_line(&mut side_choice)?;
                                let (side, position_side) = if side_choice.trim() == "2" {
                                    ("SELL", "SHORT")
                                } else {
                                    ("BUY", "LONG")
                                };

                                print!("Ingrese Margen en USDT a operar: ");
                                io::stdout().flush()?;
                                let mut margin_input = String::new();
                                io::stdin().read_line(&mut margin_input)?;
                                let margin: f64 = margin_input.trim().parse().unwrap_or(10.0);

                                print!("Ingrese Apalancamiento a usar [actual: {}]: ", leverage);
                                io::stdout().flush()?;
                                let mut leverage_input = String::new();
                                io::stdin().read_line(&mut leverage_input)?;
                                let leverage_val: u32 = leverage_input.trim().parse().unwrap_or(leverage);

                                let size_usdt = margin * leverage_val as f64;
                                
                                if let Err(e) = set_leverage(client, &api_key, &api_secret, symbol, leverage_val, position_side, use_testnet).await {
                                    println!("⚠️ Error al configurar apalancamiento: {}", e);
                                }

                                match get_ticker_price(client, symbol, use_testnet).await {
                                    Ok(price) => {
                                        let qty = size_usdt / price;
                                        println!("- Cantidad calculada: {:.4} BTC", qty);
                                        match open_market_order(client, &api_key, &api_secret, symbol, side, position_side, qty, None, use_testnet).await {
                                            Ok(res) => {
                                                println!("✅ ¡Orden de Mercado abierta de manera EXITOSA!");
                                                if let Some(avg_price) = res.get("avgPrice").and_then(|v| v.as_str()) {
                                                    println!("- Precio promedio: {} USDT", avg_price);
                                                }
                                            }
                                            Err(e) => println!("❌ Error al colocar la orden: {}", e),
                                        }
                                    }
                                    Err(e) => println!("❌ Error al obtener precio actual: {}", e),
                                }
                            }
                            "2" => {
                                println!("\n[Cerrar Posición Manualmente]");
                                if let Ok(positions) = get_open_positions(client, &api_key, &api_secret, symbol, use_testnet).await {
                                    let mut active = Vec::new();
                                    for pos in &positions {
                                        let amt_str = pos.get("positionAmt").and_then(|a| a.as_str()).unwrap_or("0");
                                        let amt: f64 = amt_str.parse::<f64>().unwrap_or(0.0).abs();
                                        if amt > 0.0 {
                                            active.push(pos.clone());
                                        }
                                    }

                                    if active.is_empty() {
                                        println!("No hay posiciones abiertas para cerrar.");
                                    } else {
                                        println!("Seleccione la posición que desea cerrar:");
                                        for (idx, pos) in active.iter().enumerate() {
                                            let side = pos.get("positionSide").and_then(|s| s.as_str()).unwrap_or("LONG");
                                            let amt = pos.get("positionAmt").and_then(|a| a.as_str()).unwrap_or("0");
                                            println!("{}) Lado: {} | Cantidad: {}", idx + 1, side, amt);
                                        }
                                        print!("Selección: ");
                                        io::stdout().flush()?;
                                        let mut pos_choice = String::new();
                                        io::stdin().read_line(&mut pos_choice)?;
                                        let choice_idx: usize = pos_choice.trim().parse::<usize>().unwrap_or(1);
                                        if choice_idx > 0 && choice_idx <= active.len() {
                                            let selected = &active[choice_idx - 1];
                                            let side = selected.get("positionSide").and_then(|s| s.as_str()).unwrap_or("LONG");
                                            let amt_str = selected.get("positionAmt").and_then(|a| a.as_str()).unwrap_or("0");
                                            let qty: f64 = amt_str.parse::<f64>().unwrap_or(0.0).abs();
                                            let exit_side = if side == "LONG" { "SELL" } else { "BUY" };

                                            println!("Cerrando posición {} de {:.4} BTC...", side, qty);
                                            match open_market_order(client, &api_key, &api_secret, symbol, exit_side, side, qty, None, use_testnet).await {
                                                Ok(_) => println!("✅ ¡Posición {} cerrada de manera EXITOSA!", side),
                                                Err(e) => println!("❌ Error al cerrar posición: {}", e),
                                            }
                                        } else {
                                            println!("Selección no válida.");
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    None => println!("No hay credenciales configuradas en la DB. Utilice la opción 1."),
                }
            }
            "5" => {
                println!("\n[Trading en Vivo con Gemma (Automatizado)]");
                match get_api_config(db_path)? {
                    Some((api_key, api_secret, leverage, _, use_testnet)) => {
                        println!("Validando conexión inicial antes de arrancar...");
                        match get_stable_balance(client, &api_key, &api_secret, use_testnet).await {
                            Ok(balance) => {
                                println!("✅ Conexión con BingX exitosa. Capital inicial: ${:.2} USDT", balance);
                                
                                println!("Selecciona la temporalidad para operar en vivo:");
                                println!("1) 1H (1 Hora)");
                                println!("2) 4H (4 Horas)");
                                println!("3) 1D (1 Día)");
                                print!("Selecciona: ");
                                io::stdout().flush()?;
                                let mut tf_input = String::new();
                                io::stdin().read_line(&mut tf_input)?;
                                let timeframe = match tf_input.trim() {
                                    "2" => "4h",
                                    "3" => "1d",
                                    _ => "1h",
                                };

                                println!("🤖 Iniciando bucle de trading automatizado con Gemma ({}).", timeframe);
                                println!("Presione ENTER para detener y volver al menú anterior en cualquier momento.");
                                
                                use std::sync::atomic::{AtomicBool, Ordering};
                                use std::sync::Arc;
                                
                                let stop_signal = Arc::new(AtomicBool::new(false));
                                let stop_signal_clone = stop_signal.clone();
                                
                                tokio::spawn(async move {
                                    let mut line = String::new();
                                    let _ = io::stdin().read_line(&mut line);
                                    stop_signal_clone.store(true, Ordering::SeqCst);
                                });
                                
                                while !stop_signal.load(Ordering::SeqCst) {
                                    // Update candles first
                                    println!("🔄 Actualizando velas desde Binance ({})...", timeframe);
                                    if let Err(e) = download_candles(db_path, timeframe).await {
                                        println!("⚠️ Error descargando velas: {}", e);
                                    }
                                    
                                    // Run one live step
                                    println!("🧠 Evaluando mercado con Gemma ({})...", timeframe);
                                    if let Err(e) = run_live_gemma_step(db_path, timeframe, client, &api_key, &api_secret, leverage, use_testnet, 60).await {
                                        println!("⚠️ Error en el paso de trading: {}", e);
                                    }
                                    
                                    // Wait until next hour/4-hour/1-day closes (or check stop_signal every 5 seconds)
                                    let now = chrono::Utc::now().timestamp();
                                    let interval_secs = if timeframe == "1d" {
                                        86400
                                    } else if timeframe == "4h" {
                                        14400
                                    } else {
                                        3600
                                    };
                                    let seconds_until_next_candle = interval_secs - (now % interval_secs);
                                    let sleep_secs = seconds_until_next_candle + 10; // 10 seconds buffer
                                    
                                    println!("⏰ Esperando al próximo cierre de vela de {} ({} segundos)...", timeframe, sleep_secs);
                                    
                                    let mut elapsed = 0;
                                    while elapsed < sleep_secs && !stop_signal.load(Ordering::SeqCst) {
                                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                                        elapsed += 5;
                                    }
                                }
                                println!("🛑 Bucle de trading detenido por el usuario.");
                            }
                            Err(e) => println!("❌ No se puede iniciar el trading. Falló la conexión con BingX: {}", e),
                        }
                    }
                    None => println!("No hay credenciales configuradas en la DB. Utilice la opción 1."),
                }
            }
            "6" => {
                break;
            }
            _ => {
                println!("Opción no válida.");
            }
        }
    }
    Ok(())
}
