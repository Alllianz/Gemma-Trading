use std::io::{self, Write};
use chrono::TimeZone;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum PositionType {
    None,
    Long,
    Short,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
struct Position {
    position_type: PositionType,
    margin: f64,
    size_btc: f64,
    entry_price: f64,
    liquidation_price: f64,
}

fn calculate_correlation(series_a: &[f64], series_b: &[f64]) -> f64 {
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

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Candle {
    open_time: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    close_time: i64,
}

#[derive(Debug, Deserialize, Serialize)]
struct GemmaResponse {
    analisis: String,
    accion: String, // "COMPRAR", "VENDER", "MANTENER"
}

fn parse_gemma_response(text: &str) -> Option<GemmaResponse> {
    let cleaned = text.trim();
    let cleaned = if cleaned.starts_with("```json") {
        cleaned.strip_prefix("```json").unwrap_or(cleaned)
    } else if cleaned.starts_with("```") {
        cleaned.strip_prefix("```").unwrap_or(cleaned)
    } else {
        cleaned
    };
    let cleaned = if cleaned.ends_with("```") {
        cleaned.strip_suffix("```").unwrap_or(cleaned)
    } else {
        cleaned
    };
    let cleaned = cleaned.trim();
    serde_json::from_str(cleaned).ok()
}

async fn download_candles(db_path: &str, timeframe: &str) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open(db_path)?;
    
    // Check if candles table exists and has timeframe column
    let has_timeframe: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('candles') WHERE name='timeframe')",
        [],
        |row| row.get(0),
    ).unwrap_or(false);

    let table_exists: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type='table' AND name='candles')",
        [],
        |row| row.get(0),
    ).unwrap_or(false);

    if table_exists && !has_timeframe {
        println!("🔄 Migrando tabla de velas existente al nuevo formato con soporte de temporalidad...");
        let _ = conn.execute("ALTER TABLE candles RENAME TO candles_old", []);
        conn.execute(
            "CREATE TABLE candles (
                timeframe TEXT NOT NULL,
                open_time INTEGER NOT NULL,
                open REAL NOT NULL,
                high REAL NOT NULL,
                low REAL NOT NULL,
                close REAL NOT NULL,
                volume REAL NOT NULL,
                close_time INTEGER NOT NULL,
                PRIMARY KEY (timeframe, open_time)
            )",
            [],
        )?;
        let _ = conn.execute(
            "INSERT INTO candles (timeframe, open_time, open, high, low, close, volume, close_time)
             SELECT '1h', open_time, open, high, low, close, volume, close_time FROM candles_old",
            [],
        );
        let _ = conn.execute("DROP TABLE candles_old", []);
        println!("✅ Migración completada exitosamente.");
    } else {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS candles (
                timeframe TEXT NOT NULL,
                open_time INTEGER NOT NULL,
                open REAL NOT NULL,
                high REAL NOT NULL,
                low REAL NOT NULL,
                close REAL NOT NULL,
                volume REAL NOT NULL,
                close_time INTEGER NOT NULL,
                PRIMARY KEY (timeframe, open_time)
            )",
            [],
        )?;
    }

    // Get the last open_time from the database for this timeframe
    let mut stmt = conn.prepare("SELECT MAX(open_time) FROM candles WHERE timeframe = ?1")?;
    let last_time: Option<i64> = stmt.query_row([timeframe], |row| {
        let val: Option<f64> = row.get(0)?;
        Ok(val.map(|v| v as i64))
    }).unwrap_or(None);

    let start_time_millis = match last_time {
        Some(t) => {
            println!("🔄 Datos existentes encontrados para {}. Reanudando desde la última vela registrada...", timeframe);
            t + 1
        }
        None => {
            // 2020-04-20 00:00:00 UTC
            println!("🆕 No se encontraron datos para {}. Iniciando descarga desde el 20-04-2020...", timeframe);
            chrono::NaiveDate::from_ymd_opt(2020, 4, 20)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        }
    };

    let client = reqwest::Client::new();
    let mut current_start_time = start_time_millis;

    loop {
        let url = format!(
            "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval={}&startTime={}&limit=1000",
            timeframe, current_start_time
        );

        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            println!("❌ Error al consultar la API de Binance: {}", resp.status());
            break;
        }

        let klines: Vec<Vec<serde_json::Value>> = resp.json().await?;
        if klines.is_empty() {
            println!("✅ Todas las velas para {} descargadas.", timeframe);
            break;
        }

        let mut candles_batch = Vec::new();
        for kline in klines {
            if kline.len() >= 7 {
                let open_time = kline[0].as_i64().unwrap_or(0);
                let open: f64 = kline[1].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                let high: f64 = kline[2].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                let low: f64 = kline[3].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                let close: f64 = kline[4].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                let volume: f64 = kline[5].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                let close_time = kline[6].as_i64().unwrap_or(0);

                candles_batch.push(Candle {
                    open_time,
                    open,
                    high,
                    low,
                    close,
                    volume,
                    close_time,
                });
            }
        }

        if candles_batch.is_empty() {
            break;
        }

        let mut tx_conn = Connection::open(db_path)?;
        let tx = tx_conn.transaction()?;
        {
            let mut insert_stmt = tx.prepare(
                "INSERT OR REPLACE INTO candles (timeframe, open_time, open, high, low, close, volume, close_time)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
            )?;
            for candle in &candles_batch {
                insert_stmt.execute(rusqlite::params![
                    timeframe,
                    candle.open_time as f64,
                    candle.open,
                    candle.high,
                    candle.low,
                    candle.close,
                    candle.volume,
                    candle.close_time as f64,
                ])?;
            }
        }
        tx.commit()?;

        let last_candle_time = candles_batch.last().unwrap().open_time;
        let last_date = chrono::Utc.timestamp_millis_opt(last_candle_time)
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        println!("💾 Guardadas {} velas. Última fecha cargada: {} UTC", candles_batch.len(), last_date);

        current_start_time = last_candle_time + 1;
        tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
    }

    Ok(())
}

fn get_candles(db_path: &str, timeframe: &str, limit: Option<usize>) -> Result<Vec<Candle>, rusqlite::Error> {
    let conn = Connection::open(db_path)?;
    let query = match limit {
        Some(lim) => format!(
            "SELECT open_time, open, high, low, close, volume, close_time FROM candles WHERE timeframe = ?1 ORDER BY open_time ASC LIMIT {}",
            lim
        ),
        None => "SELECT open_time, open, high, low, close, volume, close_time FROM candles WHERE timeframe = ?1 ORDER BY open_time ASC".to_string(),
    };
    let mut stmt = conn.prepare(&query)?;
    let candle_iter = stmt.query_map([timeframe], |row| {
        Ok(Candle {
            open_time: row.get::<_, f64>(0)? as i64,
            open: row.get(1)?,
            high: row.get(2)?,
            low: row.get(3)?,
            close: row.get(4)?,
            volume: row.get(5)?,
            close_time: row.get::<_, f64>(6)? as i64,
        })
    })?;

    let mut candles = Vec::new();
    for candle in candle_iter {
        candles.push(candle?);
    }
    Ok(candles)
}


async fn call_gemma(
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
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_prompt }
        ],
        "temperature": 0.2
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
            return Err(format!(
                "No se encontró el contenido del mensaje. Respuesta completa de LM Studio: {}",
                json_resp
            ).into());
        }
    };

    Ok(content)
}

fn save_equity_curve(curve: &[(String, f64, f64)], filename: &str) -> Result<(), std::io::Error> {
    use std::fs::File;
    use std::io::Write;
    let mut file = File::create(filename)?;
    writeln!(file, "time,equity,buy_and_hold")?;
    for (time_str, eq, bh) in curve {
        writeln!(file, "{},{},{}", time_str, eq, bh)?;
    }
    Ok(())
}

fn generate_dashboard(
    curve: &[(String, f64, f64)],
    num_compras: usize,
    num_ventas: usize,
    num_liquidaciones: usize,
    max_drawdown: f64,
    correlation: f64,
    filename: &str,
) -> Result<(), std::io::Error> {
    use std::fs::File;
    use std::io::Write;

    let initial_balance = curve.first().map(|(_, eq, _)| *eq).unwrap_or(0.0);
    let final_balance = curve.last().map(|(_, eq, _)| *eq).unwrap_or(0.0);
    let roi = if initial_balance > 0.0 {
        ((final_balance - initial_balance) / initial_balance) * 100.0
    } else {
        0.0
    };

    let labels: Vec<String> = curve.iter().map(|(time, _, _)| time.clone()).collect();
    let data: Vec<f64> = curve.iter().map(|(_, eq, _)| *eq).collect();
    let bh_data: Vec<f64> = curve.iter().map(|(_, _, bh)| *bh).collect();

    let labels_json = serde_json::to_string(&labels).unwrap_or_else(|_| "[]".to_string());
    let data_json = serde_json::to_string(&data).unwrap_or_else(|_| "[]".to_string());
    let bh_data_json = serde_json::to_string(&bh_data).unwrap_or_else(|_| "[]".to_string());

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="es">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Gemma Trading Bot - Allianz - Dashboard</title>
    <script src="https://cdn.tailwindcss.com"></script>
    <script src="https://cdn.jsdelivr.net/npm/chart.js"></script>
    <link href="https://fonts.googleapis.com/css2?family=Outfit:wght@300;400;500;600;700&display=swap" rel="stylesheet">
    <style>
        body {{
            background: radial-gradient(circle at top, #111827 0%, #030712 100%);
            font-family: 'Outfit', sans-serif;
        }}
    </style>
</head>
<body class="min-h-screen text-slate-100 p-6 md:p-12">
    <div class="max-w-6xl mx-auto space-y-8">
        
        <!-- Header -->
        <div class="flex flex-col md:flex-row justify-between items-start md:items-center border-b border-slate-800 pb-6 gap-4">
            <div>
                <h1 class="text-3xl font-bold tracking-tight bg-gradient-to-r from-violet-400 via-fuchsia-500 to-pink-500 bg-clip-text text-transparent">
                    Gemma Trading Bot - Allianz
                </h1>
                <p class="text-slate-400 text-sm mt-1">Reporte de Backtesting - BTCUSDT Futuros Apalancado</p>
            </div>
            <div class="bg-slate-900/80 backdrop-blur-md border border-slate-800 rounded-xl px-4 py-2 text-xs text-slate-400 flex items-center gap-2 shadow-lg">
                <span class="w-2.5 h-2.5 rounded-full bg-emerald-500 animate-pulse"></span>
                Simulación Completada
            </div>
        </div>

        <!-- Metrics Grid -->
        <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-6">
            <div class="bg-slate-900/60 backdrop-blur-md border border-slate-800/80 rounded-2xl p-6 shadow-xl flex flex-col justify-between hover:border-slate-700/80 transition duration-300">
                <span class="text-slate-400 text-sm font-medium">Balance Inicial</span>
                <span class="text-2xl font-bold text-slate-200 mt-2">${initial_balance:.2}</span>
                <span class="text-xs text-slate-500 mt-1">USDT</span>
            </div>

            <div class="bg-slate-900/60 backdrop-blur-md border border-slate-800/80 rounded-2xl p-6 shadow-xl flex flex-col justify-between hover:border-slate-700/80 transition duration-300">
                <span class="text-slate-400 text-sm font-medium">Balance Final</span>
                <span class="text-2xl font-bold text-slate-100 mt-2">${final_balance:.2}</span>
                <span class="text-xs text-slate-500 mt-1">USDT</span>
            </div>

            <div class="bg-slate-900/60 backdrop-blur-md border border-slate-800/80 rounded-2xl p-6 shadow-xl flex flex-col justify-between hover:border-slate-700/80 transition duration-300">
                <span class="text-slate-400 text-sm font-medium">Retorno de Inversión (ROI)</span>
                <span class="text-2xl font-bold mt-2 {roi_color}">{roi:+.2}%</span>
                <span class="text-xs text-slate-500 mt-1">Desde el inicio</span>
            </div>

            <div class="bg-slate-900/60 backdrop-blur-md border border-slate-800/80 rounded-2xl p-6 shadow-xl flex flex-col justify-between hover:border-slate-700/80 transition duration-300">
                <span class="text-slate-400 text-sm font-medium">Máximo Drawdown</span>
                <span class="text-2xl font-bold text-rose-500 mt-2">-{max_drawdown:.2}%</span>
                <span class="text-xs text-slate-500 mt-1">Pérdida máxima pico a valle</span>
            </div>
        </div>

        <!-- Secondary Metrics Row -->
        <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-5 gap-6">
            <div class="bg-slate-900/40 backdrop-blur-md border border-slate-800/60 rounded-xl p-5 flex items-center justify-between">
                <span class="text-slate-400 text-sm">Operaciones Totales</span>
                <span class="text-xl font-semibold text-violet-400">{total_trades}</span>
            </div>
            <div class="bg-slate-900/40 backdrop-blur-md border border-slate-800/60 rounded-xl p-5 flex items-center justify-between">
                <span class="text-slate-400 text-sm">Compras</span>
                <span class="text-xl font-semibold text-emerald-400">{num_compras}</span>
            </div>
            <div class="bg-slate-900/40 backdrop-blur-md border border-slate-800/60 rounded-xl p-5 flex items-center justify-between">
                <span class="text-slate-400 text-sm">Ventas</span>
                <span class="text-xl font-semibold text-amber-400">{num_ventas}</span>
            </div>
            <div class="bg-slate-900/40 backdrop-blur-md border border-slate-800/60 rounded-xl p-5 flex items-center justify-between">
                <span class="text-slate-400 text-sm">Liquidaciones</span>
                <span class="text-xl font-semibold {liq_color}">{num_liquidaciones}</span>
            </div>
            <div class="bg-slate-900/40 backdrop-blur-md border border-slate-800/60 rounded-xl p-5 flex items-center justify-between">
                <span class="text-slate-400 text-sm">Corr. Buy & Hold</span>
                <span class="text-xl font-semibold text-sky-400">{correlation:+.4}</span>
            </div>
        </div>

        <!-- Chart -->
        <div class="bg-slate-900/60 backdrop-blur-md border border-slate-800/80 rounded-3xl p-6 md:p-8 shadow-2xl">
            <h2 class="text-lg font-semibold text-slate-200 mb-6 flex items-center gap-2">
                <svg class="w-5 h-5 text-violet-400" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M7 12l3-3 3 3 4-4M8 21h8a2 2 0 002-2V5a2 2 0 00-2-2H8a2 2 0 00-2 2v14a2 2 0 002 2z"></path></svg>
                Comparativa de Equidad (Gemma vs Buy & Hold)
            </h2>
            <div class="relative w-full h-[400px]">
                <canvas id="equityChart"></canvas>
            </div>
        </div>
    </div>

    <script>
        const labels = {labels_json};
        const dataPoints = {data_json};
        const bhDataPoints = {bh_data_json};

        const ctx = document.getElementById('equityChart').getContext('2d');
        const gradient = ctx.createLinearGradient(0, 0, 0, 400);
        gradient.addColorStop(0, 'rgba(139, 92, 246, 0.4)');
        gradient.addColorStop(1, 'rgba(139, 92, 246, 0.0)');

        new Chart(ctx, {{
            type: 'line',
            data: {{
                labels: labels,
                datasets: [
                    {{
                        label: 'Gemma Trading Bot (USDT)',
                        data: dataPoints,
                        borderColor: '#a78bfa',
                        borderWidth: 2,
                        pointRadius: labels.length > 50 ? 0 : 3,
                        pointHoverRadius: 6,
                        fill: true,
                        backgroundColor: gradient,
                        tension: 0.2
                    }},
                    {{
                        label: 'Buy & Hold BTC (USDT)',
                        data: bhDataPoints,
                        borderColor: '#64748b',
                        borderWidth: 1.5,
                        borderDash: [5, 5],
                        pointRadius: 0,
                        fill: false,
                        tension: 0.2
                    }}
                ]
            }},
            options: {{
                responsive: true,
                maintainAspectRatio: false,
                plugins: {{
                    legend: {{
                        display: true,
                        labels: {{
                            color: '#cbd5e1',
                            font: {{ family: 'Outfit', size: 12 }}
                        }}
                    }},
                    tooltip: {{
                        mode: 'index',
                        intersect: false,
                        backgroundColor: '#1e293b',
                        titleColor: '#cbd5e1',
                        bodyColor: '#f8fafc',
                        borderColor: '#475569',
                        borderWidth: 1
                    }}
                }},
                scales: {{
                    x: {{
                        grid: {{ display: false }},
                        ticks: {{
                            color: '#94a3b8',
                            font: {{ family: 'Outfit' }},
                            callback: function(value, index, ticks) {{
                                const total = ticks.length;
                                if (total <= 10) {{
                                    return this.getLabelForValue(value);
                                }}
                                if (index === 0 || index === total - 1) {{
                                    return this.getLabelForValue(value);
                                }}
                                const step = (total - 1) / 9;
                                for (let i = 1; i <= 8; i++) {{
                                    if (Math.abs(index - Math.round(i * step)) < 0.5) {{
                                        return this.getLabelForValue(value);
                                    }}
                                }}
                                return '';
                            }},
                            autoSkip: false,
                            maxRotation: 0,
                            minRotation: 0
                        }}
                    }},
                    y: {{
                        grid: {{ color: '#1e293b' }},
                        ticks: {{
                            color: '#94a3b8',
                            font: {{ family: 'Outfit' }},
                            callback: function(value) {{
                                return '$' + value.toLocaleString();
                            }}
                        }}
                    }}
                }}
            }}
        }});
    </script>
</body>
</html>"#,
        initial_balance = initial_balance,
        final_balance = final_balance,
        roi = roi,
        roi_color = if roi >= 0.0 { "text-emerald-400" } else { "text-rose-500" },
        max_drawdown = max_drawdown * 100.0,
        total_trades = num_compras + num_ventas,
        num_compras = num_compras,
        num_ventas = num_ventas,
        num_liquidaciones = num_liquidaciones,
        liq_color = if num_liquidaciones > 0 { "text-rose-500 animate-pulse" } else { "text-slate-400" },
        correlation = correlation,
        labels_json = labels_json,
        data_json = data_json,
        bh_data_json = bh_data_json
    );

    let mut file = File::create(filename)?;
    write!(file, "{}", html)?;
    Ok(())
}

fn get_liquidation_percentage(leverage: f64) -> f64 {
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

async fn run_backtest(
    db_path: &str,
    timeframe: &str,
    leverage: f64,
    risk_percent: f64,
    limit: Option<usize>,
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
        "Eres un agente de trading experto para BTCUSDT en el mercado de futuros de criptomonedas. Tu objetivo es aumentar el valor total de tu cuenta en USDT.\n\
         Operas en modo de MARGEN AISLADO (Isolated Margin) con apalancamiento {}X. En cada operación utilizas exactamente el {}% de tu equidad actual como margen.\n\
         Cada operación (apertura y cierre) tiene una comisión de transacción del 0.05% sobre el volumen de la posición (Volumen = Margen x Apalancamiento).\n\
         \n\
         REGLAS DE OPERATORIA:\n\
         - Si no tienes posiciones abiertas:\n\
           * 'COMPRAR': Abre una posición LONG (Alza) con apalancamiento {}X utilizando el {}% de tu equidad como margen.\n\
           * 'VENDER': Abre una posición SHORT (Baja) con apalancamiento {}X utilizando el {}% de tu equidad como margen.\n\
           * 'MANTENER': No abres posición.\n\
         - Si tienes posiciones LONG activas:\n\
           * 'VENDER': Cierra TODAS las posiciones LONG actuales a precio de mercado y te devuelve el margen restante más la ganancia/pérdida (menos comisiones).\n\
           * 'COMPRAR': Abre otra posición LONG (acumulando posiciones) utilizando el {}% de tu equidad actual como margen (si hay saldo disponible).\n\
           * 'MANTENER': Mantiene las posiciones LONG activas.\n\
         - Si tienes posiciones SHORT activas:\n\
           * 'COMPRAR': Cierra TODAS las posiciones SHORT actuales a precio de mercado y te devuelve el margen restante más la ganancia/pérdida (menos comisiones).\n\
           * 'VENDER': Abre otra posición SHORT (acumulando posiciones) utilizando el {}% de tu equidad actual como margen (si hay saldo disponible).\n\
           * 'MANTENER': Mantiene las posiciones SHORT activas.\n\
         \n\
         Debes responder ESTRICTAMENTE en formato JSON con la siguiente estructura y nada más:\n\
         {{\n\
           \"analisis\": \"Breve explicación del motivo de tu decisión fundamentada en el análisis de las velas recientes y las posiciones abiertas\",\n\
           \"accion\": \"COMPRAR\", \"VENDER\" o \"MANTENER\"\n\
         }}", leverage, risk_percent, leverage, risk_percent, leverage, risk_percent, risk_percent, risk_percent
    );

    let mut active_positions: Vec<Position> = Vec::new();

    for (i, candle) in candles.iter().enumerate() {
        let date_format = if timeframe == "1d" { "%Y-%m-%d" } else { "%Y-%m-%d %H:%M:%S" };
        let date_str = chrono::Utc.timestamp_millis_opt(candle.open_time)
            .unwrap()
            .format(date_format)
            .to_string();

        let precio_actual = candle.close;

        // 1. Verificar si hay liquidación en esta vela (usando high/low)
        let mut liquidated_indices = Vec::new();
        for (idx, pos) in active_positions.iter().enumerate() {
            let mut liquidado = false;
            match pos.position_type {
                PositionType::Long => {
                    if candle.low <= pos.liquidation_price {
                        liquidado = true;
                    }
                }
                PositionType::Short => {
                    if candle.high >= pos.liquidation_price {
                        liquidado = true;
                    }
                }
                _ => {}
            }
            if liquidado {
                liquidated_indices.push(idx);
            }
        }

        // Process liquidations from last to first
        liquidated_indices.reverse();
        for idx in liquidated_indices {
            let pos = active_positions.remove(idx);
            println!("🔥 LIQUIDACIÓN DETECTADA: La posición {:?} fue liquidada al tocar el precio de {:.2} USDT (Entrada: {:.2} USDT). Se perdió el margen de {:.2} USDT.",
                pos.position_type, pos.liquidation_price, pos.entry_price, pos.margin
            );
            num_liquidaciones += 1;
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
        for (idx, prev_candle) in candles[start_idx..=i].iter().enumerate() {
            let candle_time = chrono::Utc.timestamp_millis_opt(prev_candle.open_time)
                .unwrap()
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();
            
            let label = if start_idx + idx == i {
                " (Vela Actual)"
            } else {
                ""
            };
            
            history_str.push_str(&format!(
                "- {}{}: Open={:.2}, High={:.2}, Low={:.2}, Close={:.2}, Volume={:.2}\n",
                candle_time, label, prev_candle.open, prev_candle.high, prev_candle.low, prev_candle.close, prev_candle.volume
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
                positions_str.push_str(&format!(
                    "- Posición #{}: {:?} | Entrada: {:.2} USDT | Margen: {:.2} USDT | Tamaño: {:.6} BTC | Liq: {:.2} USDT | PnL Flotante: {:.2} USDT\n",
                    idx + 1, pos.position_type, pos.entry_price, pos.margin, pos.size_btc, pos.liquidation_price, pnl
                ));
            }
        }

        // 4. Prompt a Gemma
        let user_prompt = format!(
            "Precio actual de BTC (Cierre): {:.2} USDT\n\n\
             Historial de las últimas 10 velas (de más antigua a más reciente):\n\
             {}\n\
             Estado de tu Cartera:\n\
             - Saldo libre en USDT (no en margen): {:.2} USDT\n\
             - Posiciones Activas:\n\
             {}\n\
             - Equidad total de la cuenta (Equity): {:.2} USDT\n\
             - Apalancamiento actual: {:.1}x\n\
             - Comisión por operación: 0.05% sobre el volumen operado\n\n\
             ¿Qué acción tomas? Responde estrictamente en formato JSON.",
            precio_actual, history_str, saldo_usdt, positions_str, equity, leverage
        );

        let mut retries = 3;
        let mut gemma_action = "MANTENER".to_string();
        let mut gemma_analisis = "Error al obtener respuesta".to_string();

        while retries > 0 {
            match call_gemma(&client, &api_url, &api_token, &system_prompt, &user_prompt).await {
                Ok(content) => {
                    if let Some(parsed) = parse_gemma_response(&content) {
                        gemma_action = parsed.accion.to_uppercase();
                        gemma_analisis = parsed.analisis;
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

        println!("🤖 Gemma dice: {}", gemma_analisis);
        println!("📈 Acción elegida: {}", gemma_action);

        // 5. Ejecutar la acción elegida
        if gemma_action == "COMPRAR" {
            let has_shorts = active_positions.iter().any(|p| p.position_type == PositionType::Short);
            if has_shorts {
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
            } else {
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
                    });
                    num_compras += 1;
                    println!("🛒 LONG ABIERTO: Margen: {:.2} USDT | Tamaño: {:.6} BTC (${:.2}) | Liq: {:.2} USDT | Fee: {:.2} USDT",
                        margin, pos_size_btc, size_usdt, pos_liq_price, opening_fee
                    );
                } else {
                    println!("⏳ Margen/saldo insuficiente para abrir LONG.");
                }
            }
        } else if gemma_action == "VENDER" {
            let has_longs = active_positions.iter().any(|p| p.position_type == PositionType::Long);
            if has_longs {
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
            } else {
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
                    });
                    num_ventas += 1;
                    println!("🛒 SHORT ABIERTO: Margen: {:.2} USDT | Tamaño: {:.6} BTC (${:.2}) | Liq: {:.2} USDT | Fee: {:.2} USDT",
                        margin, pos_size_btc, size_usdt, pos_liq_price, opening_fee
                    );
                } else {
                    println!("⏳ Margen/saldo insuficiente para abrir SHORT.");
                }
            }
        } else {
            println!("⏳ Manteniendo posición/Sin acción ejecutada.");
        }

        // Wait a bit to avoid overloading LM Studio or too fast output
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    // Guardar Equity Curve
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

// ================= BingX API Integration =================

#[derive(Debug, Clone)]
struct BingXAccountInfo {
    wallet_balance: f64,
    available_margin: f64,
    user_id: String,
}

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

async fn test_api_connection(
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

async fn set_leverage(
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

async fn get_ticker_price(
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

async fn open_market_order(
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

async fn get_open_positions(
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

async fn get_account_details(
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

async fn get_stable_balance(
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

// Database Helpers for API Config
fn init_api_db(db_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open(db_path)?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS api_config (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timeframe TEXT NOT NULL,
            api_key TEXT NOT NULL,
            api_secret TEXT NOT NULL,
            leverage INTEGER NOT NULL DEFAULT 10,
            exchange TEXT NOT NULL,
            use_testnet INTEGER NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS llm_config (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            api_url TEXT NOT NULL,
            api_token TEXT NOT NULL
        )",
        [],
    )?;
    // Insert default values if not exists
    conn.execute(
        "INSERT OR IGNORE INTO llm_config (id, api_url, api_token) VALUES (1, 'http://127.0.0.1:5508/v1/chat/completions', 'lm-studio')",
        [],
    )?;
    Ok(())
}

fn get_llm_config(db_path: &str) -> Result<(String, String), Box<dyn std::error::Error>> {
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare("SELECT api_url, api_token FROM llm_config WHERE id = 1")?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        let api_url: String = row.get(0)?;
        let api_token: String = row.get(1)?;
        Ok((api_url, api_token))
    } else {
        Ok(("http://127.0.0.1:5508/v1/chat/completions".to_string(), "lm-studio".to_string()))
    }
}

fn save_llm_config(db_path: &str, api_url: &str, api_token: &str) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open(db_path)?;
    conn.execute(
        "INSERT OR REPLACE INTO llm_config (id, api_url, api_token) VALUES (1, ?1, ?2)",
        rusqlite::params![api_url, api_token],
    )?;
    Ok(())
}

fn get_api_config(db_path: &str) -> Result<Option<(String, String, u32, String, bool)>, Box<dyn std::error::Error>> {
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare("SELECT api_key, api_secret, leverage, exchange, use_testnet FROM api_config LIMIT 1")?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        let api_key: String = row.get(0)?;
        let api_secret: String = row.get(1)?;
        let leverage: u32 = row.get(2)?;
        let exchange: String = row.get(3)?;
        let use_testnet_int: i32 = row.get(4)?;
        Ok(Some((api_key, api_secret, leverage, exchange, use_testnet_int == 1)))
    } else {
        Ok(None)
    }
}

fn save_api_config(
    db_path: &str,
    api_key: &str,
    api_secret: &str,
    leverage: u32,
    use_testnet: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open(db_path)?;
    conn.execute("DELETE FROM api_config", [])?; // Clear old configs
    conn.execute(
        "INSERT INTO api_config (timeframe, api_key, api_secret, leverage, exchange, use_testnet)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params!["Cuenta Principal", api_key, api_secret, leverage, "BingX", if use_testnet { 1 } else { 0 }],
    )?;
    Ok(())
}

fn delete_api_config(db_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open(db_path)?;
    conn.execute("DELETE FROM api_config", [])?;
    Ok(())
}

fn get_latest_candles(db_path: &str, timeframe: &str, limit: usize) -> Result<Vec<Candle>, rusqlite::Error> {
    let conn = Connection::open(db_path)?;
    let query = format!(
        "SELECT open_time, open, high, low, close, volume, close_time FROM candles WHERE timeframe = ?1 ORDER BY open_time DESC LIMIT {}",
        limit
    );
    let mut stmt = conn.prepare(&query)?;
    let candle_iter = stmt.query_map([timeframe], |row| {
        Ok(Candle {
            open_time: row.get::<_, f64>(0)? as i64,
            open: row.get(1)?,
            high: row.get(2)?,
            low: row.get(3)?,
            close: row.get(4)?,
            volume: row.get(5)?,
            close_time: row.get::<_, f64>(6)? as i64,
        })
    })?;

    let mut candles = Vec::new();
    for candle in candle_iter {
        candles.push(candle?);
    }
    // Reverse so it is ordered from oldest to newest
    candles.reverse();
    Ok(candles)
}

async fn run_live_gemma_step(
    db_path: &str,
    timeframe: &str,
    client: &reqwest::Client,
    api_key: &str,
    api_secret: &str,
    leverage: u32,
    use_testnet: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Fetch latest 10 candles from DB
    let candles = get_latest_candles(db_path, timeframe, 10)?;
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
    
    // Format candle history for Gemma user prompt
    let mut history_str = String::new();
    for (idx, prev_candle) in candles.iter().enumerate() {
        let candle_time = chrono::Utc.timestamp_millis_opt(prev_candle.open_time)
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        
        let label = if idx == candles.len() - 1 {
            " (Vela Actual)"
        } else {
            ""
        };
        
        history_str.push_str(&format!(
            "- {}{}: Open={:.2}, High={:.2}, Low={:.2}, Close={:.2}, Volume={:.2}\n",
            candle_time, label, prev_candle.open, prev_candle.high, prev_candle.low, prev_candle.close, prev_candle.volume
        ));
    }
    
    // System and User prompt
    let system_prompt = format!(
        "Eres un agente de trading experto para BTCUSDT en el mercado de futuros de criptomonedas. Tu objetivo es aumentar el valor total de tu cuenta en USDT.\n\
         Operas en modo de MARGEN AISLADO (Isolated Margin) con apalancamiento {}X. En cada operación utilizas exactamente el 10% de tu saldo disponible total como margen.\n\
         Cada operación (apertura y cierre) tiene una comisión de transacción del 0.05% sobre el volumen de la posición (Volumen = Margen x Apalancamiento).\n\
         \n\
         REGLAS DE OPERATORIA:\n\
         - Si no tienes posición abierta actualmente:\n\
           * 'COMPRAR': Abre una posición LONG (Alza) con apalancamiento {}X utilizando el 10% de tu saldo disponible como margen.\n\
           * 'VENDER': Abre una posición SHORT (Baja) con apalancamiento {}X utilizando el 10% de tu saldo disponible como margen.\n\
           * 'MANTENER': No abres posición y te mantienes en cash (USDT).\n\
         - Si tienes una posición LONG activa:\n\
           * 'VENDER': Cierra la posición LONG actual a precio de mercado y te devuelve el margen restante más la ganancia/pérdida (menos comisiones).\n\
           * 'COMPRAR' o 'MANTENER': Mantiene la posición LONG activa.\n\
         - Si tienes una posición SHORT activa:\n\
           * 'COMPRAR': Cierra la posición SHORT actual a precio de mercado y te devuelve el margen restante más la ganancia/pérdida (menos comisiones).\n\
           * 'VENDER' o 'MANTENER': Mantiene la posición SHORT activa.\n\
         \n\
         Debes responder ESTRICTAMENTE en formato JSON con la siguiente estructura y nada más:\n\
         {{\n\
           \"analisis\": \"Breve explicación del motivo de tu decisión fundamentada en el análisis de las velas recientes\",\n\
           \"accion\": \"COMPRAR\", \"VENDER\" o \"MANTENER\"\n\
         }}", leverage, leverage, leverage
    );
    
    let user_prompt = format!(
        "Precio actual de BTC (Cierre): {:.2} USDT\n\n\
         Historial de las últimas 10 velas (de más antigua a más reciente):\n\
         {}\n\
         Estado de tu Cartera:\n\
         - Saldo libre en USDT (no en margen): {:.2} USDT\n\
         - Posición activa: {:?}\n\
         - Margen de la posición: {:.2} USDT (Modo Aislado)\n\
         - Tamaño de posición equivalente: {:.6} BTC (${:.2})\n\
         - Precio de entrada: {:.2} USDT\n\
         - Precio de liquidación estimado: {:.2} USDT (Si se mueve {:.3}% en contra)\n\
         - PnL Flotante actual: {:.2} USDT\n\
         - Equidad total de la cuenta (Equity): {:.2} USDT\n\
         - Comisión por operación: 0.05% sobre el volumen operado\n\n\
         ¿Qué acción tomas? Responde estrictamente en formato JSON.",
        precio_actual, history_str, saldo_usdt, position_type, position_margin,
        position_size_btc, position_size_btc * precio_actual, precio_entrada, liquidation_price, liq_percent,
        floating_pnl, equity
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
    let mut gemma_action = "MANTENER".to_string();
    let mut gemma_analisis = "Error al obtener respuesta".to_string();
    let mut retries = 3;
    
    while retries > 0 {
        match call_gemma(&client, &api_url, &api_token, &system_prompt, &user_prompt).await {
            Ok(content) => {
                if let Some(parsed) = parse_gemma_response(&content) {
                    gemma_action = parsed.accion.to_uppercase();
                    gemma_analisis = parsed.analisis;
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
    
    println!("🤖 Gemma dice: {}", gemma_analisis);
    println!("📈 Acción elegida: {}", gemma_action);
    
    // Execute live action
    if gemma_action == "COMPRAR" {
        match position_type {
            PositionType::None => {
                // Open LONG with 10% of equity as margin
                let margin = equity * 0.1;
                let size_usdt = margin * leverage as f64;
                let price = get_ticker_price(client, "BTC-USDT", use_testnet).await?;
                let qty = size_usdt / price;
                
                println!("🛒 Abriendo LONG... Margen: {:.2} USDT | Tamaño: {:.4} BTC | Apalancamiento: {}x", margin, qty, leverage);
                
                // Set leverage first
                if let Err(e) = set_leverage(client, api_key, api_secret, "BTC-USDT", leverage, "LONG", use_testnet).await {
                    println!("⚠️ Error configurando apalancamiento: {}", e);
                }
                
                match open_market_order(client, api_key, api_secret, "BTC-USDT", "BUY", "LONG", qty, None, use_testnet).await {
                    Ok(res) => {
                        println!("✅ LONG ABIERTO EXITOSAMENTE EN BINGX.");
                        if let Some(avg_price) = res.get("avgPrice").and_then(|v| v.as_str()) {
                            println!("- Precio promedio: {} USDT", avg_price);
                        }
                    }
                    Err(e) => println!("❌ Error abriendo LONG: {}", e),
                }
            }
            PositionType::Short => {
                // Close SHORT
                println!("💰 Cerrando SHORT de {:.4} BTC...", position_size_btc);
                match open_market_order(client, api_key, api_secret, "BTC-USDT", "BUY", "SHORT", position_size_btc, None, use_testnet).await {
                    Ok(_) => println!("✅ SHORT CERRADO EXITOSAMENTE EN BINGX."),
                    Err(e) => println!("❌ Error cerrando SHORT: {}", e),
                }
            }
            PositionType::Long => {
                println!("⏳ Ya tienes una posición LONG activa. Manteniendo...");
            }
        }
    } else if gemma_action == "VENDER" {
        match position_type {
            PositionType::None => {
                // Open SHORT with 10% of equity as margin
                let margin = equity * 0.1;
                let size_usdt = margin * leverage as f64;
                let price = get_ticker_price(client, "BTC-USDT", use_testnet).await?;
                let qty = size_usdt / price;
                
                println!("🛒 Abriendo SHORT... Margen: {:.2} USDT | Tamaño: {:.4} BTC | Apalancamiento: {}x", margin, qty, leverage);
                
                // Set leverage first
                if let Err(e) = set_leverage(client, api_key, api_secret, "BTC-USDT", leverage, "SHORT", use_testnet).await {
                    println!("⚠️ Error configurando apalancamiento: {}", e);
                }
                
                match open_market_order(client, api_key, api_secret, "BTC-USDT", "SELL", "SHORT", qty, None, use_testnet).await {
                    Ok(res) => {
                        println!("✅ SHORT ABIERTO EXITOSAMENTE EN BINGX.");
                        if let Some(avg_price) = res.get("avgPrice").and_then(|v| v.as_str()) {
                            println!("- Precio promedio: {} USDT", avg_price);
                        }
                    }
                    Err(e) => println!("❌ Error abriendo SHORT: {}", e),
                }
            }
            PositionType::Long => {
                // Close LONG
                println!("💰 Cerrando LONG de {:.4} BTC...", position_size_btc);
                match open_market_order(client, api_key, api_secret, "BTC-USDT", "SELL", "LONG", position_size_btc, None, use_testnet).await {
                    Ok(_) => println!("✅ LONG CERRADO EXITOSAMENTE EN BINGX."),
                    Err(e) => println!("❌ Error cerrando LONG: {}", e),
                }
            }
            PositionType::Short => {
                println!("⏳ Ya tienes una posición SHORT activa. Manteniendo...");
            }
        }
    } else {
        println!("⏳ Manteniendo posición/Sin acción ejecutada.");
    }
    
    Ok(())
}

async fn trading_en_vivo_menu(db_path: &str, client: &reqwest::Client) -> Result<(), Box<dyn std::error::Error>> {
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
                                    if let Err(e) = run_live_gemma_step(db_path, timeframe, client, &api_key, &api_secret, leverage, use_testnet).await {
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = "btcusdt.db";
    let client = reqwest::Client::new();
    
    // Initialize DB tables
    let _ = init_api_db(db_path);

    loop {
        println!("\n                      Gemma Trading Bot");
        println!("1. Update DB");
        println!("2. Backtest Completo");
        println!("3. Prueba de Backtest");
        println!("4. Configurar Modelo Local (Gemma)");
        println!("5. Trading en Vivo");
        println!("6. Salir");
        print!("Selecciona una opción: ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let choice = input.trim();

        match choice {
            "1" => {
                println!("\nSeleccione temporalidad a descargar:");
                println!("1) 1H (1 Hora)");
                println!("2) 4H (4 Horas)");
                println!("3) 1D (1 Día)");
                println!("4) Todas (1H, 4H y 1D)");
                print!("Selecciona una opción: ");
                let _ = io::stdout().flush();
                let mut tf_choice = String::new();
                let mut timeframes = Vec::new();
                if io::stdin().read_line(&mut tf_choice).is_ok() {
                    match tf_choice.trim() {
                        "2" => timeframes.push("4h"),
                        "3" => timeframes.push("1d"),
                        "4" => {
                            timeframes.push("1h");
                            timeframes.push("4h");
                            timeframes.push("1d");
                        }
                        _ => timeframes.push("1h"),
                    }
                } else {
                    timeframes.push("1h");
                }

                for tf in timeframes {
                    println!("🔄 Descargando/Actualizando velas de {}...", tf);
                    if let Err(e) = download_candles(db_path, tf).await {
                        println!("❌ Error al descargar velas de {}: {}", tf, e);
                    }
                }
            }
            "2" => {
                println!("\nSelecciona la temporalidad para el backtest:");
                println!("1) 1H (1 Hora)");
                println!("2) 4H (4 Horas)");
                println!("3) 1D (1 Día)");
                print!("Selecciona una opción: ");
                let _ = io::stdout().flush();
                let mut tf_choice = String::new();
                let timeframe = if io::stdin().read_line(&mut tf_choice).is_ok() {
                    match tf_choice.trim() {
                        "2" => "4h",
                        "3" => "1d",
                        _ => "1h",
                    }
                } else {
                    "1h"
                };

                print!("Introduce el apalancamiento a usar (ej. 10): ");
                let _ = io::stdout().flush();
                let mut lev_input = String::new();
                let mut leverage = 10.0;
                if io::stdin().read_line(&mut lev_input).is_ok() {
                    if let Ok(num) = lev_input.trim().parse::<f64>() {
                        if num > 0.0 {
                            leverage = num;
                        }
                    }
                }

                print!("Introduce el porcentaje de capital a arriesgar por operación (ej. 10 para 10%): ");
                let _ = io::stdout().flush();
                let mut risk_input = String::new();
                let mut risk_percent = 10.0;
                if io::stdin().read_line(&mut risk_input).is_ok() {
                    if let Ok(num) = risk_input.trim().parse::<f64>() {
                        if num > 0.0 && num <= 100.0 {
                            risk_percent = num;
                        }
                    }
                }

                print!("Introduce la cantidad de velas para el backtest (0 para evaluar todas): ");
                let _ = io::stdout().flush();
                let mut limit_input = String::new();
                let mut limit = None;
                if io::stdin().read_line(&mut limit_input).is_ok() {
                    if let Ok(num) = limit_input.trim().parse::<usize>() {
                        if num > 0 {
                            limit = Some(num);
                        }
                    }
                }
                if let Err(e) = run_backtest(db_path, timeframe, leverage, risk_percent, limit).await {
                    println!("❌ Error en el backtest: {}", e);
                }
            }
            "3" => {
                println!("\nSelecciona la temporalidad para la prueba de backtest:");
                println!("1) 1H (1 Hora)");
                println!("2) 4H (4 Horas)");
                println!("3) 1D (1 Día)");
                print!("Selecciona una opción: ");
                let _ = io::stdout().flush();
                let mut tf_choice = String::new();
                let timeframe = if io::stdin().read_line(&mut tf_choice).is_ok() {
                    match tf_choice.trim() {
                        "2" => "4h",
                        "3" => "1d",
                        _ => "1h",
                    }
                } else {
                    "1h"
                };

                print!("Introduce el apalancamiento a usar (ej. 10): ");
                let _ = io::stdout().flush();
                let mut lev_input = String::new();
                let mut leverage = 10.0;
                if io::stdin().read_line(&mut lev_input).is_ok() {
                    if let Ok(num) = lev_input.trim().parse::<f64>() {
                        if num > 0.0 {
                            leverage = num;
                        }
                    }
                }

                print!("Introduce el porcentaje de capital a arriesgar por operación (ej. 10 para 10%): ");
                let _ = io::stdout().flush();
                let mut risk_input = String::new();
                let mut risk_percent = 10.0;
                if io::stdin().read_line(&mut risk_input).is_ok() {
                    if let Ok(num) = risk_input.trim().parse::<f64>() {
                        if num > 0.0 && num <= 100.0 {
                            risk_percent = num;
                        }
                    }
                }

                if let Err(e) = run_backtest(db_path, timeframe, leverage, risk_percent, Some(10)).await {
                    println!("❌ Error en la prueba: {}", e);
                }
            }
            "4" => {
                println!("\n[Configurar Modelo Local (Gemma)]");
                let (curr_url, curr_token) = get_llm_config(db_path).unwrap_or((
                    "http://127.0.0.1:5508/v1/chat/completions".to_string(),
                    "lm-studio".to_string()
                ));
                print!("Ingrese URL de API local [actual: {}]: ", curr_url);
                io::stdout().flush()?;
                let mut url_input = String::new();
                io::stdin().read_line(&mut url_input)?;
                let url = if url_input.trim().is_empty() { curr_url } else { url_input.trim().to_string() };

                print!("Ingrese API Token local [actual: {}]: ", curr_token);
                io::stdout().flush()?;
                let mut token_input = String::new();
                io::stdin().read_line(&mut token_input)?;
                let token = if token_input.trim().is_empty() { curr_token } else { token_input.trim().to_string() };

                if let Err(e) = save_llm_config(db_path, &url, &token) {
                    println!("❌ Error al guardar configuración de LLM: {}", e);
                } else {
                    println!("✅ Configuración de Gemma guardada con éxito en la base de datos.");
                }
            }
            "5" => {
                if let Err(e) = trading_en_vivo_menu(db_path, &client).await {
                    println!("❌ Error en el menú de trading en vivo: {}", e);
                }
            }
            "6" => {
                println!("👋 ¡Hasta luego!");
                break;
            }
            _ => {
                println!("❌ Opción inválida, por favor intenta de nuevo.");
            }
        }
    }

    Ok(())
}
