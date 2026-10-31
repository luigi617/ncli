from pathlib import Path

import pytest
from fakes import posix_only

from ncli.cli import main
from ncli.commands.review.command import ReviewCommand
from ncli.commands.review.reviewers import Reviewer
from ncli.errors import ExitCode
from ncli.process import Cli

pytestmark = posix_only

LINK = "https://github.com/owner/repo/pull/1"

FAKE_REVIEWER = """\
#!/bin/sh
prompt=$(cat)
n=$(ls "$0".prompt.* 2>/dev/null | wc -l | tr -d ' ')
printf '%s' "$prompt" > "$0.prompt.$n"
echo "$(basename "$0") answer $n"
"""


def fake_reviewer(tmp_path: Path, name: str, script: str = FAKE_REVIEWER) -> Reviewer:
    path = tmp_path / name
    path.write_text(script)
    path.chmod(0o755)
    return Reviewer(name, Cli((str(path),)))


def prompts(tmp_path: Path, name: str) -> list[str]:
    return [p.read_text() for p in sorted(tmp_path.glob(f"{name}.prompt.*"))]


@pytest.fixture
def reviewers(tmp_path: Path) -> list[Reviewer]:
    return [fake_reviewer(tmp_path, name) for name in ("a", "b", "c")]


def test_reviewers_review_rebut_and_judge(
    tmp_path: Path, reviewers: list[Reviewer], capsys: pytest.CaptureFixture[str]
) -> None:
    assert main(["review", LINK, "--judge", "b"], commands=[ReviewCommand(reviewers)]) == 0

    review, rebuttal = prompts(tmp_path, "a")
    assert LINK in review
    own, others = rebuttal.split("The other reviewers' findings:")
    assert "a answer 0" in own
    assert "b answer 0" in others and "c answer 0" in others and "a answer 0" not in others

    verdict_prompt = prompts(tmp_path, "b")[2]
    for name in ("a", "b", "c"):
        assert f"## Review: {name}\n\n{name} answer 0" in verdict_prompt
        assert f"## Rebuttal: {name}\n\n{name} answer 1" in verdict_prompt

    out = capsys.readouterr().out
    assert out.index("## Review: a") < out.index("## Rebuttal: a") < out.index("## Verdict: b")
    assert "## Verdict: b\n\nb answer 2" in out


def test_judge_defaults_to_first_reviewer(tmp_path: Path, reviewers: list[Reviewer]) -> None:
    assert main(["review", LINK], commands=[ReviewCommand(reviewers)]) == 0
    assert len(prompts(tmp_path, "a")) == 3
    assert len(prompts(tmp_path, "b")) == 2


def test_failing_reviewer_fails_the_review(
    tmp_path: Path, reviewers: list[Reviewer], capsys: pytest.CaptureFixture[str]
) -> None:
    broken = fake_reviewer(tmp_path, "broken", "#!/bin/sh\necho 'not logged in' >&2\nexit 3\n")
    command = ReviewCommand([*reviewers, broken])
    assert main(["review", LINK], commands=[command]) == ExitCode.FAILURE
    captured = capsys.readouterr()
    assert "Verdict" not in captured.out
    assert "exited with code 3: not logged in" in captured.err
