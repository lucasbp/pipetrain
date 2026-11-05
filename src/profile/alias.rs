use anyhow::{Result, bail};
use toml::{Table, Value};

use super::Profile;

const URL_PREFIXES: [&str; 2] = ["https://github.com/", "http://github.com/"];

#[derive(Debug, Clone, Default)]
pub struct OrgAliases {
    pairs: Vec<(String, String)>,
}

impl OrgAliases {
    pub fn parse(shared: &mut Table) -> Result<Self> {
        let Some(value) = shared.remove("org_alias") else {
            return Ok(Self::default());
        };
        let Value::Table(all) = value else {
            bail!("`org_alias` must be a table of alias = \"real-org-name\" entries");
        };
        let mut pairs: Vec<(String, String)> = Vec::new();
        for (alias, target) in all {
            if alias.trim().is_empty() {
                bail!("`org_alias` has an empty alias");
            }
            let Value::String(real) = target else {
                bail!("`org_alias.{alias}` must be a string with the real org name");
            };
            if real.trim().is_empty() {
                bail!("`org_alias.{alias}` is empty; it must name the real org");
            }
            if let Some((other, _)) = pairs.iter().find(|(a, _)| a.eq_ignore_ascii_case(&alias)) {
                bail!(
                    "org alias `{alias}` is defined twice (also as `{other}`; aliases are case-insensitive)"
                );
            }
            pairs.push((alias, real));
        }
        for (alias, real) in &pairs {
            if alias.eq_ignore_ascii_case(real) {
                bail!("org alias `{alias}` points to itself");
            }
            if let Some((other, _)) = pairs.iter().find(|(a, _)| a.eq_ignore_ascii_case(real)) {
                bail!(
                    "org alias `{alias}` points to `{real}`, which is itself an alias (`{other}`); name the real org"
                );
            }
        }
        Ok(Self { pairs })
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    pub fn check_profiles(&self, profiles: &[Profile]) -> Result<()> {
        for (alias, _) in &self.pairs {
            for profile in profiles {
                if profile
                    .config
                    .orgs
                    .iter()
                    .any(|org| org.eq_ignore_ascii_case(alias))
                {
                    bail!(
                        "org alias `{alias}` is also an org name in profile `{}`; an alias must not shadow a real org",
                        profile.name
                    );
                }
            }
        }
        Ok(())
    }

    pub fn resolve(&self, org: &str) -> String {
        self.pairs
            .iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(org))
            .map_or_else(|| org.to_string(), |(_, real)| real.clone())
    }

    pub fn resolve_all(&self, orgs: &[String]) -> Vec<String> {
        orgs.iter().map(|org| self.resolve(org)).collect()
    }

    pub fn resolve_ref(&self, raw: &str) -> String {
        let trimmed = raw.trim();
        let (prefix, rest) = URL_PREFIXES
            .iter()
            .find_map(|prefix| trimmed.strip_prefix(prefix).map(|rest| (*prefix, rest)))
            .unwrap_or(("", trimmed));
        match rest.split_once('/') {
            Some((org, tail)) => format!("{prefix}{}/{tail}", self.resolve(org)),
            None => raw.to_string(),
        }
    }

    pub fn resolve_refs(&self, raws: &[String]) -> Vec<String> {
        raws.iter().map(|raw| self.resolve_ref(raw)).collect()
    }

    pub fn listing(&self) -> Option<String> {
        if self.pairs.is_empty() {
            return None;
        }
        let rows: Vec<String> = self
            .pairs
            .iter()
            .map(|(alias, real)| format!("{alias} = {real}"))
            .collect();
        Some(rows.join(", "))
    }
}
