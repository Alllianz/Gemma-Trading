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
use crate::indicators::calculate_indicators;

pub async fn run_live_gemma_step(
    db_path: &str,
    timeframe: &str,
    client: &reqwest::Client,
    api_key: &str,
    api_secret: &str,
    leverage: u32,
    use_testnet: bool,
    confidence_threshold: u32,
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
    let actual_history_len = candles.len();
    for (idx, prev_candle) in candles.iter().enumerate() {
        let label = if idx == candles.len() - 1 { " (Actual)" } else { "" };
        history_str.push_str(&format!(
            "- t-{}: O:{:.1}, H:{:.1}, L:{:.1}, C:{:.1}, V:{:.0}{}\n",
            actual_history_len - 1 - idx, prev_candle.open, prev_candle.high, prev_candle.low, prev_candle.close, prev_candle.volume, label
        ));
    }
    
    // System and User prompt
    let system_prompt = format!(
        "Bot de trading de futuros BTCUSDT (Margen Aislado {}X, Margen operado: 10% saldo). Comisión: 0.05%.\n\n\
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
           \"confianza\": entero_de_0_a_100\n\
         }}", leverage
    );
    
    // Calcular indicadores técnicos para pasárselos a Gemma
    let (indicador_tendencia, indicador_volatilidad, _indicador_posicion, indicador_presion) = 
        calculate_indicators(&candles, 0, candles.len() - 1, precio_actual);

    let user_prompt = format!(
        "Precio actual de BTC (Cierre): {:.2} USDT\n\n\
         Historial de las últimas 10 velas (de más antigua a más reciente):\n\
         {}\n\
         Indicadores Técnicos (Ventana de 10 velas):\n\
         - Tendencia: {}\n\
         - Volatilidad: {}\n\
         - Presión Cuerpo/Volumen: {}\n\n\
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
        precio_actual, history_str, indicador_tendencia, indicador_volatilidad, indicador_presion, saldo_usdt, position_type, position_margin,
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
    let mut gemma_action = "FLAT".to_string();
    let mut gemma_analisis = "Error al obtener respuesta".to_string();
    let mut gemma_confidence = None;
    let mut retries = 3;
    
    while retries > 0 {
        println!("\n=== [ENVÍO A GEMMA] ===");
        println!("System Prompt:\n{}", system_prompt);
        println!("User Prompt:\n{}", user_prompt);
        println!("=======================");
        match call_gemma(&client, &api_url, &api_token, &system_prompt, &user_prompt).await {
            Ok(content) => {
                println!("\n=== [RESPUESTA DE GEMMA] ===");
                println!("{}", content.trim());
                println!("============================");
                if let Some(parsed) = parse_gemma_response(&content) {
                    gemma_action = parsed.accion.to_uppercase().replace(" ", "_");
                    gemma_analisis = parsed.analisis.unwrap_or_else(|| "Sin análisis".to_string());
                    gemma_confidence = parsed.confianza;
                    
                    let conf = gemma_confidence.unwrap_or(0);
                    if confidence_threshold > 0 && conf < confidence_threshold {
                        if position_type != PositionType::None {
                            println!("⚠️ Confianza ({}%) por debajo del umbral ({}%). Iniciando cierre de posiciones activas por seguridad.", conf, confidence_threshold);
                            gemma_action = "CERRAR_TODO".to_string();
                        } else if gemma_action != "FLAT" {
                            println!("⚠️ Gemma sugirió {} con confianza {}%, pero el umbral es {}%. Acción cambiada a FLAT.", gemma_action, conf, confidence_threshold);
                            gemma_action = "FLAT".to_string();
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
    
    if let Some(conf) = gemma_confidence {
        if gemma_analisis != "Sin análisis" && gemma_analisis != "Error al obtener respuesta" && !gemma_analisis.trim().is_empty() {
            println!("🤖 Gemma dice: {} (Confianza: {}%)", gemma_analisis, conf);
        } else {
            println!("🤖 Gemma (Confianza: {}%)", conf);
        }
    } else {
        if gemma_analisis != "Sin análisis" && gemma_analisis != "Error al obtener respuesta" && !gemma_analisis.trim().is_empty() {
            println!("🤖 Gemma dice: {}", gemma_analisis);
        }
    }
    println!("📈 Acción elegida: {}", gemma_action);
    
    // Execute live action
    if gemma_action == "ABRIR_LONG" {
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
            _ => {
                println!("⏳ Acción ABRIR_LONG recibida, pero ya tienes una posición activa ({:?}). Manteniendo...", position_type);
            }
        }
    } else if gemma_action == "CERRAR_LONG" {
        match position_type {
            PositionType::Long => {
                // Close LONG
                println!("💰 Cerrando LONG de {:.4} BTC...", position_size_btc);
                match open_market_order(client, api_key, api_secret, "BTC-USDT", "SELL", "LONG", position_size_btc, None, use_testnet).await {
                    Ok(_) => println!("✅ LONG CERRADO EXITOSAMENTE EN BINGX."),
                    Err(e) => println!("❌ Error cerrando LONG: {}", e),
                }
            }
            _ => {
                println!("⏳ Acción CERRAR_LONG recibida, pero no tienes ninguna posición LONG activa (Posición actual: {:?}).", position_type);
            }
        }
    } else if gemma_action == "ABRIR_SHORT" {
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
            _ => {
                println!("⏳ Acción ABRIR_SHORT recibida, pero ya tienes una posición activa ({:?}). Manteniendo...", position_type);
            }
        }
    } else if gemma_action == "CERRAR_SHORT" {
        match position_type {
            PositionType::Short => {
                // Close SHORT
                println!("💰 Cerrando SHORT de {:.4} BTC...", position_size_btc);
                match open_market_order(client, api_key, api_secret, "BTC-USDT", "BUY", "SHORT", position_size_btc, None, use_testnet).await {
                    Ok(_) => println!("✅ SHORT CERRADO EXITOSAMENTE EN BINGX."),
                    Err(e) => println!("❌ Error cerrando SHORT: {}", e),
                }
            }
            _ => {
                println!("⏳ Acción CERRAR_SHORT recibida, pero no tienes ninguna posición SHORT activa (Posición actual: {:?}).", position_type);
            }
        }
    } else if gemma_action == "CERRAR_TODO" {
        match position_type {
            PositionType::Long => {
                println!("💰 Cerrando LONG de {:.4} BTC por seguridad (confianza por debajo del umbral)...", position_size_btc);
                match open_market_order(client, api_key, api_secret, "BTC-USDT", "SELL", "LONG", position_size_btc, None, use_testnet).await {
                    Ok(_) => println!("✅ LONG CERRADO EXITOSAMENTE EN BINGX."),
                    Err(e) => println!("❌ Error cerrando LONG: {}", e),
                }
            }
            PositionType::Short => {
                println!("💰 Cerrando SHORT de {:.4} BTC por seguridad (confianza por debajo del umbral)...", position_size_btc);
                match open_market_order(client, api_key, api_secret, "BTC-USDT", "BUY", "SHORT", position_size_btc, None, use_testnet).await {
                    Ok(_) => println!("✅ SHORT CERRADO EXITOSAMENTE EN BINGX."),
                    Err(e) => println!("❌ Error cerrando SHORT: {}", e),
                }
            }
            PositionType::None => {
                println!("⏳ No hay posiciones activas para cerrar.");
            }
        }
    } else {
        println!("⏳ Manteniendo posición/Sin acción ejecutada ({}).", gemma_action);
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
                                    if let Err(e) = run_live_gemma_step(db_path, timeframe, client, &api_key, &api_secret, leverage, use_testnet, 70).await {
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
