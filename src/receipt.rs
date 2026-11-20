//! Exit-Codes und das JSON-Receipt. Jeder Ausgang druckt genau ein Objekt auf stdout.

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Ok = 0,
    NotFound = 1,
    FlatImage = 2,
    AmbiguousMatch = 3,
    /// Enthält den konkreten error_code `window_minimized` oder `no_frame`.
    NoFrame = 4,
    CaptureDenied = 5,
    DeniedSensitive = 6,
    RuntimeError = 7,
    PolicyDenied = 8,
}

impl Exit {
    pub fn code(self) -> i32 {
        self as i32
    }
}

#[derive(Debug, Clone, Default)]
pub struct Receipt {
    pub backend: Option<&'static str>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub distinct_colors: Option<usize>,
    pub top_color_share: Option<f64>,
    pub flat: Option<bool>,
    pub duration_ms: Option<u128>,
    pub windows: Vec<Value>,
    pub candidates: Vec<Value>,
    pub extra: Vec<(String, Value)>,
}

impl Receipt {
    /// Erzeugt das JSON. `error_code` ist `None` nur bei Exit 0.
    pub fn render(&self, exit: Exit, error_code: Option<&str>, message: &str) -> Value {
        let mut value = json!({
            "ok": exit == Exit::Ok,
            "exit_code": exit.code(),
            "error_code": error_code,
            "message": message,
            "backend": self.backend,
            "width": self.width,
            "height": self.height,
            "distinct_colors": self.distinct_colors,
            "top_color_share": self.top_color_share,
            "flat": self.flat.unwrap_or(false),
            "duration_ms": self.duration_ms,
            "windows": self.windows,
        });
        if !self.candidates.is_empty() || exit == Exit::AmbiguousMatch {
            value["candidates"] = json!(self.candidates);
        }
        for (key, extra) in &self.extra {
            value[key.as_str()] = extra.clone();
        }
        value
    }
}

pub fn error(exit: Exit, error_code: &str, message: &str) -> (Exit, Value) {
    (exit, Receipt::default().render(exit, Some(error_code), message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_the_contract_table() {
        assert_eq!(Exit::Ok.code(), 0);
        assert_eq!(Exit::NotFound.code(), 1);
        assert_eq!(Exit::FlatImage.code(), 2);
        assert_eq!(Exit::AmbiguousMatch.code(), 3);
        assert_eq!(Exit::NoFrame.code(), 4);
        assert_eq!(Exit::CaptureDenied.code(), 5);
        assert_eq!(Exit::DeniedSensitive.code(), 6);
        assert_eq!(Exit::RuntimeError.code(), 7);
        assert_eq!(Exit::PolicyDenied.code(), 8);
    }

    #[test]
    fn every_receipt_carries_the_full_key_set() {
        let value = Receipt::default().render(Exit::NotFound, Some("not_found"), "kein Fenster");
        for key in ["ok", "exit_code", "error_code", "message", "backend", "width", "height", "distinct_colors",
                    "top_color_share", "flat", "duration_ms", "windows"] {
            assert!(value.get(key).is_some(), "Schlüssel fehlt: {key}");
        }
        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["error_code"], json!("not_found"));
        assert!(value.get("candidates").is_none(), "candidates nur bei Mehrdeutigkeit");
    }

    #[test]
    fn ambiguous_receipt_lists_candidates() {
        let receipt = Receipt { candidates: vec![json!({"hwnd": 7, "process": "p", "rect": [0, 0, 1, 1]})], ..Default::default() };
        let value = receipt.render(Exit::AmbiguousMatch, Some("ambiguous_match"), "mehrdeutig");
        assert_eq!(value["candidates"][0]["hwnd"], json!(7));
    }

    #[test]
    fn success_receipt_is_ok_with_null_error_code() {
        let value = Receipt::default().render(Exit::Ok, None, "ok");
        assert_eq!(value["ok"], json!(true));
        assert!(value["error_code"].is_null());
    }
}
