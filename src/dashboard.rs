use std::fs::File;
use std::io::Write;

pub fn save_equity_curve(curve: &[(String, f64, f64)], filename: &str) -> Result<(), std::io::Error> {
    let mut file = File::create(filename)?;
    writeln!(file, "time,equity,buy_and_hold")?;
    for (time_str, eq, bh) in curve {
        writeln!(file, "{},{},{}", time_str, eq, bh)?;
    }
    Ok(())
}

pub fn generate_dashboard(
    curve: &[(String, f64, f64)],
    num_compras: usize,
    num_ventas: usize,
    num_liquidaciones: usize,
    max_drawdown: f64,
    correlation: f64,
    winrate: f64,
    profit_factor: f64,
    sharpe_ratio: f64,
    recovery_factor: f64,
    avg_stagnation: f64,
    max_stagnation: usize,
    filename: &str,
) -> Result<(), std::io::Error> {
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

        <!-- Advanced Metrics Grid -->
        <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-5 gap-6">
            <div class="bg-slate-900/60 backdrop-blur-md border border-slate-800/80 rounded-2xl p-6 shadow-xl flex flex-col justify-between hover:border-slate-700/80 transition duration-300">
                <span class="text-slate-400 text-sm font-medium">Winrate</span>
                <span class="text-2xl font-bold text-violet-400 mt-2">{winrate:.2}%</span>
                <span class="text-xs text-slate-500 mt-1">Porcentaje de acierto</span>
            </div>

            <div class="bg-slate-900/60 backdrop-blur-md border border-slate-800/80 rounded-2xl p-6 shadow-xl flex flex-col justify-between hover:border-slate-700/80 transition duration-300">
                <span class="text-slate-400 text-sm font-medium">Profit Factor</span>
                <span class="text-2xl font-bold text-emerald-400 mt-2">{profit_factor:.2}</span>
                <span class="text-xs text-slate-500 mt-1">Ganancia Bruta / Pérdida Bruta</span>
            </div>

            <div class="bg-slate-900/60 backdrop-blur-md border border-slate-800/80 rounded-2xl p-6 shadow-xl flex flex-col justify-between hover:border-slate-700/80 transition duration-300">
                <span class="text-slate-400 text-sm font-medium">Sharpe Ratio</span>
                <span class="text-2xl font-bold text-sky-400 mt-2">{sharpe_ratio:.2}</span>
                <span class="text-xs text-slate-500 mt-1">Anualizado</span>
            </div>

            <div class="bg-slate-900/60 backdrop-blur-md border border-slate-800/80 rounded-2xl p-6 shadow-xl flex flex-col justify-between hover:border-slate-700/80 transition duration-300">
                <span class="text-slate-400 text-sm font-medium">Recovery Factor</span>
                <span class="text-2xl font-bold text-amber-500 mt-2">{recovery_factor:.2}</span>
                <span class="text-xs text-slate-500 mt-1">Net Profit / Max DD</span>
            </div>

            <div class="bg-slate-900/60 backdrop-blur-md border border-slate-800/80 rounded-2xl p-6 shadow-xl flex flex-col justify-between hover:border-slate-700/80 transition duration-300">
                <span class="text-slate-400 text-sm font-medium">Estancamiento</span>
                <span class="text-xl font-bold text-slate-200 mt-2">Max: {max_stagnation} v.</span>
                <span class="text-xs text-slate-400 mt-1">Promedio: {avg_stagnation:.1} v.</span>
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
        winrate = winrate,
        profit_factor = profit_factor,
        sharpe_ratio = sharpe_ratio,
        recovery_factor = recovery_factor,
        avg_stagnation = avg_stagnation,
        max_stagnation = max_stagnation,
        labels_json = labels_json,
        data_json = data_json,
        bh_data_json = bh_data_json
    );

    let mut file = File::create(filename)?;
    write!(file, "{}", html)?;
    Ok(())
}
