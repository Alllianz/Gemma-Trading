use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PositionType {
    None,
    Long,
    Short,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum BoxType {
    LT,
    ST,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub position_type: PositionType,
    pub margin: f64,
    pub size_btc: f64,
    pub entry_price: f64,
    pub liquidation_price: f64,
    pub stop_loss: Option<f64>,
    pub box_type: BoxType,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Candle {
    pub open_time: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub close_time: i64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct BoxAction {
    pub accion: String, // "LONG", "SHORT", "FLAT"
    #[serde(default)]
    pub cerrar: bool,
    pub apalancamiento: Option<f64>,
    pub stop_loss: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GemmaResponse {
    #[serde(default)]
    pub analisis: Option<String>,
    pub lt_box: BoxAction,
    pub st_box: BoxAction,
}

#[derive(Debug, Clone)]
pub struct BingXAccountInfo {
    pub wallet_balance: f64,
    pub available_margin: f64,
    pub user_id: String,
}
