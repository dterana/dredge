use std::path::Path;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;

use crate::model::Kind;

const EMBEDDED: &str = include_str!("../rules/apps.toml");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMatch {
    pub app: String,
    pub kind: Kind,
    pub description: String,
}

/// One rule as written in the rules file, before globs are compiled.
#[derive(Debug, Deserialize)]
pub struct RuleDef {
    pub app: String,
    pub kind: Kind,
    pub globs: Vec<String>,
    pub description: String,
}

#[derive(Debug, Deserialize)]
struct RawFile {
    #[serde(default)]
    rule: Vec<RuleDef>,
}

#[derive(Debug)]
struct Rule {
    app: String,
    kind: Kind,
    description: String,
    set: GlobSet,
}

#[derive(Debug, Default)]
pub struct Rules {
    rules: Vec<Rule>,
}

impl Rules {
    /// Parses the embedded rules file. Falls back to empty rules on error;
    /// a test asserts the embedded file parses.
    pub fn embedded() -> Rules {
        Rules::parse(EMBEDDED).unwrap_or_default()
    }

    /// Parses rule definitions without compiling globs. Used by schema tests.
    pub fn parse_defs(text: &str) -> Result<Vec<RuleDef>, String> {
        let raw: RawFile = toml::from_str(text).map_err(|e| e.to_string())?;
        Ok(raw.rule)
    }

    pub fn parse(text: &str) -> Result<Rules, String> {
        let raw: RawFile = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut rules = Vec::new();
        for r in raw.rule {
            let mut b = GlobSetBuilder::new();
            for g in &r.globs {
                let glob = GlobBuilder::new(g)
                    .literal_separator(true)
                    .build()
                    .map_err(|e| format!("{g}: {e}"))?;
                b.add(glob);
            }
            let set = b.build().map_err(|e| e.to_string())?;
            rules.push(Rule {
                app: r.app,
                kind: r.kind,
                description: r.description,
                set,
            });
        }
        Ok(Rules { rules })
    }

    pub fn lookup(&self, home_rel: &Path) -> Option<RuleMatch> {
        self.rules
            .iter()
            .find(|r| r.set.is_match(home_rel))
            .map(|r| RuleMatch {
                app: r.app.clone(),
                kind: r.kind,
                description: r.description.clone(),
            })
    }
}
