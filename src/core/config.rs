//! Per-user config. Values are relative to the current home directory unless
//! they are absolute. The file is the source of truth; a missing file means
//! in-memory defaults (search `~/`, module paths as declared by each module).

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::module::Module;

/// `~/Library/Application Support/maclean/config.toml` on macOS.
pub fn default_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".config")
        })
        .join("maclean")
        .join("config.toml")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    #[serde(default)]
    pub modules: BTreeMap<String, ModuleSettings>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleSettings {
    /// Missing means enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Search roots for modules that walk a project tree. Missing/empty means `~`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub paths: BTreeMap<String, String>,
}

#[derive(Debug)]
pub enum ConfigError {
    Read {
        path: PathBuf,
        err: String,
    },
    Parse {
        path: PathBuf,
        err: String,
    },
    Write {
        path: PathBuf,
        err: String,
    },
    Invalid {
        path: PathBuf,
        issues: Vec<String>,
    },
    /// HOME is missing. We refuse to treat `/` as home — that would make
    /// every delete look like it is under an allowed root.
    NoHome,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, err } => {
                write!(f, "could not read {}: {err}", path.display())
            }
            Self::Parse { path, err } => {
                write!(f, "invalid config {}: {err}", path.display())
            }
            Self::Write { path, err } => {
                write!(f, "could not write {}: {err}", path.display())
            }
            Self::Invalid { path, issues } => {
                writeln!(f, "invalid config {}:", path.display())?;
                for issue in issues {
                    writeln!(f, "  - {issue}")?;
                }
                Ok(())
            }
            Self::NoHome => write!(
                f,
                "cannot locate home directory; maclean will not fall back to /"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

pub struct ModuleSpec {
    pub id: &'static str,
    pub searches: bool,
    pub path_keys: Vec<&'static str>,
}

impl ModuleSpec {
    pub fn from_module(module: &dyn Module) -> Self {
        Self {
            id: module.id(),
            searches: module.searches(),
            path_keys: module.paths().into_iter().map(|(k, _)| k).collect(),
        }
    }
}

impl AppConfig {
    /// Missing file → defaults. Present but unreadable/unparseable → error.
    pub fn load(explicit: Option<&Path>) -> Result<(Self, PathBuf), ConfigError> {
        let path = explicit.map(Path::to_path_buf).unwrap_or_else(default_path);
        if !path.is_file() {
            return Ok((Self::default(), path));
        }
        let text = fs::read_to_string(&path).map_err(|err| ConfigError::Read {
            path: path.clone(),
            err: err.to_string(),
        })?;
        let cfg: AppConfig = toml::from_str(&text).map_err(|err| ConfigError::Parse {
            path: path.clone(),
            err: err.to_string(),
        })?;
        Ok((cfg, path))
    }

    pub fn validate(&self, specs: &[ModuleSpec], home: &Path) -> Vec<String> {
        let mut issues = Vec::new();
        for (id, settings) in &self.modules {
            let Some(spec) = specs.iter().find(|s| s.id == id.as_str()) else {
                issues.push(format!("unknown module '{id}' — see `maclean modules`"));
                continue;
            };
            if !settings.roots.is_empty() && !spec.searches {
                issues.push(format!(
                    "module '{id}' does not search project trees, but config sets roots"
                ));
            }
            for (key, value) in &settings.paths {
                if !spec.path_keys.iter().any(|k| *k == key.as_str()) {
                    issues.push(format!(
                        "module '{id}' has no path '{key}' (known: {})",
                        spec.path_keys.join(", ")
                    ));
                }
                if value.trim().is_empty() {
                    issues.push(format!("module '{id}' path '{key}' is empty"));
                    continue;
                }
                let path = expand(value, home);
                if is_forbidden_root(&path) {
                    issues.push(format!(
                        "module '{id}' path '{key}' resolves to {} — that location cannot be used",
                        path.display()
                    ));
                }
            }
            for root in &settings.roots {
                if root.trim().is_empty() {
                    issues.push(format!("module '{id}' has an empty search root"));
                    continue;
                }
                let path = expand(root, home);
                if is_forbidden_root(&path) {
                    issues.push(format!(
                        "module '{id}' search root {} is not allowed",
                        path.display()
                    ));
                }
            }
        }
        issues
    }

    pub fn validate_or_err(
        &self,
        path: &Path,
        specs: &[ModuleSpec],
        home: &Path,
    ) -> Result<(), ConfigError> {
        let issues = self.validate(specs, home);
        if issues.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid {
                path: path.to_path_buf(),
                issues,
            })
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|err| ConfigError::Write {
                path: path.to_path_buf(),
                err: err.to_string(),
            })?;
        }
        let text = toml::to_string_pretty(self).map_err(|err| ConfigError::Write {
            path: path.to_path_buf(),
            err: err.to_string(),
        })?;
        fs::write(path, text).map_err(|err| ConfigError::Write {
            path: path.to_path_buf(),
            err: err.to_string(),
        })
    }

    pub fn module_enabled(&self, id: &str) -> bool {
        self.modules.get(id).and_then(|m| m.enabled).unwrap_or(true)
    }

    pub fn path_override(&self, module: &str, key: &str) -> Option<&str> {
        self.modules
            .get(module)
            .and_then(|m| m.paths.get(key))
            .map(String::as_str)
    }

    /// Configured search roots, or `[home]` when unset/empty.
    pub fn search_roots(&self, module: &str, home: &Path) -> Vec<PathBuf> {
        let listed = self
            .modules
            .get(module)
            .map(|m| m.roots.as_slice())
            .unwrap_or(&[]);
        if listed.is_empty() {
            return vec![home.to_path_buf()];
        }
        listed.iter().map(|r| expand(r, home)).collect()
    }

    fn module_mut(&mut self, id: &str) -> &mut ModuleSettings {
        self.modules.entry(id.to_string()).or_default()
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) {
        self.module_mut(id).enabled = Some(enabled);
    }

    pub fn set_path(&mut self, id: &str, key: &str, value: String) {
        self.module_mut(id).paths.insert(key.to_string(), value);
    }

    pub fn set_roots(&mut self, id: &str, roots: Vec<String>) {
        self.module_mut(id).roots = roots;
    }

    /// A complete, explicit file for `config init`, using each module's declarations.
    pub fn populated(modules: &[&dyn Module]) -> Self {
        let mut cfg = Self::default();
        for module in modules {
            let mut settings = ModuleSettings {
                enabled: Some(true),
                roots: if module.searches() {
                    vec!["~".into()]
                } else {
                    Vec::new()
                },
                paths: BTreeMap::new(),
            };
            for (key, rel) in module.paths() {
                settings.paths.insert(key.to_string(), format!("~/{rel}"));
            }
            cfg.modules.insert(module.id().to_string(), settings);
        }
        cfg
    }
}

/// System locations that must never be a search root or a module path.
pub fn is_forbidden_root(path: &Path) -> bool {
    const EXACT: &[&str] = &[
        "/",
        "/System",
        "/usr",
        "/bin",
        "/sbin",
        "/etc",
        "/private",
        "/Applications",
        "/Library",
        "/Users",
        "/opt",
        "/var",
        "/tmp",
        "/Volumes",
    ];
    let s = path.to_string_lossy();
    EXACT.iter().any(|f| s == *f) || s.starts_with("/System/")
}

/// Resolve a configured or default location.
///
/// `~/…` and relative paths are under `home`. Absolute paths are left alone.
/// `$VAR` / `${VAR}` are expanded from the environment.
pub fn expand(raw: &str, home: &Path) -> PathBuf {
    let expanded = expand_env(raw.trim());
    if expanded == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = expanded.strip_prefix("~/") {
        return home.join(rest);
    }
    let path = PathBuf::from(&expanded);
    if path.is_absolute() {
        path
    } else {
        home.join(path)
    }
}

fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        let braced = chars.peek() == Some(&'{');
        if braced {
            chars.next();
        }
        let mut name = String::new();
        while let Some(&n) = chars.peek() {
            if n.is_ascii_alphanumeric() || n == '_' {
                name.push(n);
                chars.next();
            } else {
                break;
            }
        }
        if braced && chars.peek() == Some(&'}') {
            chars.next();
        }
        if name.is_empty() {
            out.push('$');
            continue;
        }
        match std::env::var(&name) {
            Ok(val) => out.push_str(&val),
            Err(_) => {
                out.push('$');
                if braced {
                    out.push('{');
                    out.push_str(&name);
                    out.push('}');
                } else {
                    out.push_str(&name);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_and_relative_are_under_home() {
        let home = PathBuf::from("/Users/example");
        assert_eq!(expand("~", &home), home);
        assert_eq!(
            expand("~/Library/Caches/Foo", &home),
            home.join("Library/Caches/Foo")
        );
        assert_eq!(expand(".cargo", &home), home.join(".cargo"));
        assert_eq!(expand("/opt/custom", &home), PathBuf::from("/opt/custom"));
    }

    #[test]
    fn missing_roots_mean_home() {
        let cfg = AppConfig::default();
        let home = PathBuf::from("/Users/example");
        assert_eq!(cfg.search_roots("cargo", &home), vec![home]);
    }

    #[test]
    fn unknown_module_is_invalid() {
        let mut cfg = AppConfig::default();
        cfg.modules.insert("nope".into(), ModuleSettings::default());
        let home = PathBuf::from("/Users/example");
        let issues = cfg.validate(&[], &home);
        assert!(issues.iter().any(|i| i.contains("unknown module")));
    }

    #[test]
    fn unknown_path_key_is_invalid() {
        let mut cfg = AppConfig::default();
        let mut settings = ModuleSettings::default();
        settings.paths.insert("bogus".into(), "~/x".into());
        cfg.modules.insert("spotify".into(), settings);
        let specs = [ModuleSpec {
            id: "spotify",
            searches: false,
            path_keys: vec!["cache", "app"],
        }];
        let home = PathBuf::from("/Users/example");
        let issues = cfg.validate(&specs, &home);
        assert!(issues.iter().any(|i| i.contains("bogus")));
    }

    #[test]
    fn roots_on_a_non_search_module_are_invalid() {
        let mut cfg = AppConfig::default();
        cfg.modules.insert(
            "spotify".into(),
            ModuleSettings {
                enabled: Some(true),
                roots: vec!["~".into()],
                paths: BTreeMap::new(),
            },
        );
        let specs = [ModuleSpec {
            id: "spotify",
            searches: false,
            path_keys: vec!["cache"],
        }];
        let home = PathBuf::from("/Users/example");
        let issues = cfg.validate(&specs, &home);
        assert!(issues.iter().any(|i| i.contains("does not search")));
    }

    #[test]
    fn system_roots_are_rejected() {
        let mut cfg = AppConfig::default();
        cfg.modules.insert(
            "cargo".into(),
            ModuleSettings {
                enabled: Some(true),
                roots: vec!["/".into(), "/System".into()],
                paths: BTreeMap::new(),
            },
        );
        let specs = [ModuleSpec {
            id: "cargo",
            searches: true,
            path_keys: vec!["home"],
        }];
        let home = PathBuf::from("/Users/example");
        let issues = cfg.validate(&specs, &home);
        assert!(issues.iter().any(|i| i.contains("not allowed")));
    }

    #[test]
    fn deny_unknown_top_level() {
        let err = toml::from_str::<AppConfig>("roots = [\"~\"]\n").unwrap_err();
        assert!(err.to_string().contains("unknown"));
    }

    #[test]
    fn present_but_invalid_file_is_an_error() {
        let path =
            std::env::temp_dir().join(format!("maclean-bad-config-{}.toml", std::process::id()));
        fs::write(&path, "this is not toml {{{\n").unwrap();
        let err = AppConfig::load(Some(&path)).unwrap_err();
        let _ = fs::remove_file(&path);
        assert!(matches!(err, ConfigError::Parse { .. }));
    }
}
