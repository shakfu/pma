//! The hand-set half of a project's record -- its tier and its tags -- as a
//! file, so a portfolio is retiered in one edit rather than one command per
//! project. Everything else in the record is written by a scan.

use std::path::Path;

/// One project's editable fields.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub tier: Option<u8>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Format {
    Csv,
    Json,
}

impl Format {
    /// From the extension alone, so writing and reading agree without looking
    /// at the contents.
    pub fn of(path: &Path) -> Result<Format, String> {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref()
        {
            Some("csv") => Ok(Format::Csv),
            Some("json") => Ok(Format::Json),
            _ => Err(format!(
                "{}: name the file `.csv` or `.json`",
                path.display()
            )),
        }
    }
}

/// A tag is one lowercase word, in a file as on the command line.
pub fn normal_tag(tag: &str) -> Result<String, String> {
    let tag = tag.trim().to_lowercase();
    if tag.is_empty() || tag.split_whitespace().count() > 1 {
        return Err("a tag is one word".into());
    }
    Ok(tag)
}

fn tier_of(text: &str, name: &str) -> Result<Option<u8>, String> {
    match text.trim() {
        "" | "none" => Ok(None),
        t => t
            .parse::<u8>()
            .ok()
            .filter(|t| (1..=5).contains(t))
            .map(Some)
            .ok_or_else(|| format!("{name}: tier `{t}` is not 1 to 5, or none")),
    }
}

pub fn write(entries: &[Entry], format: Format) -> Result<String, String> {
    match format {
        Format::Csv => to_csv(entries),
        Format::Json => Ok(to_json(entries)),
    }
}

pub fn read(text: &str, format: Format) -> Result<Vec<Entry>, String> {
    let entries = match format {
        Format::Csv => from_csv(text)?,
        Format::Json => from_json(text)?,
    };
    for (i, e) in entries.iter().enumerate() {
        if entries[..i].iter().any(|d| d.name == e.name) {
            return Err(format!("`{}` appears twice", e.name));
        }
    }
    Ok(entries)
}

/// `name,tier,tag,tag,...`: tags are the remaining fields, so nothing needs
/// quoting and a row is editable by hand. A name holding a comma has no such
/// row, and is refused rather than written ambiguously.
fn to_csv(entries: &[Entry]) -> Result<String, String> {
    let mut out = String::from("name,tier,tags\n");
    for e in entries {
        if e.name.contains(',') {
            return Err(format!("`{}` holds a comma; export to .json", e.name));
        }
        out.push_str(&e.name);
        out.push(',');
        if let Some(t) = e.tier {
            out.push_str(&t.to_string());
        }
        for tag in &e.tags {
            out.push(',');
            out.push_str(tag);
        }
        out.push('\n');
    }
    Ok(out)
}

fn from_csv(text: &str) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split(',').map(str::trim);
        let name = fields.next().unwrap_or_default();
        let second = fields.next().unwrap_or_default();
        if name == "name" && second == "tier" {
            continue;
        }
        if name.is_empty() {
            return Err(format!("line {}: no project name", n + 1));
        }
        let tier = tier_of(second, name)?;
        let tags = fields
            .filter(|t| !t.is_empty())
            .map(normal_tag)
            .collect::<Result<Vec<String>, String>>()
            .map_err(|e| format!("{name}: {e}"))?;
        entries.push(Entry {
            name: name.into(),
            tier,
            tags,
        });
    }
    Ok(entries)
}

fn to_json(entries: &[Entry]) -> String {
    let rows: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "tier": e.tier,
                "tags": e.tags,
            })
        })
        .collect();
    format!("{}\n", serde_json::Value::Array(rows))
}

fn from_json(text: &str) -> Result<Vec<Entry>, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let rows = value.as_array().ok_or("expected a list of projects")?;
    let mut entries = Vec::new();
    for row in rows {
        let name = row
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or("a project has no `name`")?;
        let tier = match row.get("tier") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::Number(t)) => tier_of(&t.to_string(), name)?,
            Some(serde_json::Value::String(t)) => tier_of(t, name)?,
            Some(_) => return Err(format!("{name}: tier is a number, a string, or null")),
        };
        let tags = match row.get("tags") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(tags)) => tags
                .iter()
                .map(|t| {
                    t.as_str()
                        .ok_or_else(|| format!("{name}: a tag is a string"))
                        .and_then(|t| normal_tag(t).map_err(|e| format!("{name}: {e}")))
                })
                .collect::<Result<Vec<String>, String>>()?,
            Some(_) => return Err(format!("{name}: tags are a list of strings")),
        };
        entries.push(Entry {
            name: name.into(),
            tier,
            tags,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<Entry> {
        vec![
            Entry {
                name: "alpha".into(),
                tier: Some(1),
                tags: vec!["rust".into(), "cli".into()],
            },
            Entry {
                name: "beta".into(),
                tier: None,
                tags: Vec::new(),
            },
        ]
    }

    #[test]
    fn a_csv_row_is_name_tier_then_tags() {
        assert_eq!(
            to_csv(&entries()).unwrap(),
            "name,tier,tags\nalpha,1,rust,cli\nbeta,\n"
        );
    }

    #[test]
    fn both_formats_read_back_what_they_wrote() {
        for format in [Format::Csv, Format::Json] {
            let text = write(&entries(), format).unwrap();
            assert_eq!(read(&text, format).unwrap(), entries(), "{text}");
        }
    }

    /// The file is edited by hand, so the forms a hand leaves are accepted.
    #[test]
    fn spacing_and_none_and_blank_lines_are_read() {
        let text = "name,tier,tags\n\nalpha, 1 , rust\nbeta,none\n# a note\ngamma,\n";
        let read = read(text, Format::Csv).unwrap();
        assert_eq!(read[0].tags, ["rust"]);
        assert_eq!((read[1].tier, read[2].tier), (None, None));
    }

    #[test]
    fn a_tier_outside_one_to_five_is_refused() {
        let err = read("alpha,6\n", Format::Csv).unwrap_err();
        assert!(err.contains("not 1 to 5"), "{err}");
        assert!(
            read(r#"[{"name": "alpha", "tier": 9}]"#, Format::Json).is_err(),
            "json takes the same range"
        );
    }

    #[test]
    fn a_repeated_project_is_refused() {
        let err = read("alpha,1\nalpha,2\n", Format::Csv).unwrap_err();
        assert!(err.contains("twice"), "{err}");
    }

    /// Writing an ambiguous row would lose a project on the way back.
    #[test]
    fn a_name_holding_a_comma_is_refused_in_csv() {
        let odd = [Entry {
            name: "a,b".into(),
            tier: None,
            tags: Vec::new(),
        }];
        assert!(to_csv(&odd).is_err());
        assert!(write(&odd, Format::Json).is_ok());
    }

    #[test]
    fn json_takes_null_and_missing_fields() {
        let text = r#"[{"name": "alpha"}, {"name": "beta", "tier": null, "tags": null}]"#;
        assert_eq!(read(text, Format::Json).unwrap().len(), 2);
    }

    #[test]
    fn the_extension_picks_the_format() {
        assert_eq!(Format::of(Path::new("a/b.CSV")), Ok(Format::Csv));
        assert_eq!(Format::of(Path::new("b.json")), Ok(Format::Json));
        assert!(Format::of(Path::new("b.txt")).is_err());
    }
}
