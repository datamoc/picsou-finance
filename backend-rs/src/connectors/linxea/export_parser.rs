//! Pure-Rust parser for Linxea's "données financières" XLSX export.
//!
//! Reads the single-sheet workbook (`Tous les comptes`) directly from ZIP/XML bytes without
//! external spreadsheet binaries or Python. Headers are matched by normalized label (stripping
//! accents and punctuation such as the degree sign in `N° Contrat`), never by column index.
//! Every archive part and row count is bounded so a hostile workbook cannot exhaust memory.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::str::FromStr;
use std::sync::LazyLock;

use quick_xml::Reader;
use quick_xml::events::{BytesRef, Event};
use regex::Regex;
use rust_decimal::Decimal;
use serde::Serialize;
use unicode_normalization::UnicodeNormalization;
use zip::ZipArchive;

const COL_PLACEMENT: &str = "placement";
const COL_CONTRACT: &str = "n contrat";
const COL_PRODUCT: &str = "produit";
const COL_CATEGORY: &str = "categorie";
const COL_SUPPORT: &str = "nom du support";
const COL_ISIN: &str = "isin";
const COL_QUANTITY: &str = "nbre de parts";
const COL_PRICE: &str = "derniere cotation";
const COL_VALUE: &str = "somme en compte";
const COL_PNL: &str = "plus ou moins value";
const COL_AVERAGE: &str = "prix de revient moyen";

const MAX_PART_BYTES: usize = 32 * 1024 * 1024;
const MAX_ROWS: usize = 5_000;

static ISIN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z]{2}[A-Z0-9]{10}$").expect("regex"));

/// Failure raised when the workbook is not a valid Linxea financial-data export.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ExportFormatError(pub String);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PositionPayload {
    pub isin: Option<String>,
    pub symbol: String,
    pub label: String,
    pub quantity: String,
    pub buying_price_eur: Option<String>,
    pub current_price: Option<String>,
    pub quote_currency: Option<String>,
    pub current_value_eur: String,
    pub pnl_eur: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccountPayload {
    pub external_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub account_type: String,
    pub balance_eur: String,
    pub cash_balance: String,
    pub positions: Vec<PositionPayload>,
    pub snapshot_complete: bool,
}

fn err(msg: impl Into<String>) -> ExportFormatError {
    ExportFormatError(msg.into())
}

fn strip_accents(raw: &str) -> String {
    raw.nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect()
}

fn normalize_header(raw: &str) -> String {
    let stripped = strip_accents(raw).to_ascii_lowercase();
    let mut out = String::with_capacity(stripped.len());
    let mut prev_space = true;
    for ch in stripped.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_space = false;
        } else if !prev_space {
            out.push(' ');
            prev_space = true;
        }
    }
    out.trim_end().to_owned()
}

fn slug(raw: &str, fallback: &str) -> String {
    let stripped = strip_accents(raw);
    let mut out = String::with_capacity(stripped.len());
    let mut prev_sep = true;
    for ch in stripped.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
            prev_sep = false;
        } else if !prev_sep {
            out.push('_');
            prev_sep = true;
        }
    }
    let trimmed = out.trim_matches('_');
    let base = if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    };
    base.chars().take(24).collect()
}

fn parse_decimal(raw: Option<&str>) -> Result<Option<Decimal>, ExportFormatError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let cleaned: String = trimmed
        .chars()
        .filter(|c| !matches!(c, '\u{00a0}' | '\u{202f}' | ' ' | '%'))
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    if cleaned.is_empty() || cleaned == "-" || cleaned == "+" {
        return Ok(None);
    }
    let value = Decimal::from_str(&cleaned)
        .or_else(|_| Decimal::from_scientific(&cleaned))
        .map_err(|_| err("a numeric cell is not a number"))?;
    Ok(Some(value))
}

fn account_type_for(placement: &str, category: &str) -> Option<&'static str> {
    for raw in [placement, category] {
        let key = normalize_header(raw).to_ascii_uppercase();
        let mapped = match key.as_str() {
            "PER" | "PERP" => Some("PER"),
            "MADELIN" | "AV" | "ASSURANCE VIE" => Some("ASSURANCE_VIE"),
            "PEA" => Some("PEA"),
            _ => None,
        };
        if mapped.is_some() {
            return mapped;
        }
    }
    None
}

fn read_part(archive: &mut ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<Vec<u8>, ExportFormatError> {
    let file = archive
        .by_name(name)
        .map_err(|_| err(format!("missing workbook part {name}")))?;
    if file.size() > MAX_PART_BYTES as u64 {
        return Err(err("a workbook part is larger than any real export"));
    }
    let mut buf = Vec::new();
    file.take((MAX_PART_BYTES + 1) as u64)
        .read_to_end(&mut buf)
        .map_err(|_| err("could not read workbook part"))?;
    if buf.len() > MAX_PART_BYTES {
        return Err(err("a workbook part is larger than any real export"));
    }
    Ok(buf)
}

fn find_sheet(archive: &ZipArchive<Cursor<&[u8]>>) -> Result<String, ExportFormatError> {
    let mut sheets: Vec<(u32, String)> = archive
        .file_names()
        .filter_map(Result::ok)
        .filter(|name| name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml"))
        .map(|name| {
            let num = name
                .strip_prefix("xl/worksheets/sheet")
                .and_then(|s| s.strip_suffix(".xml"))
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            (num, name.into_owned())
        })
        .collect();
    if sheets.is_empty() {
        return Err(err("the workbook has no worksheet"));
    }
    sheets.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Ok(sheets.remove(0).1)
}

fn push_entity(out: &mut String, reference: &BytesRef<'_>) -> Result<(), ExportFormatError> {
    if let Some(ch) = reference
        .resolve_char_ref()
        .map_err(|_| err("invalid XML character reference"))?
    {
        out.push(ch);
        return Ok(());
    }
    let replacement = match reference.as_ref() {
        "lt" => "<",
        "gt" => ">",
        "amp" => "&",
        "apos" => "'",
        "quot" => "\"",
        _ => return Err(err("unsupported XML entity")),
    };
    out.push_str(replacement);
    Ok(())
}

fn read_shared_strings(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
) -> Result<Vec<String>, ExportFormatError> {
    let Ok(bytes) = read_part(archive, "xl/sharedStrings.xml") else {
        return Ok(Vec::new());
    };
    let xml = std::str::from_utf8(&bytes).map_err(|_| err("invalid UTF-8 in shared strings"))?;
    let mut reader = Reader::from_str(xml);
    let mut strings = Vec::new();
    let mut in_si = false;
    let mut in_t = false;
    let mut current = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                "si" => {
                    in_si = true;
                    current.clear();
                }
                "t" if in_si => {
                    in_t = true;
                }
                _ => {}
            },
            Ok(Event::Text(e)) if in_si && in_t => {
                current.push_str(e.as_ref());
            }
            Ok(Event::GeneralRef(e)) if in_si && in_t => {
                push_entity(&mut current, &e)?;
            }
            Ok(Event::CData(e)) if in_si && in_t => {
                current.push_str(e.as_ref());
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "t" => in_t = false,
                "si" => {
                    in_si = false;
                    in_t = false;
                    strings.push(std::mem::take(&mut current));
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => return Err(err("malformed shared strings XML")),
            _ => {}
        }
    }
    Ok(strings)
}

fn col_index_from_ref(reference: &str) -> Option<usize> {
    let mut index: usize = 0;
    let mut found = false;
    for b in reference.bytes() {
        if b.is_ascii_uppercase() {
            index = index.checked_mul(26)?.checked_add((b - b'A' + 1) as usize)?;
            found = true;
        } else {
            break;
        }
    }
    if found { index.checked_sub(1) } else { None }
}

fn read_rows(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    sheet: &str,
    shared: &[String],
) -> Result<Vec<HashMap<usize, String>>, ExportFormatError> {
    let bytes = read_part(archive, sheet)?;
    let xml = std::str::from_utf8(&bytes).map_err(|_| err("invalid UTF-8 in worksheet"))?;
    let mut reader = Reader::from_str(xml);
    let mut rows: Vec<HashMap<usize, String>> = Vec::new();

    let mut in_row = false;
    let mut current_row: HashMap<usize, String> = HashMap::new();
    let mut next_col: usize = 0;

    let mut in_cell = false;
    let mut cell_col: usize = 0;
    let mut cell_kind = String::new();
    let mut in_v = false;
    let mut in_t = false;
    let mut cell_text = String::new();
    let mut has_value = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                "row" => {
                    in_row = true;
                    current_row.clear();
                    next_col = 0;
                }
                "c" if in_row => {
                    in_cell = true;
                    cell_kind.clear();
                    cell_text.clear();
                    has_value = false;
                    let mut ref_col = None;
                    for attr in e.attributes().flatten() {
                        match attr.key.local_name().as_ref() {
                            "r" => {
                                ref_col = col_index_from_ref(attr.value.as_ref());
                            }
                            "t" => {
                                cell_kind = attr.value.as_ref().to_owned();
                            }
                            _ => {}
                        }
                    }
                    cell_col = ref_col.unwrap_or(next_col);
                    next_col = cell_col + 1;
                }
                "v" if in_cell => {
                    in_v = true;
                }
                "t" if in_cell && cell_kind == "inlineStr" => {
                    in_t = true;
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                if e.local_name().as_ref() == "c" && in_row {
                    let mut ref_col = None;
                    for attr in e.attributes().flatten() {
                        if attr.key.local_name().as_ref() == "r" {
                            ref_col = col_index_from_ref(attr.value.as_ref());
                        }
                    }
                    next_col = ref_col.unwrap_or(next_col) + 1;
                }
            }
            Ok(Event::Text(e)) if in_cell && (in_v || in_t) => {
                cell_text.push_str(e.as_ref());
                has_value = true;
            }
            Ok(Event::GeneralRef(e)) if in_cell && (in_v || in_t) => {
                push_entity(&mut cell_text, &e)?;
                has_value = true;
            }
            Ok(Event::CData(e)) if in_cell && (in_v || in_t) => {
                cell_text.push_str(e.as_ref());
                has_value = true;
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "v" => in_v = false,
                "t" => in_t = false,
                "c" if in_cell => {
                    in_cell = false;
                    in_v = false;
                    in_t = false;
                    if has_value {
                        let resolved = if cell_kind == "s" {
                            let idx: usize = cell_text
                                .trim()
                                .parse()
                                .map_err(|_| err("a shared string index is invalid"))?;
                            shared
                                .get(idx)
                                .cloned()
                                .ok_or_else(|| err("a shared string index is invalid"))?
                        } else {
                            std::mem::take(&mut cell_text)
                        };
                        current_row.insert(cell_col, resolved);
                    }
                }
                "row" if in_row => {
                    in_row = false;
                    rows.push(std::mem::take(&mut current_row));
                    if rows.len() > MAX_ROWS {
                        return Err(err("the export has more rows than any real contract"));
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => return Err(err("malformed worksheet XML")),
            _ => {}
        }
    }
    Ok(rows)
}

fn pick<'a>(
    row: &'a HashMap<usize, String>,
    columns: &HashMap<String, usize>,
    header: &str,
) -> Option<&'a str> {
    columns
        .get(header)
        .and_then(|idx| row.get(idx))
        .map(String::as_str)
}

/// Parses one Linxea contract's XLSX export into an [`AccountPayload`] matching the backend contract.
pub fn parse_export(
    data: &[u8],
    contract_id: &str,
    contract_number: &str,
    product_name: &str,
) -> Result<AccountPayload, ExportFormatError> {
    let mut archive = ZipArchive::new(Cursor::new(data))
        .map_err(|_| err("the export is not a zip package"))?;
    let shared = read_shared_strings(&mut archive)?;
    let sheet = find_sheet(&archive)?;
    let rows = read_rows(&mut archive, &sheet, &shared)?;

    let Some(header_row) = rows.first() else {
        return Err(err("the export is empty"));
    };
    let mut columns: HashMap<String, usize> = HashMap::new();
    for (&idx, value) in header_row {
        let header = normalize_header(value);
        if !header.is_empty() {
            columns.insert(header, idx);
        }
    }
    if !columns.contains_key(COL_SUPPORT) || !columns.contains_key(COL_VALUE) {
        return Err(err("the export is missing its support columns"));
    }

    let mut placement = String::new();
    let mut export_product = String::new();
    let mut export_contract_number = String::new();
    let mut positions: Vec<PositionPayload> = Vec::new();
    let mut balance = Decimal::ZERO;

    for row in rows.iter().skip(1) {
        let support = pick(row, &columns, COL_SUPPORT)
            .map(str::trim)
            .unwrap_or_default();
        if support.is_empty() {
            continue;
        }
        if placement.is_empty()
            && let Some(p) = pick(row, &columns, COL_PLACEMENT).map(str::trim)
            && !p.is_empty()
        {
            placement = p.to_owned();
        }
        if export_product.is_empty()
            && let Some(p) = pick(row, &columns, COL_PRODUCT).map(str::trim)
            && !p.is_empty()
        {
            export_product = p.to_owned();
        }
        if export_contract_number.is_empty()
            && let Some(n) = pick(row, &columns, COL_CONTRACT).map(str::trim)
            && !n.is_empty()
        {
            export_contract_number = n.to_owned();
        }

        let value = parse_decimal(pick(row, &columns, COL_VALUE))?
            .ok_or_else(|| err("a support row has no value"))?;
        let mut quantity = parse_decimal(pick(row, &columns, COL_QUANTITY))?.unwrap_or(Decimal::ZERO);
        let mut price = parse_decimal(pick(row, &columns, COL_PRICE))?;
        let mut average = parse_decimal(pick(row, &columns, COL_AVERAGE))?;
        let pnl = parse_decimal(pick(row, &columns, COL_PNL))?;

        if quantity.is_zero() && value.is_zero() {
            continue;
        }

        let isin_raw = pick(row, &columns, COL_ISIN)
            .map(|s| s.trim().to_ascii_uppercase())
            .unwrap_or_default();
        let isin = if ISIN_RE.is_match(&isin_raw) {
            Some(isin_raw.clone())
        } else {
            None
        };
        let mut symbol = isin.clone().unwrap_or_else(|| {
            let source = if isin_raw.is_empty() {
                support
            } else {
                &isin_raw
            };
            format!("LINXEA_{}", slug(source, "SUPPORT"))
        });
        if symbol.len() > 30 {
            symbol.truncate(30);
        }

        if quantity.is_zero() {
            // A non-unitised line (a fonds euro) reports 0 parts and 0.00 in Prix de Revient
            // Moyen. One unit at its value keeps quantity × price = value and derives cost basis
            // from value - pnl so invested_amount never collapses to zero.
            quantity = Decimal::ONE;
            if average.is_none_or(|a| a.is_zero()) {
                average = Some(match pnl {
                    Some(p) => value - p,
                    None => value,
                });
            }
            if price.is_none_or(|p| p.is_zero()) {
                price = Some(value);
            }
        }
        let resolved_price = match price {
            Some(p) => p,
            None if !quantity.is_zero() => value / quantity,
            None => value,
        };

        balance += value;
        positions.push(PositionPayload {
            isin,
            symbol,
            label: support.chars().take(200).collect(),
            quantity: quantity.to_string(),
            buying_price_eur: average.map(|a| a.to_string()),
            current_price: Some(resolved_price.to_string()),
            quote_currency: Some("EUR".to_owned()),
            current_value_eur: value.to_string(),
            pnl_eur: pnl.map(|p| p.to_string()),
        });
    }

    if positions.is_empty() {
        return Err(err("the export lists no support"));
    }

    let category = rows
        .iter()
        .skip(1)
        .filter_map(|row| pick(row, &columns, COL_CATEGORY).map(str::trim))
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    let account_type = account_type_for(&placement, category)
        .ok_or_else(|| err("the contract placement is not supported"))?;

    let mut name = if !export_product.is_empty() {
        export_product
    } else if !product_name.trim().is_empty() {
        product_name.trim().to_owned()
    } else {
        format!("Linxea {account_type}")
    };
    let resolved_contract_number = if !contract_number.trim().is_empty() {
        contract_number.trim()
    } else {
        export_contract_number.as_str()
    };
    if !resolved_contract_number.is_empty() {
        name = format!("{name} ({resolved_contract_number})");
    }
    let name: String = name.chars().take(200).collect();

    Ok(AccountPayload {
        external_id: format!("linxea_{contract_id}"),
        name,
        account_type: account_type.to_owned(),
        balance_eur: balance.to_string(),
        cash_balance: "0".to_owned(),
        positions,
        snapshot_complete: true,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use zip::write::SimpleFileOptions;

    use super::*;

    fn col_letters(mut index: usize) -> String {
        let mut out = String::new();
        index += 1;
        while index > 0 {
            let rem = (index - 1) % 26;
            out.insert(0, (b'A' + rem as u8) as char);
            index = (index - 1) / 26;
        }
        out
    }

    fn xml_escape(raw: &str) -> String {
        raw.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    fn build_workbook(rows: &[Vec<(&str, bool)>]) -> Vec<u8> {
        let mut body = String::new();
        for (row_idx, row) in rows.iter().enumerate() {
            body.push_str(&format!("<row r=\"{}\">", row_idx + 1));
            for (col_idx, (val, is_num)) in row.iter().enumerate() {
                let r = format!("{}{}", col_letters(col_idx), row_idx + 1);
                if *is_num {
                    body.push_str(&format!("<c r=\"{r}\"><v>{val}</v></c>"));
                } else {
                    body.push_str(&format!(
                        "<c r=\"{r}\" t=\"str\"><v>{}</v></c>",
                        xml_escape(val)
                    ));
                }
            }
            body.push_str("</row>");
        }
        let sheet = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
             <sheetData>{body}</sheetData></worksheet>"
        );
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            zip.start_file("xl/worksheets/sheet1.xml", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(sheet.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        cursor.into_inner()
    }

    fn headers() -> Vec<(&'static str, bool)> {
        vec![
            ("Placement", false),
            ("N° Contrat", false),
            ("Produit", false),
            ("Catégorie", false),
            ("Nom du support", false),
            ("ISIN", false),
            ("Nbre de parts", false),
            ("Dernière cotation", false),
            ("Date", false),
            ("Somme en Compte", false),
            ("Plus ou Moins Value", false),
            ("Prix de Revient Moyen", false),
        ]
    }

    #[test]
    fn reads_euro_fund_and_etf_with_derived_euro_cost_basis() {
        let xlsx = build_workbook(&[
            headers(),
            vec![
                ("PER", false),
                ("123456789", false),
                ("Linxea Spirit PER", false),
                ("Fonds Euro", false),
                ("Fonds Euro Nouvelle Generation", false),
                ("FONDSPERNG", false),
                ("0.000000", false),
                ("250.00", true),
                ("26/11/2026", false),
                ("250.00", true),
                ("0.00", true),
                ("0.00", true),
            ],
            vec![
                ("PER", false),
                ("123456789", false),
                ("Linxea Spirit PER", false),
                ("Fonds Actions", false),
                ("Synthetic World UCITS ETF Acc", false),
                ("IE0000000001", false),
                ("10.500000", false),
                ("120.00", true),
                ("25/11/2026", false),
                ("1260.00", true),
                ("210.00", true),
                ("100.00", true),
            ],
        ]);

        let account = parse_export(
            &xlsx,
            "00000000-0000-4000-8000-000000000001",
            "",
            "",
        )
        .expect("valid export");

        assert_eq!(
            account.external_id,
            "linxea_00000000-0000-4000-8000-000000000001"
        );
        assert_eq!(account.account_type, "PER");
        assert_eq!(account.name, "Linxea Spirit PER (123456789)");
        assert_eq!(Decimal::from_str(&account.balance_eur).unwrap(), Decimal::from_str("1510.00").unwrap());
        assert_eq!(account.positions.len(), 2);

        let euro = &account.positions[0];
        assert_eq!(euro.symbol, "LINXEA_FONDSPERNG");
        assert_eq!(euro.isin, None);
        assert_eq!(euro.quantity, "1");
        assert_eq!(Decimal::from_str(euro.buying_price_eur.as_deref().unwrap()).unwrap(), Decimal::from_str("250.00").unwrap());
        assert_eq!(Decimal::from_str(&euro.current_value_eur).unwrap(), Decimal::from_str("250.00").unwrap());

        let etf = &account.positions[1];
        assert_eq!(etf.isin.as_deref(), Some("IE0000000001"));
        assert_eq!(etf.symbol, "IE0000000001");
        assert_eq!(Decimal::from_str(&etf.quantity).unwrap(), Decimal::from_str("10.5").unwrap());
        assert_eq!(Decimal::from_str(etf.buying_price_eur.as_deref().unwrap()).unwrap(), Decimal::from_str("100.00").unwrap());
        assert_eq!(Decimal::from_str(&etf.current_value_eur).unwrap(), Decimal::from_str("1260.00").unwrap());
    }

    #[test]
    fn derives_euro_fund_cost_basis_from_value_minus_pnl() {
        let xlsx = build_workbook(&[
            headers(),
            vec![
                ("Assurance Vie", false),
                ("987654321", false),
                ("Linxea Spirit 2", false),
                ("Fonds Euro", false),
                ("Fonds Euro", false),
                ("FEURO", false),
                ("0", false),
                ("1050.00", true),
                ("26/11/2026", false),
                ("1050.00", true),
                ("50.00", true),
                ("0.00", true),
            ],
        ]);

        let account = parse_export(&xlsx, "av-1", "", "").expect("valid export");
        assert_eq!(account.account_type, "ASSURANCE_VIE");
        assert_eq!(
            Decimal::from_str(account.positions[0].buying_price_eur.as_deref().unwrap()).unwrap(),
            Decimal::from_str("1000.00").unwrap()
        );
    }

    #[test]
    fn rejects_unsupported_placement_and_empty_workbooks() {
        let scpi = build_workbook(&[
            headers(),
            vec![
                ("SCPI", false),
                ("111", false),
                ("Linxea SCPI", false),
                ("Pierre Papier", false),
                ("SCPI Exemple", false),
                ("SCPI1", false),
                ("10", false),
                ("200", true),
                ("26/11/2026", false),
                ("2000", true),
                ("0", true),
                ("200", true),
            ],
        ]);
        assert!(parse_export(&scpi, "scpi-1", "", "").is_err());

        let empty = build_workbook(&[headers()]);
        assert!(parse_export(&empty, "empty-1", "", "").is_err());
    }
}
