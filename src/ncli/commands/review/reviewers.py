from dataclasses import dataclass
from pathlib import Path

from ncli.process import Cli, StderrTracer


def _cli(*argv: str) -> Cli:
    return Cli(argv, timeout=30 * 60, attempts=3, retry_delay=10, tracer=StderrTracer())


@dataclass(frozen=True)
class Reviewer:
    name: str
    cli: Cli

    def ask(self, prompt: str, cwd: Path | None = None) -> str:
        return self.cli.run(input=prompt, cwd=cwd)


REVIEWERS = (
    Reviewer(
        "claude",
        _cli(
            "claude",
            "--allowedTools",
            "Bash(gh pr view:*)",
            "Bash(gh pr diff:*)",
            "mcp__builder-mcp__CodeReviewReadActions",
            "--print",
        ),
    ),
    Reviewer(
        "codex",
        _cli(
            "codex",
            "exec",
            "--sandbox",
            "workspace-write",
            "--config",
            "sandbox_workspace_write.network_access=true",
            "--skip-git-repo-check",
        ),
    ),
)
