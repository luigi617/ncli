import argparse

import pytest

from ncli.command import Argument, Command, Flag
from ncli.utils import IS_WINDOWS

posix_only = pytest.mark.skipif(IS_WINDOWS, reason="POSIX shell scripts and signals")


class FakeCommand(Command):
    name = "fake"
    help = "A fake command for tests."
    arguments = (Argument("target", help="what to act on"),)
    flags = (Flag("--command", help="executable to use"),)

    def __init__(self, exit_code: int = 0, requires: tuple[str, ...] = ()) -> None:
        self.exit_code = exit_code
        self.required_executables = requires
        self.received: argparse.Namespace | None = None

    def run(self, args: argparse.Namespace) -> int:
        self.received = args
        return self.exit_code
