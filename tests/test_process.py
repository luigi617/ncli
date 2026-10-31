import os
import signal
import sys
import threading
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest
from fakes import posix_only

from ncli import process
from ncli.errors import MissingExecutablesError, ProcessError
from ncli.process import (
    Call,
    Cli,
    Result,
    Tracer,
    require_executables,
)

PYTHON = Path(sys.executable).name


class RecordingTracer(Tracer):
    def __init__(self) -> None:
        self.events: list[tuple[str, int]] = []

    def before_call(self, call: Call) -> None:
        self.events.append(("before", call.attempt))

    def after_call(self, call: Call, result: Result) -> None:
        self.events.append((f"after:{result.returncode}", call.attempt))

    def on_error(self, call: Call, error: ProcessError) -> None:
        self.events.append(("error", call.attempt))

    def on_retry(self, call: Call, error: ProcessError, delay: float) -> None:
        self.events.append((f"retry:{delay:g}", call.attempt))


@pytest.fixture(autouse=True)
def fresh_cancellation(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(process, "_cancelled", threading.Event())


SPAWNS_CHILD_AND_SLEEPS = (
    "import subprocess, sys, time\n"
    "subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])\n"
    "time.sleep(30)"
)


def python(code: str, **options: Any) -> Cli:
    return Cli((sys.executable, "-c", code), **options)


def flaky(tmp_path: Path, failures: int) -> str:
    counter = tmp_path / "count"
    return (
        "import pathlib, sys\n"
        f"p = pathlib.Path({str(counter)!r})\n"
        "n = int(p.read_text()) if p.exists() else 0\n"
        "p.write_text(str(n + 1))\n"
        f"if n < {failures}:\n"
        "    sys.exit('transient')\n"
        "print('ok')\n"
    )


def test_run_returns_stdout_and_passes_args_input_and_cwd(tmp_path: Path) -> None:
    cli = python("import os, sys; print(sys.argv[1], sys.stdin.read(), os.getcwd())")
    arg, stdin, cwd = cli.run("arg", input="stdin", cwd=tmp_path).split()
    assert (arg, stdin) == ("arg", "stdin")
    assert os.path.samefile(cwd, tmp_path)


def test_nonzero_exit_raises_with_code_and_stderr() -> None:
    with pytest.raises(ProcessError) as exc:
        python("import sys; sys.exit('boom')").run()
    assert str(exc.value) == f"{PYTHON} exited with code 1: boom"


def test_timeout_raises() -> None:
    with pytest.raises(ProcessError) as exc:
        python("import time; time.sleep(5)", timeout=0.2).run()
    assert str(exc.value) == f"{PYTHON} timed out after 0.2s"


def test_unstartable_executable_raises(tmp_path: Path) -> None:
    with pytest.raises(ProcessError) as exc:
        Cli((str(tmp_path / "missing"),)).run()
    assert str(exc.value).startswith("missing could not be started:")


def test_retries_until_success_and_traces_every_attempt(tmp_path: Path) -> None:
    tracer = RecordingTracer()
    cli = python(flaky(tmp_path, failures=2), attempts=3, retry_delay=0.01, tracer=tracer)
    assert cli.run().strip() == "ok"
    assert tracer.events == [
        ("before", 1),
        ("after:1", 1),
        ("error", 1),
        ("retry:0.01", 1),
        ("before", 2),
        ("after:1", 2),
        ("error", 2),
        ("retry:0.01", 2),
        ("before", 3),
        ("after:0", 3),
    ]


def test_gives_up_after_the_last_attempt(tmp_path: Path) -> None:
    tracer = RecordingTracer()
    with pytest.raises(ProcessError, match="transient"):
        python(flaky(tmp_path, failures=5), attempts=2, tracer=tracer).run()
    assert [event for event in tracer.events if event[0] == "error"] == [("error", 1), ("error", 2)]


def test_timeouts_are_not_retried() -> None:
    tracer = RecordingTracer()
    with pytest.raises(ProcessError):
        python("import time; time.sleep(5)", timeout=0.2, attempts=3, tracer=tracer).run()
    assert tracer.events == [("before", 1), ("error", 1)]


def test_unstartable_executable_is_not_retried(tmp_path: Path) -> None:
    tracer = RecordingTracer()
    with pytest.raises(ProcessError):
        Cli((str(tmp_path / "missing"),), attempts=3, tracer=tracer).run()
    assert tracer.events == [("before", 1), ("error", 1)]


@posix_only
def test_killed_by_signal_is_reported_and_not_retried() -> None:
    tracer = RecordingTracer()
    code = f"import os; os.kill(os.getpid(), {int(signal.SIGTERM)})"
    with pytest.raises(ProcessError) as exc:
        python(code, attempts=3, tracer=tracer).run()
    assert str(exc.value) == f"{PYTHON} was killed by signal {int(signal.SIGTERM)}"
    assert len([event for event in tracer.events if event[0] == "before"]) == 1


def test_cancel_kills_running_calls_without_retrying() -> None:
    tracer = RecordingTracer()
    threading.Timer(0.3, process.cancel).start()
    started = time.monotonic()
    with pytest.raises(ProcessError):
        python(SPAWNS_CHILD_AND_SLEEPS, attempts=3, tracer=tracer).run()
    assert time.monotonic() - started < 5
    assert len([event for event in tracer.events if event[0] == "before"]) == 1


def test_cancel_stops_pending_retries(tmp_path: Path) -> None:
    tracer = RecordingTracer()
    cli = python(flaky(tmp_path, failures=1), attempts=3, retry_delay=30, tracer=tracer)
    threading.Timer(0.5, process.cancel).start()
    started = time.monotonic()
    with pytest.raises(ProcessError, match="transient"):
        cli.run()
    assert time.monotonic() - started < 5
    assert [event[0] for event in tracer.events] == ["before", "after:1", "error", "retry:30"]


def test_timeout_kills_children_holding_the_output_pipe() -> None:
    started = time.monotonic()
    with pytest.raises(ProcessError, match="timed out"):
        python(SPAWNS_CHILD_AND_SLEEPS, timeout=0.3).run()
    assert time.monotonic() - started < 5


def test_error_message_keeps_only_the_end_of_stderr() -> None:
    code = "import sys\nfor i in range(100): print(f'line {i}', file=sys.stderr)\nsys.exit(1)"
    with pytest.raises(ProcessError) as exc:
        python(code).run()
    message = str(exc.value)
    assert "line 99" in message and "line 80" in message
    assert "line 79" not in message


def test_attempts_must_be_positive() -> None:
    with pytest.raises(ValueError):
        Cli(("x",), attempts=0)


def test_require_executables_reports_every_missing_name_once_in_order(
    path_with: Callable[..., None],
) -> None:
    path_with("claude")
    with pytest.raises(MissingExecutablesError) as exc:
        require_executables(["gemini", "claude", "codex", "gemini"])
    assert str(exc.value) == "required executables not found on PATH: gemini, codex"


def test_require_executables_treats_non_executable_files_as_missing(
    path_with: Callable[..., None], tmp_path: Path
) -> None:
    path_with()
    (tmp_path / "claude").write_text("")
    with pytest.raises(MissingExecutablesError):
        require_executables(["claude"])


def test_require_executables_passes_when_all_installed(path_with: Callable[..., None]) -> None:
    path_with("claude", "codex")
    require_executables(["claude", "codex"])
