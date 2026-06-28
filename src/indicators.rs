use crate::types::Candle;

pub fn calculate_ema(candles: &[Candle], i: usize, period: usize) -> f64 {
    if candles.is_empty() || i >= candles.len() {
        return 0.0;
    }
    let alpha = 2.0 / (period as f64 + 1.0);
    let mut ema = candles[0].close;
    for idx in 1..=i {
        ema = (candles[idx].close * alpha) + (ema * (1.0 - alpha));
    }
    ema
}

fn calculate_sma(candles: &[Candle], i: usize, period: usize) -> f64 {
    let start = i.saturating_sub(period - 1);
    let slice = &candles[start..=i];
    if slice.is_empty() {
        return 0.0;
    }
    slice.iter().map(|c| c.close).sum::<f64>() / slice.len() as f64
}

fn calculate_macd(candles: &[Candle], i: usize) -> (f64, f64, f64) {
    let mut macd_series = Vec::new();
    let mut ema_12 = candles[0].close;
    let mut ema_26 = candles[0].close;
    let alpha_12 = 2.0 / 13.0;
    let alpha_26 = 2.0 / 27.0;

    for idx in 0..=i {
        if idx > 0 {
            ema_12 = (candles[idx].close * alpha_12) + (ema_12 * (1.0 - alpha_12));
            ema_26 = (candles[idx].close * alpha_26) + (ema_26 * (1.0 - alpha_26));
        }
        macd_series.push(ema_12 - ema_26);
    }

    let alpha_9 = 2.0 / 10.0;
    let mut signal = macd_series[0];
    for idx in 1..=i {
        signal = (macd_series[idx] * alpha_9) + (signal * (1.0 - alpha_9));
    }

    let macd_line = macd_series[i];
    let histogram = macd_line - signal;
    (macd_line, signal, histogram)
}

fn calculate_rsi(candles: &[Candle], i: usize, period: usize) -> f64 {
    if i < period {
        return 50.0;
    }
    let mut gains = 0.0;
    let mut losses = 0.0;
    
    for idx in 1..=period {
        let diff = candles[idx].close - candles[idx - 1].close;
        if diff > 0.0 {
            gains += diff;
        } else {
            losses += diff.abs();
        }
    }
    
    let mut avg_gain = gains / period as f64;
    let mut avg_loss = losses / period as f64;
    
    for idx in (period + 1)..=i {
        let diff = candles[idx].close - candles[idx - 1].close;
        let gain = if diff > 0.0 { diff } else { 0.0 };
        let loss = if diff < 0.0 { diff.abs() } else { 0.0 };
        avg_gain = (avg_gain * (period - 1) as f64 + gain) / period as f64;
        avg_loss = (avg_loss * (period - 1) as f64 + loss) / period as f64;
    }
    
    if avg_loss == 0.0 {
        return 100.0;
    }
    let rs = avg_gain / avg_loss;
    100.0 - (100.0 / (1.0 + rs))
}

fn calculate_stoch_rsi(candles: &[Candle], i: usize, period: usize) -> (f64, f64) {
    if i < period * 2 {
        return (50.0, 50.0);
    }
    let mut rsi_series = Vec::new();
    for idx in (i - period + 1)..=i {
        rsi_series.push(calculate_rsi(candles, idx, period));
    }
    let min_rsi = rsi_series.iter().fold(f64::INFINITY, |a, &b| a.min(b));
    let max_rsi = rsi_series.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
    
    let stoch_rsi = if max_rsi > min_rsi {
        (rsi_series.last().unwrap() - min_rsi) / (max_rsi - min_rsi) * 100.0
    } else {
        50.0
    };
    (stoch_rsi, stoch_rsi)
}

fn calculate_bollinger_bands(candles: &[Candle], i: usize, period: usize, k: f64) -> (f64, f64, f64) {
    let basis = calculate_sma(candles, i, period);
    let start = i.saturating_sub(period - 1);
    let slice = &candles[start..=i];
    if slice.is_empty() {
        return (basis, basis, basis);
    }
    let sum_sq_diff: f64 = slice.iter().map(|c| {
        let diff = c.close - basis;
        diff * diff
    }).sum();
    let std_dev = (sum_sq_diff / slice.len() as f64).sqrt();
    let upper = basis + k * std_dev;
    let lower = basis - k * std_dev;
    (upper, basis, lower)
}

fn calculate_atr(candles: &[Candle], i: usize, period: usize) -> f64 {
    if candles.is_empty() {
        return 0.0;
    }
    let mut tr_series = Vec::new();
    for idx in 0..=i {
        let tr = if idx > 0 {
            let prev_close = candles[idx - 1].close;
            let val1 = candles[idx].high - candles[idx].low;
            let val2 = (candles[idx].high - prev_close).abs();
            let val3 = (candles[idx].low - prev_close).abs();
            val1.max(val2).max(val3)
        } else {
            candles[idx].high - candles[idx].low
        };
        tr_series.push(tr);
    }
    
    if i < period {
        return tr_series.iter().sum::<f64>() / (i + 1) as f64;
    }
    
    let mut atr = tr_series[0..period].iter().sum::<f64>() / period as f64;
    for idx in period..=i {
        atr = (atr * (period - 1) as f64 + tr_series[idx]) / period as f64;
    }
    atr
}

fn calculate_vwap(candles: &[Candle], i: usize) -> f64 {
    let start = i.saturating_sub(100);
    let slice = &candles[start..=i];
    let mut sum_pv = 0.0;
    let mut sum_v = 0.0;
    for c in slice {
        let typical_price = (c.high + c.low + c.close) / 3.0;
        sum_pv += typical_price * c.volume;
        sum_v += c.volume;
    }
    if sum_v == 0.0 {
        return candles[i].close;
    }
    sum_pv / sum_v
}

fn calculate_obv(candles: &[Candle], i: usize) -> f64 {
    let mut obv = 0.0;
    for idx in 1..=i {
        if candles[idx].close > candles[idx - 1].close {
            obv += candles[idx].volume;
        } else if candles[idx].close < candles[idx - 1].close {
            obv -= candles[idx].volume;
        }
    }
    obv
}

pub fn calculate_indicators(
    candles: &[Candle],
    i: usize,
    precio_actual: f64,
) -> String {
    // 1. Tendencia
    let ema_20 = calculate_ema(candles, i, 20);
    let ema_40 = calculate_ema(candles, i, 40);
    let ema_100 = calculate_ema(candles, i, 100);
    let ema_200 = calculate_ema(candles, i, 200);
    let sma_20 = calculate_sma(candles, i, 20);
    let (macd_line, macd_signal, macd_hist) = calculate_macd(candles, i);

    // 2. Momentum
    let rsi = calculate_rsi(candles, i, 14);
    let (stoch_k, _) = calculate_stoch_rsi(candles, i, 14);

    // 3. Volatilidad
    let (bb_upper, bb_basis, bb_lower) = calculate_bollinger_bands(candles, i, 20, 2.0);
    let atr = calculate_atr(candles, i, 14);
    let atr_pct = if precio_actual > 0.0 { (atr / precio_actual) * 100.0 } else { 0.0 };

    // 4. Volumen
    let vwap = calculate_vwap(candles, i);
    let obv = calculate_obv(candles, i);

    format!(
        "EMA20:{:.1} EMA40:{:.1} EMA100:{:.1} EMA200:{:.1} SMA20:{:.1} MACD:[{:.2},{:.2},{:.2}] RSI:{:.1} Stoch:{:.1} BB:[{:.1},{:.1},{:.1}] ATR:{:.1}({:.1}%) VWAP:{:.1} OBV:{:.0}",
        ema_20, ema_40, ema_100, ema_200, sma_20, macd_line, macd_signal, macd_hist, rsi, stoch_k, bb_upper, bb_basis, bb_lower, atr, atr_pct, vwap, obv
    )
}
