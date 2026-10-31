import argparse
import signal
import threading
from collections.abc import Callable

import pytest
from fakes import FakeCommand

from ncli import __version__, process
from ncli.cli import main
from ncli.command import CommandGroup
from ncli.errors import ExitCode, NcliError


def test_version_prints_and_exits_zero(capsys: pytest.CaptureFixture[str]) -> None:
    with pytest.raises(SystemExit) as exc:
        main(["--version"])
    assert exc.value.code == 0
    assert capsys.readouterr().out.strip() == f"ncli {__version__}"


def test_no_command_is_usage_error(capsys: pytest.CaptureFixture[str]) -> None:
    with pytest.raises(SystemExit) as exc:
        main([])
    assert exc.value.code == 2
    assert "required" in capsys.readouterr().err


def test_runs_command_and_returns_its_exit_code() -> None:
    command = FakeCommand(exit_code=3)
    assert main(["fake", "target"], commands=[command]) == 3
    assert command.received is not None
    assert command.received.target == "target"


def test_missing_executables_stop_before_run(
    path_with: Callable[..., None], capsys: pytest.CaptureFixture[str]
) -> None:
    path_with("claude")
    command = FakeCommand(requires=("claude", "codex", "gemini"))
    assert main(["fake", "target"], commands=[command]) == ExitCode.MISSING_EXECUTABLE
    assert command.received is None
    assert "ncli fake: error:" in (err := capsys.readouterr().err)
    assert "codex, gemini" in err


def test_present_executables_allow_run(path_with: Callable[..., None]) -> None:
    path_with("claude", "codex")
    command = FakeCommand(requires=("claude", "codex"))
    assert main(["fake", "target"], commands=[command]) == 0
    assert command.received is not None


def test_help_works_without_required_executables(path_with: Callable[..., None]) -> None:
    path_with()
    with pytest.raises(SystemExit) as exc:
        main(["fake", "--help"], commands=[FakeCommand(requires=("claude",))])
    assert exc.value.code == 0


def test_ncli_error_from_run_maps_to_its_exit_code(capsys: pytest.CaptureFixture[str]) -> None:
    class Failing(FakeCommand):
        def run(self, args: argparse.Namespace) -> int:
            raise NcliError("bad input")

    assert main(["fake", "target"], commands=[Failing()]) == ExitCode.FAILURE
    assert capsys.readouterr().err.strip() == "ncli fake: error: bad input"


def test_registered_commands_are_available() -> None:
    with pytest.raises(SystemExit) as exc:
        main(["review", "--help"])
    assert exc.value.code == 0


def test_subcommand_errors_use_the_full_command_path(capsys: pytest.CaptureFixture[str]) -> None:
    class Failing(FakeCommand):
        def run(self, args: argparse.Namespace) -> int:
            raise NcliError("bad input")

    group = CommandGroup("group", "a group", [Failing()])
    assert main(["group", "fake", "target"], commands=[group]) == ExitCode.FAILURE
    assert capsys.readouterr().err.strip() == "ncli group fake: error: bad input"


def test_interrupt_cancels_processes_and_exits_130(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(process, "_cancelled", cancelled := threading.Event())

    class Interrupted(FakeCommand):
        def run(self, args: argparse.Namespace) -> int:
            signal.raise_signal(signal.SIGINT)
            return 0

    previous = signal.getsignal(signal.SIGINT)
    assert main(["fake", "target"], commands=[Interrupted()]) == ExitCode.INTERRUPTED
    assert cancelled.is_set()
    assert signal.getsignal(signal.SIGINT) is previous
