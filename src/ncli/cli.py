import sys
from collections.abc import Sequence

from ncli.command import Command
from ncli.errors import NcliError
from ncli.executables import require_executables
from ncli.parser import Parser

COMMANDS: tuple[Command, ...] = ()


def main(argv: Sequence[str] | None = None, commands: Sequence[Command] = COMMANDS) -> int:
    parser = Parser(commands)
    command, args = parser.parse(argv)
    try:
        require_executables(command.required_executables(args))
        return command.run(args)
    except NcliError as error:
        print(parser.format_error(command, error), file=sys.stderr)
        return error.exit_code
