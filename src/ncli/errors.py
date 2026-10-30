from enum import IntEnum


class ExitCode(IntEnum):
    FAILURE = 1
    MISSING_EXECUTABLE = 127


class NcliError(Exception):
    exit_code: ExitCode = ExitCode.FAILURE
