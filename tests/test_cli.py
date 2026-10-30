import pytest

from ncli import __version__
from ncli.cli import main


def test_version_prints_and_exits_zero(capsys: pytest.CaptureFixture[str]) -> None:
    with pytest.raises(SystemExit) as exc:
        main(["--version"])
    assert exc.value.code == 0
    assert capsys.readouterr().out.strip() == f"ncli {__version__}"


def test_no_arguments_succeeds() -> None:
    assert main([]) == 0
