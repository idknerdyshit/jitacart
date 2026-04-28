//! EVE MultiBuy paste parser. Pure function — no I/O.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedMultibuy {
    pub lines: Vec<ParsedLine>,
    pub errors: Vec<LineError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedLine {
    pub line_nos: Vec<u32>,
    pub name: String,
    pub qty: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineError {
    pub line_no: u32,
    pub raw: String,
    pub reason: String,
}

/// Lowercase + NBSP-normalize for case-insensitive aggregation.
pub fn name_key(name: &str) -> String {
    name.replace('\u{00A0}', " ").trim().to_lowercase()
}

/// Strip commas, then parse as i64. Returns None on failure or non-positive.
fn parse_qty(s: &str) -> Option<i64> {
    let cleaned = s.replace(',', "");
    cleaned.parse::<i64>().ok().filter(|&q| q > 0)
}

/// Returns true if a token (after comma-strip) parses as a number — int or float.
/// Used by the whitespace-fallback tokenizer to identify trailing numeric columns.
fn is_numeric_token(s: &str) -> bool {
    let cleaned = s.replace(',', "");
    if cleaned.is_empty() {
        return false;
    }
    cleaned.parse::<f64>().is_ok()
}

pub fn parse_multibuy(input: &str) -> ParsedMultibuy {
    let mut agg: HashMap<String, ParsedLine> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut errors: Vec<LineError> = Vec::new();

    for (idx, raw) in input.lines().enumerate() {
        let line_no: u32 = (idx as u32) + 1;
        let normalized = raw.replace('\u{00A0}', " ");
        let trimmed = normalized.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parsed = if trimmed.contains('\t') {
            parse_tab_line(trimmed)
        } else {
            parse_whitespace_line(trimmed)
        };

        match parsed {
            Ok((name, qty)) => {
                let key = name_key(&name);
                if key.is_empty() {
                    errors.push(LineError {
                        line_no,
                        raw: raw.to_string(),
                        reason: "missing item name".into(),
                    });
                    continue;
                }
                match agg.get_mut(&key) {
                    Some(existing) => {
                        existing.qty = existing.qty.saturating_add(qty);
                        existing.line_nos.push(line_no);
                    }
                    None => {
                        order.push(key.clone());
                        agg.insert(
                            key,
                            ParsedLine {
                                line_nos: vec![line_no],
                                name,
                                qty,
                            },
                        );
                    }
                }
            }
            Err(reason) => errors.push(LineError {
                line_no,
                raw: raw.to_string(),
                reason,
            }),
        }
    }

    let lines = order
        .into_iter()
        .filter_map(|k| agg.remove(&k))
        .collect::<Vec<_>>();

    ParsedMultibuy { lines, errors }
}

fn parse_tab_line(line: &str) -> Result<(String, i64), String> {
    let cols: Vec<&str> = line.split('\t').map(str::trim).collect();
    if cols.len() < 2 {
        return Err("tab line needs at least name and qty".into());
    }
    let name = cols[0].to_string();
    if name.is_empty() {
        return Err("missing item name".into());
    }
    let qty = parse_qty(cols[1])
        .ok_or_else(|| format!("quantity must be a positive integer (got '{}')", cols[1]))?;
    Ok((name, qty))
}

fn parse_whitespace_line(line: &str) -> Result<(String, i64), String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 2 {
        return Err("need at least an item name and a quantity".into());
    }

    // Walk right-to-left, find the run of trailing numeric tokens.
    let mut numeric_start = tokens.len();
    while numeric_start > 0 && is_numeric_token(tokens[numeric_start - 1]) {
        numeric_start -= 1;
    }
    if numeric_start == tokens.len() {
        return Err("no trailing quantity column found".into());
    }
    if numeric_start == 0 {
        return Err("missing item name".into());
    }

    // Of the trailing numerics, the LEFTMOST is the qty; rest are vol/price.
    let qty_tok = tokens[numeric_start];
    let qty = parse_qty(qty_tok)
        .ok_or_else(|| format!("quantity must be a positive integer (got '{qty_tok}')"))?;

    let name = tokens[..numeric_start].join(" ");
    if name.is_empty() {
        return Err("missing item name".into());
    }
    Ok((name, qty))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_two_columns() {
        let p = parse_multibuy("Tritanium\t1000");
        assert!(p.errors.is_empty());
        assert_eq!(p.lines.len(), 1);
        assert_eq!(p.lines[0].name, "Tritanium");
        assert_eq!(p.lines[0].qty, 1000);
        assert_eq!(p.lines[0].line_nos, vec![1]);
    }

    #[test]
    fn tab_three_columns_with_volume() {
        let p = parse_multibuy("Pyerite\t500\t2.5");
        assert!(p.errors.is_empty());
        assert_eq!(p.lines[0].qty, 500);
    }

    #[test]
    fn tab_four_columns_full() {
        let p = parse_multibuy("Mexallon\t250\t10.0\t75.50");
        assert!(p.errors.is_empty());
        assert_eq!(p.lines[0].qty, 250);
    }

    #[test]
    fn tab_extra_trailing_tabs() {
        let p = parse_multibuy("Tritanium\t1000\t\t");
        assert!(p.errors.is_empty());
        assert_eq!(p.lines[0].qty, 1000);
    }

    #[test]
    fn whitespace_fallback_full_columns() {
        let p = parse_multibuy("Expanded Cargohold II 5 25.00 3000000.00");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        assert_eq!(p.lines[0].name, "Expanded Cargohold II");
        assert_eq!(p.lines[0].qty, 5);
    }

    #[test]
    fn whitespace_qty_with_comma() {
        let p = parse_multibuy("Mexallon 1,000");
        assert!(p.errors.is_empty());
        assert_eq!(p.lines[0].name, "Mexallon");
        assert_eq!(p.lines[0].qty, 1000);
    }

    #[test]
    fn whitespace_single_qty_one() {
        let p = parse_multibuy("Tritanium 1");
        assert!(p.errors.is_empty());
        assert_eq!(p.lines[0].qty, 1);
    }

    #[test]
    fn nbsp_normalized() {
        // NBSP between qty cols, plus regular space in the name
        let p = parse_multibuy("Damage\u{00A0}Control\u{00A0}II\t3");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        assert_eq!(p.lines[0].name, "Damage Control II");
        assert_eq!(p.lines[0].qty, 3);
    }

    #[test]
    fn duplicate_aggregation_case_insensitive() {
        let input = "Tritanium\t100\nTRITANIUM\t250";
        let p = parse_multibuy(input);
        assert!(p.errors.is_empty());
        assert_eq!(p.lines.len(), 1);
        // First-seen casing preserved
        assert_eq!(p.lines[0].name, "Tritanium");
        assert_eq!(p.lines[0].qty, 350);
        assert_eq!(p.lines[0].line_nos, vec![1, 2]);
    }

    #[test]
    fn blank_lines_ignored() {
        let p = parse_multibuy("\n\nTritanium\t1\n\n");
        assert!(p.errors.is_empty());
        assert_eq!(p.lines.len(), 1);
    }

    #[test]
    fn comment_like_lines_emit_error() {
        // A comment-like line has no trailing numeric column → emit a LineError.
        let p = parse_multibuy("# this is a heading\nTritanium\t1");
        assert_eq!(p.lines.len(), 1);
        assert_eq!(p.errors.len(), 1);
        assert_eq!(p.errors[0].line_no, 1);
    }

    #[test]
    fn negative_qty_rejected() {
        let p = parse_multibuy("Tritanium\t-1");
        assert_eq!(p.lines.len(), 0);
        assert_eq!(p.errors.len(), 1);
    }

    #[test]
    fn zero_qty_rejected() {
        let p = parse_multibuy("Tritanium\t0");
        assert_eq!(p.lines.len(), 0);
        assert_eq!(p.errors.len(), 1);
    }

    #[test]
    fn non_numeric_qty_rejected() {
        let p = parse_multibuy("Tritanium\tlots");
        assert_eq!(p.lines.len(), 0);
        assert_eq!(p.errors.len(), 1);
    }

    #[test]
    fn name_with_numbers_preserved() {
        // The name itself may contain digits ("Damage Control II"). Right→left
        // walk only consumes trailing numeric *tokens*, and "II" is not numeric.
        let p = parse_multibuy("Damage Control II 5");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        assert_eq!(p.lines[0].name, "Damage Control II");
        assert_eq!(p.lines[0].qty, 5);
    }

    #[test]
    fn line_order_preserved() {
        let p = parse_multibuy("Tritanium\t1\nPyerite\t1\nMexallon\t1");
        assert_eq!(p.lines.len(), 3);
        assert_eq!(p.lines[0].name, "Tritanium");
        assert_eq!(p.lines[1].name, "Pyerite");
        assert_eq!(p.lines[2].name, "Mexallon");
    }
}
