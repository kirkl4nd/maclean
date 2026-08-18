//! Run log for scheduled jobs. One JSON line per run, newest last.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize)]
pub struct JobStats {
    pub runs: u64,
    pub bytes: u64,
    pub last_unix: Option<u64>,
    pub last_ok: Option<bool>,
    pub errors: u64,
}

impl JobStats {
    pub fn last_run_label(&self) -> String {
        match self.last_unix {
            Some(ts) => format_ago(ts),
            None => "never run".into(),
        }
    }

    pub fn summary(&self) -> String {
        if self.runs == 0 {
            return "never run".into();
        }
        let saved = crate::core::format_bytes(self.bytes);
        let result = match self.last_ok {
            Some(true) => "ok",
            Some(false) => "error",
            None => "",
        };
        if result.is_empty() {
            format!("{} · {} saved", self.last_run_label(), saved)
        } else {
            format!("{} · {} saved · {result}", self.last_run_label(), saved)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Line {
    job: String,
    at: u64,
    bytes: u64,
    errors: u64,
}

pub fn record(job_id: &str, bytes: u64, errors: u64) {
    let Ok(path) = history_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let line = Line {
        job: job_id.to_string(),
        at: now_unix(),
        bytes,
        errors,
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    if let Ok(json) = serde_json::to_string(&line) {
        let _ = writeln!(file, "{json}");
    }
}

pub fn stats(job_id: &str) -> JobStats {
    let Ok(path) = history_path() else {
        return JobStats::default();
    };
    let Ok(file) = fs::File::open(&path) else {
        return JobStats::default();
    };
    let mut out = JobStats::default();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(row) = serde_json::from_str::<Line>(&line) else {
            continue;
        };
        if row.job != job_id {
            continue;
        }
        out.runs += 1;
        out.bytes += row.bytes;
        out.last_unix = Some(row.at);
        out.last_ok = Some(row.errors == 0);
        if row.errors > 0 {
            out.errors += row.errors;
        }
    }
    out
}

/// True if a scheduled job should actually reclaim now.
///
/// A successful run starts the interval. Restarts do not. A failed run
/// is due again on the next nudge so it can retry.
pub fn due(job_id: &str, every: u64) -> bool {
    due_at(&stats(job_id), every, now_unix())
}

pub fn due_at(stats: &JobStats, every: u64, now: u64) -> bool {
    match (stats.last_unix, stats.last_ok) {
        (Some(last), Some(true)) => now.saturating_sub(last) >= every,
        _ => true,
    }
}

fn history_path() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("home"))?;
    Ok(home.join("Library/Logs/maclean/history.jsonl"))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn format_ago(ts: u64) -> String {
    let now = now_unix();
    let delta = now.saturating_sub(ts);
    if delta < 60 {
        return "just now".into();
    }
    if delta < 3600 {
        let n = delta / 60;
        return format!("{n}m ago");
    }
    if delta < 86400 {
        let n = delta / 3600;
        return format!("{n}h ago");
    }
    let n = delta / 86400;
    format!("{n}d ago")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_labels() {
        let now = now_unix();
        assert_eq!(format_ago(now), "just now");
        assert_eq!(format_ago(now.saturating_sub(120)), "2m ago");
    }

    #[test]
    fn due_uses_last_successful_run_not_a_stopwatch() {
        let never = JobStats::default();
        assert!(due_at(&never, 7 * 24 * 3600, 1_000));

        let recent_ok = JobStats {
            last_unix: Some(1_000),
            last_ok: Some(true),
            ..JobStats::default()
        };
        assert!(!due_at(&recent_ok, 7 * 24 * 3600, 1_000 + 6 * 24 * 3600));
        assert!(due_at(&recent_ok, 7 * 24 * 3600, 1_000 + 7 * 24 * 3600));

        let failed = JobStats {
            last_unix: Some(1_000),
            last_ok: Some(false),
            ..JobStats::default()
        };
        assert!(due_at(&failed, 7 * 24 * 3600, 1_000 + 60));
    }
}
