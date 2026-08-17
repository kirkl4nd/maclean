//! macOS scheduled jobs via launchd.
//!
//! Jobs live as `~/Library/LaunchAgents/com.maclean.job.*.plist`.
//! The plist *is* the job: ProgramArguments is a full `maclean reclaim … --yes`
//! invocation. There is no side database, so deleting the plist deletes the job.
//!
//! Identity is [`super::is_maclean_job`]: prefix plus our schema keys (or the
//! original Comment, for jobs written before schema 1). Uninstall walks that
//! same check so a coincidentally named LaunchAgent is left alone.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use plist::{Dictionary, Value};

use super::{
    COMMENT, ITEM_KEY, JOB_SCHEMA, LABEL_PREFIX, MANAGED_KEY, SCHEMA_KEY, ScheduledJob, Scheduler,
    is_maclean_job,
};

pub struct LaunchdScheduler;

struct JobFile {
    path: PathBuf,
    label: String,
    job: ScheduledJob,
}

impl LaunchdScheduler {
    fn agents_dir() -> Result<PathBuf> {
        let home = dirs::home_dir().context("cannot locate home directory")?;
        let dir = home.join("Library/LaunchAgents");
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn plist_path(item_id: &str) -> Result<PathBuf> {
        Ok(Self::agents_dir()?.join(format!("{}.plist", label_for(item_id))))
    }

    fn uid() -> Result<u32> {
        let output = Command::new("id").arg("-u").output()?;
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(s.parse()?)
    }

    fn load(path: &Path, label: &str) -> Result<()> {
        let uid = Self::uid()?;
        let target = format!("gui/{uid}/{label}");
        let _ = Command::new("launchctl")
            .args(["bootout", &target])
            .status();
        let domain = format!("gui/{uid}");
        let status = Command::new("launchctl")
            .args(["bootstrap", &domain])
            .arg(path)
            .status()?;
        if !status.success() {
            bail!("launchctl bootstrap failed for {label}");
        }
        Ok(())
    }

    fn unload(label: &str) -> Result<()> {
        let uid = Self::uid()?;
        let target = format!("gui/{uid}/{label}");
        let _ = Command::new("launchctl")
            .args(["bootout", &target])
            .status();
        Ok(())
    }

    fn scan_jobs() -> Result<Vec<JobFile>> {
        let dir = Self::agents_dir()?;
        let mut jobs = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return Ok(jobs),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(job) = read_job(&path) {
                jobs.push(job);
            }
        }
        jobs.sort_by(|a, b| a.job.item_id.cmp(&b.job.item_id));
        Ok(jobs)
    }

    fn remove_logs(label: &str) {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let log_dir = home.join("Library/Logs/maclean");
        let stem = label.trim_start_matches(LABEL_PREFIX);
        let _ = fs::remove_file(log_dir.join(format!("{stem}.log")));
        let _ = fs::remove_file(log_dir.join(format!("{stem}.err")));
    }
}

impl Scheduler for LaunchdScheduler {
    fn list(&self) -> Result<Vec<ScheduledJob>> {
        Ok(Self::scan_jobs()?.into_iter().map(|j| j.job).collect())
    }

    fn add(&self, job: &ScheduledJob) -> Result<()> {
        let label = label_for(&job.item_id);
        let path = Self::plist_path(&job.item_id)?;
        write_plist(&path, &label, job)?;
        Self::load(&path, &label)?;
        Ok(())
    }

    fn remove(&self, item_id: &str) -> Result<()> {
        let label = label_for(item_id);
        Self::unload(&label)?;
        let path = Self::plist_path(item_id)?;
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Self::remove_logs(&label);
        Ok(())
    }

    fn purge(&self) -> Result<Vec<ScheduledJob>> {
        let files = Self::scan_jobs()?;
        let mut removed = Vec::new();
        for file in files {
            Self::unload(&file.label)?;
            if file.path.exists() {
                fs::remove_file(&file.path)?;
            }
            Self::remove_logs(&file.label);
            removed.push(file.job);
        }
        Ok(removed)
    }
}

fn label_for(item_id: &str) -> String {
    let sanitized: String = item_id
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' => c,
            _ => '-',
        })
        .collect();
    let mut label = format!("{LABEL_PREFIX}{sanitized}");
    if label.len() > 90 {
        let hash = simple_hash(item_id);
        label = format!("{LABEL_PREFIX}{hash:x}");
    }
    label
}

fn simple_hash(s: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn write_plist(path: &Path, label: &str, job: &ScheduledJob) -> Result<()> {
    let mut dict = Dictionary::new();
    dict.insert("Label".into(), Value::String(label.into()));
    dict.insert("Comment".into(), Value::String(COMMENT.into()));
    dict.insert(MANAGED_KEY.into(), Value::Boolean(true));
    dict.insert(SCHEMA_KEY.into(), Value::Integer(JOB_SCHEMA.into()));
    dict.insert(ITEM_KEY.into(), Value::String(job.item_id.clone()));
    dict.insert(
        "ProgramArguments".into(),
        Value::Array(job.command.iter().cloned().map(Value::String).collect()),
    );
    dict.insert(
        "StartInterval".into(),
        Value::Integer(i64::try_from(job.every.seconds).unwrap_or(i64::MAX).into()),
    );
    dict.insert("RunAtLoad".into(), Value::Boolean(false));

    let home = dirs::home_dir().context("home")?;
    let log_dir = home.join("Library/Logs/maclean");
    fs::create_dir_all(&log_dir)?;
    let stem = label.trim_start_matches(LABEL_PREFIX);
    dict.insert(
        "StandardOutPath".into(),
        Value::String(log_dir.join(format!("{stem}.log")).display().to_string()),
    );
    dict.insert(
        "StandardErrorPath".into(),
        Value::String(log_dir.join(format!("{stem}.err")).display().to_string()),
    );

    Value::Dictionary(dict)
        .to_file_xml(path)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn read_job(path: &Path) -> Result<JobFile> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .context("plist name")?;
    let value = Value::from_file(path).with_context(|| format!("read {}", path.display()))?;
    let dict = value.as_dictionary().context("plist is not a dictionary")?;
    let label = dict.get("Label").and_then(Value::as_string);
    let comment = dict.get("Comment").and_then(Value::as_string);
    let schema = dict.get(SCHEMA_KEY).and_then(Value::as_signed_integer);
    let managed = dict.get(MANAGED_KEY).and_then(Value::as_boolean);
    if !is_maclean_job(name, label, comment, schema, managed) {
        bail!("not a maclean job");
    }
    let label = label.context("missing Label")?.to_string();
    let args = dict
        .get("ProgramArguments")
        .and_then(Value::as_array)
        .context("missing ProgramArguments")?;
    let command: Vec<String> = args
        .iter()
        .filter_map(|v| v.as_string().map(|s| s.to_string()))
        .collect();
    let item_id = dict
        .get(ITEM_KEY)
        .and_then(Value::as_string)
        .map(|s| s.to_string())
        .or_else(|| {
            command
                .iter()
                .position(|a| a == "reclaim")
                .and_then(|i| command.get(i + 1))
                .cloned()
        })
        .context("plist is missing MacleanItemId and is not `maclean reclaim <id>`")?;
    let seconds = dict
        .get("StartInterval")
        .and_then(Value::as_signed_integer)
        .unwrap_or(0) as u64;
    Ok(JobFile {
        path: path.to_path_buf(),
        label,
        job: ScheduledJob {
            item_id,
            every: super::Every {
                seconds,
                label: "custom",
            },
            command,
            schema: schema.unwrap_or(0),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_launchd_safe() {
        let l = label_for("spotify:cache");
        assert!(l.starts_with(LABEL_PREFIX));
        assert!(!l.contains(':'));
    }

    #[test]
    fn schema_1_roundtrip_keeps_item_id() {
        let dir = std::env::temp_dir().join(format!("maclean-plist-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("com.maclean.job.spotify-cache.plist");
        let job = ScheduledJob {
            item_id: "spotify:cache".into(),
            every: crate::schedule::Every {
                seconds: 86_400,
                label: "custom",
            },
            command: vec![
                "/tmp/maclean".into(),
                "reclaim".into(),
                "spotify:cache".into(),
                "--yes".into(),
            ],
            schema: JOB_SCHEMA,
        };
        write_plist(&path, "com.maclean.job.spotify-cache", &job).unwrap();
        let read = read_job(&path).unwrap();
        assert_eq!(read.job.item_id, "spotify:cache");
        assert_eq!(read.job.schema, JOB_SCHEMA);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn schema_0_comment_is_still_recognized() {
        let dir = std::env::temp_dir().join(format!("maclean-legacy-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("com.maclean.job.spotify-cache.plist");
        let mut dict = Dictionary::new();
        dict.insert(
            "Label".into(),
            Value::String("com.maclean.job.spotify-cache".into()),
        );
        dict.insert("Comment".into(), Value::String(COMMENT.into()));
        dict.insert(
            "ProgramArguments".into(),
            Value::Array(vec![
                Value::String("/tmp/maclean".into()),
                Value::String("reclaim".into()),
                Value::String("spotify:cache".into()),
                Value::String("--yes".into()),
            ]),
        );
        dict.insert("StartInterval".into(), Value::Integer(86_400.into()));
        Value::Dictionary(dict).to_file_xml(&path).unwrap();
        let read = read_job(&path).unwrap();
        assert_eq!(read.job.item_id, "spotify:cache");
        assert_eq!(read.job.schema, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_filename_without_our_markers_is_ignored() {
        let dir = std::env::temp_dir().join(format!("maclean-foreign-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("com.maclean.job.evil.plist");
        let mut dict = Dictionary::new();
        dict.insert("Label".into(), Value::String("com.maclean.job.evil".into()));
        dict.insert("Comment".into(), Value::String("not ours".into()));
        dict.insert(
            "ProgramArguments".into(),
            Value::Array(vec![Value::String("/usr/bin/true".into())]),
        );
        Value::Dictionary(dict).to_file_xml(&path).unwrap();
        assert!(read_job(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
