import argparse
import sys
import tempfile
from collections.abc import Callable, Iterable, Sequence
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from ncli.command import Argument, Command, Flag
from ncli.commands.review.prompts import JUDGE_PROMPT, REBUTTAL_PROMPT, REVIEW_PROMPT
from ncli.commands.review.reviewers import REVIEWERS, Reviewer


def _transcript(sections: Iterable[tuple[str, str]]) -> str:
    return "\n\n".join(f"## {title}\n\n{body.strip()}" for title, body in sections)


def _sections(stage: str, answers: dict[str, str]) -> list[tuple[str, str]]:
    return [(f"{stage}: {name}", answer) for name, answer in answers.items()]


def _progress(stage: str) -> None:
    print(f"ncli review: {stage}...", file=sys.stderr)


def _ask_all(
    reviewers: Sequence[Reviewer], workdir: Path, stage: str, prompt_for: Callable[[str], str]
) -> dict[str, str]:
    _progress(stage)
    with ThreadPoolExecutor(len(reviewers)) as pool:
        answers = pool.map(lambda r: r.ask(prompt_for(r.name), workdir), reviewers)
        return {r.name: answer for r, answer in zip(reviewers, answers, strict=True)}


class ReviewCommand(Command):
    name = "review"
    help = "Review a PR/CR with several AI CLIs that debate their findings."
    arguments = (Argument("link", help="URL of the pull request or code review"),)

    def __init__(self, reviewers: Sequence[Reviewer] = REVIEWERS) -> None:
        self.reviewers = {reviewer.name: reviewer for reviewer in reviewers}
        self.required_executables = [reviewer.cli.executable for reviewer in reviewers]
        self.flags = (
            Flag(
                "--judge",
                help="reviewer that writes the final verdict",
                choices=tuple(self.reviewers),
                default=reviewers[0].name,
            ),
        )

    def run(self, args: argparse.Namespace) -> int:
        link = args.link
        reviewers = list(self.reviewers.values())
        with tempfile.TemporaryDirectory(prefix="ncli-review-") as tmp:
            workdir = Path(tmp)
            reviews = _ask_all(
                reviewers, workdir, "review", lambda _: REVIEW_PROMPT.format(link=link)
            )

            def rebuttal_prompt(name: str) -> str:
                others = {other: review for other, review in reviews.items() if other != name}
                return REBUTTAL_PROMPT.format(
                    link=link,
                    own=reviews[name].strip(),
                    others=_transcript(_sections("Review", others)),
                )

            rebuttals = _ask_all(reviewers, workdir, "rebuttal", rebuttal_prompt)
            debate = _sections("Review", reviews) + _sections("Rebuttal", rebuttals)
            _progress("verdict")
            verdict = self.reviewers[args.judge].ask(
                JUDGE_PROMPT.format(link=link, debate=_transcript(debate)), workdir
            )
        print(_transcript([*debate, (f"Verdict: {args.judge}", verdict)]))
        return 0
