use std::io::{self, Write};
use chrono::TimeZone;
use crate::types::{Position, PositionType};
use crate::db::{get_candles, get_llm_config, save_llm_config};
use crate::llm::{call_gemma, parse_gemma_response};
use crate::dashboard::{save_equity_curve, generate_dashboard};
use crate::indicators::calculate_indicators;

pub fn calculate_correlation(series_a: &[f64], series_b: &[f64]) -> f64 {
    let n = series_a.len();
    if n == 0 || n != series_b.len() {
        return 0.0;
    }
    let mean_a = series_a.iter().sum::<f64>() / n as f64;
    let mean_b = series_b.iter().sum::<f64>() / n as f64;

    let mut num = 0.0;
    let mut den_a = 0.0;
    let mut den_b = 0.0;

    for i in 0..n {
        let diff_a = series_a[i] - mean_a;
        let diff_b = series_b[i] - mean_b;
        num += diff_a * diff_b;
        den_a += diff_a * diff_a;
        den_b += diff_b * diff_b;
    }

    if den_a == 0.0 || den_b == 0.0 {
        return 0.0;
    }
    num / (den_a * den_b).sqrt()
}

pub fn get_liquidation_percentage(leverage: f64) -> f64 {
    let f = if leverage <= 5.0 { 0.05 }
    else if leverage <= 10.0 { 0.08 }
    else if leverage <= 15.0 { 0.10 }
    else if leverage <= 20.0 { 0.12 }
    else if leverage <= 30.0 { 0.15 }
    else if leverage <= 35.0 { 0.18 }
    else if leverage <= 45.0 { 0.20 }
    else if leverage <= 50.0 { 0.25 }
    else if leverage <= 100.0 { 0.30 }
    else { 0.35 };

    ((100.0 / leverage) - (100.0 / leverage * f)) - 0.045
}

pub async fn run_backtest(
    db_path: &str,
    timeframe: &str,
    leverage: f64,
    risk_percent: f64,
    limit: Option<usize>,
    confidence_threshold: u32,
    verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let candles = get_candles(db_path, timeframe, limit)?;
    if candles.is_empty() {
        println!("❌ No hay velas en la base de datos. Descarga velas primero (Opción 1).");
        return Ok(());
    }

    println!("📊 Iniciando backtest con {} velas...", candles.len());

    let client = reqwest::Client::new();
    let (api_url, mut api_token) = get_llm_config(db_path).unwrap_or((
        "http://127.0.0.1:5508/v1/chat/completions".to_string(),
        "lm-studio".to_string()
    ));
    let mut saldo_usdt = 10000.0;
    
    let fee_rate = 0.0005; // 0.05% comisión

    let mut num_compras = 0;
    let mut num_ventas = 0;
    let mut num_liquidaciones = 0;
    let mut peak_equity = 10000.0;
    let mut max_drawdown = 0.0;

    let mut equity_curve = Vec::new();
    let initial_price = candles.first().map(|c| c.close).unwrap_or(1.0);
    let initial_balance = 10000.0;
    let mut trade_pnls: Vec<f64> = Vec::new();

    // El umbral de liquidación según la fórmula
    let liq_percent = get_liquidation_percentage(leverage);

    let system_prompt = format!(
        "Bot de trading de futuros BTCUSDT (Margen Aislado {}X, Margen operado: {}% equidad). Comisión: 0.05%.\n\n\
         CONTEXTO DEL ACTIVO:\n\
         Bitcoin (BTC) es un activo altamente técnico y fuertemente tendencial. Respeta las estructuras de mercado y los indicadores técnicos clave. Debes centrarte en identificar la tendencia dominante y explotarla.\n\n\
         OPCIÓN DE QUEDARSE FLAT / MANTENERSE AL MARGEN:\n\
         Quedarse FLAT (sin operar, eligiendo 'Flat') es una de las decisiones más inteligentes y válidas cuando no hay una dirección clara o cuando hay alta incertidumbre. Si el mercado está en rango lateral, muestra señales contradictorias o volumen bajo, quédate FLAT eligiendo 'Flat'. No sientas la presión de tener que operar en cada vela.\n\n\
         Si ya tienes una posición abierta (Long o Short) y deseas mantenerla sin abrir nuevas posiciones ni cerrar la actual, debes responder con 'Flat'.\n\n\
         REGLAS ALLIANZ (Riesgo y Posiciones):\n\
         1. NUEVA POSICIÓN: Solo abre otra posición en la misma dirección si alguna posición activa tiene >+200% ROE. No acumules seguidas (All-In).\n\
         2. STOP LOSS (SL): Define o ajusta un SL (ej. para asegurar ganancias). Si el precio lo cruza, la posición se cierra en ese valor.\n\
         3. CIERRES PARCIALES: Cierra posiciones indicando sus índices (1-based) en 'cerrar_posiciones'.\n\n\
         REGLA DE CONFIANZA:\n\
         Evalúa tu convicción en el movimiento direccional de 0 a 100.\n\
         - Si la tendencia es sumamente clara, con soporte técnico y volumen saludable, tu confianza debe ser alta (100).\n\
         - Si hay dudas, señales contradictorias o el mercado está en rango lateral, tu confianza debe ser baja (0). En este caso, tu respuesta debe inclinarse a quedar FLAT eligiendo 'Flat'.\n\n\
         Responde ESTRICTAMENTE con este JSON y nada más (sin explicaciones):\n\
         {{\n\
           \"accion\": \"Abrir Long\"|\"Cerrar Long\"|\"Flat\"|\"Abrir Short\"|\"Cerrar Short\",\n\
           \"cerrar_posiciones\": [índices_1_based_a_cerrar], // o [] si ninguno\n\
           \"stop_losses\": [sl_posicion1, null, ...],\n\
           \"confianza\": entero_de_0_a_100\n\
         }}", leverage, risk_percent
    );

    let mut active_positions: Vec<Position> = Vec::new();

    for (i, candle) in candles.iter().enumerate() {
        let date_format = if timeframe == "1d" { "%Y-%m-%d" } else { "%Y-%m-%d %H:%M:%S" };
        let date_str = chrono::Utc.timestamp_millis_opt(candle.open_time)
            .unwrap()
            .format(date_format)
            .to_string();

        let precio_actual = candle.close;

        // 1. Verificar si hay liquidación o ejecución de SL/TP en esta vela (usando high/low)
        let mut closed_indices = Vec::new();
        for (idx, pos) in active_positions.iter().enumerate() {
            let mut liquidado = false;
            let mut hit_sl = false;
            let mut hit_tp = false;
            match pos.position_type {
                PositionType::Long => {
                    if candle.low <= pos.liquidation_price {
                        liquidado = true;
                    } else if let Some(sl) = pos.stop_loss {
                        if candle.low <= sl {
                            hit_sl = true;
                        }
                    } else if let Some(tp) = pos.take_profit {
                        if candle.high >= tp {
                            hit_tp = true;
                        }
                    }
                }
                PositionType::Short => {
                    if candle.high >= pos.liquidation_price {
                        liquidado = true;
                    } else if let Some(sl) = pos.stop_loss {
                        if candle.high >= sl {
                            hit_sl = true;
                        }
                    } else if let Some(tp) = pos.take_profit {
                        if candle.low <= tp {
                            hit_tp = true;
                        }
                    }
                }
                _ => {}
            }
            if liquidado {
                closed_indices.push((idx, 0, pos.liquidation_price)); // 0: Liq, 1: SL, 2: TP
            } else if hit_sl {
                closed_indices.push((idx, 1, pos.stop_loss.unwrap()));
            } else if hit_tp {
                closed_indices.push((idx, 2, pos.take_profit.unwrap()));
            }
        }

        // Process closures from last to first
        closed_indices.reverse();
        for (idx, close_type, exit_price) in closed_indices {
            let pos = active_positions.remove(idx);
            let opening_fee = pos.size_btc * pos.entry_price * fee_rate;
            if close_type == 0 {
                println!("🔥 LIQUIDACIÓN DETECTADA: La posición {:?} fue liquidada al tocar el precio de {:.2} USDT (Entrada: {:.2} USDT). Se perdió el margen de {:.2} USDT.",
                    pos.position_type, pos.liquidation_price, pos.entry_price, pos.margin
                );
                num_liquidaciones += 1;
                let net_pnl = -pos.margin - opening_fee;
                trade_pnls.push(net_pnl);
            } else {
                let closing_value = pos.size_btc * exit_price;
                let closing_fee = closing_value * fee_rate;
                let real_pnl = match pos.position_type {
                    PositionType::Long => (exit_price - pos.entry_price) * pos.size_btc,
                    PositionType::Short => (pos.entry_price - exit_price) * pos.size_btc,
                    _ => 0.0,
                };
                let return_value = pos.margin + real_pnl - closing_fee;
                saldo_usdt += return_value;
                let net_pnl = real_pnl - opening_fee - closing_fee;
                trade_pnls.push(net_pnl);
                if close_type == 1 {
                    println!("🛑 STOP LOSS EJECUTADO: Posición {:?} cerrada a {:.2} USDT (Entrada: {:.2} USDT). Retorno: {:.2} USDT | Fee: {:.2} USDT | PnL Realizado: {:.2} USDT",
                        pos.position_type, exit_price, pos.entry_price, return_value, closing_fee, real_pnl
                    );
                } else {
                    println!("🎯 TAKE PROFIT EJECUTADO: Posición {:?} cerrada a {:.2} USDT (Entrada: {:.2} USDT). Retorno: {:.2} USDT | Fee: {:.2} USDT | PnL Realizado: {:.2} USDT",
                        pos.position_type, exit_price, pos.entry_price, return_value, closing_fee, real_pnl
                    );
                }
            }
        }

        // 2. Calcular PnL flotante y equidad
        let mut total_floating_pnl = 0.0;
        let mut total_margins = 0.0;
        for pos in &active_positions {
            let pnl = match pos.position_type {
                PositionType::Long => (precio_actual - pos.entry_price) * pos.size_btc,
                PositionType::Short => (pos.entry_price - precio_actual) * pos.size_btc,
                _ => 0.0,
            };
            total_floating_pnl += pnl;
            total_margins += pos.margin;
        }

        let equity = saldo_usdt + total_margins + total_floating_pnl;

        let bh_equity = initial_balance * (candle.close / initial_price);
        equity_curve.push((date_str.clone(), equity, bh_equity));

        if equity > peak_equity {
            peak_equity = equity;
        }
        let dd = (peak_equity - equity) / peak_equity;
        if dd > max_drawdown {
            max_drawdown = dd;
        }

        println!("\n=== [Paso {}/{}] {} | Precio Actual: {:.2} USDT ===", i + 1, candles.len(), date_str, precio_actual);
        println!("💼 Estado: Saldo: {:.2} USDT | Margen Total: {:.2} USDT | Posiciones Activas: {} | PnL Flotante: {:.2} USDT | Equity: {:.2} USDT",
            saldo_usdt, total_margins, active_positions.len(), total_floating_pnl, equity
        );

        // 3. Generar la ventana deslizante
        let mut history_str = String::new();
        let start_idx = i.saturating_sub(9);
        let actual_history_len = i - start_idx + 1;
        for (idx, prev_candle) in candles[start_idx..=i].iter().enumerate() {
            let label = if start_idx + idx == i { " (Actual)" } else { "" };
            history_str.push_str(&format!(
                "- t-{}: O:{:.1}, H:{:.1}, L:{:.1}, C:{:.1}, V:{:.0}{}\n",
                actual_history_len - 1 - idx, prev_candle.open, prev_candle.high, prev_candle.low, prev_candle.close, prev_candle.volume, label
            ));
        }

        // List open positions for prompt
        let mut positions_str = String::new();
        if active_positions.is_empty() {
            positions_str.push_str("- Ninguna posición activa.");
        } else {
            for (idx, pos) in active_positions.iter().enumerate() {
                let pnl = match pos.position_type {
                    PositionType::Long => (precio_actual - pos.entry_price) * pos.size_btc,
                    PositionType::Short => (pos.entry_price - precio_actual) * pos.size_btc,
                    _ => 0.0,
                };
                let roe = (pnl / pos.margin) * 100.0;
                let sl_str = match pos.stop_loss {
                    Some(sl) => format!("{:.2} USDT", sl),
                    None => "Ninguno".to_string(),
                };
                let tp_str = match pos.take_profit {
                    Some(tp) => format!("{:.2} USDT", tp),
                    None => "Ninguno".to_string(),
                };
                positions_str.push_str(&format!(
                    "- Posición #{}: {:?} | Entrada: {:.2} USDT | Margen: {:.2} USDT | Tamaño: {:.6} BTC | Liq: {:.2} USDT | SL: {} | TP: {} | PnL Flotante: {:.2} USDT (ROE: {:.2}%)\n",
                    idx + 1, pos.position_type, pos.entry_price, pos.margin, pos.size_btc, pos.liquidation_price, sl_str, tp_str, pnl, roe
                ));
            }
        }

        // Calcular la progresión de los indicadores técnicos para pasárselos a Gemma
        let mut indicators_str = String::new();
        for idx in start_idx..=i {
            let win_start = idx.saturating_sub(9);
            let (tend, vol, _, pres) = calculate_indicators(&candles, win_start, idx, candles[idx].close);
            let label = if idx == i { " (Actual)" } else { "" };
            let offset = i - idx;
            indicators_str.push_str(&format!(
                "- t-{}: Tendencia: {}, Volatilidad: {}, Presión Cuerpo/Volumen: {}{}\n",
                offset, tend, vol, pres, label
            ));
        }

        // 4. Prompt a Gemma
        let user_prompt = format!(
            "Precio actual de BTC (Cierre): {:.2} USDT\n\n\
             Historial de las últimas 10 velas (de más antigua a más reciente):\n\
             {}\n\
             Indicadores Técnicos (Ventana de 10 velas, de más antigua a más reciente):\n\
             {}\n\
             Estado de tu Cartera:\n\
             - Saldo libre en USDT (no en margen): {:.2} USDT\n\
             - Posiciones Activas:\n\
             {}\n\
             - Equidad total de la cuenta (Equity): {:.2} USDT\n\
             - Apalancamiento actual: {:.1}x\n\
             - Comisión por operación: 0.05% sobre el volumen operado\n\n\
             ¿Qué acción tomas? Responde estrictamente en formato JSON.",
            precio_actual, history_str, indicators_str, saldo_usdt, positions_str, equity, leverage
        );

        let mut retries = 3;
        let mut gemma_action = "FLAT".to_string();
        let mut gemma_analisis = "Sin análisis".to_string();
        let mut gemma_confidence = None;

        while retries > 0 {
            if verbose {
                println!("\n=== [ENVÍO A GEMMA] ===");
                println!("System Prompt:\n{}", system_prompt);
                println!("User Prompt:\n{}", user_prompt);
                println!("=======================");
            }
            match call_gemma(&client, &api_url, &api_token, &system_prompt, &user_prompt).await {
                Ok(content) => {
                    if verbose {
                        println!("\n=== [RESPUESTA DE GEMMA] ===");
                        println!("{}", content.trim());
                        println!("============================");
                    }
                    if let Some(parsed) = parse_gemma_response(&content) {
                        gemma_action = parsed.accion.to_uppercase().replace(" ", "_");
                        if let Some(ref ans) = parsed.analisis {
                            gemma_analisis = ans.clone();
                        }
                        gemma_confidence = parsed.confianza;

                        let conf = gemma_confidence.unwrap_or(0);
                        if confidence_threshold > 0 && conf < confidence_threshold {
                            if !active_positions.is_empty() {
                                println!("⚠️ Confianza ({}%) por debajo del umbral ({}%). Iniciando cierre de posiciones activas por seguridad.", conf, confidence_threshold);
                                gemma_action = "CERRAR_TODO".to_string();
                            } else if gemma_action != "FLAT" {
                                println!("⚠️ Gemma sugirió {} con confianza {}%, pero el umbral es {}%. Acción cambiada a FLAT.", gemma_action, conf, confidence_threshold);
                                gemma_action = "FLAT".to_string();
                            }
                        }
                        
                        // Apply Stop Loss updates
                        if let Some(ref sls) = parsed.stop_losses {
                            for (idx, sl_val) in sls.iter().enumerate() {
                                if idx < active_positions.len() {
                                    active_positions[idx].stop_loss = *sl_val;
                                    println!("⚙️ STOP LOSS ACTUALIZADO: Posición #{} -> {:?}", idx + 1, sl_val);
                                }
                            }
                        }

                        // Apply Take Profit updates
                        if let Some(ref tps) = parsed.take_profits {
                            for (idx, tp_val) in tps.iter().enumerate() {
                                if idx < active_positions.len() {
                                    active_positions[idx].take_profit = *tp_val;
                                    println!("⚙️ TAKE PROFIT ACTUALIZADO: Posición #{} -> {:?}", idx + 1, tp_val);
                                }
                            }
                        }

                        // Apply partial closures
                        if let Some(ref indices_to_close) = parsed.cerrar_posiciones {
                            let mut sorted_indices = indices_to_close.clone();
                            sorted_indices.sort_by(|a, b| b.cmp(a));
                            for idx_1based in sorted_indices {
                                if idx_1based > 0 && idx_1based <= active_positions.len() {
                                    let pos = active_positions.remove(idx_1based - 1);
                                    let closing_value = pos.size_btc * precio_actual;
                                    let closing_fee = closing_value * fee_rate;
                                    let opening_fee = pos.size_btc * pos.entry_price * fee_rate;
                                    let real_pnl = match pos.position_type {
                                        PositionType::Long => (precio_actual - pos.entry_price) * pos.size_btc,
                                        PositionType::Short => (pos.entry_price - precio_actual) * pos.size_btc,
                                        _ => 0.0,
                                    };
                                    let return_value = pos.margin + real_pnl - closing_fee;
                                    saldo_usdt += return_value;
                                    let net_pnl = real_pnl - opening_fee - closing_fee;
                                    trade_pnls.push(net_pnl);
                                    println!("💰 POSICIÓN CERRADA PARCIALMENTE: {:?} #{} cerrada a {:.2} USDT (Entrada: {:.2} USDT). Retorno: {:.2} USDT | Fee: {:.2} USDT | PnL Realizado: {:.2} USDT",
                                        pos.position_type, idx_1based, precio_actual, pos.entry_price, return_value, closing_fee, real_pnl
                                    );
                                }
                            }
                        }
                        break;
                    } else {
                        println!("⚠️ No se pudo parsear el JSON de Gemma. Reintentando... (Respuesta recibida: {})", content.trim());
                    }
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    println!("⚠️ Error en petición a Gemma: {}. Reintentando...", err_msg);
                    
                    if err_msg.contains("invalid_api_key") || err_msg.contains("Malformed LM Studio API token") || err_msg.contains("token") {
                        println!("\n🔑 LM Studio requiere un Token de API válido.");
                        println!("Puedes copiarlo de la UI de LM Studio (Developer tab -> API Keys).");
                        print!("Introduce tu LM Studio API Token: ");
                        let _ = io::stdout().flush();
                        let mut input = String::new();
                        if io::stdin().read_line(&mut input).is_ok() {
                            let new_token = input.trim().to_string();
                            if !new_token.is_empty() {
                                let _ = save_llm_config(db_path, &api_url, &new_token);
                                api_token = new_token;
                                println!("✅ Token guardado en la base de datos. Reintentando petición...");
                            }
                        }
                    }
                }
            }
            retries -= 1;
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }

        if let Some(conf) = gemma_confidence {
            if gemma_analisis != "Sin análisis" && !gemma_analisis.trim().is_empty() {
                println!("🤖 Gemma dice: {} (Confianza: {}%)", gemma_analisis, conf);
            } else {
                println!("🤖 Gemma (Confianza: {}%)", conf);
            }
        } else {
            if gemma_analisis != "Sin análisis" && !gemma_analisis.trim().is_empty() {
                println!("🤖 Gemma dice: {}", gemma_analisis);
            }
        }
        println!("📈 Acción elegida: {}", gemma_action);

        // 5. Ejecutar la acción elegida
        if gemma_action == "ABRIR_LONG" {
            // Abrir nuevo LONG
            let margin = equity * (risk_percent / 100.0);
            let size_usdt = margin * leverage;
            let opening_fee = size_usdt * fee_rate;

            if saldo_usdt >= margin + opening_fee {
                saldo_usdt -= margin + opening_fee;
                let pos_size_btc = size_usdt / precio_actual;
                let pos_liq_price = precio_actual * (1.0 - liq_percent / 100.0);
                active_positions.push(Position {
                    position_type: PositionType::Long,
                    margin,
                    size_btc: pos_size_btc,
                    entry_price: precio_actual,
                    liquidation_price: pos_liq_price,
                    stop_loss: None,
                    take_profit: None,
                });
                num_compras += 1;
                println!("🛒 LONG ABIERTO: Margen: {:.2} USDT | Tamaño: {:.6} BTC (${:.2}) | Liq: {:.2} USDT | Fee: {:.2} USDT",
                    margin, pos_size_btc, size_usdt, pos_liq_price, opening_fee
                );
            } else {
                println!("⏳ Margen/saldo insuficiente para abrir LONG.");
            }
        } else if gemma_action == "CERRAR_LONG" {
            // Cerrar todos los LONGS
            let mut temp_positions = Vec::new();
            std::mem::swap(&mut active_positions, &mut temp_positions);
            for pos in temp_positions {
                if pos.position_type == PositionType::Long {
                    let closing_value = pos.size_btc * precio_actual;
                    let closing_fee = closing_value * fee_rate;
                    let opening_fee = pos.size_btc * pos.entry_price * fee_rate;
                    let real_pnl = (precio_actual - pos.entry_price) * pos.size_btc;
                    let return_value = pos.margin + real_pnl - closing_fee;

                    saldo_usdt += return_value;
                    let net_pnl = real_pnl - opening_fee - closing_fee;
                    trade_pnls.push(net_pnl);
                    println!("💰 LONG CERRADO: Retorno: {:.2} USDT | Fee: {:.2} USDT | PnL Realizado: {:.2} USDT",
                        return_value, closing_fee, real_pnl
                    );
                } else {
                    active_positions.push(pos);
                }
            }
            num_ventas += 1;
        } else if gemma_action == "ABRIR_SHORT" {
            // Abrir nuevo SHORT
            let margin = equity * (risk_percent / 100.0);
            let size_usdt = margin * leverage;
            let opening_fee = size_usdt * fee_rate;

            if saldo_usdt >= margin + opening_fee {
                saldo_usdt -= margin + opening_fee;
                let pos_size_btc = size_usdt / precio_actual;
                let pos_liq_price = precio_actual * (1.0 + liq_percent / 100.0);
                active_positions.push(Position {
                    position_type: PositionType::Short,
                    margin,
                    size_btc: pos_size_btc,
                    entry_price: precio_actual,
                    liquidation_price: pos_liq_price,
                    stop_loss: None,
                    take_profit: None,
                });
                num_ventas += 1;
                println!("🛒 SHORT ABIERTO: Margen: {:.2} USDT | Tamaño: {:.6} BTC (${:.2}) | Liq: {:.2} USDT | Fee: {:.2} USDT",
                    margin, pos_size_btc, size_usdt, pos_liq_price, opening_fee
                );
            } else {
                println!("⏳ Margen/saldo insuficiente para abrir SHORT.");
            }
        } else if gemma_action == "CERRAR_SHORT" {
            // Cerrar todos los SHORTS
            let mut temp_positions = Vec::new();
            std::mem::swap(&mut active_positions, &mut temp_positions);
            for pos in temp_positions {
                if pos.position_type == PositionType::Short {
                    let closing_value = pos.size_btc * precio_actual;
                    let closing_fee = closing_value * fee_rate;
                    let opening_fee = pos.size_btc * pos.entry_price * fee_rate;
                    let real_pnl = (pos.entry_price - precio_actual) * pos.size_btc;
                    let return_value = pos.margin + real_pnl - closing_fee;

                    saldo_usdt += return_value;
                    let net_pnl = real_pnl - opening_fee - closing_fee;
                    trade_pnls.push(net_pnl);
                    println!("💰 SHORT CERRADO: Retorno: {:.2} USDT | Fee: {:.2} USDT | PnL Realizado: {:.2} USDT",
                        return_value, closing_fee, real_pnl
                    );
                } else {
                    active_positions.push(pos);
                }
            }
            num_compras += 1;
        } else if gemma_action == "CERRAR_TODO" {
            if !active_positions.is_empty() {
                let mut temp_positions = Vec::new();
                std::mem::swap(&mut active_positions, &mut temp_positions);
                for pos in temp_positions {
                    let closing_value = pos.size_btc * precio_actual;
                    let closing_fee = closing_value * fee_rate;
                    let opening_fee = pos.size_btc * pos.entry_price * fee_rate;
                    let real_pnl = match pos.position_type {
                        PositionType::Long => (precio_actual - pos.entry_price) * pos.size_btc,
                        PositionType::Short => (pos.entry_price - precio_actual) * pos.size_btc,
                        _ => 0.0,
                    };
                    let return_value = pos.margin + real_pnl - closing_fee;
                    saldo_usdt += return_value;
                    let net_pnl = real_pnl - opening_fee - closing_fee;
                    trade_pnls.push(net_pnl);
                    println!("💰 POSICIÓN {:?} CERRADA POR SEGURIDAD (CONFIANZA BAJA): Retorno: {:.2} USDT | Fee: {:.2} USDT | PnL Realizado: {:.2} USDT",
                        pos.position_type, return_value, closing_fee, real_pnl
                    );
                }
            }
        } else {
            println!("⏳ Manteniendo posición/Sin acción ejecutada ({}).", gemma_action);
        }

        // Guardar progreso intermedio cada 10 pasos para ver actualización en vivo del dashboard y CSV
        if (i + 1) % 10 == 0 {
            let bot_equity_series: Vec<f64> = equity_curve.iter().map(|(_, eq, _)| *eq).collect();
            let bh_equity_series: Vec<f64> = equity_curve.iter().map(|(_, _, bh)| *bh).collect();
            let temp_correlation = calculate_correlation(&bot_equity_series, &bh_equity_series);
            let _ = save_equity_curve(&equity_curve, "equity_curve.csv");
            
            // Calc temp advanced metrics
            let total_closed_trades = trade_pnls.len();
            let wins: Vec<f64> = trade_pnls.iter().cloned().filter(|&p| p > 0.0).collect();
            let losses: Vec<f64> = trade_pnls.iter().cloned().filter(|&p| p <= 0.0).collect();
            let temp_winrate = if total_closed_trades > 0 {
                (wins.len() as f64 / total_closed_trades as f64) * 100.0
            } else {
                0.0
            };
            let gross_profit: f64 = wins.iter().sum();
            let gross_loss: f64 = losses.iter().sum::<f64>().abs();
            let temp_profit_factor = if gross_loss > 0.0 {
                gross_profit / gross_loss
            } else if gross_profit > 0.0 {
                99.9
            } else {
                0.0
            };

            let mut temp_max_dd_usd = 0.0;
            let mut temp_peak_eq_usd = initial_balance;
            for (_, eq, _) in &equity_curve {
                if *eq > temp_peak_eq_usd {
                    temp_peak_eq_usd = *eq;
                }
                let dd_usd = temp_peak_eq_usd - *eq;
                if dd_usd > temp_max_dd_usd {
                    temp_max_dd_usd = dd_usd;
                }
            }
            let current_eq = equity_curve.last().map(|(_, eq, _)| *eq).unwrap_or(initial_balance);
            let temp_recovery_factor = if temp_max_dd_usd > 0.0 {
                (current_eq - initial_balance) / temp_max_dd_usd
            } else {
                0.0
            };

            let returns: Vec<f64> = equity_curve.windows(2).map(|w| (w[1].1 - w[0].1) / w[0].1).collect();
            let temp_sharpe = if returns.len() > 1 {
                let mean = returns.iter().sum::<f64>() / returns.len() as f64;
                let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (returns.len() - 1) as f64;
                let std_dev = variance.sqrt();
                if std_dev > 0.0 {
                    let step_sharpe = mean / std_dev;
                    let annualization_factor = match timeframe {
                        "1h" => (24.0 * 365.0f64).sqrt(),
                        "4h" => (6.0 * 365.0f64).sqrt(),
                        "1d" => 365.0f64.sqrt(),
                        _ => 1.0,
                    };
                    step_sharpe * annualization_factor
                } else {
                    0.0
                }
            } else {
                0.0
            };

            let mut max_eq_stag = initial_balance;
            let mut current_stagnation = 0;
            let mut stagnation_periods = Vec::new();
            for (_, eq, _) in &equity_curve {
                if *eq >= max_eq_stag {
                    if current_stagnation > 0 {
                        stagnation_periods.push(current_stagnation);
                        current_stagnation = 0;
                    }
                    max_eq_stag = *eq;
                } else {
                    current_stagnation += 1;
                }
            }
            if current_stagnation > 0 {
                stagnation_periods.push(current_stagnation);
            }
            let temp_max_stagnation = stagnation_periods.iter().max().copied().unwrap_or(0);
            let temp_avg_stagnation = if !stagnation_periods.is_empty() {
                stagnation_periods.iter().sum::<usize>() as f64 / stagnation_periods.len() as f64
            } else {
                0.0
            };

            let _ = generate_dashboard(
                &equity_curve,
                num_compras,
                num_ventas,
                num_liquidaciones,
                max_drawdown,
                temp_correlation,
                temp_winrate,
                temp_profit_factor,
                temp_sharpe,
                temp_recovery_factor,
                temp_avg_stagnation,
                temp_max_stagnation,
                "dashboard.html"
            );
        }

        // Wait a bit to avoid overloading LM Studio or too fast output
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    // Guardar Equity Curve final
    let mut final_floating_pnl = 0.0;
    let mut final_margins = 0.0;
    for pos in &active_positions {
        let closing_value = pos.size_btc * candles.last().unwrap().close;
        let closing_fee = closing_value * fee_rate;
        let opening_fee = pos.size_btc * pos.entry_price * fee_rate;
        let pnl = match pos.position_type {
            PositionType::Long => (candles.last().unwrap().close - pos.entry_price) * pos.size_btc,
            PositionType::Short => (pos.entry_price - candles.last().unwrap().close) * pos.size_btc,
            _ => 0.0,
        };
        let net_pnl = pnl - opening_fee - closing_fee;
        trade_pnls.push(net_pnl);
        final_floating_pnl += pnl;
        final_margins += pos.margin;
    }
    let final_equity = saldo_usdt + final_margins + final_floating_pnl;

    println!("\n🏁 Backtest completado.");
    println!("📈 Equidad Final: {:.2} USDT", final_equity);

    let bot_equity_series: Vec<f64> = equity_curve.iter().map(|(_, eq, _)| *eq).collect();
    let bh_equity_series: Vec<f64> = equity_curve.iter().map(|(_, _, bh)| *bh).collect();
    let correlation = calculate_correlation(&bot_equity_series, &bh_equity_series);
    println!("📈 Correlación con Buy & Hold: {:.4}", correlation);

    // Calc final metrics
    let total_closed_trades = trade_pnls.len();
    let wins: Vec<f64> = trade_pnls.iter().cloned().filter(|&p| p > 0.0).collect();
    let losses: Vec<f64> = trade_pnls.iter().cloned().filter(|&p| p <= 0.0).collect();
    let final_winrate = if total_closed_trades > 0 {
        (wins.len() as f64 / total_closed_trades as f64) * 100.0
    } else {
        0.0
    };
    let gross_profit: f64 = wins.iter().sum();
    let gross_loss: f64 = losses.iter().sum::<f64>().abs();
    let final_profit_factor = if gross_loss > 0.0 {
        gross_profit / gross_loss
    } else if gross_profit > 0.0 {
        99.9
    } else {
        0.0
    };

    let mut final_max_dd_usd = 0.0;
    let mut final_peak_eq_usd = initial_balance;
    for (_, eq, _) in &equity_curve {
        if *eq > final_peak_eq_usd {
            final_peak_eq_usd = *eq;
        }
        let dd_usd = final_peak_eq_usd - *eq;
        if dd_usd > final_max_dd_usd {
            final_max_dd_usd = dd_usd;
        }
    }
    let final_recovery_factor = if final_max_dd_usd > 0.0 {
        (final_equity - initial_balance) / final_max_dd_usd
    } else {
        0.0
    };

    let returns: Vec<f64> = equity_curve.windows(2).map(|w| (w[1].1 - w[0].1) / w[0].1).collect();
    let final_sharpe = if returns.len() > 1 {
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (returns.len() - 1) as f64;
        let std_dev = variance.sqrt();
        if std_dev > 0.0 {
            let step_sharpe = mean / std_dev;
            let annualization_factor = match timeframe {
                "1h" => (24.0 * 365.0f64).sqrt(),
                "4h" => (6.0 * 365.0f64).sqrt(),
                "1d" => 365.0f64.sqrt(),
                _ => 1.0,
            };
            step_sharpe * annualization_factor
        } else {
            0.0
        }
    } else {
        0.0
    };

    let mut max_eq_stag = initial_balance;
    let mut current_stagnation = 0;
    let mut stagnation_periods = Vec::new();
    for (_, eq, _) in &equity_curve {
        if *eq >= max_eq_stag {
            if current_stagnation > 0 {
                stagnation_periods.push(current_stagnation);
                current_stagnation = 0;
            }
            max_eq_stag = *eq;
        } else {
            current_stagnation += 1;
        }
    }
    if current_stagnation > 0 {
        stagnation_periods.push(current_stagnation);
    }
    let final_max_stagnation = stagnation_periods.iter().max().copied().unwrap_or(0);
    let final_avg_stagnation = if !stagnation_periods.is_empty() {
        stagnation_periods.iter().sum::<usize>() as f64 / stagnation_periods.len() as f64
    } else {
        0.0
    };

    println!("📈 Winrate: {:.2}% ({} / {})", final_winrate, wins.len(), total_closed_trades);
    println!("📈 Profit Factor: {:.2}", final_profit_factor);
    println!("📈 Sharpe Ratio: {:.2}", final_sharpe);
    println!("📈 Recovery Factor: {:.2}", final_recovery_factor);
    println!("📈 Stagnation: Max {} velas, Promedio {:.2} velas", final_max_stagnation, final_avg_stagnation);

    save_equity_curve(&equity_curve, "equity_curve.csv")?;
    println!("📊 Curva de equidad guardada en 'equity_curve.csv'");
    
    generate_dashboard(
        &equity_curve,
        num_compras,
        num_ventas,
        num_liquidaciones,
        max_drawdown,
        correlation,
        final_winrate,
        final_profit_factor,
        final_sharpe,
        final_recovery_factor,
        final_avg_stagnation,
        final_max_stagnation,
        "dashboard.html"
    )?;
    println!("🖥️ Dashboard interactivo guardado en 'dashboard.html'");

    Ok(())
}
