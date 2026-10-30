import argparse

import pytest
from fakes import FakeCommand

from ncli.parser import Parser


def test_parse_returns_selected_command_and_its_arguments() -> None:
    command = FakeCommand()
    selected, args = Parser([command]).parse(["fake", "https://example.com/pr/1", "--command", "x"])
    assert selected is command
    assert vars(args) == {"target": "https://example.com/pr/1", "command": "x"}


def test_duplicate_command_names_fail_when_building() -> None:
    with pytest.raises(argparse.ArgumentError):
        Parser([FakeCommand(), FakeCommand()])


@pytest.mark.parametrize("argv", [[], ["nope"], ["fake"]])
def test_usage_errors_exit_two(argv: list[str]) -> None:
    with pytest.raises(SystemExit) as exc:
        Parser([FakeCommand()]).parse(argv)
    assert exc.value.code == 2


def test_format_error_uses_the_command_prog() -> None:
    command = FakeCommand()
    assert Parser([command]).format_error(command, ValueError("boom")) == "ncli fake: error: boom"
