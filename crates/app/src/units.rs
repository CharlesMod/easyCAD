//! Display units. The document is always millimetres; inch mode only changes how
//! lengths are shown and how bare numbers typed into fields are read.
//!
//! Inputs accept `0.125in`, `1/8"`, `1 1/2"`, `3mm`, `2cm` (the expression engine
//! knows the unit suffixes; fractional inches are rewritten here, see GUI_NEEDS).

use std::sync::atomic::{AtomicBool, Ordering};

static INCH: AtomicBool = AtomicBool::new(false);

pub fn inch() -> bool {
    INCH.load(Ordering::Relaxed)
}

pub fn set_inch(on: bool) {
    INCH.store(on, Ordering::Relaxed);
}

pub fn label() -> &'static str {
    if inch() {
        "in"
    } else {
        "mm"
    }
}

/// A length for display (no unit): "86.00" or "3.386".
pub fn len(mm: f64) -> String {
    if inch() {
        format!("{:.3}", mm / 25.4)
    } else {
        format!("{mm:.2}")
    }
}

/// A length with its unit: "86.00 mm" or "3.386 in".
pub fn len_u(mm: f64) -> String {
    format!("{} {}", len(mm), label())
}

/// Compact length for chips ("32", "1.26in").
pub fn short(mm: f64) -> String {
    if inch() {
        let s = format!("{:.3}", mm / 25.4);
        format!("{}in", s.trim_end_matches('0').trim_end_matches('.'))
    } else {
        crate::dialogs::fmt(mm)
    }
}

/// Rewrite what the user typed into something the expression engine reads:
/// `1/8"` → `(1/8*1in)`, `1 1/2"` → `((1+1/2)*1in)`, and in inch mode a bare number
/// gets `in` (the document itself stays in mm).
pub fn normalize_input(text: &str) -> String {
    let t = text.trim();
    let mut out = String::new();
    let b = t.as_bytes();
    let mut i = 0;
    while i < b.len() {
        // try "<int> <int>/<int>\"" or "<int>/<int>\""
        if b[i].is_ascii_digit()
            && (i == 0
                || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'.' || b[i - 1] == b'_'))
        {
            if let Some((expr, len)) = fraction_inch(&t[i..]) {
                out.push_str(&expr);
                i += len;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    if inch() && out.parse::<f64>().is_ok() {
        return format!("{out} in");
    }
    out
}

fn digits(s: &str) -> usize {
    s.bytes().take_while(|c| c.is_ascii_digit()).count()
}

/// `1/8"` or `1 1/2"` at the start of `s` → ("(1+1/2)in", consumed bytes).
fn fraction_inch(s: &str) -> Option<(String, usize)> {
    let n1 = digits(s);
    let rest = &s[n1..];
    // mixed number: whole, spaces, numerator / denominator "
    let ws = rest.bytes().take_while(|c| *c == b' ').count();
    if ws > 0 {
        let r2 = &rest[ws..];
        let n2 = digits(r2);
        if n2 > 0 && r2[n2..].starts_with('/') {
            let r3 = &r2[n2 + 1..];
            let n3 = digits(r3);
            if n3 > 0 && r3[n3..].trim_start().starts_with('"') {
                let q = r3[n3..].find('"').unwrap();
                let len = n1 + ws + n2 + 1 + n3 + q + 1;
                return Some((
                    format!("(({}+{}/{})*1in)", &s[..n1], &r2[..n2], &r3[..n3]),
                    len,
                ));
            }
        }
    }
    if let Some(r2) = rest.strip_prefix('/') {
        let n2 = digits(r2);
        if n2 > 0 && r2[n2..].trim_start().starts_with('"') {
            let q = r2[n2..].find('"').unwrap();
            return Some((
                format!("({}/{}*1in)", &s[..n1], &r2[..n2]),
                n1 + 1 + n2 + q + 1,
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str) -> f64 {
        casework_model::params::eval(&normalize_input(s), &Default::default()).unwrap()
    }

    #[test]
    fn inch_inputs() {
        set_inch(false);
        assert!((ev("0.125in") - 3.175).abs() < 1e-9);
        assert!((ev("1/8\"") - 3.175).abs() < 1e-9);
        assert!((ev("1 1/2\"") - 38.1).abs() < 1e-9);
        assert!((ev("3mm") - 3.0).abs() < 1e-9);
        assert!((ev("2 * 1/4\" + 1") - (12.7 + 1.0)).abs() < 1e-9);
        assert_eq!(normalize_input("wall + 1"), "wall + 1");
        assert!(
            (ev("5") - 5.0).abs() < 1e-9,
            "bare numbers are mm in mm mode"
        );
        set_inch(true);
        assert!(
            (ev("0.5") - 12.7).abs() < 1e-9,
            "bare numbers are inches in inch mode"
        );
        assert!((ev("3mm") - 3.0).abs() < 1e-9);
        assert_eq!(len(25.4), "1.000");
        set_inch(false);
        assert_eq!(len(25.4), "25.40");
    }
}
