import signal
import sys
from collections.abc import Sequence
from types import FrameType

from ncli import process
from ncli.command import CommandNode
from ncli.commands import COMMANDS
from ncli.errors import ExitCode, NcliError
from ncli.parser import Parser


def _interrupt(signum: int, frame: FrameType | None) -> None:
    process.cancel()
    signal.default_int_handler(signum, frame)


def main(argv: Sequence[str] | None = None, commands: Sequence[CommandNode] = COMMANDS) -> int:
    parser = Parser(commands)
    command, args = parser.parse(argv)
    previous = signal.signal(signal.SIGINT, _interrupt)
    try:
        process.require_executables(command.required_executables)
        return command.run(args)
    except NcliError as error:
        print(parser.format_error(command, error), file=sys.stderr)
        return error.exit_code
    except KeyboardInterrupt:
        return ExitCode.INTERRUPTED
    finally:
        signal.signal(signal.SIGINT, previous)
