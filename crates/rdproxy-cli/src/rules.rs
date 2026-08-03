//! `rdproxy rules` subcommands: list/export/import against the on-disk
//! rules store.
//!
//! Like `cert`, these operate directly on `<data-dir>/rules.json`; there is
//! no IPC with a running `rdproxy run` process.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use rdproxy_core::{Rule, RulesStore};

use crate::resolve_data_dir;

/// `rdproxy rules` subcommands.
#[derive(Subcommand, Debug)]
pub enum RulesCommand {
    /// List all rules (id, name, enabled, priority).
    List,
    /// Export rules as pretty JSON, to a file or stdout.
    Export {
        /// Destination file; defaults to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Import rules from a JSON file (a `Rule[]` array).
    Import {
        /// Path to a JSON file containing a `Rule[]` array.
        file: PathBuf,
        /// Replace the entire rule set instead of merging by id.
        #[arg(long)]
        replace: bool,
    },
}

/// Dispatches a [`RulesCommand`].
pub fn dispatch(cmd: RulesCommand) -> Result<()> {
    match cmd {
        RulesCommand::List => list(),
        RulesCommand::Export { out } => export(out),
        RulesCommand::Import { file, replace } => import(&file, replace),
    }
}

fn rules_path() -> PathBuf {
    resolve_data_dir(None).join("rules.json")
}

fn list() -> Result<()> {
    let store = RulesStore::load(&rules_path());
    let rules = store.list();
    if rules.is_empty() {
        println!("no rules");
        return Ok(());
    }
    for rule in &rules {
        let status = if rule.enabled { "enabled" } else { "disabled" };
        println!(
            "{}  {:<24}  {:<8}  priority={}",
            rule.id, rule.name, status, rule.priority
        );
    }
    Ok(())
}

fn export(out: Option<PathBuf>) -> Result<()> {
    let store = RulesStore::load(&rules_path());
    let json =
        serde_json::to_string_pretty(&store.export()).context("failed to serialize rules")?;
    match out {
        Some(path) => std::fs::write(&path, &json)
            .with_context(|| format!("failed to write {}", path.display()))?,
        None => println!("{json}"),
    }
    Ok(())
}

/// Merges `incoming` into `current`: rules whose `id` matches an existing
/// entry overwrite it in place; rules with a new `id` are appended.
///
/// Mirrors `rdproxy-api`'s `routes::rules::import` non-replace semantics
/// (read-merge-write via `RulesStore::import`, since `RulesStore` has no
/// native partial-import method), so `rules import` without `--replace`
/// behaves identically to the API's merge import.
pub(crate) fn merge_rules(mut current: Vec<Rule>, incoming: Vec<Rule>) -> Vec<Rule> {
    for rule in incoming {
        match current.iter().position(|r| r.id == rule.id) {
            Some(pos) => current[pos] = rule,
            None => current.push(rule),
        }
    }
    current
}

fn import(file: &Path, replace: bool) -> Result<()> {
    let contents = std::fs::read_to_string(file)
        .with_context(|| format!("failed to read {}", file.display()))?;
    let incoming: Vec<Rule> = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {} as a Rule[] array", file.display()))?;
    let count = incoming.len();

    let path = rules_path();
    let store = RulesStore::load(&path);
    if replace {
        store.import(incoming).context("failed to save rules")?;
    } else {
        let merged = merge_rules(store.list(), incoming);
        store.import(merged).context("failed to save rules")?;
    }
    println!("imported {count} rule(s) into {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdproxy_core::Matcher;

    fn rule(id: &str, name: &str) -> Rule {
        Rule {
            id: id.to_string(),
            name: name.to_string(),
            enabled: true,
            priority: 0,
            group: None,
            notes: None,
            matcher: Matcher::default(),
            actions: vec![],
        }
    }

    #[test]
    fn merge_overwrites_matching_id_and_appends_new() {
        let current = vec![rule("a", "old-a"), rule("b", "b")];
        let incoming = vec![rule("a", "new-a"), rule("c", "c")];
        let merged = merge_rules(current, incoming);
        let ids: Vec<&str> = merged.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
        assert_eq!(merged[0].name, "new-a");
    }

    #[test]
    fn merge_with_empty_current_just_appends() {
        let merged = merge_rules(vec![], vec![rule("x", "x")]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, "x");
    }
}
