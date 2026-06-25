mod types;
mod indicators;
mod db;
mod llm;
mod dashboard;
mod bingx;
mod backtest;
mod live;

use std::io::{self, Write};
use db::{init_api_db, get_llm_config, save_llm_config, download_candles};
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

                if let Err(e) = run_backtest(db_path, timeframe, leverage, risk_percent, limit, 70).await {
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

                if let Err(e) = run_backtest(db_path, timeframe, leverage, risk_percent, Some(10), 70).await {
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
