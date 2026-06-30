mod types;
mod indicators;
mod db;
mod llm;
mod dashboard;
mod bingx;
mod backtest;
mod live;

use std::io::{self, Write};
use db::{init_api_db, get_llm_config, save_llm_config, download_candles, clear_candles};
use backtest::run_backtest;
use live::trading_en_vivo_menu;

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
        println!("3. Backtest Completo (Verbose)");
        println!("4. Backtest Completo (Gemma decide apalancamiento y capital)");
        println!("5. Configurar Modelo Local (Gemma)");
        println!("6. Trading en Vivo");
        println!("7. Test de Determinismo (5 Backtests consecutivos)");
        println!("8. Salir");
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

                print!("¿Desea hacer una descarga limpia desde 2017 borrando los datos previos en la base de datos para estas temporalidades? (s/N): ");
                let _ = io::stdout().flush();
                let mut clean_input = String::new();
                let clean_download = if io::stdin().read_line(&mut clean_input).is_ok() {
                    let trim_input = clean_input.trim().to_lowercase();
                    trim_input == "s" || trim_input == "si" || trim_input == "sí"
                } else {
                    false
                };

                for tf in timeframes {
                    if clean_download {
                        println!("🧹 Limpiando base de datos para la temporalidad {}...", tf);
                        if let Err(e) = clear_candles(db_path, tf) {
                            println!("❌ Error al limpiar base de datos: {}", e);
                        }
                    }
                    println!("🔄 Descargando/Actualizando velas de {}...", tf);
                    if let Err(e) = download_candles(db_path, tf).await {
                        println!("❌ Error al descargar velas de {}: {}", tf, e);
                    }
                }
            }
            "2" | "3" | "4" => {
                let verbose = choice == "3";
                let dynamic_risk_leverage = choice == "4";
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

                let mut leverage = 10.0;
                let mut risk_percent = 100.0;

                if !dynamic_risk_leverage {
                    print!("Introduce el apalancamiento a usar (ej. 10): ");
                    let _ = io::stdout().flush();
                    let mut lev_input = String::new();
                    if io::stdin().read_line(&mut lev_input).is_ok() {
                        if let Ok(num) = lev_input.trim().parse::<f64>() {
                            if num > 0.0 {
                                leverage = num;
                            }
                        }
                    }

                    print!("Introduce el porcentaje de capital a arriesgar por operación (ej. 100 para 100% de la caja): ");
                    let _ = io::stdout().flush();
                    let mut risk_input = String::new();
                    if io::stdin().read_line(&mut risk_input).is_ok() {
                        if let Ok(num) = risk_input.trim().parse::<f64>() {
                            if num > 0.0 && num <= 100.0 {
                                risk_percent = num;
                            }
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

                print!("Introduce la fecha de inicio del trading para el backtest (AAAA-MM-DD) [Enter para usar 2020-04-20]: ");
                let _ = io::stdout().flush();
                let mut start_date_input = String::new();
                let mut trading_start_date = Some("2020-04-20".to_string());
                if io::stdin().read_line(&mut start_date_input).is_ok() {
                    let val = start_date_input.trim().to_string();
                    if !val.is_empty() {
                        trading_start_date = Some(val);
                    }
                }

                let conf_threshold = if dynamic_risk_leverage { 0 } else { 60 };
                if let Err(e) = run_backtest(
                    db_path,
                    timeframe,
                    leverage,
                    risk_percent,
                    limit,
                    conf_threshold,
                    verbose,
                    dynamic_risk_leverage,
                    trading_start_date,
                ).await {
                    println!("❌ Error en el backtest: {}", e);
                }
            }
            "5" => {
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
            "6" => {
                if let Err(e) = trading_en_vivo_menu(db_path, &client).await {
                    println!("❌ Error en el menú de trading en vivo: {}", e);
                }
            }
            "7" => {
                println!("\n[Test de Determinismo - 5 Backtests Consecutivos]");
                println!("Selecciona la temporalidad para el test:");
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

                let mut leverage = 10.0;
                print!("Introduce el apalancamiento a usar (ej. 10): ");
                let _ = io::stdout().flush();
                let mut lev_input = String::new();
                if io::stdin().read_line(&mut lev_input).is_ok() {
                    if let Ok(num) = lev_input.trim().parse::<f64>() {
                        if num > 0.0 {
                            leverage = num;
                        }
                    }
                }

                let mut risk_percent = 100.0;
                print!("Introduce el porcentaje de capital a arriesgar (ej. 100): ");
                let _ = io::stdout().flush();
                let mut risk_input = String::new();
                if io::stdin().read_line(&mut risk_input).is_ok() {
                    if let Ok(num) = risk_input.trim().parse::<f64>() {
                        if num > 0.0 && num <= 100.0 {
                            risk_percent = num;
                        }
                    }
                }

                print!("Introduce la cantidad de velas para el test (0 para evaluar todas) [Enter para usar 50]: ");
                let _ = io::stdout().flush();
                let mut limit_input = String::new();
                let mut limit = Some(50);
                if io::stdin().read_line(&mut limit_input).is_ok() {
                    let val = limit_input.trim();
                    if !val.is_empty() {
                        if let Ok(num) = val.parse::<usize>() {
                            if num > 0 {
                                limit = Some(num);
                            } else {
                                limit = None;
                            }
                        }
                    }
                }

                print!("Introduce la fecha de inicio del trading (AAAA-MM-DD) [Enter para usar 2020-04-20]: ");
                let _ = io::stdout().flush();
                let mut start_date_input = String::new();
                let mut trading_start_date = Some("2020-04-20".to_string());
                if io::stdin().read_line(&mut start_date_input).is_ok() {
                    let val = start_date_input.trim().to_string();
                    if !val.is_empty() {
                        trading_start_date = Some(val);
                    }
                }

                println!("\n🚀 Iniciando prueba de determinismo (5 ejecuciones consecutivas)...");
                let mut summaries = Vec::new();
                let mut error_ocurrido = false;

                for run_idx in 1..=5 {
                    println!("\n--------------------------------------------------");
                    println!("▶️ Ejecutando Backtest {} de 5...", run_idx);
                    println!("--------------------------------------------------");
                    match run_backtest(
                        db_path,
                        timeframe,
                        leverage,
                        risk_percent,
                        limit,
                        60,
                        false, // Verbose en false para no saturar consola
                        false, // Dynamic risk en false
                        trading_start_date.clone(),
                    ).await {
                        Ok(summary) => {
                            summaries.push(summary);
                        }
                        Err(e) => {
                            println!("❌ Error en la ejecución {}: {}", run_idx, e);
                            error_ocurrido = true;
                            break;
                        }
                    }
                }

                if !error_ocurrido && summaries.len() == 5 {
                    println!("\n==================================================");
                    println!("📊 RESULTADOS DE LA COMPARACIÓN DE DETERMINISMO");
                    println!("==================================================");
                    println!("{:<5} | {:<15} | {:<8} | {:<10} | {:<12} | {:<15}", "Corr", "Equity Final", "Winrate", "Profit Fac", "Total Trades", "Acciones");
                    println!("----------------------------------------------------------------------------------");
                    
                    for (i, s) in summaries.iter().enumerate() {
                        let hash_part = if s.actions_sequence.is_empty() {
                            "N/A".to_string()
                        } else {
                            let truncated = if s.actions_sequence.len() > 15 {
                                format!("{}...", &s.actions_sequence[0..15])
                            } else {
                                s.actions_sequence.clone()
                            };
                            truncated.replace("\n", " ").replace("|", ", ")
                        };
                        println!(
                            "{:<5} | {:<15.2} | {:<7.2}% | {:<10.2} | {:<12} | {}",
                            i + 1, s.final_equity, s.winrate, s.profit_factor, s.total_trades, hash_part
                        );
                    }
                    println!("----------------------------------------------------------------------------------");

                    let first = &summaries[0];
                    let mut es_determinista = true;
                    for (idx, other) in summaries.iter().enumerate().skip(1) {
                        let diff_equity = (first.final_equity - other.final_equity).abs();
                        let diff_actions = first.actions_sequence != other.actions_sequence;
                        
                        if diff_equity > 0.001 || diff_actions {
                            println!("⚠️ Discrepancia detectada en la corrida {} comparada con la 1:", idx + 1);
                            if diff_equity > 0.001 {
                                println!("   - Diferencia en Equity Final: Corr 1 ({:.4}) vs Corr {} ({:.4})", first.final_equity, idx + 1, other.final_equity);
                            }
                            if diff_actions {
                                println!("   - Diferencia en la secuencia de acciones/decisiones tomadas.");
                            }
                            es_determinista = false;
                        }
                    }

                    if es_determinista {
                        println!("\n✅ ¡TEST EXITOSO! El modelo es 100% DETERMINISTA en las 5 ejecuciones.");
                        println!("Todas las métricas de retorno, la curva de equidad y las acciones del bot fueron idénticas.");
                    } else {
                        println!("\n❌ TEST FALLIDO. Se detectó no-determinismo entre las ejecuciones.");
                        println!("Revisa si el servidor de LM Studio tiene otros samplers activos o si hay hilos paralelos interfiriendo.");
                    }
                    println!("==================================================");
                } else if error_ocurrido {
                    println!("\n❌ La prueba de determinismo se canceló debido a un error en una ejecución.");
                }
            }
            "8" => {
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
