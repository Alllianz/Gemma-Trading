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

struct AdvancedMetrics {
    winrate: f64,
    profit_factor: f64,
    max_drawdown: f64,
    recovery_factor: f64,
    sortino_ratio: f64,
    max_stagnation: usize,
    avg_stagnation: f64,
    correlation: f64,
}

fn calculate_advanced_metrics(
    equity_curve: &[(String, f64, f64, String, String)],
    trade_pnls: &[f64],
    initial_balance: f64,
    timeframe: &str,
) -> AdvancedMetrics {
    let total_closed_trades = trade_pnls.len();
    let wins: Vec<f64> = trade_pnls.iter().cloned().filter(|&p| p > 0.0).collect();
    let losses: Vec<f64> = trade_pnls.iter().cloned().filter(|&p| p <= 0.0).collect();
    
    let winrate = if total_closed_trades > 0 {
        (wins.len() as f64 / total_closed_trades as f64) * 100.0
    } else {
        0.0
    };
    
    let gross_profit: f64 = wins.iter().sum();
    let gross_loss: f64 = losses.iter().sum::<f64>().abs();
    let profit_factor = if gross_loss > 0.0 {
        gross_profit / gross_loss
    } else if gross_profit > 0.0 {
        99.9
    } else {
        0.0
    };

    // Max Drawdown % & USD
    let mut peak_eq = initial_balance;
    let mut max_dd_pct = 0.0;
    let mut max_dd_usd = 0.0;
    for (_, eq, _, _, _) in equity_curve {
        if *eq > peak_eq {
            peak_eq = *eq;
        }
        let dd_pct = (peak_eq - *eq) / peak_eq;
        if dd_pct > max_dd_pct {
            max_dd_pct = dd_pct;
        }
        let dd_usd = peak_eq - *eq;
        if dd_usd > max_dd_usd {
            max_dd_usd = dd_usd;
        }
    }

    let current_eq = equity_curve.last().map(|(_, eq, _, _, _)| *eq).unwrap_or(initial_balance);
    let recovery_factor = if max_dd_usd > 0.0 {
        (current_eq - initial_balance) / max_dd_usd
    } else {
        0.0
    };

    // Sortino Ratio
    let returns: Vec<f64> = equity_curve.windows(2).map(|w| (w[1].1 - w[0].1) / w[0].1).collect();
    let sortino_ratio = if returns.len() > 1 {
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let downside_sum_sq: f64 = returns.iter()
            .map(|&r| if r < 0.0 { r.powi(2) } else { 0.0 })
            .sum();
        let downside_deviation = (downside_sum_sq / (returns.len() - 1) as f64).sqrt();
        if downside_deviation > 0.0 {
            let step_sortino = mean / downside_deviation;
            let annualization_factor = match timeframe {
                "1h" => (24.0 * 365.0f64).sqrt(),
                "4h" => (6.0 * 365.0f64).sqrt(),
                "1d" => 365.0f64.sqrt(),
                _ => 1.0,
            };
            step_sortino * annualization_factor
        } else {
            0.0
        }
    } else {
        0.0
    };

    // Stagnation
    let mut max_eq_stag = initial_balance;
    let mut current_stagnation = 0;
    let mut stagnation_periods = Vec::new();
    for (_, eq, _, _, _) in equity_curve {
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
    let max_stagnation = stagnation_periods.iter().max().copied().unwrap_or(0);
    let avg_stagnation = if !stagnation_periods.is_empty() {
        stagnation_periods.iter().sum::<usize>() as f64 / stagnation_periods.len() as f64
    } else {
        0.0
    };

    // Correlation
    let bot_equity_series: Vec<f64> = equity_curve.iter().map(|(_, eq, _, _, _)| *eq).collect();
    let bh_equity_series: Vec<f64> = equity_curve.iter().map(|(_, _, bh, _, _)| *bh).collect();
    let correlation = calculate_correlation(&bot_equity_series, &bh_equity_series);

    AdvancedMetrics {
        winrate,
        profit_factor,
        max_drawdown: max_dd_pct,
        recovery_factor,
        sortino_ratio,
        max_stagnation,
        avg_stagnation,
        correlation,
    }
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
    step_positions_log: &mut Vec<String>,
    directional_score: &mut i32,
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

                let hit_direction = match pos.position_type {
                    PositionType::Long => precio_actual > pos.entry_price,
                    PositionType::Short => precio_actual < pos.entry_price,
                    _ => false,
                };
                if hit_direction {
                    *directional_score += 10;
                } else {
                    *directional_score -= 10;
                }
                
                let roe = (real_pnl / pos.margin) * 100.0;
                step_positions_log.push(format!(
                    "{:?}: {:?}\nEntry Price: {:.2} USDT\nClose Price: {:.2} USDT (CLOSED)\nMargin: {:.2} USDT\nVolume: {:.6} BTC (${:.2})\nPNL: {:.2} USDT (ROE: {:.2}%)\nStop Loss: {}\nFee: {:.2} USDT",
                    box_type, pos.position_type, pos.entry_price, precio_actual, pos.margin, pos.size_btc, pos.size_btc * precio_actual, real_pnl, roe, pos.stop_loss.map(|s| format!("{:.2} USDT", s)).unwrap_or_else(|| "None".to_string()), opening_fee + closing_fee
                ));

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
                let final_sl = sl_val;
                pos.stop_loss = Some(final_sl);
                
                let pnl = match pos.position_type {
                    PositionType::Long => (precio_actual - pos.entry_price) * pos.size_btc,
                    PositionType::Short => (pos.entry_price - precio_actual) * pos.size_btc,
                    _ => 0.0,
                };
                let roe = (pnl / pos.margin) * 100.0;
                let volume_usdt = pos.size_btc * precio_actual;

                step_positions_log.push(format!(
                    "{:?}: {:?}\nEntry Price: {:.2} USDT\nClose Price: 0.00 USDT\nMargin: {:.2} USDT\nVolume: {:.6} BTC (${:.2})\nPNL: {:.2} USDT (ROE: {:.2}%)\nStop Loss: {:.2} USDT\nFee: 0.00 USDT",
                    box_type, pos.position_type, pos.entry_price, pos.margin, pos.size_btc, volume_usdt, pnl, roe, final_sl
                ));
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
        
        let can_open = active_box_positions.len() < 2;

        if !can_open {
            println!("⏳ [{}] Bloqueado abrir {:?}: Ya existen 2 posiciones activas de este tipo.", box_name, desired_type);
        } else {
            let box_leverage = if dynamic_risk_leverage {
                box_action.apalancamiento.unwrap_or(leverage)
            } else {
                leverage
            };
            
            // Asignación de capital según la caja: ST: 100% del equity de la cuenta.
            let box_allocation = match box_type {
                BoxType::ST => 1.0,
            };
            
            // Si ya hay una posición en esta caja, la posición adicional debe ser del mismo tamaño exacto (mismo margen)
            let mut margin = if let Some(first_pos) = active_positions.iter().find(|p| p.box_type == box_type) {
                first_pos.margin
            } else {
                equity * (risk_percent / 100.0)
            };
            
            // Verificar que no excedamos el límite de capital asignado a la caja (80% para LT, 20% para ST)
            let current_box_margin: f64 = active_positions.iter()
                .filter(|p| p.box_type == box_type)
                .map(|p| p.margin)
                .sum();
            
            let box_limit = equity * box_allocation;
            if current_box_margin + margin > box_limit {
                margin = box_limit - current_box_margin;
            }

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
                
                // Aplicar el Stop Loss sugerido por la IA
                let final_sl = box_action.stop_loss;

                active_positions.push(Position {
                    position_type: desired_type,
                    margin,
                    size_btc: pos_size_btc,
                    entry_price: precio_actual,
                    liquidation_price: pos_liq_price,
                    stop_loss: final_sl,
                    box_type,
                });
                
                if desired_type == PositionType::Long {
                    *num_compras += 1;
                } else {
                    *num_ventas += 1;
                }

                step_positions_log.push(format!(
                    "{:?}: {:?}\nEntry Price: {:.2} USDT\nClose Price: 0.00 USDT\nMargin: {:.2} USDT\nVolume: {:.6} BTC (${:.2})\nPNL: 0.00 USDT (ROE: 0.00%)\nStop Loss: {}\nFee: {:.2} USDT",
                    box_type, desired_type, precio_actual, margin, pos_size_btc, size_usdt, final_sl.map(|s| format!("{:.2} USDT", s)).unwrap_or_else(|| "None".to_string()), opening_fee
                ));

                paso_acciones.push(format!("ABRIR_{:?}_{:?}", box_type, desired_type));
                paso_precios.push(format!("{:.2}", precio_actual));
            } else {
                println!("⏳ [{}] Margen/saldo insuficiente ({:.2} USDT de saldo) para abrir {:?}.", box_name, *saldo_usdt, desired_type);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct BacktestSummary {
    pub final_equity: f64,
    pub winrate: f64,
    pub profit_factor: f64,
    pub total_trades: usize,
    pub max_drawdown: f64,
    pub correlation: f64,
    pub actions_sequence: String,
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
) -> Result<BacktestSummary, Box<dyn std::error::Error>> {
    let candles = get_candles(db_path, timeframe, limit)?;
    if candles.is_empty() {
        return Err("No hay velas en la base de datos. Descarga velas primero (Opción 1).".into());
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
        return Err(format!("La fecha de inicio de trading especificada ({:?}) está después de la última vela en la DB.", trading_start_date).into());
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

    let mut num_liquidaciones = 0;
    let mut peak_equity = 10000.0;
    let mut max_drawdown = 0.0;

    // Contadores específicos por caja
    let mut num_compras_st = 0;
    let mut num_ventas_st = 0;
    let mut num_liquidaciones_st = 0;

    let mut directional_score = 0;

    let mut equity_curve: Vec<(String, f64, f64, String, String)> = Vec::new();

    let initial_price = candles[start_trade_idx].close;
    let initial_balance = 10000.0;

    let mut trade_pnls: Vec<f64> = Vec::new();

    // Insertar punto de inicio (momento 0 antes del trading) para reflejar el capital inicial real
    let start_date_format = if timeframe == "1d" { "%Y-%m-%d" } else { "%Y-%m-%d %H:%M:%S" };
    let pre_trading_date = chrono::Utc.timestamp_millis_opt(candles[start_trade_idx].open_time)
        .unwrap()
        .format(start_date_format)
        .to_string();
    equity_curve.push((
        format!("{} (Inicio)", pre_trading_date),
        initial_balance,
        initial_balance,
        "".to_string(),
        "".to_string(),
    ));

    // El umbral de liquidación según la fórmula

    let system_prompt = format!(
        "CRITICAL: DO NOT use any <think> tags. You are strictly FORBIDDEN from reasoning, explaining, or writing thoughts. You must immediately output raw JSON. Your response MUST start with the character '{{' and end with '}}'.

INSTRUCTIONS:

Strategy & Capital Allocation (Base Leverage: {}X):
- Short-Term (ST) Box (Long-Term operational mode): 100 percent of the total account equity. This proportion represents the max margin limit of the box, not the volume/size.
- Leverage: Select between 5.0 and 10.0 for any position (include \"apalancamiento\": X in the box JSON).
- Add position: You are authorized to open up to a MAXIMUM of 2 concurrent positions of the same type at your discretion. Additional positions will always have the exact same size/margin as the first position.

Trend Priority & Guidelines: 
- Short-Term (ST) Box (Long-Term operational mode): Actively trade long-term trends guided by EMA100 and EMA200.

Position Actions & Stop Loss Rules:
- To open a new trade: set \"accion\" to \"LONG\" or \"SHORT\" and \"cerrar\" to false.
- To maintain an active trade without changes: set \"accion\" to \"HOLD\" and \"cerrar\" to false.
- To close an active trade completely: set \"accion\" to \"FLAT\" and \"cerrar\" to true.
- If the box has no active position and you do not want to open one: set \"accion\" to \"HOLD\", \"cerrar\" to false, and \"stop_loss\" to null.
- Stop Loss (SL) Rules:
  * ST Box (Long-Term): Set a stop loss below/above EMA200, or use EMA100 as a trailing stop.
- Trailing Stop: ONLY when you have guaranteed profit (position is strictly in profit compared to the entry price), set the \"stop_loss\" as a Trailing Stop and update it dynamically to the current EMA100/EMA200 (for ST/Long-Term) to lock in profits. Do not start trailing or moving the Stop Loss if the position is not in profit.

Scoring System (Directional Accuracy):
- Tu objetivo principal es maximizar tu \"Directional Accuracy Score\". Cada operación que se cierre en la dirección correcta te sumará +10 puntos. Cada operación que se cierre en la dirección incorrecta te restará -10 puntos. Utiliza este feedback para corregir tus predicciones direccionales.

CRITICAL EXECUTION RULES:
1. DO NOT use any <think> tags. Do not think, do not reason, do not explain, and do not write any prose. 
2. Go directly from the market data to the raw JSON output.
3. Output: Respond ONLY with a raw JSON matching the structure below. No markdown (```json), no extra fields.

Example:
{{
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
        let mut step_positions_log = Vec::new();

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
            let target_trade_pnls = &mut trade_pnls;
            let (target_compras, target_ventas, target_liquidaciones) = (&mut num_compras_st, &mut num_ventas_st, &mut num_liquidaciones_st);

            if close_type == 0 {
                let closing_fee = pos.size_btc * pos.liquidation_price * fee_rate;
                *target_liquidaciones += 1;
                num_liquidaciones += 1;
                let net_pnl = -pos.margin - opening_fee;
                target_trade_pnls.push(net_pnl);
                trade_pnls.push(net_pnl);
                
                directional_score -= 10; // Liquidación siempre es un fallo direccional
                
                step_positions_log.push(format!(
                    "{:?}: {:?}\nEntry Price: {:.2} USDT\nClose Price: {:.2} USDT (LIQUIDATED)\nMargin: {:.2} USDT\nVolume: {:.6} BTC (${:.2})\nPNL: {:.2} USDT (ROE: -100.00%)\nStop Loss: None\nFee: {:.2} USDT",
                    pos.box_type, pos.position_type, pos.entry_price, pos.liquidation_price, pos.margin, pos.size_btc, pos.size_btc * pos.liquidation_price, -pos.margin, opening_fee + closing_fee
                ));

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
                target_trade_pnls.push(net_pnl);
                trade_pnls.push(net_pnl);
                
                let hit_direction = match pos.position_type {
                    PositionType::Long => exit_price > pos.entry_price,
                    PositionType::Short => exit_price < pos.entry_price,
                    _ => false,
                };
                if hit_direction {
                    directional_score += 10;
                } else {
                    directional_score -= 10;
                }
                
                if pos.position_type == PositionType::Long {
                    *target_ventas += 1;
                } else {
                    *target_compras += 1;
                }
                
                let roe = (real_pnl / pos.margin) * 100.0;
                step_positions_log.push(format!(
                    "{:?}: {:?}\nEntry Price: {:.2} USDT\nClose Price: {:.2} USDT SL\nMargin: {:.2} USDT\nVolume: {:.6} BTC (${:.2})\nPNL: {:.2} USDT (ROE: {:.2}%)\nStop Loss: {}\nFee: {:.2} USDT",
                    pos.box_type, pos.position_type, pos.entry_price, exit_price, pos.margin, pos.size_btc, pos.size_btc * exit_price, real_pnl, roe, pos.stop_loss.map(|s| format!("{:.2} USDT", s)).unwrap_or_else(|| "None".to_string()), opening_fee + closing_fee
                ));

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

        // Reservamos la impresión de Status y Position para después de las acciones de Gemma en este paso.

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
Directional Accuracy Score: {}
Active Positions & Pending Orders (SL):
{}
Recent trades history (Realized PnLs of closed trades):
{:?}

What action do you take? Respond strictly in JSON format",
            precio_actual, history_str, num_liquidaciones, indicators_str, saldo_usdt, equity, leverage, risk_percent, directional_score, positions_str, trade_pnls.iter().rev().take(5).collect::<Vec<_>>()
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
                            &mut num_compras_st,
                            &mut num_ventas_st,
                            &mut step_positions_log,
                            &mut directional_score,
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

        // Consolidador e Impresión de logs estructurados solicitados por el usuario
        for pos in &active_positions {
            let box_prefix = format!("{:?}:", pos.box_type);
            if !step_positions_log.iter().any(|log| log.starts_with(&box_prefix)) {
                let pnl = match pos.position_type {
                    PositionType::Long => (precio_actual - pos.entry_price) * pos.size_btc,
                    PositionType::Short => (pos.entry_price - precio_actual) * pos.size_btc,
                    _ => 0.0,
                };
                let roe = (pnl / pos.margin) * 100.0;
                let volume_usdt = pos.size_btc * precio_actual;
                step_positions_log.push(format!(
                    "{:?}: {:?}\nEntry Price: {:.2} USDT\nClose Price: 0.00 USDT\nMargin: {:.2} USDT\nVolume: {:.6} BTC (${:.2})\nPNL: {:.2} USDT (ROE: {:.2}%)\nStop Loss: {}\nFee: 0.00 USDT",
                    pos.box_type, pos.position_type, pos.entry_price, pos.margin, pos.size_btc, volume_usdt, pnl, roe, pos.stop_loss.map(|s| format!("{:.2} USDT", s)).unwrap_or_else(|| "None".to_string())
                ));
            }
        }

        let step_num = i - start_trade_idx;
        let step_total = candles.len() - start_trade_idx;

        println!("\nPaso {}/{} - {}", step_num, step_total, date_str.replace(" ", " - "));
        println!("Precio Actual: {:.2} USDT", precio_actual);
        println!("Buy & Hold ($10k): {:.2} USDT", 10000.0 * (precio_actual / initial_price));
        println!("\nStatus:");
        println!("Equity: {:.2} USDT", equity);
        println!("Saldo: {:.2} USDT", saldo_usdt);
        println!("Margen Total: {:.2} USDT", total_margins);
        println!("Posiciones Activas: {}", active_positions.len());
        println!("PnL Flotante: {:.2} USDT", total_floating_pnl);
        println!("\nPosition:");
        if step_positions_log.is_empty() {
            println!("No active positions.");
        } else {
            for (idx, log) in step_positions_log.iter().enumerate() {
                if idx > 0 {
                    println!("");
                }
                println!("{}", log);
            }
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
            actions_str.clone(),
            prices_str.clone(),
        ));

        // Guardar progreso intermedio cada 1 paso para ver actualización en vivo del dashboard y CSV
        if (i + 1) % 1 == 0 {
            let _ = save_equity_curve(&equity_curve, "equity_curve.csv");
            
            let metrics_global = calculate_advanced_metrics(&equity_curve, &trade_pnls, initial_balance, timeframe);

            let _ = generate_dashboard(
                &equity_curve,
                num_compras_st,
                num_ventas_st,
                num_liquidaciones_st,
                metrics_global.max_drawdown,
                metrics_global.correlation,
                metrics_global.winrate,
                metrics_global.profit_factor,
                metrics_global.sortino_ratio,
                metrics_global.recovery_factor,
                metrics_global.avg_stagnation,
                metrics_global.max_stagnation,
                "dashboard.html",
                false
            );
        }
    } // Fin del loop for de velas

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

    let metrics_global = calculate_advanced_metrics(&equity_curve, &trade_pnls, initial_balance, timeframe);

    println!("📈 Correlación con Buy & Hold (Global): {:.4}", metrics_global.correlation);
    println!("📈 Winrate (Global): {:.2}%", metrics_global.winrate);
    println!("📈 Profit Factor (Global): {:.2}", metrics_global.profit_factor);
    println!("📈 Sortino Ratio (Global): {:.2}", metrics_global.sortino_ratio);
    println!("📈 Recovery Factor (Global): {:.2}", metrics_global.recovery_factor);
    println!("📈 Stagnation (Global): Max {} velas, Promedio {:.2} velas", metrics_global.max_stagnation, metrics_global.avg_stagnation);

    save_equity_curve(&equity_curve, "equity_curve.csv")?;
    
    generate_dashboard(
        &equity_curve,
        num_compras_st,
        num_ventas_st,
        num_liquidaciones_st,
        metrics_global.max_drawdown,
        metrics_global.correlation,
        metrics_global.winrate,
        metrics_global.profit_factor,
        metrics_global.sortino_ratio,
        metrics_global.recovery_factor,
        metrics_global.avg_stagnation,
        metrics_global.max_stagnation,
        "dashboard.html",
        true,
    )?;

    let mut actions_sequence = String::new();
    for (_, _, _, actions, _) in &equity_curve {
        if !actions.is_empty() {
            actions_sequence.push_str(actions);
            actions_sequence.push('|');
        }
    }

    Ok(BacktestSummary {
        final_equity,
        winrate: metrics_global.winrate,
        profit_factor: metrics_global.profit_factor,
        total_trades: trade_pnls.len(),
        max_drawdown: metrics_global.max_drawdown,
        correlation: metrics_global.correlation,
        actions_sequence,
    })
}
