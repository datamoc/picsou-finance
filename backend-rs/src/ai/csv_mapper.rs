//! AI-assisted CSV column mapping with deterministic heuristic fallback.

use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::AppState;
use crate::auth::Auth;
use crate::error::AppResult;
use crate::http::json::{Errors, Json, Validate};
use crate::routes::Routes;

use super::{DynamicCategorizer, is_configured, provider};

pub fn register(routes: &mut Routes) {
    routes.post("/api/ai/csv/suggest-mapping", suggest_csv_mapping);
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CsvSuggestRequest {
    mode: String,
    headers: Vec<String>,
    sample_rows: Option<Vec<Vec<String>>>,
}

impl Validate for CsvSuggestRequest {
    fn validate(&self, errors: &mut Errors) {
        if self.headers.is_empty() {
            errors.add("headers", "At least one column header is required");
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CsvSuggestResponse {
    source: &'static str,
    suggested_mapping: Map<String, Value>,
}

/// Deterministic multilingual header matcher used both as a zero-latency fallback when AI is not
/// configured and as a safety net when the model outputs invalid JSON.
pub fn heuristic_mapping(mode: &str, headers: &[String]) -> Map<String, Value> {
    let mut out = Map::new();
    let lower: Vec<String> = headers
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect();

    let find = |needles: &[&str]| -> Option<usize> {
        lower
            .iter()
            .position(|h| needles.iter().any(|n| h.contains(n)))
    };

    if let Some(i) = find(&["date", "datum", "fecha", "jour", "opération", "operation"]) {
        out.insert("date".into(), Value::from(i));
    }
    if let Some(i) = find(&["currency", "devise", "monnaie", "währung", "moneda", "ccy"]) {
        out.insert("currency".into(), Value::from(i));
    }

    if mode.eq_ignore_ascii_case("TRADES") {
        if let Some(i) = find(&["side", "sens", "type", "action", "operation", "achat", "vente"]) {
            out.insert("side".into(), Value::from(i));
        }
        if let Some(i) = find(&["isin", "ticker", "symbol", "symbole", "code", "valeur"]) {
            out.insert("tickerOrIsin".into(), Value::from(i));
        }
        if let Some(i) = find(&["name", "libellé", "libelle", "nom", "titre", "instrument", "security"]) {
            out.insert("name".into(), Value::from(i));
        }
        if let Some(i) = find(&["quantity", "quantité", "quantite", "qty", "shares", "parts", "nombre"]) {
            out.insert("quantity".into(), Value::from(i));
        }
        if let Some(i) = find(&["unit price", "price", "cours", "prix", "vl", "nav"]) {
            out.insert("unitPrice".into(), Value::from(i));
        }
        if let Some(i) = find(&["fee", "frais", "commission", "courtage", "tax"]) {
            out.insert("fees".into(), Value::from(i));
        }
        if let Some(i) = find(&["amount", "montant", "total", "net", "value", "valeur"]) {
            out.insert("amount".into(), Value::from(i));
        }
    } else {
        if let Some(i) = find(&["description", "libellé", "libelle", "label", "merchant", "motif", "detail"]) {
            out.insert("description".into(), Value::from(i));
        }
        if let Some(i) = find(&["debit", "débit", "sortie", "outflow"]) {
            out.insert("debit".into(), Value::from(i));
        }
        if let Some(i) = find(&["credit", "crédit", "entrée", "entree", "inflow"]) {
            out.insert("credit".into(), Value::from(i));
        }
        if !out.contains_key("debit") && !out.contains_key("credit")
            && let Some(i) = find(&["amount", "montant", "betrag", "importe", "solde", "valeur"])
        {
            out.insert("amount".into(), Value::from(i));
        }
    }
    out
}

fn parse_ai_mapping_json(raw: &str, max_col: usize) -> Option<Map<String, Value>> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    let slice = raw.get(start..=end)?;
    let parsed: Value = serde_json::from_str(slice).ok()?;
    let obj = parsed.as_object()?;
    let allowed = [
        "date",
        "side",
        "tickerOrIsin",
        "name",
        "quantity",
        "unitPrice",
        "fees",
        "currency",
        "amount",
        "description",
        "debit",
        "credit",
    ];
    let mut clean = Map::new();
    for key in allowed {
        if let Some(v) = obj.get(key).and_then(Value::as_u64)
            && (v as usize) < max_col
        {
            clean.insert(key.to_owned(), Value::from(v));
        }
    }
    (!clean.is_empty()).then_some(clean)
}

async fn suggest_csv_mapping(
    State(state): State<AppState>,
    _auth: Auth,
    Json(req): Json<CsvSuggestRequest>,
) -> AppResult<axum::Json<CsvSuggestResponse>> {
    let fallback = heuristic_mapping(&req.mode, &req.headers);

    let categorizer = DynamicCategorizer::new(
        state.db.clone(),
        state.crypto.clone(),
        state.http.clone(),
    );

    if let Ok(Some(cfg)) = categorizer.current_config().await
        && is_configured(&cfg)
    {
        let system = "Map CSV column indexes (0-based integers) to target fields. Return ONLY a flat JSON object mapping field names to integer column indexes or null.";
        let fields = if req.mode.eq_ignore_ascii_case("TRADES") {
            "date, side, tickerOrIsin, name, quantity, unitPrice, fees, currency, amount"
        } else {
            "date, description, amount, debit, credit, currency"
        };
        let prompt = format!(
            "Mode: {}\nTarget fields: {}\nHeaders (0-based): {:?}\nSample rows: {:?}",
            req.mode,
            fields,
            req.headers,
            req.sample_rows.as_deref().unwrap_or(&[])
        );
        let http = provider::build_http_client();
        if let Ok(resp) = provider::call_provider(&http, &cfg, system, &prompt, provider::DEFAULT_TIMEOUT).await
            && let Some(text) = resp.text
            && let Some(ai_map) = parse_ai_mapping_json(&text, req.headers.len())
        {
            return Ok(axum::Json(CsvSuggestResponse {
                source: "ai",
                suggested_mapping: ai_map,
            }));
        }
    }

    Ok(axum::Json(CsvSuggestResponse {
        source: "heuristic",
        suggested_mapping: fallback,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_maps_french_broker_and_bank_headers() {
        let trade_headers = vec![
            "Date opération".into(),
            "Sens".into(),
            "ISIN".into(),
            "Libellé".into(),
            "Quantité".into(),
            "Cours".into(),
            "Frais".into(),
            "Devise".into(),
        ];
        let m = heuristic_mapping("TRADES", &trade_headers);
        assert_eq!(m.get("date").and_then(Value::as_u64), Some(0));
        assert_eq!(m.get("tickerOrIsin").and_then(Value::as_u64), Some(2));
        assert_eq!(m.get("quantity").and_then(Value::as_u64), Some(4));
        assert_eq!(m.get("fees").and_then(Value::as_u64), Some(6));

        let cash_headers = vec!["Date".into(), "Libellé".into(), "Débit".into(), "Crédit".into()];
        let c = heuristic_mapping("CASH", &cash_headers);
        assert_eq!(c.get("debit").and_then(Value::as_u64), Some(2));
        assert_eq!(c.get("credit").and_then(Value::as_u64), Some(3));
    }

    #[test]
    fn parses_llm_json_and_rejects_out_of_bounds_columns() {
        let raw = "Here is the mapping:\n```json\n{\"date\": 0, \"amount\": 2, \"debit\": 99}\n```";
        let parsed = parse_ai_mapping_json(raw, 4).unwrap();
        assert_eq!(parsed.get("date").and_then(Value::as_u64), Some(0));
        assert_eq!(parsed.get("amount").and_then(Value::as_u64), Some(2));
        assert!(!parsed.contains_key("debit"));
    }
}
