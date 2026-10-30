import argparse
from collections.abc import Iterable

from ncli.command import Command


class FakeCommand(Command):
    name = "fake"
    help = "A fake command for tests."

    def __init__(self, exit_code: int = 0, requires: tuple[str, ...] = ()) -> None:
        self.exit_code = exit_code
        self.requires = requires
        self.received: argparse.Namespace | None = None

    def configure(self, parser: argparse.ArgumentParser) -> None:
        parser.add_argument("target")
        parser.add_argument("--command")

    def required_executables(self, args: argparse.Namespace) -> Iterable[str]:
        return self.requires

    def run(self, args: argparse.Namespace) -> int:
        self.received = args
        return self.exit_code
