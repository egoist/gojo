use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub user: String,
    pub region: String,
    pub sku: String,
    pub qty: i64,
    pub cents: i64,
    pub ok: bool,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionRow {
    pub k: String,
    pub n: i64,
    pub cents: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub matched: i64,
    pub cents: i64,
    pub regions: Vec<RegionRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub lines: i64,
    pub needle: i64,
    pub errors: i64,
    pub han: i64,
    pub s2: i64,
    pub s4: i64,
    pub s5: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: i64,
    pub sku: String,
    pub region: String,
    pub cents: i64,
    pub tag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub region: String,
    pub min_cents: i64,
    pub tag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryOut {
    pub n: i64,
    pub ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsOut {
    pub n: i64,
    pub cents: i64,
}

pub const NEEDLE: &str = "GET /api/orders/";
pub const HAN: &str = "订单";
