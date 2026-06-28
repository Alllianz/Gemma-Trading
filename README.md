# 📊 Gemma Trading Bot

<div align="center">
  <img src="https://img.shields.io/badge/Rust-100%25-orange?style=for-the-badge&logo=rust" alt="Rust 100%">
  <img src="https://img.shields.io/badge/LLM-Gemma%202-blue?style=for-the-badge&logo=google" alt="Gemma 2">
  <img src="https://img.shields.io/badge/Exchange-BingX-cyan?style=for-the-badge" alt="BingX">
  <img src="https://img.shields.io/badge/Database-SQLite-blueviolet?style=for-the-badge&logo=sqlite" alt="SQLite">
  <img src="https://img.shields.io/badge/LM_Studio-Local_AI-green?style=for-the-badge" alt="LM Studio">
</div>

---

**Gemma Trading Bot** es un sistema de trading algorítmico de alto rendimiento desarrollado **100% en Rust**. Utiliza el modelo de lenguaje **Google Gemma** ejecutándose localmente en **LM Studio** como cerebro de toma de decisiones financieras para operar futuros de **BTC-USDT** en el exchange **BingX**, analizando patrones técnicos de velas japonesas, indicadores (EMA, RSI, MACD, ATR, Bollinger Bands) y el estado de cuenta en tiempo real.

El sistema implementa una estrategia de **doble caja** (LT 80% + ST 20%) con gestión de riesgo autónoma, soporte de backtesting histórico completo con reportes visuales interactivos, y operaciones en vivo totalmente automatizadas.

---

## 🚀 Características Clave

| Característica | Detalle |
|---|---|
| **100% Rust** | Rendimiento óptimo, seguridad de memoria garantizada con `Tokio` async |
| **LLM Local** | Integración con LM Studio (Gemma) sin costos de API externos |
| **Doble Caja** | Estrategia LT (80%) + ST (20%) con posiciones simultáneas |
| **Base de Datos** | SQLite integrada con descarga incremental desde Binance |
| **Backtesting** | Simulación con comisiones (0.05%), apalancamiento ajustable y liquidación real |
| **Dashboard Web** | Reporte visual interactivo con Chart.js generado automáticamente |
| **Trading en Vivo** | API de BingX con soporte Real y Testnet (VST Demo) |
| **Multi-Timeframe** | Soporte para velas de 1H, 4H y 1D |

---

## 🧠 Arquitectura del Sistema

```
┌─────────────────────────────────────────────────────────┐
│                    Gemma Trading Bot                     │
├─────────────────────────────────────────────────────────┤
│  src/main.rs       → Menú principal y orquestación      │
│  src/live.rs       → Motor de trading en vivo           │
│  src/backtest.rs   → Motor de backtesting histórico     │
│  src/llm.rs        → Cliente HTTP para LM Studio        │
│  src/bingx.rs      → Integración API BingX              │
│  src/db.rs         → Base de datos SQLite               │
│  src/indicators.rs → EMA, RSI, MACD, ATR, BB           │
│  src/dashboard.rs  → Generador de reporte HTML          │
│  src/types.rs      → Tipos y estructuras compartidas    │
├─────────────────────────────────────────────────────────┤
│  LM Studio (Puerto 5508) ←→ Gemma 2 (Local)            │
│  BingX API           ←→ Futuros BTC-USDT                │
│  Binance API         ←→ Descarga de velas históricas    │
└─────────────────────────────────────────────────────────┘
```

---

## 🛠️ Requisitos Previos

### 1. Rust (2024 Edition o superior)
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 2. LM Studio
- Descargar desde [lmstudio.ai](https://lmstudio.ai)
- Cargar un modelo de la familia **Gemma 4** (recomendado: `gemma-4-26b-A4B-QAT`)
- El servidor local de inferencia debe estar **activo** en el puerto `5508`

### 3. Credenciales BingX
- API Key y API Secret Key desde el panel de [BingX](https://bingx.com)
- Pueden ser credenciales reales o VST Demo (Testnet)

---

## ⚙️ Configuración de LM Studio

> [!IMPORTANT]
> Esta configuración es **crítica** para que el bot funcione correctamente. El modelo debe responder únicamente con JSON sin razonamiento previo.

### Paso 1: Cargar el modelo Gemma

En LM Studio, descarga y carga cualquier modelo de la familia **Gemma 2** (recomendado `gemma-2-9b-it-GGUF` o superior).

### Paso 2: Configurar parámetros del modelo

Antes de iniciar el servidor local, ajusta los siguientes parámetros en LM Studio:

| Parámetro | Valor | Razón |
|---|---|---|
| **Temperature** | `0.1` | Respuestas deterministas y consistentes |
| **Limit Response Length** | `2048` | Suficiente para el JSON de respuesta |
| **Repeat Penalty** | `1` | Sin penalización por repetición |
| **Presence Penalty** | `0.5` | Leve diversidad sin creatividad excesiva |

### Paso 3: Copiar y pegar el System Prompt

> [!IMPORTANT]
> Debes copiar el siguiente system prompt y **pegarlo en el campo "System Prompt"** de LM Studio antes de iniciar el servidor local.

```
CRITICAL: DO NOT use any <think> tags. You are strictly FORBIDDEN from reasoning, explaining, or writing thoughts. You must immediately output raw JSON. Your response MUST start with the character '{' and end with '}'.

INSTRUCTIONS:

Strategy & Capital Allocation (Base Leverage: 10X):
- Two boxes: 80 percent Long-Term (LT) and 20 percent Short-Term (ST) of the total account equity. This proportion represents the max margin limit of the boxes, not the volume/size.
- Box Independence: The LT and ST boxes are independent trading modules. You can, and should, hold positions in BOTH boxes simultaneously if conditions allow. Do not wait for one box to close or be empty before trading in the other.
- Leverage: Select between 5.0 and 10.0 for any position (include "apalancamiento": X in the box JSON).
- Add position per Box: You are authorized to open your first position in any box freely. You are authorized to open an ADDITIONAL/SECOND position in the same box ONLY if the existing position in that box has a profit of >= 200 percent ROI (measured relative to its initial MARGIN). Additional positions in a box will always have the exact same size/margin as the first position.

Trend Priority:
- Long-Term (LT) Box: Trade ONLY in the direction of the long-term trend (EMA50 and EMA100).
- Short-Term (ST) Box: Authorized to trade against the macro trend based on short-term fluctuations.

Position Actions & Stop Loss Rules per Box:
- To open a new trade: set "accion" to "LONG" or "SHORT" and "cerrar" to false.
- To maintain an active trade without changes: set "accion" to "HOLD" and "cerrar" to false.
- To close an active trade completely: set "accion" to "FLAT" and "cerrar" to true.
- If a box has no active position and you do not want to open one: set "accion" to "HOLD", "cerrar" to false, and "stop_loss" to null.
- Stop Loss (SL) Rules:
  * LT Box (Long-Term): Set a wider stop loss below/above EMA100, or use EMA50 as a trailing stop to protect long-term trends.
  * ST Box (Short-Term): Set a very tight stop loss. You MUST set it at a maximum of 2% distance from the entry price (using EMA15 or similar). Never set a deep/wide stop loss for ST.
- Trailing Stop: ONLY when you have guaranteed profit (position is strictly in profit compared to the entry price), set the "stop_loss" as a Trailing Stop and update it dynamically to the current EMA50 or EMA100 to lock in profits. Do not start trailing or moving the Stop Loss if the position is not in profit.

CRITICAL EXECUTION RULES:
1. DO NOT use any <think> tags. Do not think, do not reason, do not explain, and do not write any prose.
2. Go directly from the market data to the raw JSON output.
3. Output: Respond ONLY with a raw JSON matching the structure below. No markdown (```json), no extra fields.

Example:
{
  "lt_box": {
    "accion": "HOLD",
    "cerrar": false,
    "apalancamiento": 5.0,
    "stop_loss": null
  },
  "st_box": {
    "accion": "HOLD",
    "cerrar": false,
    "apalancamiento": 5.0,
    "stop_loss": null
  }
}
```

> [!NOTE]
> El bot también envía el system prompt dinámicamente en cada llamada a la API con el apalancamiento configurado inyectado automáticamente, por lo que este prompt en LM Studio actúa como contexto base de refuerzo.

### Paso 4: Iniciar el servidor local

En LM Studio presiona **"Start Server"** y verifica que esté escuchando en `http://localhost:5508`.

---

## 📂 Estructura del Proyecto

```
Gemma-Trading/
├── src/
│   ├── main.rs          # Menú principal y orquestación general
│   ├── live.rs          # Motor de trading en vivo con Gemma
│   ├── backtest.rs      # Motor de backtesting histórico completo
│   ├── llm.rs           # Cliente HTTP hacia LM Studio
│   ├── bingx.rs         # API de BingX (órdenes, saldo, posiciones)
│   ├── db.rs            # Base de datos SQLite (velas, config API)
│   ├── indicators.rs    # Indicadores técnicos (EMA, RSI, MACD, ATR, BB)
│   ├── dashboard.rs     # Generador de reporte HTML interactivo
│   └── types.rs         # Tipos y estructuras compartidas
├── Cargo.toml           # Dependencias del proyecto
├── btcusdt.db           # Base de datos local (ignorado en Git)
├── dashboard.html       # Último reporte de backtest (ignorado en Git)
├── equity_curve.csv     # Curva de equidad del backtest (ignorado en Git)
└── .gitignore           # Protección de archivos sensibles
```

---

## 🔧 Instalación y Compilación

### 1. Clonar el repositorio
```bash
git clone https://github.com/Alllianz/Gemma-Trading.git
cd Gemma-Trading
```

### 2. Configurar el Token de LM Studio (opcional)
Si tu instancia de LM Studio requiere un token de autenticación, configúralo desde el menú del bot `Configurar Modelo Local (Gemma)`. Si no requiere token, se usa `"lm-studio"` por defecto.

### 3. Compilar el proyecto
```bash
cargo build --release
```

---

## 🕹️ Modos de Uso

Ejecuta el bot desde la consola:
```bash
cargo run --release
```

### Menú Principal
```
                      Gemma Trading Bot
1. Update DB
2. Backtest Completo
3. Backtest Completo (Verbose)
4. Backtest Completo (Gemma decide apalancamiento y capital)
5. Configurar Modelo Local (Gemma)
6. Trading en Vivo
7. Salir
```

| Opción | Función |
|---|---|
| **1. Update DB** | Descarga o actualiza velas (1H/4H/1D) de BTC-USDT desde Binance de forma incremental |
| **2. Backtest Completo** | Simula la estrategia con apalancamiento y capital fijo; genera `equity_curve.csv` y `dashboard.html` |
| **3. Backtest Verbose** | Igual que el anterior pero muestra en consola cada decisión de Gemma |
| **4. Backtest Dinámico** | Gemma decide autónomamente el apalancamiento y el capital por operación |
| **5. Configurar Gemma** | Configura la URL de API local y el token de LM Studio |
| **6. Trading en Vivo** | Ingresa al módulo de trading autónomo con BingX |
| **7. Salir** | Cierra la aplicación de forma segura |

---

### Menú de Trading en Vivo (BingX)
```
                  Gemma Trading en Vivo

1. Configurar API y Apalancamiento
2. Eliminar credenciales de DB
3. Test de API y Saldo
4. Prueba de Ordenes (Manual)
5. Trading en Vivo con Gemma (Automatizado)
6. Volver al menú principal
```

| Opción | Función |
|---|---|
| **1. Configurar API** | Registra API Key y Secret de BingX. Permite elegir apalancamiento y alternar Real/Testnet (VST) |
| **2. Eliminar credenciales** | Borra completamente las claves de la base de datos local |
| **3. Test de API y Saldo** | Valida la conexión y muestra el saldo neto de la cuenta de futuros |
| **4. Prueba de Órdenes** | Abre/cierra posiciones LONG/SHORT manualmente para verificar la API |
| **5. Trading Automatizado** | Inicia el agente autónomo: descarga velas al cierre de cada hora y ejecuta órdenes según Gemma |
| **6. Volver** | Regresa al menú principal |

---

## 📈 Estrategia de Trading — Doble Caja

El sistema implementa una estrategia de **dos cajas independientes** que operan simultáneamente:

```
┌─────────────────────────────────────────────┐
│              Capital Total (100%)            │
├──────────────────────┬──────────────────────┤
│   LT Box — 80%       │   ST Box — 20%        │
│   Long-Term Trend    │   Short-Term Trade    │
│   Solo sigue EMA50 + │   Puede ir contra     │
│   EMA100 (macro)     │   el macro trend      │
│   SL amplio          │   SL máx 2% distancia │
└──────────────────────┴──────────────────────┘
```

**Reglas clave:**
- Ambas cajas pueden tener posiciones **abiertas al mismo tiempo**
- Se puede agregar una **segunda posición** por caja solo si la primera tiene **≥ 200% ROI** sobre el margen inicial
- El apalancamiento lo elige Gemma entre **5x y 10x** según las condiciones del mercado
- Stop Loss dinámico basado en EMAs (EMA100 para LT, EMA15 para ST)

---

## 📊 Indicadores Técnicos Disponibles

El bot calcula y envía a Gemma los siguientes indicadores por cada vela:

| Indicador | Períodos |
|---|---|
| EMA (Media Móvil Exponencial) | 9, 15, 21, 50, 100, 200 |
| RSI (Relative Strength Index) | 14 |
| MACD | 12/26/9 |
| ATR (Average True Range) | 14 |
| Bollinger Bands | 20 períodos, 2 desviaciones |

---

## 📋 Parámetros enviados a LM Studio en cada llamada

Estos son los parámetros que el bot envía automáticamente en cada request HTTP a LM Studio:

```json
{
  "model": "local-model",
  "temperature": 0.1,
  "max_tokens": 1000,
  "frequency_penalty": 0.5,
  "presence_penalty": 0.5,
  "thinking_budget": 150,
  "stop": ["<think>", "\n*"]
}
```

> [!NOTE]
> El parámetro `stop` con `"<think>"` evita activamente que el modelo entre en modo de razonamiento extendido, forzando una respuesta JSON directa y eficiente.

---

## 🛡️ Seguridad y Buenas Prácticas

> [!WARNING]
> **NUNCA compartas tu base de datos `btcusdt.db`**, ya que contiene tus claves API de BingX y la configuración de LM Studio almacenadas localmente. El archivo `.gitignore` ya está configurado para ignorar automáticamente archivos `.db`, `.txt`, `.csv` y los ejecutables compilados de Rust.

> [!CAUTION]
> **Riesgo financiero real**: El modo de Trading en Vivo con la red **Real** (no Testnet) ejecuta órdenes reales en BingX. Usa siempre el **Testnet (VST Demo)** para probar antes de operar con capital real. El bot no garantiza rentabilidad.

---

## 🗃️ Base de Datos (SQLite)

El archivo `btcusdt.db` almacena:

- **Velas históricas** de BTC-USDT (1H, 4H, 1D) descargadas desde Binance
- **Configuración de API BingX** (key, secret, apalancamiento, red)
- **Configuración de LM Studio** (URL del servidor, token de autenticación)

---

## 📄 Licencia

Este proyecto está disponible bajo los términos de la licencia **MIT**.

---

<div align="center">
  <sub>Desarrollado con ❤️ en Rust · Powered by Google Gemma · Trading en BingX</sub>
</div>
