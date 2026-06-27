use std::io::{self, Write};
use chrono::TimeZone;
use crate::types::{Position, PositionType, BoxType, BoxAction};
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

fn execute_box_action(
    box_name: &str,
    box_action: &BoxAction,
    box_type: BoxType,
    saldo_usdt: &mut f64,
    active_positions: &mut Vec<Position>,
    equity: f64,
    risk_percent: f64,
    leverage: f64,
    fee_rate: f64,
    precio_actual: f64,
    dynamic_risk_leverage: bool,
    trade_pnls: &mut Vec<f64>,
    paso_acciones: &mut Vec<String>,
    paso_precios: &mut Vec<String>,
    num_compras: &mut usize,
    num_ventas: &mut usize,
) {
    // 1. Procesar cierres (cerrar == true)
    if box_action.cerrar {
        let mut temp_positions = Vec::new();
        std::mem::swap(active_positions, &mut temp_positions);
        for pos in temp_positions {
            if pos.box_type == box_type {
                let closing_value = pos.size_btc * precio_actual;
                let closing_fee = closing_value * fee_rate;
                let opening_fee = pos.size_btc * pos.entry_price * fee_rate;
                let real_pnl = match pos.position_type {
                    PositionType::Long => (precio_actual - pos.entry_price) * pos.size_btc,
                    PositionType::Short => (pos.entry_price - precio_actual) * pos.size_btc,
                    _ => 0.0,
                };
                let return_value = pos.margin + real_pnl - closing_fee;
                *saldo_usdt += return_value;
                let net_pnl = real_pnl - opening_fee - closing_fee;
                trade_pnls.push(net_pnl);
                println!("💰 [{}] POSICIÓN CERRADA: {:?} al precio de {:.2} USDT (Entrada: {:.2} USDT). Retorno: {:.2} USDT | Fee: {:.2} USDT | PnL Realizado: {:.2} USDT",
                    box_name, pos.position_type, precio_actual, pos.entry_price, return_value, closing_fee, real_pnl
                );
                paso_acciones.push(format!("CERRAR_{:?}_{:?}", box_type, pos.position_type));
                paso_precios.push(format!("{:.2}", precio_actual));
                if pos.position_type == PositionType::Long {
                    *num_ventas += 1;
                } else {
                    *num_compras += 1;
                }
            } else {
                active_positions.push(pos);
            }
        }
    }

    // 2. Procesar Stop Loss updates
    if let Some(sl_val) = box_action.stop_loss {
        for pos in active_positions.iter_mut() {
            if pos.box_type == box_type {
                pos.stop_loss = Some(sl_val);
                println!("⚙️ [{}] STOP LOSS ACTUALIZADO: Posición {:?} -> {:.2} USDT", box_name, pos.position_type, sl_val);
            }
        }
    }

    // 3. Procesar nuevas aperturas (accion == "LONG" o "SHORT")
    let action_upper = box_action.accion.to_uppercase();
    if action_upper == "LONG" || action_upper == "SHORT" {
        let desired_type = if action_upper == "LONG" { PositionType::Long } else { PositionType::Short };
        
        let active_box_positions: Vec<&Position> = active_positions.iter()
            .filter(|p| p.box_type == box_type && p.position_type == desired_type)
            .collect();
        
        let can_open = if active_box_positions.is_empty() {
            true
        } else {
            // Solo se puede abrir otra posición si alguna posición activa de la caja y tipo deseado tiene >= 200% ROI
            active_box_positions.iter().any(|pos| {
                let pnl = match pos.position_type {
                    PositionType::Long => (precio_actual - pos.entry_price) * pos.size_btc,
                    PositionType::Short => (pos.entry_price - precio_actual) * pos.size_btc,
                    _ => 0.0,
                };
                let roe = (pnl / pos.margin) * 100.0;
                roe >= 200.0
            })
        };

        if !can_open {
            println!("⏳ [{}] Bloqueado abrir {:?}: Existe una posición activa pero ninguna tiene >= +200% ROI.", box_name, desired_type);
        } else {
            let box_leverage = if dynamic_risk_leverage {
                box_action.apalancamiento.unwrap_or(leverage)
            } else {
                leverage
            };
            
            // Asignación de capital según la caja:
            // LT: 80% del equity de la cuenta. ST: 20% del equity de la cuenta.
            let box_allocation = match box_type {
                BoxType::LT => 0.80,
                BoxType::ST => 0.20,
            };
            
            let mut margin = (equity * box_allocation) * (risk_percent / 100.0);
            let mut size_usdt = margin * box_leverage;
            let mut opening_fee = size_usdt * fee_rate;

            if margin + opening_fee > *saldo_usdt {
                margin = (*saldo_usdt / (1.0 + box_leverage * fee_rate)) - 0.05;
                size_usdt = margin * box_leverage;
                opening_fee = size_usdt * fee_rate;
            }

            if margin > 0.01 && *saldo_usdt >= margin + opening_fee {
                *saldo_usdt -= margin + opening_fee;
                let pos_size_btc = size_usdt / precio_actual;
                let pos_liq_percent = get_liquidation_percentage(box_leverage);
                let pos_liq_price = match desired_type {
                    PositionType::Long => precio_actual * (1.0 - pos_liq_percent / 100.0),
                    PositionType::Short => precio_actual * (1.0 + pos_liq_percent / 100.0),
                    _ => precio_actual,
                };
                
                active_positions.push(Position {
                    position_type: desired_type,
                    margin,
                    size_btc: pos_size_btc,
                    entry_price: precio_actual,
                    liquidation_price: pos_liq_price,
                    stop_loss: box_action.stop_loss,
                    box_type,
                });
                
                if desired_type == PositionType::Long {
                    *num_compras += 1;
                } else {
                    *num_ventas += 1;
                }

                println!("🛒 [{}] {:?} ABIERTO: Margen: {:.2} USDT | Apalancamiento: {:.1}x | Tamaño: {:.6} BTC (${:.2}) | Liq: {:.2} USDT | Fee: {:.2} USDT | SL: {:?}",
                    box_name, desired_type, margin, box_leverage, pos_size_btc, size_usdt, pos_liq_price, opening_fee, box_action.stop_loss
                );
                paso_acciones.push(format!("ABRIR_{:?}_{:?}", box_type, desired_type));
                paso_precios.push(format!("{:.2}", precio_actual));
            } else {
                println!("⏳ [{}] Margen/saldo insuficiente ({:.2} USDT de saldo) para abrir {:?}.", box_name, *saldo_usdt, desired_type);
            }
        }
    }
}

pub async fn run_backtest(
    db_path: &str,
    timeframe: &str,
    leverage: f64,
    risk_percent: f64,
    limit: Option<usize>,
    _confidence_threshold: u32,
    verbose: bool,
    dynamic_risk_leverage: bool,
    trading_start_date: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let candles = get_candles(db_path, timeframe, limit)?;
    if candles.is_empty() {
        println!("❌ No hay velas en la base de datos. Descarga velas primero (Opción 1).");
        return Ok(());
    }

    // Parse trading start timestamp
    let start_timestamp = if let Some(ref date_str) = trading_start_date {
        if let Ok(naive_date) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
            naive_date.and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        } else {
            0
        }
    } else {
        0
    };

    // Find the index where trading should start
    let mut start_trade_idx = 0;
    for (idx, candle) in candles.iter().enumerate() {
        if candle.open_time >= start_timestamp {
            start_trade_idx = idx;
            break;
        }
    }

    if start_trade_idx >= candles.len() {
        println!("❌ La fecha de inicio de trading especificada ({:?}) está después de la última vela en la DB.", trading_start_date);
        return Ok(());
    }

    if start_trade_idx > 0 {
        println!("📈 Período de precalentamiento activo: Se usarán {} velas previas para los indicadores técnicos.", start_trade_idx);
        let start_date_str = chrono::Utc.timestamp_millis_opt(candles[start_trade_idx].open_time)
            .unwrap()
            .format(if timeframe == "1d" { "%Y-%m-%d" } else { "%Y-%m-%d %H:%M:%S" })
            .to_string();
        println!("🚀 Las operaciones comenzarán en la vela del: {} UTC", start_date_str);
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

    let mut equity_curve: Vec<(String, f64, f64, String, String)> = Vec::new();
    let initial_price = candles[start_trade_idx].close;
    let initial_balance = 10000.0;
    let mut trade_pnls: Vec<f64> = Vec::new();

    // El umbral de liquidación según la fórmula

    let system_prompt = format!(
        "CRITICAL RISK MANAGEMENT:
- DO NOT use the <think> tag. You are FORBIDDEN from thinking, reasoning, or analyzing.
- Go straight from the market data to the raw JSON. Do not write a single word of prose.
- If you violate this rule, the parser will crash. Start your response directly with '{{'.

INSTRUCTIONS:

Strategy & Capital Allocation (Base Leverage: {}X):
- Two boxes: 80 percent Long-Term (LT), 20 percent Short-Term (ST). Can hold 1 LT and 1 ST position simultaneously (e.g. both Long & Short).
- Leverage: Select between 5.0 and 10.0 for any position (include \"apalancamiento\": X in the box JSON).
- Add position: You are authorized to open your first LT position freely. You are authorized to open an ADDITIONAL/SECOND LT position only if an existing one has >= 200 percent ROI.

Trend Priority: 
- Long-Term (LT) Box: Trade ONLY in the direction of the long-term trend (EMA50 and EMA200).
- Short-Term (ST) Box: Authorized to trade against the macro trend based on short-term fluctuations.

Position Actions per Box:
- To open a new trade: set \"accion\" to \"LONG\" or \"SHORT\" and \"cerrar\" to false.
- To maintain an active trade without changes: set \"accion\" to \"HOLD\" and \"cerrar\" to false.
- To close an active trade completely: set \"accion\" to \"FLAT\" and \"cerrar\" to true.
- If a box has no active position and you do not want to open one: set \"accion\" to \"HOLD\", \"cerrar\" to false, and \"stop_loss\" to null.

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

    let mut active_positions: Vec<Position> = Vec::new();

    for (i, candle) in candles.iter().enumerate() {
        if i < start_trade_idx {
            continue;
        }

        let date_format = if timeframe == "1d" { "%Y-%m-%d" } else { "%Y-%m-%d %H:%M:%S" };
        let date_str = chrono::Utc.timestamp_millis_opt(candle.open_time)
            .unwrap()
            .format(date_format)
            .to_string();

        let precio_actual = candle.close;

        let mut paso_acciones = Vec::new();
        let mut paso_precios = Vec::new();

        // 1. Verificar si hay liquidación o ejecución de SL/TP en esta vela (usando high/low)
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
                closed_indices.push((idx, 0, pos.liquidation_price)); // 0: Liq, 1: SL
            } else if hit_sl {
                closed_indices.push((idx, 1, pos.stop_loss.unwrap()));
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
                paso_acciones.push(format!("LIQUIDACION_{:?}", pos.position_type));
                paso_precios.push(format!("{:.2}", pos.liquidation_price));
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
                println!("🛑 STOP LOSS EJECUTADO: Posición {:?} cerrada a {:.2} USDT (Entrada: {:.2} USDT). Retorno: {:.2} USDT | Fee: {:.2} USDT | PnL Realizado: {:.2} USDT",
                    pos.position_type, exit_price, pos.entry_price, return_value, closing_fee, real_pnl
                );
                paso_acciones.push(format!("STOP_LOSS_{:?}", pos.position_type));
                paso_precios.push(format!("{:.2}", exit_price));
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

        println!("\n=== [Paso {}/{}] {} | Precio Actual: {:.2} USDT | Buy & Hold ($10k): {:.2} USDT ===", i + 1, candles.len(), date_str, precio_actual, 10000.0 * (precio_actual / initial_price));
        println!("💼 Estado: Saldo: {:.2} USDT | Margen Total: {:.2} USDT | Posiciones Activas: {} | PnL Flotante: {:.2} USDT | Equity: {:.2} USDT",
            saldo_usdt, total_margins, active_positions.len(), total_floating_pnl, equity
        );

        // 3. Generar la ventana deslizante
        let mut history_str = String::new();
        let start_idx = i.saturating_sub(19);
        let actual_history_len = i - start_idx + 1;
        for (idx, prev_candle) in candles[start_idx..=i].iter().enumerate() {
            let global_idx = start_idx + idx;
            let label = if global_idx == i { " (Current)" } else { "" };
            
            // Obtener operaciones coincidentes con la fecha de la vela
            let mut acciones_vela = String::new();
            if global_idx == i {
                if !paso_acciones.is_empty() {
                    acciones_vela = format!(" [Trade: {} at {}]", paso_acciones.join("; "), paso_precios.join("; "));
                }
            } else if global_idx >= start_trade_idx {
                let curve_idx = global_idx - start_trade_idx;
                if curve_idx < equity_curve.len() {
                    let act = &equity_curve[curve_idx].3;
                    let prc = &equity_curve[curve_idx].4;
                    if !act.is_empty() {
                        acciones_vela = format!(" [Trade: {} at {}]", act, prc);
                    }
                }
            }

            history_str.push_str(&format!(
                "- t-{}: O:{:.1}, H:{:.1}, L:{:.1}, C:{:.1}, V:{:.0}{}{}\n",
                actual_history_len - 1 - idx, prev_candle.open, prev_candle.high, prev_candle.low, prev_candle.close, prev_candle.volume, label, acciones_vela
            ));
        }

        // List open positions for prompt
        let mut positions_str = String::new();
        if active_positions.is_empty() {
            positions_str.push_str("- No active positions.");
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
                    None => "None".to_string(),
                };
                positions_str.push_str(&format!(
                    "- Position #{}: {:?} | Box: {:?} | Entry: {:.2} USDT | Margin: {:.2} USDT | Size: {:.6} BTC | Liq: {:.2} USDT | SL: {} | Floating PnL: {:.2} USDT (ROE: {:.2}%)\n",
                    idx + 1, pos.position_type, pos.box_type, pos.entry_price, pos.margin, pos.size_btc, pos.liquidation_price, sl_str, pnl, roe
                ));
            }
        }

        // Calcular la progresion de los indicadores tecnicos de las ultimas 20 velas (para ver aceleracion/desaceleracion)
        let mut indicators_str = String::new();
        for idx in start_idx..=i {
            let label = if idx == i { " (Current)" } else { "" };
            let offset = i - idx;
            let ind_val = calculate_indicators(&candles, idx, candles[idx].close);
            
            // Obtener operaciones coincidentes con la fecha del indicador
            let mut acciones_vela = String::new();
            if idx == i {
                if !paso_acciones.is_empty() {
                    acciones_vela = format!(" | Trade: {} at {}", paso_acciones.join("; "), paso_precios.join("; "));
                }
            } else if idx >= start_trade_idx {
                let curve_idx = idx - start_trade_idx;
                if curve_idx < equity_curve.len() {
                    let act = &equity_curve[curve_idx].3;
                    let prc = &equity_curve[curve_idx].4;
                    if !act.is_empty() {
                        acciones_vela = format!(" | Trade: {} at {}", act, prc);
                    }
                }
            }

            indicators_str.push_str(&format!(
                "- t-{}{}: {}{}\n",
                offset, label, ind_val, acciones_vela
            ));
        }

        // 4. Prompt a Gemma
        let user_prompt = format!(
            "DATA (Recent)
Current BTC Price (Close): {:.2} USDT
Last 20 candles history:
{}
Liquidations in this simulation: {}

TECHNICAL INDICATORS
{}

ACCOUNT STATUS
Free balance (not in margin): {:.2} USDT
Total Equity: {:.2} USDT
Leverage: {:.1}x
Risk parameters: Max % risk per trade: {}%
Active Positions & Pending Orders (SL):
{}
Recent trades history (Realized PnLs of closed trades):
{:?}

What action do you take? Respond strictly in JSON format",
            precio_actual, history_str, num_liquidaciones, indicators_str, saldo_usdt, equity, leverage, risk_percent, positions_str, trade_pnls.iter().rev().take(5).collect::<Vec<_>>()
        );

        let mut retries = 3;
        let mut gemma_analisis = "Sin análisis".to_string();

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
                        if let Some(ref ans) = parsed.analisis {
                            gemma_analisis = ans.clone();
                        }

                        // Execute LT Box actions
                        execute_box_action(
                            "LT_BOX",
                            &parsed.lt_box,
                            BoxType::LT,
                            &mut saldo_usdt,
                            &mut active_positions,
                            equity,
                            risk_percent,
                            leverage,
                            fee_rate,
                            precio_actual,
                            dynamic_risk_leverage,
                            &mut trade_pnls,
                            &mut paso_acciones,
                            &mut paso_precios,
                            &mut num_compras,
                            &mut num_ventas,
                        );

                        // Execute ST Box actions
                        execute_box_action(
                            "ST_BOX",
                            &parsed.st_box,
                            BoxType::ST,
                            &mut saldo_usdt,
                            &mut active_positions,
                            equity,
                            risk_percent,
                            leverage,
                            fee_rate,
                            precio_actual,
                            dynamic_risk_leverage,
                            &mut trade_pnls,
                            &mut paso_acciones,
                            &mut paso_precios,
                            &mut num_compras,
                            &mut num_ventas,
                        );

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

        if gemma_analisis != "Sin análisis" && !gemma_analisis.trim().is_empty() {
            println!("🤖 Gemma dice: {}", gemma_analisis);
        }

        // 6. Recalcular equidad final y guardar curva de equidad con las acciones y precios del paso
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
        let equity_final = saldo_usdt + total_margins + total_floating_pnl;

        if equity_final > peak_equity {
            peak_equity = equity_final;
        }
        let dd = (peak_equity - equity_final) / peak_equity;
        if dd > max_drawdown {
            max_drawdown = dd;
        }

        let actions_str = paso_acciones.join("; ");
        let prices_str = paso_precios.join("; ");

        equity_curve.push((
            date_str.clone(),
            equity_final,
            initial_balance * (candle.close / initial_price),
            actions_str,
            prices_str,
        ));

        // Guardar progreso intermedio cada 1 paso para ver actualización en vivo del dashboard y CSV
        if (i + 1) % 1 == 0 {
            let bot_equity_series: Vec<f64> = equity_curve.iter().map(|(_, eq, _, _, _)| *eq).collect();
            let bh_equity_series: Vec<f64> = equity_curve.iter().map(|(_, _, bh, _, _)| *bh).collect();
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
            for (_, eq, _, _, _) in &equity_curve {
                if *eq > temp_peak_eq_usd {
                    temp_peak_eq_usd = *eq;
                }
                let dd_usd = temp_peak_eq_usd - *eq;
                if dd_usd > temp_max_dd_usd {
                    temp_max_dd_usd = dd_usd;
                }
            }
            let current_eq = equity_curve.last().map(|(_, eq, _, _, _)| *eq).unwrap_or(initial_balance);
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
            for (_, eq, _, _, _) in &equity_curve {
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
                "dashboard.html",
                false
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

    let bot_equity_series: Vec<f64> = equity_curve.iter().map(|(_, eq, _, _, _)| *eq).collect();
    let bh_equity_series: Vec<f64> = equity_curve.iter().map(|(_, _, bh, _, _)| *bh).collect();
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
    for (_, eq, _, _, _) in &equity_curve {
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
    for (_, eq, _, _, _) in &equity_curve {
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
        "dashboard.html",
        true,
    )?;
    println!("🖥️ Dashboard interactivo guardado en 'dashboard.html'");

    Ok(())
}
