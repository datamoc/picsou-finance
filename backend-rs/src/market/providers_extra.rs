//! Secondary market-data quote providers (Börse Frankfurt, EODHD, Marketstack) orchestrated behind
//! `ProviderCircuitBreaker` so Picsou can fall back gracefully when Yahoo Finance fails or misses
//! a European ISIN/symbol.

use std::sync::Arc;
use std::time::Duration;

use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use serde_json::Value;

use super::circuit_breaker::ProviderCircuitBreaker;

const TIMEOUT: Duration = Duration::from_secs(8);

pub struct ExtraQuoteProviders {
    http: reqwest::Client,
    breaker: Arc<ProviderCircuitBreaker>,
    eodhd_api_key: Option<String>,
    marketstack_api_key: Option<String>,
}

impl ExtraQuoteProviders {
    pub fn new(http: reqwest::Client, breaker: Arc<ProviderCircuitBreaker>) -> Self {
        let eodhd_api_key = std::env::var("EODHD_API_KEY")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let marketstack_api_key = std::env::var("MARKETSTACK_API_KEY")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        Self {
            http,
            breaker,
            eodhd_api_key,
            marketstack_api_key,
        }
    }

    /// Tries Börse Frankfurt (by ISIN, no API key), then EODHD (if configured), then Marketstack
    /// (if configured), respecting each provider's circuit breaker.
    pub async fn fetch_fallback_price_eur(&self, symbol_or_isin: &str) -> Option<Decimal> {
        let clean = symbol_or_isin.trim().to_uppercase();
        if clean.is_empty() {
            return None;
        }

        if is_isin(&clean)
            && self.breaker.allow_request("boerse_frankfurt")
        {
            match self.fetch_boerse_frankfurt(&clean).await {
                Some(price) => {
                    self.breaker.record_success("boerse_frankfurt");
                    return Some(price);
                }
                None => self.breaker.record_failure("boerse_frankfurt"),
            }
        }

        if let Some(key) = self.eodhd_api_key.as_deref()
            && self.breaker.allow_request("eodhd")
        {
            match self.fetch_eodhd(&clean, key).await {
                Some(price) => {
                    self.breaker.record_success("eodhd");
                    return Some(price);
                }
                None => self.breaker.record_failure("eodhd"),
            }
        }

        if let Some(key) = self.marketstack_api_key.as_deref()
            && self.breaker.allow_request("marketstack")
        {
            match self.fetch_marketstack(&clean, key).await {
                Some(price) => {
                    self.breaker.record_success("marketstack");
                    return Some(price);
                }
                None => self.breaker.record_failure("marketstack"),
            }
        }

        None
    }

    async fn fetch_boerse_frankfurt(&self, isin: &str) -> Option<Decimal> {
        let url = format!(
            "https://api.boerse-frankfurt.de/v1/data/price_information?isin={isin}&mic=XETR"
        );
        let resp = self.http.get(&url).timeout(TIMEOUT).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: Value = resp.json().await.ok()?;
        parse_boerse_frankfurt_price(&body)
    }

    async fn fetch_eodhd(&self, symbol: &str, api_key: &str) -> Option<Decimal> {
        let url = format!(
            "https://eodhd.com/api/real-time/{symbol}?api_token={api_key}&fmt=json"
        );
        let resp = self.http.get(&url).timeout(TIMEOUT).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: Value = resp.json().await.ok()?;
        parse_eodhd_price(&body)
    }

    async fn fetch_marketstack(&self, symbol: &str, api_key: &str) -> Option<Decimal> {
        let url = format!(
            "https://api.marketstack.com/v1/eod/latest?access_key={api_key}&symbols={symbol}"
        );
        let resp = self.http.get(&url).timeout(TIMEOUT).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: Value = resp.json().await.ok()?;
        parse_marketstack_price(&body)
    }
}

fn is_isin(s: &str) -> bool {
    s.len() == 12
        && s.as_bytes()[0..2].iter().all(|b| b.is_ascii_uppercase())
        && s.as_bytes()[2..].iter().all(|b| b.is_ascii_alphanumeric())
}

pub fn parse_boerse_frankfurt_price(body: &Value) -> Option<Decimal> {
    let raw = body
        .get("lastPrice")
        .or_else(|| body.get("close"))
        .and_then(Value::as_f64)?;
    (raw.is_finite() && raw > 0.0)
        .then(|| Decimal::from_f64(raw))
        .flatten()
}

pub fn parse_eodhd_price(body: &Value) -> Option<Decimal> {
    let raw = body
        .get("close")
        .or_else(|| body.get("previousClose"))
        .and_then(Value::as_f64)?;
    (raw.is_finite() && raw > 0.0)
        .then(|| Decimal::from_f64(raw))
        .flatten()
}

pub fn parse_marketstack_price(body: &Value) -> Option<Decimal> {
    let raw = body
        .get("data")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("close"))
        .and_then(Value::as_f64)?;
    (raw.is_finite() && raw > 0.0)
        .then(|| Decimal::from_f64(raw))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_all_three_provider_payloads() {
        assert_eq!(
            parse_boerse_frankfurt_price(&json!({ "lastPrice": 512.45 })),
            Some(Decimal::from_f64(512.45).unwrap())
        );
        assert_eq!(
            parse_eodhd_price(&json!({ "close": 123.4 })),
            Some(Decimal::from_f64(123.4).unwrap())
        );
        assert_eq!(
            parse_marketstack_price(&json!({ "data": [{ "close": 89.25 }] })),
            Some(Decimal::from_f64(89.25).unwrap())
        );
        assert!(is_isin("LU1681043599"));
        assert!(!is_isin("CW8.PA"));
    }
}
