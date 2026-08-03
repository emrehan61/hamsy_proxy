//! Persisted, thread-safe storage for [`Rule`]s with a cached compiled
//! [`RuleSet`].

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::error::{CoreError, Result};
use crate::rule::{Rule, RuleSet};
use crate::settings::atomic_write_json;

/// Thread-safe store of [`Rule`]s, persisted as JSON to a fixed path and
/// backed by a cached, automatically-rebuilt [`RuleSet`].
pub struct RulesStore {
    path: PathBuf,
    rules: RwLock<Vec<Rule>>,
    ruleset: RwLock<Arc<RuleSet>>,
}

impl RulesStore {
    /// Loads rules from `path` (an empty/missing/corrupt file yields an
    /// empty rule list rather than an error) and compiles the initial
    /// [`RuleSet`]. All subsequent mutations persist back to `path`.
    pub fn load(path: &Path) -> Self {
        let rules: Vec<Rule> = fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let ruleset = Arc::new(RuleSet::new(rules.clone()));
        RulesStore {
            path: path.to_path_buf(),
            rules: RwLock::new(rules),
            ruleset: RwLock::new(ruleset),
        }
    }

    /// Returns a clone of all stored rules, in their persisted order.
    pub fn list(&self) -> Vec<Rule> {
        self.rules.read().clone()
    }

    /// Returns a clone of the rule with the given `id`, if present.
    pub fn get(&self, id: &str) -> Option<Rule> {
        self.rules.read().iter().find(|r| r.id == id).cloned()
    }

    /// Appends a new rule and persists.
    pub fn create(&self, rule: Rule) -> Result<()> {
        let mut rules = self.rules.write();
        rules.push(rule);
        self.persist(&rules)
    }

    /// Replaces the rule with the given `id`, if present, and persists.
    /// Returns [`CoreError::NotFound`] if no rule with that id exists.
    pub fn update(&self, id: &str, rule: Rule) -> Result<()> {
        let mut rules = self.rules.write();
        let pos = rules
            .iter()
            .position(|r| r.id == id)
            .ok_or(CoreError::NotFound)?;
        rules[pos] = rule;
        self.persist(&rules)
    }

    /// Removes the rule with the given `id`, if present, and persists.
    /// Returns [`CoreError::NotFound`] if no rule with that id exists.
    pub fn delete(&self, id: &str) -> Result<()> {
        let mut rules = self.rules.write();
        let pos = rules
            .iter()
            .position(|r| r.id == id)
            .ok_or(CoreError::NotFound)?;
        rules.remove(pos);
        self.persist(&rules)
    }

    /// Flips the `enabled` flag of the rule with the given `id` and
    /// persists. Returns [`CoreError::NotFound`] if no rule with that id
    /// exists.
    pub fn toggle(&self, id: &str) -> Result<()> {
        let mut rules = self.rules.write();
        let rule = rules
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or(CoreError::NotFound)?;
        rule.enabled = !rule.enabled;
        self.persist(&rules)
    }

    /// Reorders rules to match the order of `ids`. Rules whose id isn't
    /// listed keep their relative order and are appended after the listed
    /// ones.
    pub fn reorder(&self, ids: &[String]) -> Result<()> {
        let mut rules = self.rules.write();
        let mut reordered = Vec::with_capacity(rules.len());
        for id in ids {
            if let Some(pos) = rules.iter().position(|r| &r.id == id) {
                reordered.push(rules.remove(pos));
            }
        }
        reordered.extend(rules.drain(..));
        *rules = reordered;
        self.persist(&rules)
    }

    /// Replaces the entire rule set with `rules` and persists.
    pub fn import(&self, rules: Vec<Rule>) -> Result<()> {
        let mut current = self.rules.write();
        *current = rules;
        self.persist(&current)
    }

    /// Returns a clone of all stored rules, for export.
    pub fn export(&self) -> Vec<Rule> {
        self.list()
    }

    /// Returns the currently-cached compiled [`RuleSet`], rebuilt after
    /// every mutation.
    pub fn ruleset(&self) -> Arc<RuleSet> {
        self.ruleset.read().clone()
    }

    /// Rebuilds the compiled [`RuleSet`] cache and writes `rules` to disk.
    fn persist(&self, rules: &[Rule]) -> Result<()> {
        *self.ruleset.write() = Arc::new(RuleSet::new(rules.to_vec()));
        atomic_write_json(&self.path, &rules)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::Matcher;

    fn sample_rule(id: &str) -> Rule {
        Rule {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            priority: 0,
            group: None,
            notes: None,
            matcher: Matcher::default(),
            actions: vec![],
        }
    }

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("flproxy-rules-test-{}.json", uuid::Uuid::new_v4()))
    }

    #[test]
    fn create_list_get() {
        let path = temp_path();
        let store = RulesStore::load(&path);
        store.create(sample_rule("r1")).unwrap();
        assert_eq!(store.list().len(), 1);
        assert!(store.get("r1").is_some());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn update_replaces_rule() {
        let path = temp_path();
        let store = RulesStore::load(&path);
        store.create(sample_rule("r1")).unwrap();
        let mut updated = sample_rule("r1");
        updated.name = "renamed".to_string();
        store.update("r1", updated).unwrap();
        assert_eq!(store.get("r1").unwrap().name, "renamed");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn update_missing_returns_not_found() {
        let path = temp_path();
        let store = RulesStore::load(&path);
        let err = store.update("missing", sample_rule("missing")).unwrap_err();
        assert!(matches!(err, CoreError::NotFound));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn delete_removes_rule() {
        let path = temp_path();
        let store = RulesStore::load(&path);
        store.create(sample_rule("r1")).unwrap();
        store.delete("r1").unwrap();
        assert!(store.get("r1").is_none());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn toggle_flips_enabled() {
        let path = temp_path();
        let store = RulesStore::load(&path);
        store.create(sample_rule("r1")).unwrap();
        assert!(store.get("r1").unwrap().enabled);
        store.toggle("r1").unwrap();
        assert!(!store.get("r1").unwrap().enabled);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn reorder_moves_rules_to_front() {
        let path = temp_path();
        let store = RulesStore::load(&path);
        store.create(sample_rule("a")).unwrap();
        store.create(sample_rule("b")).unwrap();
        store.create(sample_rule("c")).unwrap();
        store.reorder(&["c".to_string(), "a".to_string()]).unwrap();
        let ids: Vec<String> = store.list().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec!["c", "a", "b"]);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn import_export_round_trip() {
        let path = temp_path();
        let store = RulesStore::load(&path);
        store
            .import(vec![sample_rule("x"), sample_rule("y")])
            .unwrap();
        let exported = store.export();
        assert_eq!(exported.len(), 2);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn ruleset_rebuilds_after_mutation() {
        let path = temp_path();
        let store = RulesStore::load(&path);
        assert_eq!(store.ruleset().rules().len(), 0);
        store.create(sample_rule("r1")).unwrap();
        assert_eq!(store.ruleset().rules().len(), 1);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn persists_across_reload() {
        let path = temp_path();
        {
            let store = RulesStore::load(&path);
            store.create(sample_rule("r1")).unwrap();
        }
        let reloaded = RulesStore::load(&path);
        assert_eq!(reloaded.list().len(), 1);
        let _ = fs::remove_file(&path);
    }
}
