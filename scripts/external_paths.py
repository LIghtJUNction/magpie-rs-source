"""Conservative external-output checks shared by packaging and benchmarks."""

import os
from pathlib import Path
import stat
import subprocess


def _stat(path, *, follow_symlinks=True):
    try:
        return path.stat(follow_symlinks=follow_symlinks)
    except (FileNotFoundError, NotADirectoryError):
        return None


def _git_directory_candidate(path):
    """Check necessary layout only; Git makes the final repository decision."""
    info = _stat(path)
    if info is None or not stat.S_ISDIR(info.st_mode):
        return False
    # HEAD may itself be a dangling symbolic ref in an unborn repository.
    if _stat(path / "HEAD", follow_symlinks=False) is None:
        return False
    if _stat(path / "commondir") is not None:
        return True
    for name in ("objects", "refs"):
        info = _stat(path / name)
        if info is None or not stat.S_ISDIR(info.st_mode):
            return False
    return True


def _marker_directory(marker):
    info = _stat(marker)
    if info is None:
        return None
    if stat.S_ISDIR(info.st_mode):
        return marker
    if not stat.S_ISREG(info.st_mode):
        raise ValueError(f"Cannot inspect Git marker: {marker}")
    with marker.open("rb") as stream:
        raw = stream.read(65537)
    if len(raw) > 65536:
        raise ValueError(f"Git marker is too large to inspect: {marker}")
    if not raw.startswith(b"gitdir: "):
        return None
    target = raw[8:].rstrip(b"\r\n")
    if not target or b"\0" in target:
        return None
    directory = Path(os.fsdecode(target))
    return directory if directory.is_absolute() else marker.parent / directory


def _confirm_git_directory(path):
    # Discovery/configuration inherited from a calling Git command must not
    # redirect the probe or enable a different repository layout.
    env = {key: value for key, value in os.environ.items()
           if not key.upper().startswith("GIT_")}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
               GIT_OPTIONAL_LOCKS="0", GIT_TERMINAL_PROMPT="0")
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--resolve-git-dir", str(path)],
            cwd=Path(path.anchor), env=env, capture_output=True, timeout=5,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ValueError(f"Cannot determine Git repository status: {path}") from error
    # A plausible Git layout that Git cannot validate is ambiguous, rather
    # than permission to write into a possibly damaged/inaccessible repository.
    if result.returncode != 0 or not result.stdout.strip():
        raise ValueError(f"Cannot validate Git repository: {path}")
    return True


def _repository_at(directory):
    marker = _marker_directory(directory / ".git")
    if marker is not None and _git_directory_candidate(marker):
        return _confirm_git_directory(marker)
    # Bare repositories and Git administrative directories have no .git child.
    if _git_directory_candidate(directory):
        return _confirm_git_directory(directory)
    return False


def external_path(path, *, root):
    """Reject checkouts on both lexical/resolved paths; fail on uncertain I/O."""
    path = Path(path)
    if not path.is_absolute():
        raise ValueError(f"Use an absolute external path: {path}")
    try:
        resolved = path.resolve()
        seen = set()
        for candidate in (path, resolved):
            for parent in (candidate, *candidate.parents):
                if parent in seen:
                    continue
                seen.add(parent)
                if parent == root or _repository_at(parent):
                    raise ValueError(f"Output is inside a repository: {path}")
        return resolved
    except (OSError, RuntimeError) as error:
        raise ValueError(f"Cannot safely inspect output path: {path}") from error
