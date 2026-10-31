import contextlib
import os
import shutil
import signal
import subprocess
import sys
import threading
import time
from collections.abc import Iterable
from dataclasses import dataclass, field
from pathlib import Path

from ncli.errors import MissingExecutablesError, ProcessError
from ncli.utils import IS_WINDOWS

_STDERR_TAIL_LINES = 20
_cancelled = threading.Event()
_lock = threading.RLock()
_running: set[subprocess.Popen[str]] = set()


def require_executables(names: Iterable[str]) -> None:
    if missing := [name for name in dict.fromkeys(names) if shutil.which(name) is None]:
        raise MissingExecutablesError(missing)


def cancel() -> None:
    with _lock:
        _cancelled.set()
        for proc in _running:
            _kill(proc)


if IS_WINDOWS:
    _NEW_GROUP: dict[str, int] = {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP}

    def _kill(proc: subprocess.Popen[str]) -> None:
        subprocess.run(
            ["taskkill", "/F", "/T", "/PID", str(proc.pid)],
            stdin=subprocess.DEVNULL,
            capture_output=True,
        )

else:
    _NEW_GROUP = {"process_group": 0}

    def _kill(proc: subprocess.Popen[str]) -> None:
        with contextlib.suppress(ProcessLookupError, PermissionError):
            os.killpg(proc.pid, signal.SIGKILL)


@dataclass(frozen=True)
class Call:
    argv: tuple[str, ...]
    attempt: int
    attempts: int

    @property
    def name(self) -> str:
        return Path(self.argv[0]).name


@dataclass(frozen=True)
class Result:
    returncode: int
    stdout: str
    stderr: str
    duration: float


class Tracer:
    def before_call(self, call: Call) -> None:
        pass

    def after_call(self, call: Call, result: Result) -> None:
        pass

    def on_error(self, call: Call, error: ProcessError) -> None:
        pass

    def on_retry(self, call: Call, error: ProcessError, delay: float) -> None:
        pass


class StderrTracer(Tracer):
    def before_call(self, call: Call) -> None:
        self._log(call, "started")

    def after_call(self, call: Call, result: Result) -> None:
        self._log(call, f"exited with code {result.returncode} after {result.duration:.1f}s")

    def on_error(self, call: Call, error: ProcessError) -> None:
        self._log(call, f"failed: {error}")

    def on_retry(self, call: Call, error: ProcessError, delay: float) -> None:
        self._log(call, f"retrying in {delay:g}s")

    def _log(self, call: Call, message: str) -> None:
        attempt = f"attempt {call.attempt}/{call.attempts}"
        sys.stderr.write(f"ncli: {call.name} ({attempt}) {message}\n")


def _failure(call: Call, result: Result) -> ProcessError:
    if result.returncode < 0:
        return ProcessError(f"{call.name} was killed by signal {-result.returncode}")
    tail = "\n".join(result.stderr.strip().splitlines()[-_STDERR_TAIL_LINES:])
    detail = f": {tail}" if tail else ""
    return ProcessError(f"{call.name} exited with code {result.returncode}{detail}", retryable=True)


@dataclass(frozen=True)
class Cli:
    argv: tuple[str, ...]
    timeout: float | None = None
    attempts: int = 1
    retry_delay: float = 0
    tracer: Tracer = field(default_factory=Tracer)

    def __post_init__(self) -> None:
        if self.attempts < 1:
            raise ValueError(f"attempts must be at least 1, got {self.attempts}")

    @property
    def executable(self) -> str:
        return self.argv[0]

    def run(self, *args: str, input: str | None = None, cwd: Path | None = None) -> str:
        argv = (*self.argv, *args)
        for attempt in range(1, self.attempts):
            call = Call(argv, attempt, self.attempts)
            try:
                return self._attempt(call, input, cwd)
            except ProcessError as error:
                if not error.retryable or _cancelled.is_set():
                    raise
                self.tracer.on_retry(call, error, self.retry_delay)
                if _cancelled.wait(self.retry_delay):
                    raise
        return self._attempt(Call(argv, self.attempts, self.attempts), input, cwd)

    def _attempt(self, call: Call, input: str | None, cwd: Path | None) -> str:
        self.tracer.before_call(call)
        try:
            result = self._execute(call, input, cwd)
            self.tracer.after_call(call, result)
            if result.returncode != 0:
                raise _failure(call, result)
        except ProcessError as error:
            self.tracer.on_error(call, error)
            raise
        return result.stdout

    def _execute(self, call: Call, input: str | None, cwd: Path | None) -> Result:
        start = time.monotonic()
        try:
            proc = subprocess.Popen(
                [shutil.which(call.argv[0]) or call.argv[0], *call.argv[1:]],
                cwd=cwd,
                stdin=subprocess.DEVNULL if input is None else subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                encoding="utf-8",
                errors="replace",
                **_NEW_GROUP,
            )
        except OSError as error:
            raise ProcessError(f"{call.name} could not be started: {error}") from error
        with _lock:
            _running.add(proc)
            if _cancelled.is_set():
                _kill(proc)
        try:
            stdout, stderr = proc.communicate(input, timeout=self.timeout)
        except subprocess.TimeoutExpired:
            _kill(proc)
            proc.communicate()
            raise ProcessError(f"{call.name} timed out after {self.timeout:g}s") from None
        finally:
            with _lock:
                _running.discard(proc)
        return Result(proc.returncode, stdout, stderr, time.monotonic() - start)
