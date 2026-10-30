import argparse
from collections.abc import Iterable, Sequence

from ncli import __version__
from ncli.command import Command

_COMMAND = "_ncli_command"


class Parser:
    def __init__(self, commands: Iterable[Command]) -> None:
        self._parser = argparse.ArgumentParser(
            prog="ncli", description="Run workflows that chain other CLIs."
        )
        self._parser.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
        subparsers = self._parser.add_subparsers(
            title="commands", metavar="<command>", required=True
        )
        self._progs: dict[str, str] = {}
        for command in commands:
            subparser = subparsers.add_parser(
                command.name, help=command.help, description=command.help
            )
            command.configure(subparser)
            subparser.set_defaults(**{_COMMAND: command})
            self._progs[command.name] = subparser.prog

    def parse(self, argv: Sequence[str] | None = None) -> tuple[Command, argparse.Namespace]:
        args = self._parser.parse_args(argv)
        return vars(args).pop(_COMMAND), args

    def format_error(self, command: Command, error: Exception) -> str:
        return f"{self._progs[command.name]}: error: {error}"
