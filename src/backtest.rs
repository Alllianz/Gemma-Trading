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

        // 1. Verificar si hay liquidación o ejecución de SL en esta vela (usando high/low)
        let mut closed_indices = Vec::new();
        for (idx, pos) in active_positions.iter().enumerate() {
            let mut liquidado = false;
            let mut hit_sl = false;
            match pos.position_type {
                PositionType::Long => {
                    if candle.low <= pos.liquidation_price {
                        liquidado = true;
                    } else if let Some(sl) = pos.stop_loss {
                        if candle.low <= sl {
                            hit_sl = true;
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
                    }
                }
                _ => {}
            }
            if liquidado {
                closed_indices.push((idx, true, pos.liquidation_price));
            } else if hit_sl {
                closed_indices.push((idx, false, pos.stop_loss.unwrap()));
            }
        }

        // Process closures from last to first
        closed_indices.reverse();
        for (idx, is_liq, exit_price) in closed_indices {
            let pos = active_positions.remove(idx);
            if is_liq {
                println!("🔥 LIQUIDACIÓN DETECTADA: La posición {:?} fue liquidada al tocar el precio de {:.2} USDT (Entrada: {:.2} USDT). Se perdió el margen de {:.2} USDT.",
                    pos.position_type, pos.liquidation_price, pos.entry_price, pos.margin
                );
                num_liquidaciones += 1;
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
                println!("🛑 STOP LOSS EJECUTADO: Posición {:?} cerrada a {:.2} USDT (Entrada: {:.2} USDT). Retorno: {:.2} USDT | Fee: {:.2} USDT | PnL Realizado: {:.2} USDT",
                    pos.position_type, exit_price, pos.entry_price, return_value, closing_fee, real_pnl
                );
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
        let start_idx = i.saturating_sub(29);
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
                positions_str.push_str(&format!(
                    "- Posición #{}: {:?} | Entrada: {:.2} USDT | Margen: {:.2} USDT | Tamaño: {:.6} BTC | Liq: {:.2} USDT | SL: {} | PnL Flotante: {:.2} USDT (ROE: {:.2}%)\n",
                    idx + 1, pos.position_type, pos.entry_price, pos.margin, pos.size_btc, pos.liquidation_price, sl_str, pnl, roe
                ));
            }
        }

        // Calcular indicadores técnicos para pasárselos a Gemma
        let (indicador_tendencia, indicador_volatilidad, _indicador_posicion, indicador_presion) = 
            calculate_indicators(&candles, start_idx, i, precio_actual);

        // 4. Prompt a Gemma
        let user_prompt = format!(
            "Precio actual de BTC (Cierre): {:.2} USDT\n\n\
             Historial de las últimas 30 velas (de más antigua a más reciente):\n\
             {}\n\
             Indicadores Técnicos (Ventana de 30 velas):\n\
             - Tendencia: {}\n\
             - Volatilidad: {}\n\
             - Presión Cuerpo/Volumen: {}\n\n\
             Estado de tu Cartera:\n\
             - Saldo libre en USDT (no en margen): {:.2} USDT\n\
             - Posiciones Activas:\n\
             {}\n\
             - Equidad total de la cuenta (Equity): {:.2} USDT\n\
             - Apalancamiento actual: {:.1}x\n\
             - Comisión por operación: 0.05% sobre el volumen operado\n\n\
             ¿Qué acción tomas? Responde estrictamente en formato JSON.",
            precio_actual, history_str, indicador_tendencia, indicador_volatilidad, indicador_presion, saldo_usdt, positions_str, equity, leverage
        );

        let mut retries = 3;
        let mut gemma_action = "FLAT".to_string();
        let mut gemma_analisis = "Sin análisis".to_string();
        let mut gemma_confidence = None;

        while retries > 0 {
            match call_gemma(&client, &api_url, &api_token, &system_prompt, &user_prompt).await {
                Ok(content) => {
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

                        // Apply partial closures
                        if let Some(ref indices_to_close) = parsed.cerrar_posiciones {
                            let mut sorted_indices = indices_to_close.clone();
                            sorted_indices.sort_by(|a, b| b.cmp(a));
                            for idx_1based in sorted_indices {
                                if idx_1based > 0 && idx_1based <= active_positions.len() {
                                    let pos = active_positions.remove(idx_1based - 1);
                                    let closing_value = pos.size_btc * precio_actual;
                                    let closing_fee = closing_value * fee_rate;
                                    let real_pnl = match pos.position_type {
                                        PositionType::Long => (precio_actual - pos.entry_price) * pos.size_btc,
                                        PositionType::Short => (pos.entry_price - precio_actual) * pos.size_btc,
                                        _ => 0.0,
                                    };
                                    let return_value = pos.margin + real_pnl - closing_fee;
                                    saldo_usdt += return_value;
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
                    let real_pnl = (precio_actual - pos.entry_price) * pos.size_btc;
                    let return_value = pos.margin + real_pnl - closing_fee;

                    saldo_usdt += return_value;
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
                    let real_pnl = (pos.entry_price - precio_actual) * pos.size_btc;
                    let return_value = pos.margin + real_pnl - closing_fee;

                    saldo_usdt += return_value;
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
                    let real_pnl = match pos.position_type {
                        PositionType::Long => (precio_actual - pos.entry_price) * pos.size_btc,
                        PositionType::Short => (pos.entry_price - precio_actual) * pos.size_btc,
                        _ => 0.0,
                    };
                    let return_value = pos.margin + real_pnl - closing_fee;
                    saldo_usdt += return_value;
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
            let _ = generate_dashboard(&equity_curve, num_compras, num_ventas, num_liquidaciones, max_drawdown, temp_correlation, "dashboard.html");
        }

        // Wait a bit to avoid overloading LM Studio or too fast output
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    // Guardar Equity Curve final
    let mut final_floating_pnl = 0.0;
    let mut final_margins = 0.0;
    for pos in &active_positions {
        let pnl = match pos.position_type {
            PositionType::Long => (candles.last().unwrap().close - pos.entry_price) * pos.size_btc,
            PositionType::Short => (pos.entry_price - candles.last().unwrap().close) * pos.size_btc,
            _ => 0.0,
        };
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

    save_equity_curve(&equity_curve, "equity_curve.csv")?;
    println!("📊 Curva de equidad guardada en 'equity_curve.csv'");
    generate_dashboard(&equity_curve, num_compras, num_ventas, num_liquidaciones, max_drawdown, correlation, "dashboard.html")?;
    println!("🖥️ Dashboard interactivo guardado en 'dashboard.html'");

    Ok(())
}
