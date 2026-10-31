from collections.abc import Iterable
from enum import IntEnum


class ExitCode(IntEnum):
    FAILURE = 1
    MISSING_EXECUTABLE = 127
    INTERRUPTED = 130


class NcliError(Exception):
    exit_code: ExitCode = ExitCode.FAILURE


class ProcessError(NcliError):
    def __init__(self, message: str, retryable: bool = False) -> None:
        super().__init__(message)
        self.retryable = retryable


class MissingExecutablesError(NcliError):
    exit_code = ExitCode.MISSING_EXECUTABLE

    def __init__(self, names: Iterable[str]) -> None:
        super().__init__(f"required executables not found on PATH: {', '.join(names)}")
