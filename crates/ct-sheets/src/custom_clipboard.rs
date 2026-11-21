//! Chromium's UTF-16 custom MIME map and observed Sheets compact cell data.
//!
//! Unrecognized layouts/types are rejected rather than guessed from display text.

use serde_json::Value;

use crate::model::{CellKey, SheetsParseError};

pub const CUSTOM_MIME: &str = "application/x-vnd.google-spreadsheet-compact-table+json";
pub const CUSTOM_FORMAT: &str = "Chromium Web Custom MIME Data Format";

const MAX_RAW: usize = 10_000_000;
const MAX_ENTRIES: u32 = 100;
const MAX_CELLS: usize = 10_000;
/// Flag bits understood by the decoder: value, formula, 0x2/0x40/0x80 (observed
/// style bits) and character runs.
const KNOWN_FLAGS: i64 = 0x4CB;

fn err<T>(phrase: &'static str) -> Result<T, SheetsParseError> {
    Err(SheetsParseError(phrase))
}

fn read_u32(raw: &[u8], offset: usize) -> Option<u32> {
    let bytes = raw.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

pub fn decode_custom_data(raw: &[u8]) -> Result<Vec<(String, String)>, SheetsParseError> {
    if raw.len() < 8 || raw.len() > MAX_RAW {
        return err("invalid custom clipboard size");
    }
    let (Some(size), Some(count)) = (read_u32(raw, 0), read_u32(raw, 4)) else {
        return err("invalid custom clipboard size");
    };
    if size as usize != raw.len() - 4 || count > MAX_ENTRIES {
        return err("invalid custom clipboard header");
    }
    let mut offset = 8usize;
    let mut result: Vec<(String, String)> = Vec::new();
    for _ in 0..count {
        let mut strings = [String::new(), String::new()];
        for slot in &mut strings {
            let Some(units) = read_u32(raw, offset) else {
                return err("truncated custom clipboard string");
            };
            offset += 4;
            let end = (units as usize)
                .checked_mul(2)
                .and_then(|n| offset.checked_add(n))
                .filter(|&end| end <= raw.len());
            let Some(end) = end else {
                return err("truncated custom clipboard string");
            };
            let (pairs, _) = raw[offset..end].as_chunks::<2>();
            let units: Vec<u16> = pairs.iter().map(|c| u16::from_le_bytes(*c)).collect();
            *slot = String::from_utf16(&units)
                .map_err(|_| SheetsParseError("invalid UTF-16 in custom clipboard"))?;
            offset = (end + 3) & !3;
        }
        let [key, value] = strings;
        if result.iter().any(|(k, _)| *k == key) {
            return err("duplicate MIME key");
        }
        result.push((key, value));
    }
    if offset != raw.len() {
        return err("trailing custom clipboard data");
    }
    Ok(result)
}

fn push_string(payload: &mut Vec<u8>, text: &str) {
    let units: Vec<u16> = text.encode_utf16().collect();
    payload.extend_from_slice(&(units.len() as u32).to_le_bytes());
    for unit in units {
        payload.extend_from_slice(&unit.to_le_bytes());
    }
    while !payload.len().is_multiple_of(4) {
        payload.push(0);
    }
}

pub fn encode_custom_data(map: &[(String, String)]) -> Vec<u8> {
    let mut payload = (map.len() as u32).to_le_bytes().to_vec();
    for (key, value) in map {
        push_string(&mut payload, key);
        push_string(&mut payload, value);
    }
    let mut out = (payload.len() as u32).to_le_bytes().to_vec();
    out.extend(payload);
    out
}

pub fn compact_from_custom(raw: Option<&[u8]>) -> Result<Option<Value>, SheetsParseError> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    let map = decode_custom_data(raw)?;
    match map.iter().find(|(k, _)| k == CUSTOM_MIME) {
        Some((_, json)) => serde_json::from_str(json)
            .map(Some)
            .map_err(|_| SheetsParseError("invalid compact JSON")),
        None => Ok(None),
    }
}

/// Run-length list: a negative `-n` means "repeat the next value n times".
pub fn expand_runs(values: &[Value], count: usize) -> Result<Vec<i64>, SheetsParseError> {
    let mut result: Vec<i64> = Vec::new();
    let mut index = 0;
    while index < values.len() {
        let Some(mut value) = values[index].as_i64() else {
            return err("noninteger compact run");
        };
        index += 1;
        let mut repeats: usize = 1;
        if value < 0 {
            repeats = usize::try_from(value.unsigned_abs()).unwrap_or(usize::MAX);
            let Some(next) = values.get(index) else {
                return err("truncated compact run");
            };
            index += 1;
            let Some(next) = next.as_i64() else {
                return err("invalid compact run");
            };
            value = next;
        }
        if value < 0 || result.len().saturating_add(repeats) > count {
            return err("invalid compact run");
        }
        result.extend(std::iter::repeat_n(value, repeats));
    }
    if result.len() != count {
        return err("compact cell count mismatch");
    }
    Ok(result)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactItem {
    pub text: Option<String>,
    pub string_index: Option<usize>,
    pub formula: Option<String>,
    pub rich_index: Option<usize>,
}

fn array_at<'a>(
    value: Option<&'a Value>,
    phrase: &'static str,
) -> Result<&'a [Value], SheetsParseError> {
    match value {
        None => Ok(&[]),
        Some(Value::Array(items)) => Ok(items),
        Some(_) => err(phrase),
    }
}

/// Decode only the observed string/empty/formula layout, fail closed on others.
pub fn compact_cells(data: &Value) -> Result<(usize, usize, Vec<CompactItem>), SheetsParseError> {
    let dims = &data["15"];
    let (Some(rows), Some(cols)) = (dims["1"].as_u64(), dims["2"].as_u64()) else {
        return err("invalid compact dimensions");
    };
    let count = rows
        .checked_mul(cols)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|&n| n > 0 && n <= MAX_CELLS);
    let Some(count) = count else {
        return err("invalid compact dimensions");
    };
    // rows and cols are each <= count <= 10_000 here.
    let (rows, cols) = (rows as usize, cols as usize);
    let flag_values = array_at(data.get("2"), "invalid compact flags")?;
    let flags = expand_runs(flag_values, count)?;
    let values = data.get("3");
    if values.is_some_and(|v| !v.is_object()) {
        return err("invalid compact values");
    }
    let types = array_at(values.and_then(|v| v.get("1")), "invalid compact types")?;
    let strings = array_at(values.and_then(|v| v.get("4")), "invalid compact strings")?;
    let formula_indices = array_at(data.get("9"), "invalid compact formula indices")?;
    let formulas = array_at(data.get("8"), "invalid compact formulas")?;
    let runs = array_at(data.get("17"), "invalid character run payload")?;
    let (mut value_index, mut string_index, mut formula_index, mut rich_index) = (0, 0, 0, 0);
    let mut items = Vec::with_capacity(count);
    for flag in flags {
        if flag & !KNOWN_FLAGS != 0 {
            return err("unverified compact cell attributes");
        }
        let mut item = CompactItem {
            text: None,
            string_index: None,
            formula: None,
            rich_index: None,
        };
        if flag & 1 != 0 {
            // Observed: 2 = text, 4 = quote-prefixed text (the stored string has no
            // apostrophe). The type is kept on replacement, so the cell stays text.
            if !matches!(types.get(value_index).and_then(Value::as_i64), Some(2 | 4)) {
                return err("unverified compact value type");
            }
            let Some(text) = strings.get(string_index).and_then(Value::as_str) else {
                return err("missing compact string");
            };
            item.text = Some(text.to_string());
            item.string_index = Some(string_index);
            value_index += 1;
            string_index += 1;
        }
        if flag & 8 != 0 {
            let Some(index) = formula_indices.get(formula_index) else {
                return err("missing compact formula index");
            };
            let Some(formula) = index
                .as_u64()
                .and_then(|i| formulas.get(usize::try_from(i).ok()?))
            else {
                return err("invalid compact formula index");
            };
            match formula.as_str() {
                Some(f) if f.starts_with('=') => item.formula = Some(f.to_string()),
                _ => return err("invalid compact formula"),
            }
            formula_index += 1;
        }
        if flag & 1024 != 0 {
            // Opaque character runs; replacement removes them, so no style
            // interpretation is necessary.
            if !runs.get(rich_index).is_some_and(Value::is_object) {
                return err("missing character run payload");
            }
            item.rich_index = Some(rich_index);
            rich_index += 1;
        }
        items.push(item);
    }
    if value_index != types.len()
        || string_index != strings.len()
        || formula_index != formula_indices.len()
    {
        return err("unused compact values");
    }
    if rich_index != runs.len() {
        return err("unused character run payload");
    }
    Ok((rows, cols, items))
}

pub fn replace_compact_strings(
    data: &Value,
    replacements: &[(CellKey, String)],
) -> Result<Value, SheetsParseError> {
    let (_, cols, items) = compact_cells(data)?;
    let mut result = data.clone();
    let flag_values = array_at(data.get("2"), "invalid compact flags")?;
    let mut flags = expand_runs(flag_values, items.len())?;
    let mut removed_runs: Vec<usize> = Vec::new();
    for &((row, col), ref text) in replacements {
        let index = row.checked_mul(cols).and_then(|n| n.checked_add(col));
        let Some(index) = index.filter(|&i| col < cols && i < items.len()) else {
            return err("replacement outside compact table");
        };
        let item = &items[index];
        let (None, Some(string_index)) = (&item.formula, item.string_index) else {
            return err("replacement is not a text cell");
        };
        let Some(slot) = result
            .get_mut("3")
            .and_then(|v| v.get_mut("4"))
            .and_then(|v| v.get_mut(string_index))
        else {
            return err("missing compact string");
        };
        *slot = Value::String(text.clone());
        if let Some(rich) = item.rich_index {
            removed_runs.push(rich);
            flags[index] &= !1024;
        }
    }
    if !removed_runs.is_empty() {
        result["2"] = Value::Array(flags.into_iter().map(Value::from).collect());
        let remaining: Vec<Value> = array_at(data.get("17"), "invalid character run payload")?
            .iter()
            .enumerate()
            .filter(|(i, _)| !removed_runs.contains(i))
            .map(|(_, run)| run.clone())
            .collect();
        if let Some(object) = result.as_object_mut() {
            if remaining.is_empty() {
                object.remove("17");
            } else {
                object.insert("17".to_string(), Value::Array(remaining));
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn table() -> Value {
        json!({
            "1": 1, "2": [-3, 195, 203, 194],
            "3": {"1": [2, 2, 2, 2], "4": ["a", "b", "c\nd", "hello world"]},
            "4": [{"2": 4224, "10": 2, "15": "Arial"}], "5": [-5, 0],
            "8": ["=concat(\"hello\",\" world\")"], "9": [0],
            "12": [-5, 1], "13": [-5, 1], "15": {"1": 1, "2": 5}, "20": 5,
        })
    }

    fn quote_prefixed_table() -> Value {
        json!({
            "1": 1, "2": [-2, 195],
            "3": {"1": [4, 2], "4": ["aaa", "aaa"]},
            "4": [{"2": 4224, "10": 2, "15": "Arial"}], "5": [-2, 0],
            "12": [-2, 1], "13": [-2, 1], "15": {"1": 2, "2": 1}, "20": 2,
        })
    }

    fn pair(k: &str, v: &str) -> (String, String) {
        (k.to_string(), v.to_string())
    }

    #[test]
    fn custom_mime_roundtrip_and_string_formula_distinction() {
        let data = table();
        let map = vec![
            pair(CUSTOM_MIME, &data.to_string()),
            pair("other", "한글😀"),
        ];
        let blob = encode_custom_data(&map);
        let decoded = decode_custom_data(&blob).unwrap();
        assert_eq!(decoded, map);
        assert_eq!(decoded[1].1, "한글😀");
        assert_eq!(
            compact_from_custom(Some(&blob)).unwrap(),
            Some(data.clone())
        );
        let (rows, cols, cells) = compact_cells(&data).unwrap();
        assert_eq!((rows, cols), (1, 5));
        let texts: Vec<_> = cells
            .iter()
            .map(|c| c.text.clone().unwrap_or_default())
            .collect();
        assert_eq!(texts, ["a", "b", "c\nd", "hello world", ""]);
        assert_eq!(
            cells[3].formula.as_deref(),
            Some("=concat(\"hello\",\" world\")")
        );
        assert_eq!(cells[4].string_index, None);
    }

    #[test]
    fn compact_replacement_preserves_formula_empty_cells_and_styles() {
        let before = table();
        let after = replace_compact_strings(&before, &[((0, 0), "번역".to_string())]).unwrap();
        assert_eq!(after["3"]["4"], json!(["번역", "b", "c\nd", "hello world"]));
        assert_eq!(before["3"]["4"][0], json!("a"));
        for (key, value) in before.as_object().unwrap() {
            if key != "3" {
                assert_eq!(&after[key], value);
            }
        }
        assert!(replace_compact_strings(&before, &[((0, 3), "x".to_string())]).is_err());
        assert!(replace_compact_strings(&before, &[((0, 4), "x".to_string())]).is_err());
        assert!(replace_compact_strings(&before, &[((0, 5), "x".to_string())]).is_err());
        assert!(replace_compact_strings(&before, &[((1, 0), "x".to_string())]).is_err());
    }

    #[test]
    fn invalid_custom_data_rejected() {
        for blob in [vec![], b"12345678".to_vec(), vec![0u8; 12]] {
            assert!(decode_custom_data(&blob).is_err(), "{blob:?}");
        }
    }

    #[test]
    fn custom_data_rejects_more_than_100_entries() {
        let many: Vec<_> = (0..101).map(|i| pair(&format!("k{i}"), "v")).collect();
        assert_eq!(
            decode_custom_data(&encode_custom_data(&many)),
            Err(SheetsParseError("invalid custom clipboard header"))
        );
        let ok: Vec<_> = (0..100).map(|i| pair(&format!("k{i}"), "v")).collect();
        assert_eq!(decode_custom_data(&encode_custom_data(&ok)).unwrap(), ok);
    }

    #[test]
    fn custom_data_rejects_duplicates_truncation_and_trailing() {
        let dup = encode_custom_data(&[pair("a", "1"), pair("a", "2")]);
        assert_eq!(
            decode_custom_data(&dup),
            Err(SheetsParseError("duplicate MIME key"))
        );
        let mut trailing = encode_custom_data(&[pair("a", "1")]);
        trailing.extend_from_slice(&[0; 4]);
        let size = (trailing.len() - 4) as u32;
        trailing[..4].copy_from_slice(&size.to_le_bytes());
        assert_eq!(
            decode_custom_data(&trailing),
            Err(SheetsParseError("trailing custom clipboard data"))
        );
        let mut trunc = encode_custom_data(&[pair("abc", "1")]);
        trunc.truncate(trunc.len() - 8);
        let size = (trunc.len() - 4) as u32;
        trunc[..4].copy_from_slice(&size.to_le_bytes());
        assert_eq!(
            decode_custom_data(&trunc),
            Err(SheetsParseError("truncated custom clipboard string"))
        );
    }

    #[test]
    fn empty_or_absent_compact_is_none() {
        assert_eq!(compact_from_custom(None), Ok(None));
        assert_eq!(compact_from_custom(Some(&[])), Ok(None));
        let blob = encode_custom_data(&[pair("other", "{}")]);
        assert_eq!(compact_from_custom(Some(&blob)), Ok(None));
    }

    #[test]
    fn expand_runs_rules() {
        let v = |x: Value| x.as_array().unwrap().clone();
        assert_eq!(
            expand_runs(&v(json!([-3, 195, 203, 194])), 5).unwrap(),
            [195, 195, 195, 203, 194]
        );
        assert!(expand_runs(&v(json!([-3])), 3).is_err());
        assert!(expand_runs(&v(json!([-2, -1])), 2).is_err());
        assert!(expand_runs(&v(json!(["a"])), 1).is_err());
        assert!(expand_runs(&v(json!([1.5])), 1).is_err());
        assert!(expand_runs(&v(json!([-5, 1])), 3).is_err());
        assert!(expand_runs(&v(json!([1, 2])), 3).is_err());
    }

    #[test]
    fn compact_unknown_flag_bit_rejected() {
        let mut data = table();
        data["2"] = json!([-3, 195, 203, 194 | 0x10]);
        assert_eq!(
            compact_cells(&data),
            Err(SheetsParseError("unverified compact cell attributes"))
        );
    }

    #[test]
    fn quote_prefixed_text_cell_is_text() {
        let (rows, cols, cells) = compact_cells(&quote_prefixed_table()).unwrap();
        assert_eq!((rows, cols), (2, 1));
        assert_eq!(cells[0].text.as_deref(), Some("aaa"));
        assert_eq!(cells[1].text.as_deref(), Some("aaa"));
    }

    #[test]
    fn quote_prefixed_replacement_keeps_text_type() {
        let after = replace_compact_strings(
            &quote_prefixed_table(),
            &[((0, 0), "번역".into()), ((1, 0), "번역2".into())],
        )
        .unwrap();
        assert_eq!(after["3"], json!({"1": [4, 2], "4": ["번역", "번역2"]}));
    }

    #[test]
    fn unknown_compact_value_type_is_still_rejected() {
        let mut data = quote_prefixed_table();
        data["3"]["1"] = json!([3, 2]);
        assert_eq!(
            compact_cells(&data),
            Err(SheetsParseError("unverified compact value type"))
        );
    }

    #[test]
    fn compact_dimension_and_leftover_checks() {
        let mut data = table();
        data["15"] = json!({"1": 0, "2": 5});
        assert_eq!(
            compact_cells(&data),
            Err(SheetsParseError("invalid compact dimensions"))
        );
        data["15"] = json!({"1": 101, "2": 100});
        assert_eq!(
            compact_cells(&data),
            Err(SheetsParseError("invalid compact dimensions"))
        );
        let mut data = table();
        data["3"]["4"] = json!(["a", "b", "c\nd", "hello world", "extra"]);
        assert_eq!(
            compact_cells(&data),
            Err(SheetsParseError("unused compact values"))
        );
        let mut data = table();
        data["17"] = json!([{"1": 1}]);
        assert_eq!(
            compact_cells(&data),
            Err(SheetsParseError("unused character run payload"))
        );
    }

    #[test]
    fn replacement_drops_character_runs_and_expands_flags() {
        let data = json!({
            "2": [-2, 195, 1219], "3": {"1": [2, 2, 2], "4": ["a", "b", "c"]},
            "17": [{"1": 1}], "15": {"1": 1, "2": 3},
        });
        let (_, _, cells) = compact_cells(&data).unwrap();
        assert_eq!(cells[2].rich_index, Some(0));
        let after = replace_compact_strings(&data, &[((0, 2), "C".into())]).unwrap();
        assert_eq!(after["2"], json!([195, 195, 195]));
        assert!(after.get("17").is_none());
        assert_eq!(after["3"]["4"], json!(["a", "b", "C"]));
        let after = replace_compact_strings(&data, &[((0, 0), "A".into())]).unwrap();
        assert_eq!(after["2"], data["2"]);
        assert_eq!(after["17"], data["17"]);
    }
}
