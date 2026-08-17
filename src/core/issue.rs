use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What kind of access an operation needs. Scan never uses sudo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Privilege {
    #[default]
    None,
    /// macOS TCC Full Disk Access. sudo does not grant this.
    FullDiskAccess,
    /// A one-shot `sudo` for a specific command (never the whole app).
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    Permission,
    NeedsAdmin,
    NeedsFullDiskAccess,
    Unavailable,
    Warning,
}

/// A structured finding from a module. UIs render these; they do not print
/// raw stderr.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanIssue {
    pub kind: IssueKind,
    pub message: String,
    pub path: Option<PathBuf>,
    pub hint: Option<String>,
}

impl ScanIssue {
    pub fn permission(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            kind: IssueKind::Permission,
            message: message.into(),
            path: Some(path.into()),
            hint: Some(
                "If this is under ~/Library, grant Full Disk Access to your terminal in System Settings → Privacy & Security."
                    .into(),
            ),
        }
    }

    pub fn full_disk_access(message: impl Into<String>) -> Self {
        Self {
            kind: IssueKind::NeedsFullDiskAccess,
            message: message.into(),
            path: None,
            hint: Some(
                "System Settings → Privacy & Security → Full Disk Access → enable this terminal (or the maclean binary). sudo will not help."
                    .into(),
            ),
        }
    }

    pub fn needs_admin(message: impl Into<String>) -> Self {
        Self {
            kind: IssueKind::NeedsAdmin,
            message: message.into(),
            path: None,
            hint: Some(
                "This one action needs a macOS administrator password. maclean will not run itself as root; it only elevates that command."
                    .into(),
            ),
        }
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            kind: IssueKind::Warning,
            message: message.into(),
            path: None,
            hint: None,
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: IssueKind::Unavailable,
            message: message.into(),
            path: None,
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn title(&self) -> &'static str {
        match self.kind {
            IssueKind::Permission => "Permission denied",
            IssueKind::NeedsAdmin => "Administrator required",
            IssueKind::NeedsFullDiskAccess => "Full Disk Access required",
            IssueKind::Unavailable => "Unavailable",
            IssueKind::Warning => "Warning",
        }
    }
}

/// Cheap leftover-data check, with a human-readable reason either way.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relevance {
    pub relevant: bool,
    pub reason: String,
}

impl Relevance {
    pub fn yes(reason: impl Into<String>) -> Self {
        Self {
            relevant: true,
            reason: reason.into(),
        }
    }

    pub fn no(reason: impl Into<String>) -> Self {
        Self {
            relevant: false,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReclaimError {
    pub item_id: String,
    pub kind: IssueKind,
    pub message: String,
    pub hint: Option<String>,
}

impl ReclaimError {
    pub fn new(item_id: impl Into<String>, kind: IssueKind, message: impl Into<String>) -> Self {
        Self {
            item_id: item_id.into(),
            kind,
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn from_issue(item_id: impl Into<String>, issue: ScanIssue) -> Self {
        Self {
            item_id: item_id.into(),
            kind: issue.kind,
            message: issue.message,
            hint: issue.hint,
        }
    }
}

impl std::fmt::Display for ReclaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, " ({hint})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ReclaimError {}
