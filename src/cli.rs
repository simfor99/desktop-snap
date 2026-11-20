//! Reine CLI-Interpretation ohne Windows-Abhängigkeit (unter Linux testbar).

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selector {
    pub title: Option<String>,
    pub process: Option<String>,
    pub hwnd: Option<i64>,
}

impl Selector {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.process.is_none() && self.hwnd.is_none()
    }
}

/// `--match` für `group`: entweder reiner Titel-Teilstring oder `process=<exe>;title=<substr>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchSpec {
    pub process: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub out: String,
    pub flatten_bg: Option<[u8; 3]>,
    pub extra_denylist: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Version,
    Doctor,
    List { titles: bool, extra_denylist: Option<String> },
    Window { selector: Selector, output: Output },
    Group { matches: Vec<MatchSpec>, output: Output },
    Region {
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        selector: Selector,
        allow_full_monitor: bool,
        output: Output,
    },
    Monitor { index: u32, allow_full_monitor: bool, output: Output },
}

#[derive(Debug, PartialEq, Eq)]
pub struct CliError(pub String);

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(message: impl Into<String>) -> Result<T, CliError> {
    Err(CliError(message.into()))
}

/// Liest Argumente nach dem Programmnamen. `--json` wird als Kompatibilitäts-No-Op akzeptiert.
pub fn parse(args: &[String]) -> Result<Command, CliError> {
    let Some(first) = args.first() else {
        return err("Kein Unterbefehl angegeben (version, doctor, list, window, group, region, monitor).");
    };
    match first.as_str() {
        "--version" | "version" => Ok(Command::Version),
        "doctor" => Ok(Command::Doctor),
        "list" => {
            let flags = Flags::collect(&args[1..], &["--titles", "--json"], &["--extra-denylist"])?;
            Ok(Command::List {
                titles: flags.has("--titles"),
                extra_denylist: flags.value("--extra-denylist").map(str::to_string),
            })
        }
        "window" => {
            let flags = Flags::collect(
                &args[1..],
                &["--json"],
                &["--title", "--process", "--hwnd", "--out", "--flatten-bg", "--extra-denylist"],
            )?;
            let selector = selector_from(&flags)?;
            if selector.is_empty() {
                return err("window braucht --title, --process oder --hwnd.");
            }
            Ok(Command::Window { selector, output: output_from(&flags)? })
        }
        "group" => {
            let flags = Flags::collect(&args[1..], &["--json"], &["--match", "--out", "--flatten-bg", "--extra-denylist"])?;
            let raw = flags.values("--match");
            if raw.is_empty() {
                return err("group braucht mindestens ein --match.");
            }
            let matches = raw.iter().map(|s| parse_match(s)).collect::<Result<Vec<_>, _>>()?;
            Ok(Command::Group { matches, output: output_from(&flags)? })
        }
        "region" => {
            let flags = Flags::collect(
                &args[1..],
                &["--allow-full-monitor", "--json"],
                &["--x", "--y", "--w", "--h", "--title", "--process", "--hwnd", "--out", "--flatten-bg", "--extra-denylist"],
            )?;
            let x = int_flag(&flags, "--x")?;
            let y = int_flag(&flags, "--y")?;
            let w: u32 = uint_flag(&flags, "--w")?;
            let h: u32 = uint_flag(&flags, "--h")?;
            if w == 0 || h == 0 {
                return err("region braucht --w und --h größer als 0.");
            }
            Ok(Command::Region {
                x,
                y,
                w,
                h,
                selector: selector_from(&flags)?,
                allow_full_monitor: flags.has("--allow-full-monitor"),
                output: output_from(&flags)?,
            })
        }
        "monitor" => {
            let flags = Flags::collect(
                &args[1..],
                &["--allow-full-monitor", "--json"],
                &["--index", "--out", "--flatten-bg", "--extra-denylist"],
            )?;
            Ok(Command::Monitor {
                index: uint_flag(&flags, "--index")?,
                allow_full_monitor: flags.has("--allow-full-monitor"),
                output: output_from(&flags)?,
            })
        }
        other => err(format!("Unbekannter Unterbefehl: {other}")),
    }
}

pub fn parse_hex_color(raw: &str) -> Result<[u8; 3], CliError> {
    let digits = raw.strip_prefix('#').unwrap_or(raw);
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return err(format!("--flatten-bg erwartet #RRGGBB, erhalten {raw:?}."));
    }
    let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).map_err(|e| CliError(e.to_string()));
    Ok([byte(0)?, byte(2)?, byte(4)?])
}

fn parse_match(raw: &str) -> Result<MatchSpec, CliError> {
    if let Some(rest) = raw.strip_prefix("process=") {
        let (process, title) = match rest.split_once(";title=") {
            Some((p, t)) => (p.trim(), Some(t.trim())),
            None => (rest.trim(), None),
        };
        if process.is_empty() {
            return err(format!("--match {raw:?}: process= ist leer."));
        }
        return Ok(MatchSpec {
            process: Some(process.to_string()),
            title: title.filter(|t| !t.is_empty()).map(str::to_string),
        });
    }
    if raw.trim().is_empty() {
        return err("--match ist leer.");
    }
    Ok(MatchSpec { process: None, title: Some(raw.to_string()) })
}

fn selector_from(flags: &Flags) -> Result<Selector, CliError> {
    let hwnd = match flags.value("--hwnd") {
        Some(raw) => Some(raw.parse::<i64>().map_err(|_| CliError(format!("--hwnd ist keine Zahl: {raw:?}")))?),
        None => None,
    };
    Ok(Selector {
        title: flags.value("--title").map(str::to_string),
        process: flags.value("--process").map(str::to_string),
        hwnd,
    })
}

fn output_from(flags: &Flags) -> Result<Output, CliError> {
    let out = flags.value("--out").ok_or_else(|| CliError("--out <win_path> fehlt.".into()))?;
    let flatten_bg = match flags.value("--flatten-bg") {
        Some(raw) => Some(parse_hex_color(raw)?),
        None => None,
    };
    Ok(Output {
        out: out.to_string(),
        flatten_bg,
        extra_denylist: flags.value("--extra-denylist").map(str::to_string),
    })
}

fn int_flag(flags: &Flags, name: &str) -> Result<i32, CliError> {
    let raw = flags.value(name).ok_or_else(|| CliError(format!("{name} fehlt.")))?;
    raw.parse().map_err(|_| CliError(format!("{name} ist keine ganze Zahl: {raw:?}")))
}

fn uint_flag<T: std::str::FromStr>(flags: &Flags, name: &str) -> Result<T, CliError> {
    let raw = flags.value(name).ok_or_else(|| CliError(format!("{name} fehlt.")))?;
    raw.parse().map_err(|_| CliError(format!("{name} ist keine gültige Zahl: {raw:?}")))
}

/// Einfacher Flag-Parser: erlaubt Boolesche und Wert-Flags; unbekannte Flags und fehlende Werte sind Fehler.
struct Flags {
    bools: Vec<String>,
    values: Vec<(String, String)>,
}

impl Flags {
    fn collect(args: &[String], bools: &[&str], with_value: &[&str]) -> Result<Flags, CliError> {
        let mut flags = Flags { bools: Vec::new(), values: Vec::new() };
        let mut index = 0;
        while index < args.len() {
            let name = args[index].as_str();
            if bools.contains(&name) {
                flags.bools.push(name.to_string());
                index += 1;
                continue;
            }
            if with_value.contains(&name) {
                let Some(value) = args.get(index + 1) else {
                    return err(format!("{name} braucht einen Wert."));
                };
                if value.starts_with("--") {
                    return err(format!("{name} braucht einen Wert, erhalten {value:?}."));
                }
                flags.values.push((name.to_string(), value.clone()));
                index += 2;
                continue;
            }
            return err(format!("Unbekannte Option: {name}"));
        }
        Ok(flags)
    }

    fn has(&self, name: &str) -> bool {
        self.bools.iter().any(|b| b == name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn values(&self, name: &str) -> Vec<String> {
        self.values.iter().filter(|(n, _)| n == name).map(|(_, v)| v.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn version_and_doctor_parse_without_flags() {
        assert_eq!(parse(&args(&["--version"])), Ok(Command::Version));
        assert_eq!(parse(&args(&["doctor"])), Ok(Command::Doctor));
    }

    #[test]
    fn window_requires_a_selector_and_out() {
        assert!(parse(&args(&["window", "--out", "C:\\x.png"])).is_err());
        assert!(parse(&args(&["window", "--title", "Limits"])).is_err());
        let ok = parse(&args(&["window", "--title", "Limits", "--out", "C:\\x.png"])).unwrap();
        match ok {
            Command::Window { selector, output } => {
                assert_eq!(selector.title.as_deref(), Some("Limits"));
                assert_eq!(output.out, "C:\\x.png");
            }
            other => panic!("unerwartet: {other:?}"),
        }
    }

    #[test]
    fn unknown_flags_and_missing_values_are_rejected() {
        assert!(parse(&args(&["window", "--title", "x", "--out", "o.png", "--bogus"])).is_err());
        assert!(parse(&args(&["window", "--title", "--out", "o.png"])).is_err());
        assert!(parse(&args(&["window", "--title"])).is_err());
    }

    #[test]
    fn group_accepts_title_and_process_match_specs() {
        let cmd = parse(&args(&[
            "group", "--match", "Live-Status", "--match", "process=herdr-night-watch.exe;title=Limits", "--out", "o.png",
        ]))
        .unwrap();
        match cmd {
            Command::Group { matches, .. } => {
                assert_eq!(matches[0], MatchSpec { process: None, title: Some("Live-Status".into()) });
                assert_eq!(
                    matches[1],
                    MatchSpec { process: Some("herdr-night-watch.exe".into()), title: Some("Limits".into()) }
                );
            }
            other => panic!("unerwartet: {other:?}"),
        }
    }

    #[test]
    fn region_numbers_are_checked_and_zero_size_is_refused() {
        let ok = parse(&args(&["region", "--x", "-10", "--y", "5", "--w", "590", "--h", "285", "--out", "o.png"]));
        assert!(matches!(ok, Ok(Command::Region { x: -10, y: 5, w: 590, h: 285, .. })));
        assert!(parse(&args(&["region", "--x", "0", "--y", "0", "--w", "0", "--h", "1", "--out", "o.png"])).is_err());
        assert!(parse(&args(&["region", "--x", "a", "--y", "0", "--w", "1", "--h", "1", "--out", "o.png"])).is_err());
    }

    #[test]
    fn hex_colors_are_strict() {
        assert_eq!(parse_hex_color("#1a2B3c"), Ok([0x1a, 0x2b, 0x3c]));
        assert!(parse_hex_color("#12345").is_err());
        assert!(parse_hex_color("#GG0000").is_err());
    }

    #[test]
    fn list_titles_flag_is_parsed() {
        assert_eq!(parse(&args(&["list"])), Ok(Command::List { titles: false, extra_denylist: None }));
        assert_eq!(
            parse(&args(&["list", "--titles", "--json"])),
            Ok(Command::List { titles: true, extra_denylist: None })
        );
    }

    #[test]
    fn list_accepts_extra_denylist_and_rejects_a_missing_value() {
        assert_eq!(
            parse(&args(&["list", "--titles", "--extra-denylist", "C:\\Temp\\dl.json"])),
            Ok(Command::List { titles: true, extra_denylist: Some("C:\\Temp\\dl.json".into()) })
        );
        assert!(parse(&args(&["list", "--extra-denylist"])).is_err());
    }
}
