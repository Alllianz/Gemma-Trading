# 📊 Gemma Trading Bot (100% Rust)

<div align="center">
  <img src="https://img.shields.io/badge/Rust-100%25-orange?style=for-the-badge&logo=rust" alt="Rust 100%">
  <img src="https://img.shields.io/badge/LLM-Gemma%202-blue?style=for-the-badge&logo=google" alt="Gemma 2">
  <img src="https://img.shields.io/badge/Exchange-BingX-cyan?style=for-the-badge" alt="BingX">
  <img src="https://img.shields.io/badge/Database-SQLite-blueviolet?style=for-the-badge&logo=sqlite" alt="SQLite">
</div>

---

**Gemma Trading Bot** es un bot de trading algorítmico de alto rendimiento desarrollado en **Rust**. El sistema utiliza el modelo de lenguaje **Google Gemma** (ejecutándose localmente en LM Studio) como cerebro de toma de decisiones financieras para operar futuros de **BTC-USDT** en base al análisis de patrones de velas japonesas.

El bot soporta descargas históricas de Binance, simulaciones completas de backtesting de precisión con reportes interactivos, y ahora **Trading en Vivo y órdenes manuales utilizando la API de BingX**.

---

## 🚀 Características Clave

* **100% Rust**: Rendimiento óptimo, seguridad de memoria garantizada y concurrencia veloz mediante `Tokio`.
* **Cerebro LLM Local**: Integración directa con **LM Studio** para consultar a **Gemma** sobre las decisiones de mercado (`COMPRAR`, `VENDER`, `MANTENER`) garantizando privacidad y sin costos de API externos.
* **Base de Datos SQLite integrada**: Descarga y actualización incremental de velas históricas directamente desde la API oficial de Binance.
* **Backtesting de Alta Precisión**: Simulación completa del rendimiento histórico considerando comisiones de transacción (0.05%), apalancamiento ajustable (10X por defecto) y riesgo de liquidación real pico a valle.
* **Dashboard Visual Interactivo**: Genera de forma automática un dashboard web (`dashboard.html`) premium hecho con TailwindCSS y Chart.js con métricas de ROI, drawdown y la curva de equidad al completar un backtest.
* **Trading en Vivo mediante BingX**:
  * Configuración persistente de API Keys de BingX en base de datos.
  * Soporte de cuenta real y Testnet (VST Demo).
  * Ajuste automático de apalancamiento en el exchange.
  * Ejecución de órdenes de mercado y cierres automáticos bajo el modo Hedge (posiciones LONG/SHORT aisladas).
  * Módulo de pruebas manuales integrado directamente en consola.

---

## 🛠️ Requisitos Previos

1. **Rust**: Tener instalado Rust (edición 2024 o superior).
2. **LM Studio**: Tener instalado y ejecutándose LM Studio con un modelo de la familia **Gemma** cargado.
   * El puerto local de API debe estar configurado en el puerto `5508` (o modificarlo en `src/main.rs`).
   * Asegúrate de tener el servidor local de inferencia activo.
3. **Credenciales de BingX**:
   * API Key y API Secret Key de BingX (pueden ser reales o VST Demo).

---

## 📂 Estructura del Proyecto

* `src/main.rs`: Contiene toda la lógica principal del bot (bucle del menú, llamadas a LM Studio, descarga de Binance, backtesting y llamadas a la API de BingX).
* `Cargo.toml`: Gestión de dependencias (`reqwest`, `serde`, `tokio`, `rusqlite`, `hmac`, `sha2`, `hex`).
* `btcusdt.db` *(Ignorado en Git)*: Base de datos SQLite local para almacenar velas y credenciales de API.
* `token.txt` *(Ignorado en Git)*: Archivo local para almacenar tu API Token de LM Studio si es requerido.
* `dashboard.html` *(Ignorado en Git)*: Reporte gráfico interactivo de la última simulación.

---

## ⚙️ Configuración e Instalación

1. **Clonar el repositorio**:
   ```bash
   git clone https://github.com/Alllianz/Gemma-Trading.git
   cd Gemma-Trading
   ```

2. **Configurar el Token de LM Studio**:
   Crea un archivo llamado `token.txt` en la raíz del proyecto y pega tu token de LM Studio, o bien expórtalo como variable de entorno:
   ```bash
   export LM_API_TOKEN="tu_token_aqui"
   ```
   *Nota: Si tu instancia local de LM Studio no requiere token, se utilizará `"lm-studio"` por defecto.*

3. **Compilar el proyecto**:
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
3. Prueba de Backtest
4. Trading en Vivo
5. Salir
```

* **1. Update DB**: Descarga o actualiza las velas de 1 hora de BTC-USDT desde Binance de forma incremental.
* **2. Backtest Completo**: Simula la estrategia de Gemma a lo largo de las velas históricas. Al finalizar, genera `equity_curve.csv` y `dashboard.html`.
* **3. Prueba de Backtest**: Ejecuta un backtest rápido limitado a las primeras 10 velas de la DB.
* **4. Trading en Vivo**: Ingresa al módulo interactivo de trading real.
* **5. Salir**: Cierra la aplicación de forma segura.

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

1. **Configurar API y Apalancamiento**: Registra localmente tu API Key y Secret. Permite elegir apalancamiento personalizado y alternar entre red Real o Testnet (VST).
2. **Eliminar credenciales**: Limpia y borra por completo las claves de la base de datos local.
3. **Test de API y Saldo**: Valida la conexión contra BingX y muestra el saldo neto de tu cuenta de futuros (Disponible + Margen en posiciones).
4. **Prueba de Órdenes (Manual)**: Permite abrir o cerrar posiciones LONG/SHORT de forma manual para probar que la API responda correctamente y auditar el tiempo de ejecución.
5. **Trading en Vivo con Gemma (Automatizado)**: Inicia el agente autónomo. El bot descargará velas al cierre de cada hora y consultará a Gemma qué acción tomar, ejecutando las compras y ventas de forma autónoma en BingX. Presiona `ENTER` para salir de este bucle cuando lo desees.

---

## 🛡️ Seguridad y Buenas Prácticas

> [!WARNING]
> **NUNCA compartas tu base de datos `btcusdt.db` ni tu archivo `token.txt`**, ya que contienen información sensible sobre tus claves API.
> El archivo `.gitignore` de este repositorio ya está preconfigurado para ignorar automáticamente archivos `.db`, `.txt`, `.csv` y los ejecutables de Rust.

---

## 📄 Licencia

Este proyecto está disponible bajo los términos de la licencia MIT.
