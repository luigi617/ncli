use std::path::Path;
use std::time::Duration;

use clap::ValueEnum;

use crate::process::{Cli, ProcessError, StderrTracer};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ReviewerKind {
    #[default]
    Claude,
    Codex,
}

impl ReviewerKind {
    pub fn reviewer(self) -> Reviewer {
        let argv: &[&str] = match self {
            Self::Claude => &[
                "claude",
                "--allowedTools",
                "Bash(gh pr view:*)",
                "Bash(gh pr diff:*)",
                "mcp__builder-mcp__CodeReviewReadActions",
                "--print",
            ],
            Self::Codex => &[
                "codex",
                "exec",
                "--sandbox",
                "workspace-write",
                "--config",
                "sandbox_workspace_write.network_access=true",
                "--skip-git-repo-check",
            ],
        };
        let name = self
            .to_possible_value()
            .expect("no reviewer kind is skipped");
        Reviewer {
            name: name.get_name().to_owned(),
            cli: Cli::new(argv.iter().copied())
                .timeout(Duration::from_secs(30 * 60))
                .attempts(3)
                .retry_delay(Duration::from_secs(10))
                .tracer(StderrTracer),
        }
    }
}

pub struct Reviewer {
    pub name: String,
    pub cli: Cli,
}

impl Reviewer {
    pub fn ask(&self, prompt: &str, cwd: &Path) -> Result<String, ProcessError> {
        self.cli.run(Some(prompt), Some(cwd))
    }
}
