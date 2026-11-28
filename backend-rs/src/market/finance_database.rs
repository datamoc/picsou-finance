//! Offline FinanceDatabase-inspired security catalog for instant local lookup of major European
//! PEA/UCITS ETFs, global funds, equities, and crypto assets by ISIN, ticker, or name without
//! needing an upstream HTTP round trip.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogInstrument {
    pub ticker: &'static str,
    pub isin: Option<&'static str>,
    pub name: &'static str,
    pub asset_type: &'static str,
    pub currency: &'static str,
    pub exchange: &'static str,
    pub category: &'static str,
    pub geography: &'static str,
}

pub const OFFLINE_CATALOG: &[CatalogInstrument] = &[
    // Major PEA & UCITS World / US / Europe ETFs
    CatalogInstrument { ticker: "CW8.PA", isin: Some("LU1681043599"), name: "Amundi MSCI World UCITS ETF EUR (C)", asset_type: "ETF", currency: "EUR", exchange: "Euronext Paris", category: "Global Equity", geography: "WORLD" },
    CatalogInstrument { ticker: "WPEA.PA", isin: Some("IE0002XZSHO1"), name: "iShares MSCI World Swap PEA UCITS ETF EUR (Acc)", asset_type: "ETF", currency: "EUR", exchange: "Euronext Paris", category: "Global Equity", geography: "WORLD" },
    CatalogInstrument { ticker: "EWLD.PA", isin: Some("LU2655993207"), name: "Amundi MSCI World II UCITS ETF Dist", asset_type: "ETF", currency: "EUR", exchange: "Euronext Paris", category: "Global Equity", geography: "WORLD" },
    CatalogInstrument { ticker: "IWDA.AS", isin: Some("IE00B4L5Y983"), name: "iShares Core MSCI World UCITS ETF USD (Acc)", asset_type: "ETF", currency: "EUR", exchange: "Euronext Amsterdam", category: "Global Equity", geography: "WORLD" },
    CatalogInstrument { ticker: "VWCE.DE", isin: Some("IE00BK5BQT80"), name: "Vanguard FTSE All-World UCITS ETF (Acc)", asset_type: "ETF", currency: "EUR", exchange: "XETRA", category: "Global Equity", geography: "WORLD" },
    CatalogInstrument { ticker: "ESE.PA", isin: Some("FR0011550185"), name: "BNP Paribas Easy S&P 500 UCITS ETF EUR (C)", asset_type: "ETF", currency: "EUR", exchange: "Euronext Paris", category: "US Equity", geography: "US" },
    CatalogInstrument { ticker: "PSP5.PA", isin: Some("FR0011871128"), name: "Amundi PEA S&P 500 UCITS ETF EUR (C)", asset_type: "ETF", currency: "EUR", exchange: "Euronext Paris", category: "US Equity", geography: "US" },
    CatalogInstrument { ticker: "CSPX.AS", isin: Some("IE00B5BMR087"), name: "iShares Core S&P 500 UCITS ETF (Acc)", asset_type: "ETF", currency: "EUR", exchange: "Euronext Amsterdam", category: "US Equity", geography: "US" },
    CatalogInstrument { ticker: "PUST.PA", isin: Some("FR0011871110"), name: "Amundi PEA Nasdaq-100 UCITS ETF EUR (C)", asset_type: "ETF", currency: "EUR", exchange: "Euronext Paris", category: "US Tech", geography: "US" },
    CatalogInstrument { ticker: "PAEEM.PA", isin: Some("FR0013412020"), name: "Amundi PEA MSCI Emerging Markets UCITS ETF", asset_type: "ETF", currency: "EUR", exchange: "Euronext Paris", category: "Emerging Markets", geography: "EMERGING" },
    CatalogInstrument { ticker: "ETZ.PA", isin: Some("FR0011550193"), name: "BNP Paribas Easy STOXX Europe 600 UCITS ETF", asset_type: "ETF", currency: "EUR", exchange: "Euronext Paris", category: "Europe Equity", geography: "EUROPE" },
    CatalogInstrument { ticker: "CAC.PA", isin: Some("FR0007052782"), name: "Amundi CAC 40 UCITS ETF Dist", asset_type: "ETF", currency: "EUR", exchange: "Euronext Paris", category: "France Equity", geography: "FR" },
    CatalogInstrument { ticker: "CL2.PA", isin: Some("FR0010755611"), name: "Amundi Leveraged MSCI USA Daily (2x) UCITS ETF", asset_type: "ETF", currency: "EUR", exchange: "Euronext Paris", category: "US Leveraged", geography: "US" },
    CatalogInstrument { ticker: "LQQ.PA", isin: Some("FR0010342592"), name: "Amundi Nasdaq-100 Daily (2x) Leveraged UCITS ETF", asset_type: "ETF", currency: "EUR", exchange: "Euronext Paris", category: "US Leveraged", geography: "US" },
    CatalogInstrument { ticker: "XEON.DE", isin: Some("LU0290358497"), name: "Xtrackers II EUR Overnight Rate Swap UCITS ETF", asset_type: "ETF", currency: "EUR", exchange: "XETRA", category: "Money Market", geography: "EUROPE" },
    CatalogInstrument { ticker: "EUNA.DE", isin: Some("IE00BDBRDM35"), name: "iShares Core Global Aggregate Bond UCITS ETF EUR Hedged", asset_type: "ETF", currency: "EUR", exchange: "XETRA", category: "Global Bonds", geography: "WORLD" },
    CatalogInstrument { ticker: "SGLD.MI", isin: Some("IE00B579F325"), name: "Invesco Physical Gold ETC", asset_type: "ETF", currency: "EUR", exchange: "Borsa Italiana", category: "Commodities", geography: "WORLD" },
    // Major French & European Blue Chips
    CatalogInstrument { ticker: "AI.PA", isin: Some("FR0000120073"), name: "Air Liquide", asset_type: "STOCK", currency: "EUR", exchange: "Euronext Paris", category: "Materials", geography: "FR" },
    CatalogInstrument { ticker: "MC.PA", isin: Some("FR0000121014"), name: "LVMH Moët Hennessy Louis Vuitton", asset_type: "STOCK", currency: "EUR", exchange: "Euronext Paris", category: "Consumer Discretionary", geography: "FR" },
    CatalogInstrument { ticker: "TTE.PA", isin: Some("FR0000120271"), name: "TotalEnergies", asset_type: "STOCK", currency: "EUR", exchange: "Euronext Paris", category: "Energy", geography: "FR" },
    CatalogInstrument { ticker: "ASML.AS", isin: Some("NL0010273215"), name: "ASML Holding", asset_type: "STOCK", currency: "EUR", exchange: "Euronext Amsterdam", category: "Technology", geography: "NL" },
    CatalogInstrument { ticker: "RMS.PA", isin: Some("FR0000052292"), name: "Hermès International", asset_type: "STOCK", currency: "EUR", exchange: "Euronext Paris", category: "Consumer Discretionary", geography: "FR" },
    CatalogInstrument { ticker: "SU.PA", isin: Some("FR0000121972"), name: "Schneider Electric", asset_type: "STOCK", currency: "EUR", exchange: "Euronext Paris", category: "Industrials", geography: "FR" },
    // Major Crypto Assets
    CatalogInstrument { ticker: "BTC", isin: None, name: "Bitcoin", asset_type: "CRYPTO", currency: "EUR", exchange: "Crypto", category: "Layer 1", geography: "WORLD" },
    CatalogInstrument { ticker: "ETH", isin: None, name: "Ethereum", asset_type: "CRYPTO", currency: "EUR", exchange: "Crypto", category: "Layer 1", geography: "WORLD" },
    CatalogInstrument { ticker: "SOL", isin: None, name: "Solana", asset_type: "CRYPTO", currency: "EUR", exchange: "Crypto", category: "Layer 1", geography: "WORLD" },
    CatalogInstrument { ticker: "USDC", isin: None, name: "USD Coin", asset_type: "CRYPTO", currency: "EUR", exchange: "Crypto", category: "Stablecoin", geography: "WORLD" },
];

/// Searches the offline catalog by exact ISIN, ticker prefix, or case-insensitive name substring.
pub fn search_catalog(query: &str, limit: usize) -> Vec<&'static CatalogInstrument> {
    let q = query.trim().to_uppercase();
    if q.is_empty() {
        return OFFLINE_CATALOG.iter().take(limit).collect();
    }
    let mut scored: Vec<(u8, &'static CatalogInstrument)> = OFFLINE_CATALOG
        .iter()
        .filter_map(|item| {
            if item.isin.is_some_and(|i| i.eq_ignore_ascii_case(&q)) {
                return Some((0, item));
            }
            if item.ticker.eq_ignore_ascii_case(&q) {
                return Some((1, item));
            }
            if item.ticker.to_uppercase().starts_with(&q)
                || item.isin.is_some_and(|i| i.to_uppercase().starts_with(&q))
            {
                return Some((2, item));
            }
            if item.name.to_uppercase().contains(&q)
                || item.category.to_uppercase().contains(&q)
            {
                return Some((3, item));
            }
            None
        })
        .collect();
    scored.sort_by_key(|(rank, item)| (*rank, item.ticker));
    scored.into_iter().take(limit).map(|(_, i)| i).collect()
}

/// Resolves an ISIN to its canonical European Yahoo ticker offline when present in the catalog.
pub fn resolve_isin_offline(isin: &str) -> Option<&'static CatalogInstrument> {
    let q = isin.trim();
    OFFLINE_CATALOG
        .iter()
        .find(|item| item.isin.is_some_and(|i| i.eq_ignore_ascii_case(q)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_cw8_by_isin_and_name() {
        let hit = resolve_isin_offline("LU1681043599").expect("CW8 in catalog");
        assert_eq!(hit.ticker, "CW8.PA");
        let results = search_catalog("msci world", 5);
        assert!(results.iter().any(|r| r.ticker == "CW8.PA"));
        assert!(results.iter().any(|r| r.ticker == "WPEA.PA"));
    }
}
