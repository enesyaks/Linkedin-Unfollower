//! The company list, as exported by `web/unfollow.js` (`LIU.export()`).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::curl::percent_encode;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Company {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub urn: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Export {
    companies: Vec<Company>,
}

impl Company {
    /// Stable identity used for de-duplication and for the resume checkpoint.
    pub fn key(&self) -> String {
        if let Some(urn) = self.urn.as_deref().filter(|s| !s.is_empty()) {
            return urn.to_string();
        }
        if let Some(slug) = self.slug.as_deref().filter(|s| !s.is_empty()) {
            return format!("slug:{slug}");
        }
        format!("name:{}", self.label())
    }

    pub fn label(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.slug.clone())
            .or_else(|| self.urn.clone())
            .unwrap_or_else(|| "(unnamed)".to_string())
    }

    /// Numeric company id, taken from the field or recovered from the URN.
    pub fn numeric_id(&self) -> Option<String> {
        if let Some(id) = self.id.as_deref().filter(|s| !s.is_empty()) {
            return Some(id.to_string());
        }
        let urn = self.urn.as_deref()?;
        urn.rsplit(':')
            .next()
            .filter(|s| s.chars().all(|c| c.is_ascii_digit()))
            .map(str::to_string)
    }

    /// Values substituted into the request template.
    pub fn vars(&self) -> HashMap<&'static str, String> {
        let mut vars = HashMap::new();
        if let Some(urn) = &self.urn {
            vars.insert("urn", urn.clone());
            vars.insert("urn_enc", percent_encode(urn));
        }
        if let Some(id) = self.numeric_id() {
            vars.insert("id", id);
        }
        if let Some(slug) = &self.slug {
            vars.insert("slug", slug.clone());
        }
        vars.insert("name", self.label());
        vars
    }

    /// Whether this entry can fill every placeholder the template needs.
    pub fn missing_for(&self, placeholders: &[String]) -> Vec<String> {
        let vars = self.vars();
        let mut missing = Vec::new();
        for p in placeholders {
            if !vars.contains_key(p.as_str()) {
                missing.push(p.clone());
            }
        }
        missing
    }
}

/// Reads the export file. Accepts the full `LIU.export()` object, a bare array
/// of companies, or a plain text file with one URN per line.
pub fn load(path: &Path) -> Result<Vec<Company>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read company list: {}", path.display()))?;
    parse(&raw)
}

pub fn parse(raw: &str) -> Result<Vec<Company>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("the company list is empty");
    }

    let companies = if trimmed.starts_with('{') {
        serde_json::from_str::<Export>(trimmed)
            .context("not a LIU.export() object — expected a top-level \"companies\" array")?
            .companies
    } else if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<Company>>(trimmed).context("not an array of companies")?
    } else {
        trimmed
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| Company {
                urn: Some(l.to_string()),
                ..Default::default()
            })
            .collect()
    };

    if companies.is_empty() {
        bail!("the company list parsed fine but contains no entries");
    }

    // De-duplicate, keeping the first occurrence and its richer fields.
    let mut seen = std::collections::HashSet::new();
    let deduped: Vec<Company> = companies
        .into_iter()
        .filter(|c| seen.insert(c.key()))
        .collect();

    Ok(deduped)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPORT: &str = r#"{
      "tool": "linkedin-unfollower",
      "version": "0.1.0",
      "companies": [
        {"name":"Acme","slug":"acme","urn":"urn:li:fsd_company:1001","id":"1001","url":"https://x/1"},
        {"name":"Globex","slug":"globex","urn":"urn:li:fsd_company:1002","id":null,"url":null},
        {"name":"Acme","slug":"acme","urn":"urn:li:fsd_company:1001"},
        {"name":"NoUrn","slug":"nourn","urn":null}
      ]
    }"#;

    #[test]
    fn parses_the_export_object_and_dedupes() {
        let list = parse(EXPORT).unwrap();
        assert_eq!(list.len(), 3, "the duplicate Acme must collapse");
        assert_eq!(list[0].label(), "Acme");
    }

    #[test]
    fn recovers_the_numeric_id_from_the_urn() {
        let list = parse(EXPORT).unwrap();
        assert_eq!(list[1].id, None);
        assert_eq!(list[1].numeric_id().as_deref(), Some("1002"));
        assert_eq!(list[2].numeric_id(), None);
    }

    #[test]
    fn accepts_a_bare_array() {
        let list = parse(r#"[{"urn":"urn:li:fsd_company:7"}]"#).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].key(), "urn:li:fsd_company:7");
    }

    #[test]
    fn accepts_one_urn_per_line_with_comments() {
        let list = parse("# mine\nurn:li:fsd_company:1\n\nurn:li:fsd_company:2\n").unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].urn.as_deref(), Some("urn:li:fsd_company:2"));
    }

    #[test]
    fn rejects_empty_and_malformed_input() {
        assert!(parse("").is_err());
        assert!(parse("{}").is_err());
        assert!(parse(r#"{"companies":[]}"#).is_err());
        assert!(parse("[{").is_err());
    }

    #[test]
    fn vars_cover_every_placeholder_when_data_is_complete() {
        let list = parse(EXPORT).unwrap();
        let vars = list[0].vars();
        assert_eq!(vars["urn"], "urn:li:fsd_company:1001");
        assert_eq!(vars["urn_enc"], "urn%3Ali%3Afsd_company%3A1001");
        assert_eq!(vars["id"], "1001");
        assert_eq!(vars["slug"], "acme");
        assert_eq!(vars["name"], "Acme");
    }

    #[test]
    fn missing_for_flags_entries_the_template_cannot_use() {
        let list = parse(EXPORT).unwrap();
        let needs = vec!["urn".to_string(), "id".to_string()];
        assert!(list[0].missing_for(&needs).is_empty());
        assert_eq!(list[2].missing_for(&needs), vec!["urn", "id"]);
    }

    #[test]
    fn key_falls_back_from_urn_to_slug_to_name() {
        let by_slug = Company {
            slug: Some("acme".into()),
            ..Default::default()
        };
        assert_eq!(by_slug.key(), "slug:acme");
        let by_name = Company {
            name: Some("Acme".into()),
            ..Default::default()
        };
        assert_eq!(by_name.key(), "name:Acme");
        assert_eq!(Company::default().key(), "name:(unnamed)");
    }
}
