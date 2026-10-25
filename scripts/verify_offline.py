#!/usr/bin/env python3
"""Observe local Linux executables with strace; this is not a sandbox.

Unknown syscalls fail closed. Only runtime reads, private memory, local thread
coordination and stdout/stderr writes are accepted. Artifacts are retained.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile


READ_ONLY = set("""
access faccessat faccessat2 stat lstat fstat newfstatat statx read pread64
readv preadv preadv2 readlink readlinkat getdents getdents64 lseek close
brk munmap mprotect mremap madvise arch_prctl set_tid_address set_robust_list
rseq prlimit64 getrlimit getrandom rt_sigaction rt_sigprocmask rt_sigreturn
sigaltstack poll ppoll select pselect6 sched_getaffinity sched_yield
getpid getppid gettid getuid geteuid getgid getegid getresuid getresgid
clock_gettime clock_getres gettimeofday time uname futex futex_waitv
nanosleep clock_nanosleep restart_syscall exit exit_group
""".split())

EXPECTED_E2E_TESTS = {
    "approval_binding_survives_public_api_to_gateway_composition",
    "unavailable_dispatch_is_a_confirmed_terminal_local_failure",
    "cancelled_and_revoked_requests_remain_terminal",
    "sixteen_contenders_consume_approval_and_finish_only_once",
    "cancellation_before_begin_never_produces_a_dispatch_permit",
    "dispatch_cancellation_windows_preserve_the_exact_recorded_outcome",
    "simultaneous_begin_and_cancellation_have_only_legal_results",
    "simultaneous_finish_and_cancellation_have_only_legal_results",
}


def arguments(text):
    """Split strace arguments without treating quoted commas as separators."""
    result, start, depth, quoted, escaped = [], 0, 0, False, False
    for index, char in enumerate(text):
        if quoted:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                quoted = False
        elif char == '"':
            quoted = True
        elif char in "([{":
            depth += 1
        elif char in ")]}":
            depth -= 1
        elif char == "," and depth == 0:
            result.append(text[start:index].strip())
            start = index + 1
    if quoted or depth:
        raise ValueError("incomplete syscall arguments")
    return result + [text[start:].strip()]


def inspect_trace(paths, threads=False):
    violations, calls, executions = [], {}, 0
    for path in paths:
        pending = None
        for number, line in enumerate(path.read_text().splitlines(), 1):
            location = f"{path.name}:{number}"
            if line.startswith(("--- ", "+++ ")):
                continue
            if line.endswith(" <unfinished ...>"):
                if pending is not None:
                    violations.append(f"{location}: nested unfinished syscall")
                pending = line.removesuffix(" <unfinished ...>")
                continue
            if line.startswith("<... "):
                resumed = re.fullmatch(r"<\.\.\. (\w+) resumed>(.*)", line)
                if pending is None or resumed is None or not pending.startswith(resumed[1] + "("):
                    violations.append(f"{location}: unmatched resumed syscall")
                    continue
                line, pending = pending + resumed[2], None
            match = re.fullmatch(r"(\w+)\((.*)\)\s+=\s+(.+)", line)
            if match is None:
                violations.append(f"{location}: unparsed trace line")
                continue
            name, raw, returned = match.groups()
            calls[name] = calls.get(name, 0) + 1
            try:
                args = arguments(raw)
            except ValueError as error:
                violations.append(f"{location}: {error}")
                continue
            reason = None
            if name == "execve":
                executions += 1
                if executions != 1:
                    reason = "additional process execution"
                elif returned != "0":
                    reason = "initial execve did not succeed"
            elif name in {"clone", "clone3"}:
                if not threads or not re.search(r"\bflags=([^,}]*\|)?CLONE_THREAD\b", raw):
                    reason = "process creation or unexpected thread"
            elif name in {"open", "openat"}:
                flags = args[1 if name == "open" else 2]
                allowed = {"O_RDONLY", "O_CLOEXEC", "O_LARGEFILE", "O_DIRECTORY", "O_NOFOLLOW", "O_NONBLOCK"}
                if "O_RDONLY" not in flags.split("|") or not set(flags.split("|")) <= allowed:
                    reason = "writable or unsupported open"
            elif name in {"write", "writev"}:
                if args[0] not in {"1", "2"}:
                    reason = "write outside stdout/stderr"
            elif name in {"mmap", "mmap2"}:
                # Reject even read-only shared mappings to rule out later mprotect.
                flags = args[3].split("|")
                if "MAP_PRIVATE" not in flags or any(flag.startswith("MAP_SHARED") for flag in flags):
                    reason = "shared or unsupported mmap"
            elif name == "fcntl":
                if args[1] not in {"F_GETFD", "F_GETFL"}:
                    reason = "mutating or unsupported fcntl"
            elif name == "ioctl":
                if args[1] not in {"TCGETS", "TIOCGWINSZ", "FIONREAD"}:
                    reason = "mutating or unsupported ioctl"
            elif name == "prctl":
                if args[0] not in {"PR_SET_NAME", "PR_GET_NAME"}:
                    reason = "unsupported prctl"
            elif name not in READ_ONLY:
                # Includes network, filesystem mutation, descriptor duplication,
                # pwrite/sendfile/splice/io_uring, process creation and unknown calls.
                reason = "forbidden or unknown syscall"
            if reason:
                violations.append(f"{location}: {name}: {reason}")
        if pending is not None:
            violations.append(f"{path.name}: incomplete syscall at end of trace")
    if executions != 1:
        violations.append(f"expected one initial execve, observed {executions}")
    return {"syscalls": calls, "violations": violations}


def snapshot(root):
    result = {}
    for path in root.rglob("*"):
        if path.is_symlink():
            value = ("symlink", os.readlink(path))
        elif path.is_file():
            value = ("file", hashlib.sha256(path.read_bytes()).hexdigest(), path.stat().st_mode)
        else:
            value = ("directory", path.stat().st_mode)
        result[str(path.relative_to(root))] = value
    return result


def observe(strace, command, prefix, work, timeout, threads=False):
    before = snapshot(work)
    env = {"PATH": os.defpath, "LC_ALL": "C", "TZ": "UTC", "TMPDIR": str(work)}
    process = subprocess.Popen(
        [strace, "-ff", "-qq", "-s", "2048", "-o", str(prefix), "--", *command],
        cwd=work, env=env, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
    )
    timed_out = False
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        os.killpg(process.pid, signal.SIGKILL)
        stdout, stderr = process.communicate()
    prefix.with_suffix(".stdout").write_bytes(stdout)
    prefix.with_suffix(".stderr").write_bytes(stderr)
    traces = sorted(prefix.parent.glob(prefix.name + ".[0-9]*"))
    report = inspect_trace(traces, threads)
    report.update(command=command, exit_code=process.returncode, timeout=timed_out,
                  traces=[str(path) for path in traces], work_changed=before != snapshot(work))
    if timed_out:
        report["violations"].append("execution timed out")
    return report, stdout.decode(errors="replace"), stderr.decode(errors="replace")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/openquick"))
    parser.add_argument("--e2e-binary", type=Path)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--timeout", type=float, default=60)
    options = parser.parse_args()
    if sys.platform != "linux" or not shutil.which("strace"):
        parser.error("Linux and strace are required; observation cannot be skipped")
    if options.timeout <= 0:
        parser.error("--timeout must be positive")
    output = options.output_dir.resolve() if options.output_dir else Path(tempfile.mkdtemp(prefix="openquick-offline-"))
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        parser.error("--output-dir must be empty to preserve existing artifacts")
    work = output / "work"
    work.mkdir()
    results = {}
    # A real, local-only negative control proves writable opens are detected.
    control, _, _ = observe(shutil.which("strace"), ["/bin/sh", "-c", "printf observer-control > sentinel"], output / "negative-control", work, options.timeout)
    control_ok = (control["exit_code"] == 0 and not control["timeout"] and
                  (work / "sentinel").is_file() and
                  (work / "sentinel").read_text() == "observer-control" and
                  any("writable or unsupported open" in item for item in control["violations"]))
    results["negative-control"] = dict(control, expected_rejection=control_ok, passed=control_ok)
    (work / "sentinel").unlink(missing_ok=True)
    child, _, _ = observe(shutil.which("strace"), ["/bin/sh", "-c", "/bin/true; :"], output / "process-control", work, options.timeout)
    child_ok = (child["exit_code"] == 0 and not child["timeout"] and
                any("additional process execution" in item or
                    re.search(r": (clone|clone3|fork|vfork):", item)
                    for item in child["violations"]))
    results["process-control"] = dict(child, expected_rejection=child_ok, passed=child_ok)
    cases = [
        ("status", ["status"], 0, ["core: local in-memory", "AgentGateway: unavailable", "durable recovery: unavailable"], []),
        ("demo", ["demo"], 0, ["Synthetic local demo", "missing approval: denied", "no tool was dispatched", "simulated unknown outcome: retry denied", "cancelled / revoked requests: admission denied"], []),
        ("gateway", ["gateway"], 1, [], ["AgentGateway integration unavailable; no tool was dispatched"]),
        ("unknown", ["unknown"], 2, [], ["unsupported arguments"]),
        ("extra-argument", ["status", "ignored"], 2, [], ["unsupported arguments"]),
    ]
    binary = str(options.binary.resolve(strict=True))
    for name, args, expected, out_parts, err_parts in cases:
        report, stdout, stderr = observe(shutil.which("strace"), [binary, *args], output / name, work, options.timeout)
        report["passed"] = (not report["violations"] and not report["work_changed"] and
                            report["exit_code"] == expected and
                            all(part in stdout for part in out_parts) and
                            all(part in stderr for part in err_parts) and
                            (bool(stdout) if out_parts else not stdout) and
                            (bool(stderr) if err_parts else not stderr))
        results[name] = report
    if options.e2e_binary:
        report, stdout, stderr = observe(shutil.which("strace"), [str(options.e2e_binary.resolve(strict=True)), "--test-threads=1"], output / "e2e", work, options.timeout, threads=True)
        completed = set(re.findall(r"^test (\w+) \.\.\. ok$", stdout, re.MULTILINE))
        summary = f"test result: ok. {len(EXPECTED_E2E_TESTS)} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;"
        if completed != EXPECTED_E2E_TESTS or summary not in stdout:
            report["violations"].append("expected all eight named E2E tests, with none ignored or filtered")
        report["passed"] = (not report["violations"] and not report["work_changed"] and
                            report["exit_code"] == 0 and not stderr and
                            completed == EXPECTED_E2E_TESTS and summary in stdout)
        report["completed_tests"] = sorted(completed)
        results["e2e"] = report
    passed = all(value["passed"] for value in results.values())
    metadata = {"passed": passed, "observer": "strace, not a sandbox",
                "binary_sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
                "results": results}
    (output / "summary.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(f"{'PASS' if passed else 'FAIL'}: artifacts at {output}")
    for name, report in results.items():
        print(f"{name}: exit {report['exit_code']}, {sum(report['syscalls'].values())} syscalls, {len(report['violations'])} violations")
        if not name.endswith("-control"):
            for violation in report["violations"][:10]:
                print(f"  {violation}")
    return 0 if passed else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, IndexError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        sys.exit(1)
