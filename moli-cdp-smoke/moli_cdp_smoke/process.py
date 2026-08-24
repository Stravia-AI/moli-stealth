from __future__ import annotations

import asyncio
import os
import signal
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from os import PathLike


_PROCESS_CLEANUP_TIMEOUT_SECONDS = 2.0
INHERIT_PROCESS_GROUP_ENV = "MOLI_SMOKE_INHERIT_PROCESS_GROUP"


def subprocess_starts_new_session() -> bool:
    return os.name == "posix" and os.environ.get(INHERIT_PROCESS_GROUP_ENV) != "1"


@dataclass(frozen=True)
class CapturedProcessResult:
    returncode: int
    stdout: bytes
    stderr: bytes


class CapturedProcessTimeout(TimeoutError):
    def __init__(
        self,
        timeout_seconds: float,
        stdout: bytes,
        stderr: bytes,
        *,
        output_closed: bool,
    ) -> None:
        super().__init__(f"process timed out after {timeout_seconds:g}s")
        self.timeout_seconds = timeout_seconds
        self.stdout = stdout
        self.stderr = stderr
        self.output_closed = output_closed


def _consume_task_result(task: asyncio.Task[object]) -> None:
    try:
        task.exception()
    except (asyncio.CancelledError, Exception):
        pass


async def _task_completed(task: asyncio.Task[object], timeout_seconds: float) -> bool:
    done, _ = await asyncio.wait((task,), timeout=timeout_seconds)
    return task in done


def _signal_process_tree(
    process: asyncio.subprocess.Process,
    process_signal: signal.Signals,
) -> None:
    if os.name == "posix":
        try:
            os.killpg(process.pid, process_signal)
            return
        except ProcessLookupError:
            pass
        except OSError:
            # Fall back to the direct child below. The bounded waits still keep
            # cleanup from becoming a second unbounded failure mode.
            pass

    if process.returncode is not None:
        return
    try:
        if process_signal == signal.SIGTERM:
            process.terminate()
        else:
            process.kill()
    except ProcessLookupError:
        pass


async def _finish_communication_after_kill(
    process: asyncio.subprocess.Process,
    communication: asyncio.Task[tuple[bytes, bytes]],
) -> tuple[bytes, bytes, bool]:
    _signal_process_tree(process, signal.SIGKILL)
    if await _task_completed(communication, _PROCESS_CLEANUP_TIMEOUT_SECONDS):
        stdout, stderr = communication.result()
        return stdout, stderr, True

    # A process which escaped the new process group can still own one of the
    # inherited pipes. Do not let that escaped descriptor turn timeout cleanup
    # back into an infinite communicate() wait.
    communication.cancel()
    communication.add_done_callback(_consume_task_result)
    return b"", b"", False


async def run_captured_process(
    argv: Sequence[str],
    *,
    timeout_seconds: float,
    cwd: str | PathLike[str] | None = None,
    env: Mapping[str, str] | None = None,
) -> CapturedProcessResult:
    if timeout_seconds <= 0:
        raise ValueError("timeout_seconds must be positive")

    process = await asyncio.create_subprocess_exec(
        *argv,
        cwd=cwd,
        env=env,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        # Every managed external command gets its own POSIX process group so a
        # timeout can terminate descendants which inherited stdout/stderr.
        start_new_session=subprocess_starts_new_session(),
    )
    communication = asyncio.create_task(process.communicate())
    try:
        completed = await _task_completed(communication, timeout_seconds)
    except BaseException:
        if not communication.done():
            await _finish_communication_after_kill(process, communication)
        raise

    if completed:
        stdout, stderr = communication.result()
        if process.returncode is None:
            raise RuntimeError("completed subprocess has no return code")
        return CapturedProcessResult(process.returncode, stdout, stderr)

    stdout, stderr, output_closed = await _finish_communication_after_kill(
        process,
        communication,
    )
    raise CapturedProcessTimeout(
        timeout_seconds,
        stdout,
        stderr,
        output_closed=output_closed,
    )


async def terminate_process_tree(
    process: asyncio.subprocess.Process,
    *,
    terminate_timeout_seconds: float = 5.0,
    kill_timeout_seconds: float = 2.0,
) -> bool:
    if process.returncode is not None:
        _signal_process_tree(process, signal.SIGKILL)
        return True

    wait_task = asyncio.create_task(process.wait())
    _signal_process_tree(process, signal.SIGTERM)
    if await _task_completed(wait_task, terminate_timeout_seconds):
        # The dedicated group may still contain descendants after its leader
        # exits. The caller is tearing the service down, so clear any remaining
        # group members as well.
        _signal_process_tree(process, signal.SIGKILL)
        return True

    _signal_process_tree(process, signal.SIGKILL)
    if await _task_completed(wait_task, kill_timeout_seconds):
        return True
    wait_task.cancel()
    wait_task.add_done_callback(_consume_task_result)
    return False
