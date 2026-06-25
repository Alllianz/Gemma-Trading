use crate::types::Candle;

pub fn calculate_indicators(
    candles: &[Candle],
    start_idx: usize,
    i: usize,
    precio_actual: f64,
) -> (String, String, String, String) {
    let slice = &candles[start_idx..=i];
    let slice_len = slice.len() as f64;

    // Indicador 1 — Tendencia de las últimas 10 velas
    let indicador_tendencia = if !slice.is_empty() {
        let first_close = slice[0].close;
        let last_close = slice[slice.len() - 1].close;
        if first_close > 0.0 {
            let var_pct = ((last_close - first_close) / first_close) * 100.0;
            let direction = if var_pct >= 0.0 { "ALCISTA" } else { "BAJISTA" };
            format!("{} ({:+.2}%)", direction, var_pct)
        } else {
            "INDETERMINADA (0.00%)".to_string()
        }
    } else {
        "INDETERMINADA (0.00%)".to_string()
    };

    // Indicador 2 — Volatilidad (ATR - Average True Range)
    let indicador_volatilidad = if !slice.is_empty() {
        let mut sum_tr = 0.0;
        for idx in start_idx..=i {
            let tr = if idx > 0 {
                let prev_close = candles[idx - 1].close;
                let val1 = candles[idx].high - candles[idx].low;
                let val2 = (candles[idx].high - prev_close).abs();
                let val3 = (candles[idx].low - prev_close).abs();
                val1.max(val2).max(val3)
            } else {
                candles[idx].high - candles[idx].low
            };
            sum_tr += tr;
        }
        let atr = sum_tr / slice_len;
        let atr_pct = if precio_actual > 0.0 { (atr / precio_actual) * 100.0 } else { 0.0 };
        format!("ATR: {:.2} USDT ({:.2}% del precio actual)", atr, atr_pct)
    } else {
        "ATR: 0.00 USDT (0.00% del precio actual)".to_string()
    };

    // Indicador 3 — Posición del precio en el rango
    let indicador_posicion = if !slice.is_empty() {
        let high_max = slice.iter().map(|c| c.high).fold(f64::NEG_INFINITY, f64::max);
        let low_min = slice.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
        if high_max > low_min {
            let pos_pct = ((precio_actual - low_min) / (high_max - low_min)) * 100.0;
            format!("{:.0}% del rango (0%=mínimo, 100%=máximo)", pos_pct)
        } else {
            "0% del rango (0%=mínimo, 100%=máximo)".to_string()
        }
    } else {
        "0% del rango (0%=mínimo, 100%=máximo)".to_string()
    };

    // Indicador 4 — Presión de cuerpo / volumen (Body-Volume Ratio)
    let indicador_presion = if !slice.is_empty() {
        let sum_ratio: f64 = slice.iter().map(|c| {
            if c.close > 0.0 && c.volume > 0.0 {
                (((c.close - c.open).abs() / c.close) * 100.0) / c.volume
            } else {
                0.0
            }
        }).sum();
        format!("{:.6} (alto=presión direccional, bajo=absorción/indecisión)", sum_ratio / slice_len)
    } else {
        "0.000000 (alto=presión direccional, bajo=absorción/indecisión)".to_string()
    };

    (indicador_tendencia, indicador_volatilidad, indicador_posicion, indicador_presion)
}
