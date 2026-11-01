use std::fmt;
use std::num::NonZeroU32;
use std::sync::LazyLock;

use serde::Deserialize;

// Kept on one line: Windows cannot pass an argument containing a newline to a batch-file shim.
static SCHEMA: LazyLock<String> = LazyLock::new(|| {
    serde_json::from_str::<serde_json::Value>(include_str!("verdict.schema.json"))
        .expect("the verdict schema is valid JSON")
        .to_string()
});

pub fn schema() -> &'static str {
    &SCHEMA
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verdict {
    pub change_accessed: bool,
    pub findings: Vec<Finding>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub severity: Severity,
    pub title: String,
    pub file: String,
    pub line: NonZeroU32,
    pub explanation: String,
    pub fix: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    High,
    Medium,
    Low,
}

impl Verdict {
    pub fn parse(json: &str) -> serde_json::Result<Self> {
        let mut verdict: Self = serde_json::from_str(json)?;
        verdict.findings.sort_by_key(|finding| finding.severity);
        Ok(verdict)
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.findings.len() {
            0 => return write!(f, "## ncli review: no findings"),
            1 => write!(f, "## ncli review: 1 finding")?,
            count => write!(f, "## ncli review: {count} findings")?,
        }
        for finding in &self.findings {
            write!(f, "\n\n{finding}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "### {}: {}\n`{}:{}`\n{}\n**Fix:** {}",
            self.severity,
            self.title.trim(),
            self.file,
            self.line,
            self.explanation.trim(),
            self.fix.trim()
        )
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::High => "🔴 High",
            Self::Medium => "🟠 Medium",
            Self::Low => "🟡 Low",
        })
    }
}
