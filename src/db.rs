use rusqlite::Connection;
use crate::types::Candle;
use chrono::TimeZone;

pub async fn download_candles(db_path: &str, timeframe: &str) -> Result<(), Box<dyn std::error::Error>> {
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

pub fn get_candles(db_path: &str, timeframe: &str, limit: Option<usize>) -> Result<Vec<Candle>, rusqlite::Error> {
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

pub fn get_latest_candles(db_path: &str, timeframe: &str, limit: usize) -> Result<Vec<Candle>, rusqlite::Error> {
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
    candles.reverse();
    Ok(candles)
}

pub fn init_api_db(db_path: &str) -> Result<(), Box<dyn std::error::Error>> {
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
    conn.execute(
        "INSERT OR IGNORE INTO llm_config (id, api_url, api_token) VALUES (1, 'http://127.0.0.1:5508/v1/chat/completions', 'lm-studio')",
        [],
    )?;
    Ok(())
}

pub fn get_llm_config(db_path: &str) -> Result<(String, String), Box<dyn std::error::Error>> {
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

pub fn save_llm_config(db_path: &str, api_url: &str, api_token: &str) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open(db_path)?;
    conn.execute(
        "INSERT OR REPLACE INTO llm_config (id, api_url, api_token) VALUES (1, ?1, ?2)",
        rusqlite::params![api_url, api_token],
    )?;
    Ok(())
}

pub fn get_api_config(db_path: &str) -> Result<Option<(String, String, u32, String, bool)>, Box<dyn std::error::Error>> {
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

pub fn save_api_config(
    db_path: &str,
    api_key: &str,
    api_secret: &str,
    leverage: u32,
    use_testnet: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open(db_path)?;
    conn.execute("DELETE FROM api_config", [])?;
    conn.execute(
        "INSERT INTO api_config (timeframe, api_key, api_secret, leverage, exchange, use_testnet)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params!["Cuenta Principal", api_key, api_secret, leverage, "BingX", if use_testnet { 1 } else { 0 }],
    )?;
    Ok(())
}

pub fn delete_api_config(db_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open(db_path)?;
    conn.execute("DELETE FROM api_config", [])?;
    Ok(())
}
