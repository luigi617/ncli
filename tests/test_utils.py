import platform

from ncli.utils import IS_LINUX, IS_MACOS, IS_WINDOWS


def test_platform_constants_match_the_running_system() -> None:
    assert (
        platform.system() == "Windows",
        platform.system() == "Linux",
        platform.system() == "Darwin",
    ) == (IS_WINDOWS, IS_LINUX, IS_MACOS)
