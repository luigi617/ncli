import shutil
from collections.abc import Iterable

from ncli.errors import ExitCode, NcliError


class MissingExecutablesError(NcliError):
    exit_code = ExitCode.MISSING_EXECUTABLE

    def __init__(self, names: Iterable[str]) -> None:
        super().__init__(f"required executables not found on PATH: {', '.join(names)}")


def executable_exists(name: str) -> bool:
    return shutil.which(name) is not None


def missing_executables(names: Iterable[str]) -> list[str]:
    return [name for name in dict.fromkeys(names) if not executable_exists(name)]


def require_executables(names: Iterable[str]) -> None:
    if missing := missing_executables(names):
        raise MissingExecutablesError(missing)
