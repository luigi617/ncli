import argparse

import pytest
from fakes import FakeCommand

from ncli.command import CommandGroup, Flag
from ncli.parser import Parser


def test_parse_returns_selected_command_and_its_arguments() -> None:
    command = FakeCommand()
    selected, args = Parser([command]).parse(["fake", "https://example.com/pr/1", "--command", "x"])
    assert selected is command
    assert vars(args) == {"target": "https://example.com/pr/1", "command": "x"}


def test_duplicate_command_names_fail_when_building() -> None:
    with pytest.raises((argparse.ArgumentError, ValueError), match="conflicting subparser"):
        Parser([FakeCommand(), FakeCommand()])


def test_duplicate_names_inside_a_group_fail_when_building() -> None:
    with pytest.raises((argparse.ArgumentError, ValueError), match="conflicting subparser"):
        Parser([CommandGroup("group", "", [FakeCommand(), FakeCommand()])])


@pytest.mark.parametrize("argv", [[], ["nope"], ["fake"]])
def test_usage_errors_exit_two(argv: list[str]) -> None:
    with pytest.raises(SystemExit) as exc:
        Parser([FakeCommand()]).parse(argv)
    assert exc.value.code == 2


def test_format_error_uses_the_command_prog() -> None:
    command = FakeCommand()
    assert Parser([command]).format_error(command, ValueError("boom")) == "ncli fake: error: boom"


def test_flag_choices_are_enforced_and_default_is_applied(
    capsys: pytest.CaptureFixture[str],
) -> None:
    class Choosy(FakeCommand):
        flags = (Flag("--color", help="color to use", choices=("red", "blue"), default="red"),)

    parser = Parser([Choosy()])
    assert parser.parse(["fake", "t"])[1].color == "red"
    assert parser.parse(["fake", "t", "--color", "blue"])[1].color == "blue"
    with pytest.raises(SystemExit) as exc:
        parser.parse(["fake", "t", "--color", "green"])
    assert exc.value.code == 2

    with pytest.raises(SystemExit):
        parser.parse(["fake", "--help"])
    assert "color to use (default: red)" in capsys.readouterr().out


def test_subcommands_dispatch_to_the_nested_command() -> None:
    first, second = FakeCommand(), FakeCommand()
    parser = Parser([CommandGroup("one", "", [first]), CommandGroup("two", "", [second])])
    assert parser.parse(["one", "fake", "t"])[0] is first
    selected, args = parser.parse(["two", "fake", "t", "--command", "x"])
    assert selected is second
    assert vars(args) == {"target": "t", "command": "x"}
    assert parser.format_error(second, ValueError("boom")) == "ncli two fake: error: boom"


def test_nested_groups_are_supported() -> None:
    command = FakeCommand()
    parser = Parser([CommandGroup("outer", "", [CommandGroup("inner", "", [command])])])
    assert parser.parse(["outer", "inner", "fake", "t"])[0] is command


@pytest.mark.parametrize("argv", [["group"], ["group", "nope"]])
def test_group_requires_a_known_subcommand(argv: list[str]) -> None:
    with pytest.raises(SystemExit) as exc:
        Parser([CommandGroup("group", "", [FakeCommand()])]).parse(argv)
    assert exc.value.code == 2


def test_group_help_lists_subcommands(capsys: pytest.CaptureFixture[str]) -> None:
    with pytest.raises(SystemExit):
        Parser([CommandGroup("group", "group help", [FakeCommand()])]).parse(["group", "--help"])
    out = capsys.readouterr().out
    assert "group help" in out
    assert "fake" in out and FakeCommand.help in out
