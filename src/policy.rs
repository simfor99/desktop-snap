//! Sicherheitsrichtlinie: Denylist, Fensterwahl, Containment. Rein logisch, unter Linux testbar.

use crate::cli::{MatchSpec, Selector};

/// Einkompilierte Untergrenze. Nie per CLI abschaltbar, nur über `--extra-denylist` erweiterbar.
pub const BASE_DENY_PROCESSES: &[&str] = &[
    "1password",
    "bitwarden",
    "keepass",
    "keepassxc",
    "enpass",
    "dashlane",
    "nordpass",
    "credentialuibroker",
    "consent",
    "lockapp",
    "sechealthui",
    "mstsc",
    "anydesk",
    "teamviewer",
    "rustdesk",
    "putty",
    "termius",
    "pageant",
];
pub const BASE_DENY_TITLE_SUBSTRINGS: &[&str] = &["inprivate", "incognito", "inkognito", "online-banking"];
pub const REDACTED: &str = "[REDACTED]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl Rect {
    pub fn from_xywh(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { l: x, t: y, r: x.saturating_add(w as i32), b: y.saturating_add(h as i32) }
    }

    pub fn width(&self) -> i64 {
        self.r as i64 - self.l as i64
    }

    pub fn height(&self) -> i64 {
        self.b as i64 - self.t as i64
    }

    pub fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    /// `inner` liegt vollständig in `self`.
    pub fn contains(&self, inner: &Rect) -> bool {
        !inner.is_empty() && inner.l >= self.l && inner.t >= self.t && inner.r <= self.r && inner.b <= self.b
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        !self.is_empty() && !other.is_empty() && self.l < other.r && other.l < self.r && self.t < other.b && other.t < self.b
    }
}

/// Normalisiert einen Prozessnamen: Kleinbuchstaben, ohne `.exe`.
pub fn normalize_process(raw: &str) -> String {
    let lower = raw.trim().to_lowercase();
    lower.strip_suffix(".exe").map(str::to_string).unwrap_or(lower)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denylist {
    processes: Vec<String>,
    title_substrings: Vec<String>,
}

impl Denylist {
    pub fn base() -> Denylist {
        Denylist {
            processes: BASE_DENY_PROCESSES.iter().map(|s| s.to_string()).collect(),
            title_substrings: BASE_DENY_TITLE_SUBSTRINGS.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Ergänzt die Liste aus JSON `{"processes": [...], "title_substrings": [...]}`. Verkleinern ist unmöglich:
    /// es gibt keine Entfernungsoperation, und unbekannte Schlüssel oder leere Einträge sind Fehler.
    pub fn extend_from_json(&mut self, text: &str) -> Result<(), String> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("Denylist ist kein gültiges JSON: {e}"))?;
        let object = value.as_object().ok_or("Denylist muss ein JSON-Objekt sein.")?;
        for key in object.keys() {
            if key != "processes" && key != "title_substrings" {
                return Err(format!("Unbekannter Denylist-Schlüssel: {key}"));
            }
        }
        for (key, target) in [("processes", &mut self.processes), ("title_substrings", &mut self.title_substrings)] {
            let Some(entries) = object.get(key) else { continue };
            let items = entries.as_array().ok_or_else(|| format!("{key} muss ein Array sein."))?;
            for item in items {
                let raw = item.as_str().ok_or_else(|| format!("{key} enthält einen Nicht-Text-Eintrag."))?;
                let normalized = if key == "processes" { normalize_process(raw) } else { raw.trim().to_lowercase() };
                if normalized.is_empty() {
                    return Err(format!("{key} enthält einen leeren Eintrag."));
                }
                if !target.contains(&normalized) {
                    target.push(normalized);
                }
            }
        }
        Ok(())
    }

    pub fn is_denied(&self, process: &str, title: Option<&str>) -> bool {
        let process = normalize_process(process);
        // Fail-closed: Ohne lesbaren Prozessnamen (z. B. erhöhte Prozesse, bei denen OpenProcess
        // ACCESS_DENIED liefert) lässt sich nicht prüfen, ob das Fenster sensibel ist. Also gesperrt.
        if process.is_empty() {
            return true;
        }
        if self.processes.iter().any(|p| *p == process) {
            return true;
        }
        match title {
            Some(title) => {
                let lower = title.to_lowercase();
                self.title_substrings.iter().any(|s| lower.contains(s.as_str()))
            }
            None => false,
        }
    }

    /// Titel für die Ausgabe: bei gesperrtem Fenster immer geschwärzt.
    pub fn display_title(&self, process: &str, title: &str) -> String {
        if self.is_denied(process, Some(title)) {
            REDACTED.to_string()
        } else {
            title.to_string()
        }
    }
}

/// Malreihenfolge für das Zusammensetzen einer Gruppe. `EnumWindows` liefert vorne zuerst (Index 0 ist das
/// vorderste Fenster), `image_ops::compose` malt spätere Teile über frühere. Also hintere Fenster (großer
/// Index) zuerst, das vorderste (kleinster Index) zuletzt. Doppelte Indizes werden entfernt.
pub fn sort_composite_indices_back_to_front(indices: &mut Vec<usize>) {
    indices.sort_unstable_by(|a, b| b.cmp(a));
    indices.dedup();
}

pub fn selector_matches(selector: &Selector, hwnd: i64, process: &str, title: &str) -> bool {
    if let Some(expected) = selector.hwnd {
        if expected != hwnd {
            return false;
        }
    }
    if let Some(expected) = &selector.process {
        if normalize_process(expected) != normalize_process(process) {
            return false;
        }
    }
    if let Some(expected) = &selector.title {
        if !title.to_lowercase().contains(&expected.to_lowercase()) {
            return false;
        }
    }
    true
}

pub fn match_spec_matches(spec: &MatchSpec, process: &str, title: &str) -> bool {
    if let Some(expected) = &spec.process {
        if normalize_process(expected) != normalize_process(process) {
            return false;
        }
    }
    if let Some(expected) = &spec.title {
        if !title.to_lowercase().contains(&expected.to_lowercase()) {
            return false;
        }
    }
    true
}

#[derive(Debug, PartialEq, Eq)]
pub enum Selection {
    Found(usize),
    NotFound,
    Ambiguous(Vec<usize>),
}

/// Wählt aus Treffern genau einen Index. Null Treffer bedeutet `NotFound`, mehr als einer `Ambiguous`.
pub fn select(hits: &[bool]) -> Selection {
    let indices: Vec<usize> = hits.iter().enumerate().filter(|(_, hit)| **hit).map(|(i, _)| i).collect();
    match indices.len() {
        0 => Selection::NotFound,
        1 => Selection::Found(indices[0]),
        _ => Selection::Ambiguous(indices),
    }
}

/// Ein Monitor darf nur aufgenommen werden, wenn die Region vollständig darin liegt.
pub fn region_fits_monitor(monitor: &Rect, region: &Rect) -> bool {
    monitor.contains(region)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(l: i32, t: i32, r_: i32, b: i32) -> Rect {
        Rect { l, t, r: r_, b }
    }

    #[test]
    fn base_denylist_blocks_password_managers_and_browser_session_titles() {
        let dl = Denylist::base();
        assert!(dl.is_denied("1Password.exe", Some("Vault")));
        assert!(dl.is_denied("KeePassXC", None));
        assert!(dl.is_denied("msedge.exe", Some("Neuer InPrivate-Tab")));
        assert!(dl.is_denied("chrome", Some("Konto - Incognito")));
        assert!(!dl.is_denied("herdr-night-watch.exe", Some("Herdr-Nachtwächter - Limits")));
    }

    #[test]
    fn denylist_can_only_be_extended() {
        let mut dl = Denylist::base();
        let before = dl.clone();
        dl.extend_from_json(r#"{"processes": ["Beispiel.exe"], "title_substrings": ["Geheim"]}"#).unwrap();
        assert!(dl.is_denied("beispiel", None));
        assert!(dl.is_denied("1password", None), "Basis bleibt erhalten");
        assert!(before.processes.iter().all(|p| dl.processes.contains(p)));
        assert!(dl.processes.len() > before.processes.len());
    }

    #[test]
    fn extra_denylist_rejects_unknown_keys_and_empty_entries() {
        let mut dl = Denylist::base();
        assert!(dl.extend_from_json(r#"{"prozesse": ["x"]}"#).is_err());
        assert!(dl.extend_from_json(r#"{"title_substrings": [""]}"#).is_err());
        assert!(dl.extend_from_json(r#"{"processes": "x"}"#).is_err());
        assert!(dl.extend_from_json("kein json").is_err());
    }

    #[test]
    fn titles_of_denied_windows_are_redacted() {
        let dl = Denylist::base();
        assert_eq!(dl.display_title("1password", "Tresor"), REDACTED);
        assert_eq!(dl.display_title("herdr-night-watch", "Live-Status"), "Live-Status");
    }

    #[test]
    fn window_without_process_name_is_denied_fail_closed() {
        let dl = Denylist::base();
        assert!(dl.is_denied("", Some("Beliebiger Titel")));
        assert!(dl.is_denied("   ", None));
        assert_eq!(dl.display_title("", "Geheim"), REDACTED);
    }

    #[test]
    fn composite_paints_back_windows_first_and_front_window_last() {
        // EnumWindows-Indizes: 0 ist das vorderste Fenster. Doppelte Einträge sollen verschwinden.
        let mut order = vec![0, 2, 1, 2];
        sort_composite_indices_back_to_front(&mut order);
        assert_eq!(order, vec![2, 1, 0]);
        assert_eq!(order.last(), Some(&0), "vorderstes Fenster wird zuletzt gemalt");
    }

    #[test]
    fn selection_requires_exactly_one_hit() {
        assert_eq!(select(&[false, false]), Selection::NotFound);
        assert_eq!(select(&[false, true, false]), Selection::Found(1));
        assert_eq!(select(&[true, false, true]), Selection::Ambiguous(vec![0, 2]));
    }

    #[test]
    fn selector_combines_criteria_with_and() {
        let sel = Selector { title: Some("limits".into()), process: Some("herdr-night-watch.exe".into()), hwnd: None };
        assert!(selector_matches(&sel, 1, "herdr-night-watch", "Herdr - LIMITS"));
        assert!(!selector_matches(&sel, 1, "other", "Herdr - LIMITS"));
        assert!(!selector_matches(&sel, 1, "herdr-night-watch", "Live"));
        let by_hwnd = Selector { hwnd: Some(42), ..Default::default() };
        assert!(selector_matches(&by_hwnd, 42, "x", "y"));
        assert!(!selector_matches(&by_hwnd, 43, "x", "y"));
    }

    #[test]
    fn region_must_be_fully_contained_in_the_window() {
        let window = r(100, 100, 700, 500);
        assert!(window.contains(&r(100, 100, 700, 500)));
        assert!(window.contains(&r(150, 150, 300, 300)));
        assert!(!window.contains(&r(650, 150, 750, 300)), "ragt über den Rand");
        assert!(!window.contains(&r(0, 0, 0, 0)), "leere Region ist nie enthalten");
    }

    #[test]
    fn intersection_is_strict_about_edges() {
        let region = r(0, 0, 10, 10);
        assert!(region.intersects(&r(9, 9, 20, 20)));
        assert!(!region.intersects(&r(10, 0, 20, 10)), "angrenzend ist kein Schnitt");
    }

    #[test]
    fn region_fits_only_a_single_monitor() {
        let monitor = r(0, 0, 1920, 1080);
        assert!(region_fits_monitor(&monitor, &r(0, 0, 1920, 1080)));
        assert!(!region_fits_monitor(&monitor, &r(1900, 0, 2000, 100)));
    }

    #[test]
    fn match_specs_apply_title_and_process_filters() {
        let spec = MatchSpec { process: Some("herdr-night-watch.exe".into()), title: Some("Limits".into()) };
        assert!(match_spec_matches(&spec, "herdr-night-watch", "Herdr - Limits"));
        assert!(!match_spec_matches(&spec, "explorer", "Herdr - Limits"));
        let title_only = MatchSpec { process: None, title: Some("Live".into()) };
        assert!(match_spec_matches(&title_only, "any", "Live-Status"));
    }
}
