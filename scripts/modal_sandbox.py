# /// script
# requires-python = ">=3.10"
# dependencies = ["modal"]
# ///
"""Modal sandbox sidecar for Genesis.

Usage: uv run modal_sandbox.py <command>
Commands: create, exec, snapshot, terminate
Communication: JSON on stdin → JSON on stdout
"""
import json
import sys

import modal


def cmd_create(args: dict) -> dict:
    image_name = args.get("image", "nikolaik/python-nodejs:python3.11-nodejs20")
    snapshot_id = args.get("snapshot_id")
    cpu = args.get("cpu", 1)
    memory_mb = args.get("memory_mb", 5120)
    disk_mb = args.get("disk_mb", 51200)
    gpu = args.get("gpu")

    if snapshot_id:
        image = modal.Image.from_id(snapshot_id)
    else:
        image = modal.Image.from_registry(image_name)

    kwargs = {}
    if cpu:
        kwargs["cpu"] = cpu
    if memory_mb:
        kwargs["memory"] = memory_mb
    if disk_mb:
        kwargs["ephemeral_disk"] = disk_mb
    if gpu:
        kwargs["gpu"] = gpu

    app = modal.App.lookup("genesis-sandbox", create_if_missing=True)
    sandbox = modal.Sandbox.create(
        "/usr/bin/env", "bash", "-l",
        image=image,
        timeout=3600,
        app=app,
        **kwargs,
    )
    return {"sandbox_id": sandbox.object_id}


def cmd_exec(args: dict) -> dict:
    sandbox_id = args["sandbox_id"]
    command = args["command"]
    cwd = args.get("cwd", "/root")
    timeout = args.get("timeout", 120)

    sandbox = modal.Sandbox.from_id(sandbox_id)
    process = sandbox.exec(
        "bash", "-c", f"cd {cwd} && {command}",
        timeout=timeout,
    )
    try:
        process.wait()
    except modal.exception.SandboxTimeoutError:
        return {"output": f"Command timed out after {timeout}s", "exit_code": 124}

    stdout = process.stdout.read()
    stderr = process.stderr.read()
    output = stdout
    if stderr:
        output = f"{stdout}\n[stderr]\n{stderr}" if stdout else f"[stderr]\n{stderr}"

    return {"output": output, "exit_code": process.returncode}


def cmd_snapshot(args: dict) -> dict:
    sandbox_id = args["sandbox_id"]
    sandbox = modal.Sandbox.from_id(sandbox_id)
    image = sandbox.snapshot_filesystem()
    return {"image_id": image.object_id}


def cmd_terminate(args: dict) -> dict:
    sandbox_id = args["sandbox_id"]
    sandbox = modal.Sandbox.from_id(sandbox_id)
    sandbox.terminate()
    return {"ok": True}


COMMANDS = {
    "create": cmd_create,
    "exec": cmd_exec,
    "snapshot": cmd_snapshot,
    "terminate": cmd_terminate,
}


def main():
    if len(sys.argv) < 2:
        print(json.dumps({"error": f"Usage: {sys.argv[0]} <command>"}), file=sys.stderr)
        sys.exit(1)

    command = sys.argv[1]
    if command not in COMMANDS:
        print(json.dumps({"error": f"Unknown command: {command}"}), file=sys.stderr)
        sys.exit(1)

    args_str = sys.stdin.read().strip()
    args = json.loads(args_str) if args_str else {}

    try:
        result = COMMANDS[command](args)
        print(json.dumps(result))
    except Exception as e:
        print(json.dumps({"error": str(e)}), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
