use anyhow::{Result, bail};
use serde::Serialize;

/// How often a scheduled reclaim should run.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Every {
    pub seconds: u64,
    pub label: &'static str,
}

impl Every {
    pub fn display(&self) -> String {
        let s = self.seconds;
        if s % 604_800 == 0 {
            let n = s / 604_800;
            return format!("every {n} week{}", if n == 1 { "" } else { "s" });
        }
        if s % 86_400 == 0 {
            let n = s / 86_400;
            return format!("every {n} day{}", if n == 1 { "" } else { "s" });
        }
        if s % 3_600 == 0 {
            let n = s / 3_600;
            return format!("every {n} hour{}", if n == 1 { "" } else { "s" });
        }
        format!("every {s} seconds")
    }
}

/// Parse `--every 2w` / `14d` / `12h` / `30m`.
pub fn parse_every(input: &str) -> Result<Every> {
    let raw = input.trim().to_lowercase();
    let raw = raw.replace(' ', "");
    let (num, unit) = split_num_unit(&raw)?;
    let seconds = match unit.as_str() {
        "w" | "wk" | "wks" | "week" | "weeks" => num.saturating_mul(7 * 24 * 3600),
        "d" | "day" | "days" => num.saturating_mul(24 * 3600),
        "h" | "hr" | "hrs" | "hour" | "hours" => num.saturating_mul(3600),
        "m" | "min" | "mins" | "minute" | "minutes" => num.saturating_mul(60),
        other => bail!("unknown interval unit '{other}' (try 2w, 14d, 12h)"),
    };
    if seconds == 0 {
        bail!("interval must be greater than zero");
    }
    Ok(Every {
        seconds,
        label: "custom",
    })
}

fn split_num_unit(raw: &str) -> Result<(u64, String)> {
    let mut digits = String::new();
    let mut unit = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_digit() && unit.is_empty() {
            digits.push(ch);
        } else {
            unit.push(ch);
        }
    }
    if digits.is_empty() {
        bail!("missing number in interval '{raw}' (try 2w)");
    }
    if unit.is_empty() {
        bail!("missing unit in interval '{raw}' (try 2w, 14d, 12h)");
    }
    Ok((digits.parse()?, unit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_weeks() {
        assert_eq!(parse_every("2w").unwrap().seconds, 14 * 24 * 3600);
        assert_eq!(parse_every("2 weeks").unwrap().seconds, 14 * 24 * 3600);
    }
}
