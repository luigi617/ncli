use std::fmt;
use std::num::NonZeroU32;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::agent::schema;

pub static SCHEMA: LazyLock<String> = LazyLock::new(|| schema(include_str!("verdict.schema.json")));

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verdict {
    pub change_accessed: bool,
    pub findings: Vec<Finding>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub severity: Severity,
    pub title: String,
    pub file: String,
    pub line: NonZeroU32,
    pub explanation: String,
    pub fix: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

pub struct Report<'a> {
    pub prog: &'a str,
    pub verdict: &'a Verdict,
}

impl fmt::Display for Report<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prog = self.prog;
        let findings = &self.verdict.findings;
        match findings.len() {
            0 => return write!(f, "## {prog}: no findings"),
            1 => write!(f, "## {prog}: 1 finding")?,
            count => write!(f, "## {prog}: {count} findings")?,
        }
        for finding in findings {
            write!(f, "\n\n{finding}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let location = format!("{}:{}", one_line(&self.file), self.line);
        write!(
            f,
            "### {}: {}\n{}\n{}\n**Fix:** {}",
            self.severity,
            one_line(&self.title),
            code_span(&location),
            one_line(&self.explanation),
            one_line(&self.fix)
        )
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// CommonMark: a code span's fence must be longer than any run of backticks inside it, and a
// leading or trailing backtick needs a space so it is not read as part of the fence.
fn code_span(text: &str) -> String {
    let longest_run = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest_run + 1);
    let padding = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{padding}{text}{padding}{fence}")
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
