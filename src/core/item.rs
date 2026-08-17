use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::issue::{Privilege, ScanIssue};

/// Anything shown in the tree: a module, a group, or a leaf.
///
/// This is the only shape the core knows. It does not care whether a node is
/// "Spotify cache" or "a Docker volume" — that lives in the module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub module: String,
    pub title: String,
    pub summary: String,
    pub bytes: u64,
    pub paths: Vec<PathBuf>,
    pub safety: Safety,
    pub reclaimable: bool,
    #[serde(default)]
    pub privilege: Privilege,
    #[serde(default)]
    pub children: Vec<Item>,
    #[serde(default)]
    pub issues: Vec<ScanIssue>,
    /// If true, clean this node in one action instead of walking into its
    /// children (the children are then only there to show what is inside).
    #[serde(default)]
    pub clean_whole: bool,
    /// Module-authored key/value metadata for the detail screen. The core
    /// renders these verbatim and never interprets them.
    #[serde(default)]
    pub details: Vec<Detail>,
    /// Free-form notes shown under the details (what happens if you clean this).
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detail {
    pub label: String,
    pub value: String,
}

impl Detail {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

impl Item {
    pub fn new(module: impl Into<String>, id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            module: module.into(),
            title: title.into(),
            summary: String::new(),
            bytes: 0,
            paths: Vec::new(),
            safety: Safety::Safe,
            reclaimable: false,
            privilege: Privilege::None,
            children: Vec::new(),
            issues: Vec::new(),
            clean_whole: false,
            details: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn with_summary(mut self, s: impl Into<String>) -> Self {
        self.summary = s.into();
        self
    }

    pub fn with_bytes(mut self, n: u64) -> Self {
        self.bytes = n;
        self
    }

    pub fn with_path(mut self, p: PathBuf) -> Self {
        self.paths.push(p);
        self
    }

    pub fn with_safety(mut self, s: Safety) -> Self {
        self.safety = s;
        self
    }

    pub fn with_reclaimable(mut self, v: bool) -> Self {
        self.reclaimable = v;
        self
    }

    pub fn with_privilege(mut self, p: Privilege) -> Self {
        self.privilege = p;
        self
    }

    pub fn with_children(mut self, children: Vec<Item>) -> Self {
        if self.bytes == 0 {
            self.bytes = children.iter().map(|c| c.bytes).sum();
        }
        if !self.reclaimable {
            self.reclaimable = children.iter().any(|c| c.reclaimable);
        }
        self.children = children;
        self
    }

    pub fn clean_whole(mut self) -> Self {
        self.clean_whole = true;
        self
    }

    pub fn with_issue(mut self, issue: ScanIssue) -> Self {
        self.issues.push(issue);
        self
    }

    pub fn with_detail(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.details.push(Detail::new(label, value));
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn from_issue(module: &str, issue: &ScanIssue) -> Self {
        let id = format!("{module}:issue:{}", issue.title().replace(' ', "-"));
        Self::new(module, id, issue.message.clone())
            .with_summary(match &issue.hint {
                Some(hint) => format!("{}: {hint}", issue.title()),
                None => issue.title().to_string(),
            })
            .with_safety(Safety::Info)
            .with_reclaimable(false)
            .with_issue(issue.clone())
    }

    pub fn find(&self, id: &str) -> Option<&Item> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }

    pub fn walk(&self) -> Vec<&Item> {
        let mut out = vec![self];
        for child in &self.children {
            out.extend(child.walk());
        }
        out
    }

    /// Nodes that `reclaim` actually acts on: a `clean_whole` parent, or the
    /// leaves under a group. Used when applying a clean.
    pub fn reclaimable_leaves(&self) -> Vec<&Item> {
        if self.clean_whole || self.children.is_empty() {
            return if self.reclaimable {
                vec![self]
            } else {
                Vec::new()
            };
        }
        self.children
            .iter()
            .flat_map(|c| c.reclaimable_leaves())
            .collect()
    }

    /// Nodes the checkbox tree cares about. Unlike [`Self::reclaimable_leaves`],
    /// this walks into a `clean_whole` parent so its children can be ticked on
    /// their own. Selecting every child is what makes the parent fully ticked;
    /// cleaning then still runs once on the parent.
    pub fn selection_leaves(&self) -> Vec<&Item> {
        let nested: Vec<&Item> = self
            .children
            .iter()
            .flat_map(|c| c.selection_leaves())
            .collect();
        if !nested.is_empty() {
            return nested;
        }
        if self.reclaimable {
            vec![self]
        } else {
            Vec::new()
        }
    }

    /// True if this node is worth a place on the main tree: it takes up space,
    /// something under it can be cleaned, or it has something to report.
    /// Reclaimable items with no reported size (snapshots, empty files) still
    /// belong on the tree.
    pub fn worth_showing(&self) -> bool {
        self.bytes > 0
            || self
                .walk()
                .iter()
                .any(|i| i.reclaimable || !i.issues.is_empty())
    }

    /// Keep the largest children worth showing: drop anything under
    /// `min_bytes`, then cap the list at `keep`. Long tails of tiny leaves
    /// make the tree noisy without telling you anything.
    pub fn prune_children(mut self, min_bytes: u64, keep: usize) -> Self {
        self.children.retain(|c| c.bytes >= min_bytes);
        self.children.sort_by(|a, b| b.bytes.cmp(&a.bytes));
        self.children.truncate(keep);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Safety {
    Safe,
    Caution,
    Destructive,
    Info,
}

impl Safety {
    pub fn label(self) -> &'static str {
        match self {
            Self::Safe => "Safe",
            Self::Caution => "Caution",
            Self::Destructive => "Destructive",
            Self::Info => "Notice",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Safe => "Can be rebuilt automatically",
            Self::Caution => "May remove unused data you still want",
            Self::Destructive => "Cannot be undone",
            Self::Info => "Nothing will be deleted",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: &str) -> Item {
        Item::new("t", id, id).with_reclaimable(true).with_bytes(1)
    }

    #[test]
    fn selection_walks_into_a_clean_whole_parent() {
        let parent = Item::new("t", "cache", "cache")
            .with_reclaimable(true)
            .clean_whole()
            .with_children(vec![leaf("a"), leaf("b")]);
        let ids: Vec<&str> = parent
            .selection_leaves()
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(ids, ["a", "b"]);
        let reclaim: Vec<&str> = parent
            .reclaimable_leaves()
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(reclaim, ["cache"]);
    }

    #[test]
    fn selection_walks_three_levels() {
        let leaf = |id: &str| Item::new("t", id, id).with_reclaimable(true).with_bytes(1);
        let inner = Item::new("t", "images", "images")
            .with_reclaimable(true)
            .clean_whole()
            .with_children(vec![leaf("img-a"), leaf("img-b")]);
        let group = Item::new("t", "group", "group").with_children(vec![
            inner,
            Item::new("t", "other", "other")
                .with_reclaimable(true)
                .clean_whole()
                .with_children(vec![leaf("vol-a")]),
        ]);
        let ids: Vec<&str> = group
            .selection_leaves()
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(ids, ["img-a", "img-b", "vol-a"]);
        let images = group.children[0].selection_leaves();
        assert_eq!(
            images.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["img-a", "img-b"]
        );
        assert_eq!(
            group.children[0].children[0].selection_leaves()[0].id,
            "img-a"
        );
    }

    #[test]
    fn selection_leaf_when_children_are_informational() {
        let parent = Item::new("t", "project", "project")
            .with_reclaimable(true)
            .clean_whole()
            .with_children(vec![Item::new("t", "debug", "debug").with_bytes(1)]);
        let ids: Vec<&str> = parent
            .selection_leaves()
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(ids, ["project"]);
    }
}
