use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, serde::Serialize)]
pub struct DiskUsage {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub mount: String,
}

pub fn disk_usage(path: &Path) -> Result<DiskUsage> {
    let output = Command::new("df")
        .args(["-k", "-P"])
        .arg(if path == Path::new("/") {
            Path::new("/System/Volumes/Data")
        } else {
            path
        })
        .output()
        .context("failed to run df")?;
    if !output.status.success() {
        bail!("df failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().nth(1).context("df returned no data rows")?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.len() < 6 {
        bail!("unexpected df output: {line}");
    }
    let blocks = |s: &str| s.parse::<u64>().unwrap_or(0).saturating_mul(1024);
    Ok(DiskUsage {
        total_bytes: blocks(cols[1]),
        used_bytes: blocks(cols[2]),
        available_bytes: blocks(cols[3]),
        mount: cols[5..].join(" "),
    })
}
