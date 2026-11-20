//! desktop-snap: WGC-only Fenster- und Monitor-Screenshots mit eingebauter Sicherheitsrichtlinie.
//! Jeder Ausgang druckt genau ein JSON-Objekt auf stdout und endet mit dem Exit-Code aus `receipt::Exit`.

mod cli;
mod image_ops;
mod platform;
mod policy;
mod receipt;
#[cfg(windows)]
mod win;

use cli::{Command, MatchSpec, Output, Selector};
use platform::{CaptureError, Frame, RawWindow};
use policy::{Denylist, Rect, Selection};
use receipt::{error, Exit, Receipt};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const VERSION: &str = "0.1.0";
const BUILD_HASH: &str = env!("DESKTOP_SNAP_BUILD_HASH");
const CAPTURE_TIMEOUT: Duration = Duration::from_millis(2000);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (exit, value) = run(&args);
    println!("{}", serde_json::to_string(&value).unwrap_or_else(|_| "{\"ok\":false}".into()));
    std::process::exit(exit.code());
}

fn run(args: &[String]) -> (Exit, Value) {
    let command = match cli::parse(args) {
        Ok(command) => command,
        Err(e) => return error(Exit::RuntimeError, "runtime_error", &e.0),
    };
    match command {
        Command::Version => (
            Exit::Ok,
            json!({"ok": true, "exit_code": 0, "error_code": null, "version": VERSION, "build_hash": BUILD_HASH}),
        ),
        Command::Doctor => doctor(),
        Command::List { titles, extra_denylist } => list(titles, extra_denylist.as_deref()),
        Command::Window { selector, output } => window(&selector, &output),
        Command::Group { matches, output } => group(&matches, &output),
        Command::Region { x, y, w, h, selector, allow_full_monitor, output } => {
            region(Rect::from_xywh(x, y, w, h), &selector, allow_full_monitor, &output)
        }
        Command::Monitor { index, allow_full_monitor, output } => monitor(index, allow_full_monitor, &output),
    }
}

fn load_denylist(extra: Option<&str>) -> Result<Denylist, (Exit, Value)> {
    let mut denylist = Denylist::base();
    if let Some(path) = extra {
        let text = std::fs::read_to_string(path)
            .map_err(|e| error(Exit::RuntimeError, "runtime_error", &format!("Denylist nicht lesbar: {e}")))?;
        denylist
            .extend_from_json(&text)
            .map_err(|e| error(Exit::RuntimeError, "runtime_error", &e))?;
    }
    Ok(denylist)
}

fn list_windows_or_error() -> Result<Vec<RawWindow>, (Exit, Value)> {
    platform::list_windows().map_err(|e| error(Exit::RuntimeError, "runtime_error", &e))
}

fn window_json(window: &RawWindow) -> Value {
    json!({"hwnd": window.hwnd, "process": window.process, "rect": [window.rect.l, window.rect.t, window.rect.r, window.rect.b]})
}

/// Sichtbare, nicht verdeckte Fenster mit echter Fläche. Nur diese dürfen überhaupt gewählt werden.
fn selectable(window: &RawWindow) -> bool {
    window.visible && !window.cloaked && !window.rect.is_empty()
}

fn capture_error(error_kind: CaptureError) -> (Exit, Value) {
    match error_kind {
        CaptureError::Denied => error(Exit::CaptureDenied, "capture_denied", "WGC verweigert den Zugriff; kein Fallback."),
        CaptureError::NoFrame => error(Exit::NoFrame, "no_frame", "Kein WGC-Frame innerhalb des Zeitlimits."),
        CaptureError::Minimized => error(Exit::NoFrame, "window_minimized", "Zielfenster ist minimiert."),
        CaptureError::Runtime(message) => error(Exit::RuntimeError, "runtime_error", &message),
    }
}

fn ambiguous(windows: &[RawWindow], indices: &[usize]) -> (Exit, Value) {
    let candidates: Vec<Value> = indices.iter().map(|&i| window_json(&windows[i])).collect();
    let receipt = Receipt { candidates, ..Default::default() };
    (Exit::AmbiguousMatch, receipt.render(Exit::AmbiguousMatch, Some("ambiguous_match"), "Mehrere Fenster passen; bitte eindeutiger wählen."))
}

fn pick_one(windows: &[RawWindow], selector: &Selector) -> Result<usize, (Exit, Value)> {
    let hits: Vec<bool> = windows
        .iter()
        .map(|w| selectable(w) && policy::selector_matches(selector, w.hwnd, &w.process, &w.title))
        .collect();
    match policy::select(&hits) {
        Selection::Found(index) => Ok(index),
        Selection::NotFound => Err(error(Exit::NotFound, "not_found", "Kein passendes sichtbares Fenster gefunden.")),
        Selection::Ambiguous(indices) => Err(ambiguous(windows, &indices)),
    }
}

/// Gemeinsamer Abschluss: Metriken auf dem Originalbild, Flat-Prüfung, optionales Abflachen, PNG schreiben.
fn finish(rgba: Vec<u8>, width: u32, height: u32, output: &Output, mut receipt: Receipt, backend: &'static str) -> (Exit, Value) {
    let metrics = image_ops::metrics(&rgba);
    receipt.backend = Some(backend);
    receipt.width = Some(width);
    receipt.height = Some(height);
    receipt.distinct_colors = Some(metrics.distinct_colors);
    receipt.top_color_share = Some(metrics.top_color_share);
    receipt.flat = Some(image_ops::is_flat(&metrics));
    if image_ops::is_flat(&metrics) {
        let value = receipt.render(Exit::FlatImage, Some("flat_image"), "Bild ist flach; keine Datei geschrieben.");
        return (Exit::FlatImage, value);
    }
    let pixels = match output.flatten_bg {
        Some(bg) => image_ops::flatten_over(&rgba, bg),
        None => rgba,
    };
    let png = match image_ops::encode_png(&pixels, width, height) {
        Ok(png) => png,
        Err(e) => return error(Exit::RuntimeError, "runtime_error", &e),
    };
    if let Err(e) = std::fs::write(&output.out, png) {
        return error(Exit::RuntimeError, "runtime_error", &format!("Schreiben nach {} fehlgeschlagen: {e}", output.out));
    }
    (Exit::Ok, receipt.render(Exit::Ok, None, "Capture erfolgreich."))
}

fn rgba_of(frame: &Frame) -> Vec<u8> {
    image_ops::unpremultiply_bgra(&frame.bgra)
}

fn doctor() -> (Exit, Value) {
    let dpi = platform::set_dpi_awareness();
    let wgc = platform::wgc_supported();
    let exit = if wgc { Exit::Ok } else { Exit::RuntimeError };
    let value = json!({
        "ok": wgc,
        "exit_code": exit.code(),
        "error_code": if wgc { Value::Null } else { json!("runtime_error") },
        "version": VERSION,
        "build_hash": BUILD_HASH,
        "wgc_supported": wgc,
        "border_control_supported": platform::border_control_supported(),
        "dpi_awareness": if dpi { "PerMonitorV2" } else { "unknown" },
    });
    (exit, value)
}

fn list(titles: bool, extra_denylist: Option<&str>) -> (Exit, Value) {
    let denylist = match load_denylist(extra_denylist) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let windows = match list_windows_or_error() {
        Ok(w) => w,
        Err(e) => return e,
    };
    let receipt = Receipt { windows: list_entries(&windows, &denylist, titles), ..Default::default() };
    (Exit::Ok, receipt.render(Exit::Ok, None, "Fensterliste."))
}

/// Eintrags-Liste für `list`. Titel werden nur mit `--titles` ausgegeben und immer durch die Denylist geschwärzt.
fn list_entries(windows: &[RawWindow], denylist: &Denylist, titles: bool) -> Vec<Value> {
    windows
        .iter()
        .map(|w| {
            let mut entry = window_json(w);
            entry["visible"] = json!(w.visible);
            entry["minimized"] = json!(w.minimized);
            entry["cloaked"] = json!(w.cloaked);
            if titles && !w.title.is_empty() {
                entry["title"] = json!(denylist.display_title(&w.process, &w.title));
            }
            entry
        })
        .collect()
}

fn window(selector: &Selector, output: &Output) -> (Exit, Value) {
    let started = Instant::now();
    let denylist = match load_denylist(output.extra_denylist.as_deref()) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let windows = match list_windows_or_error() {
        Ok(w) => w,
        Err(e) => return e,
    };
    let index = match pick_one(&windows, selector) {
        Ok(i) => i,
        Err(e) => return e,
    };
    let target = &windows[index];
    if denylist.is_denied(&target.process, Some(&target.title)) {
        let receipt = Receipt { windows: vec![window_json(target)], ..Default::default() };
        return (Exit::DeniedSensitive, receipt.render(Exit::DeniedSensitive, Some("denied_sensitive_window"), "Fenster ist gesperrt; 0 Pixel geschrieben."));
    }
    if target.minimized {
        return capture_error(CaptureError::Minimized);
    }
    let frame = match platform::capture_window(target.hwnd, CAPTURE_TIMEOUT) {
        Ok(frame) => frame,
        Err(e) => return capture_error(e),
    };
    let receipt = Receipt {
        duration_ms: Some(started.elapsed().as_millis()),
        windows: vec![window_json(target)],
        ..Default::default()
    };
    finish(rgba_of(&frame), frame.width, frame.height, output, receipt, "wgc-window")
}

fn group(matches: &[MatchSpec], output: &Output) -> (Exit, Value) {
    let started = Instant::now();
    let denylist = match load_denylist(output.extra_denylist.as_deref()) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let windows = match list_windows_or_error() {
        Ok(w) => w,
        Err(e) => return e,
    };
    let mut chosen: Vec<usize> = Vec::new();
    for spec in matches {
        let hits: Vec<bool> = windows
            .iter()
            .map(|w| selectable(w) && policy::match_spec_matches(spec, &w.process, &w.title))
            .collect();
        match policy::select(&hits) {
            Selection::Found(index) => chosen.push(index),
            Selection::NotFound => return error(Exit::NotFound, "not_found", "Ein --match findet kein Fenster."),
            Selection::Ambiguous(indices) => return ambiguous(&windows, &indices),
        }
    }
    policy::sort_composite_indices_back_to_front(&mut chosen);

    if let Some(&denied) = chosen.iter().find(|&&i| denylist.is_denied(&windows[i].process, Some(&windows[i].title))) {
        let receipt = Receipt { windows: vec![window_json(&windows[denied])], ..Default::default() };
        return (Exit::DeniedSensitive, receipt.render(Exit::DeniedSensitive, Some("denied_sensitive_window"), "Gruppe enthält ein gesperrtes Fenster; 0 Pixel geschrieben."));
    }
    if chosen.iter().any(|&i| windows[i].minimized) {
        return capture_error(CaptureError::Minimized);
    }

    let mut frames: Vec<(Rect, Frame)> = Vec::new();
    for &index in &chosen {
        match platform::capture_window(windows[index].hwnd, CAPTURE_TIMEOUT) {
            Ok(frame) => frames.push((windows[index].rect, frame)),
            Err(e) => return capture_error(e),
        }
    }
    let union = frames.iter().fold(None::<Rect>, |acc, (r, _)| match acc {
        None => Some(*r),
        Some(a) => Some(Rect { l: a.l.min(r.l), t: a.t.min(r.t), r: a.r.max(r.r), b: a.b.max(r.b) }),
    });
    let Some(union) = union else {
        return error(Exit::NotFound, "not_found", "Gruppe ist leer.");
    };
    let rgba_parts: Vec<(usize, usize, usize, usize, Vec<u8>)> = frames
        .iter()
        .map(|(rect, frame)| {
            (
                (rect.l - union.l) as usize,
                (rect.t - union.t) as usize,
                frame.width as usize,
                frame.height as usize,
                rgba_of(frame),
            )
        })
        .collect();
    let borrowed: Vec<(usize, usize, usize, usize, &[u8])> =
        rgba_parts.iter().map(|(x, y, w, h, px)| (*x, *y, *w, *h, px.as_slice())).collect();
    let canvas = match image_ops::compose(union.width() as usize, union.height() as usize, &borrowed) {
        Ok(c) => c,
        Err(e) => return error(Exit::RuntimeError, "runtime_error", &e),
    };
    let receipt = Receipt {
        duration_ms: Some(started.elapsed().as_millis()),
        windows: chosen.iter().map(|&i| window_json(&windows[i])).collect(),
        ..Default::default()
    };
    finish(canvas, union.width() as u32, union.height() as u32, output, receipt, "wgc-composite")
}

fn region(region: Rect, selector: &Selector, allow_full_monitor: bool, output: &Output) -> (Exit, Value) {
    let started = Instant::now();
    let denylist = match load_denylist(output.extra_denylist.as_deref()) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let windows = match list_windows_or_error() {
        Ok(w) => w,
        Err(e) => return e,
    };

    if allow_full_monitor {
        let monitors = match platform::list_monitors() {
            Ok(m) => m,
            Err(e) => return error(Exit::RuntimeError, "runtime_error", &e),
        };
        let Some(monitor) = monitors.iter().find(|m| policy::region_fits_monitor(&m.rect, &region)) else {
            return error(Exit::PolicyDenied, "policy_denied", "Region liegt nicht vollständig auf einem Monitor.");
        };
        if let Some(blocked) = windows.iter().find(|w| {
            selectable(w) && w.rect.intersects(&region) && denylist.is_denied(&w.process, Some(&w.title))
        }) {
            let receipt = Receipt { windows: vec![window_json(blocked)], ..Default::default() };
            return (Exit::DeniedSensitive, receipt.render(Exit::DeniedSensitive, Some("denied_sensitive_window"), "Ein gesperrtes Fenster schneidet die Region; 0 Pixel geschrieben."));
        }
        let frame = match platform::capture_monitor(monitor.handle, CAPTURE_TIMEOUT) {
            Ok(f) => f,
            Err(e) => return capture_error(e),
        };
        let x = (region.l - monitor.rect.l) as usize;
        let y = (region.t - monitor.rect.t) as usize;
        let cropped = match image_ops::crop(&rgba_of(&frame), frame.width as usize, frame.height as usize, x, y, region.width() as usize, region.height() as usize) {
            Ok(c) => c,
            Err(e) => return error(Exit::RuntimeError, "runtime_error", &e),
        };
        let receipt = Receipt { duration_ms: Some(started.elapsed().as_millis()), ..Default::default() };
        return finish(cropped, region.width() as u32, region.height() as u32, output, receipt, "wgc-monitor");
    }

    if selector.is_empty() {
        return error(Exit::PolicyDenied, "policy_denied", "region ohne --allow-full-monitor braucht --process oder --hwnd.");
    }
    let index = match pick_one(&windows, selector) {
        Ok(i) => i,
        Err(e) => return e,
    };
    let target = &windows[index];
    if denylist.is_denied(&target.process, Some(&target.title)) {
        let receipt = Receipt { windows: vec![window_json(target)], ..Default::default() };
        return (Exit::DeniedSensitive, receipt.render(Exit::DeniedSensitive, Some("denied_sensitive_window"), "Fenster ist gesperrt; 0 Pixel geschrieben."));
    }
    if !target.rect.contains(&region) {
        return error(Exit::PolicyDenied, "policy_denied", "Region liegt nicht vollständig im Zielfenster.");
    }
    if target.minimized {
        return capture_error(CaptureError::Minimized);
    }
    let frame = match platform::capture_window(target.hwnd, CAPTURE_TIMEOUT) {
        Ok(f) => f,
        Err(e) => return capture_error(e),
    };
    let x = (region.l - target.rect.l) as usize;
    let y = (region.t - target.rect.t) as usize;
    let cropped = match image_ops::crop(&rgba_of(&frame), frame.width as usize, frame.height as usize, x, y, region.width() as usize, region.height() as usize) {
        Ok(c) => c,
        Err(e) => return error(Exit::RuntimeError, "runtime_error", &e),
    };
    let receipt = Receipt {
        duration_ms: Some(started.elapsed().as_millis()),
        windows: vec![window_json(target)],
        ..Default::default()
    };
    finish(cropped, region.width() as u32, region.height() as u32, output, receipt, "wgc-window")
}

fn monitor(index: u32, allow_full_monitor: bool, output: &Output) -> (Exit, Value) {
    let started = Instant::now();
    if !allow_full_monitor {
        return error(Exit::PolicyDenied, "policy_denied", "monitor braucht --allow-full-monitor.");
    }
    let denylist = match load_denylist(output.extra_denylist.as_deref()) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let monitors = match platform::list_monitors() {
        Ok(m) => m,
        Err(e) => return error(Exit::RuntimeError, "runtime_error", &e),
    };
    let Some(monitor) = monitors.get(index as usize) else {
        return error(Exit::RuntimeError, "runtime_error", &format!("Monitor-Index {index} existiert nicht."));
    };
    let windows = match list_windows_or_error() {
        Ok(w) => w,
        Err(e) => return e,
    };
    if let Some(blocked) = windows.iter().find(|w| {
        selectable(w) && w.rect.intersects(&monitor.rect) && denylist.is_denied(&w.process, Some(&w.title))
    }) {
        let receipt = Receipt { windows: vec![window_json(blocked)], ..Default::default() };
        return (Exit::DeniedSensitive, receipt.render(Exit::DeniedSensitive, Some("denied_sensitive_window"), "Ein gesperrtes Fenster liegt auf dem Monitor; 0 Pixel geschrieben."));
    }
    let frame = match platform::capture_monitor(monitor.handle, CAPTURE_TIMEOUT) {
        Ok(f) => f,
        Err(e) => return capture_error(e),
    };
    let receipt = Receipt { duration_ms: Some(started.elapsed().as_millis()), ..Default::default() };
    finish(rgba_of(&frame), frame.width, frame.height, output, receipt, "wgc-monitor")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(process: &str, title: &str) -> RawWindow {
        RawWindow {
            hwnd: 1,
            process: process.into(),
            title: title.into(),
            rect: Rect { l: 0, t: 0, r: 10, b: 10 },
            visible: true,
            minimized: false,
            cloaked: false,
        }
    }

    #[test]
    fn list_titles_redact_entries_from_the_extra_denylist() {
        // Gleicher Weg wie `load_denylist(Some(pfad))`: Base-Denylist plus JSON-Erweiterung aus der Datei.
        let mut denylist = Denylist::base();
        denylist.extend_from_json(r#"{"processes":["meinprozess"]}"#).unwrap();
        let windows = vec![window("meinprozess", "Geheimer Titel"), window("explorer", "Ordner")];
        let entries = list_entries(&windows, &denylist, true);
        assert_eq!(entries[0]["title"], json!("[REDACTED]"));
        assert_eq!(entries[1]["title"], json!("Ordner"));
    }

    #[test]
    fn list_titles_are_redacted_for_windows_without_process_name() {
        let windows = vec![window("", "Titel eines erhöhten Prozesses")];
        let entries = list_entries(&windows, &Denylist::base(), true);
        assert_eq!(entries[0]["title"], json!("[REDACTED]"));
    }

    #[test]
    fn list_without_titles_omits_the_title_key() {
        let windows = vec![window("explorer", "Ordner")];
        let entries = list_entries(&windows, &Denylist::base(), false);
        assert!(entries[0].get("title").is_none());
    }
}
