from collections.abc import Callable
from pathlib import Path

import pytest


@pytest.fixture
def path_with(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Callable[..., None]:
    def install(*names: str) -> None:
        for name in names:
            executable = tmp_path / name
            executable.write_text("#!/bin/sh\n")
            executable.chmod(0o755)
        monkeypatch.setenv("PATH", str(tmp_path))

    return install
