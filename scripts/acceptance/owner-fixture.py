#!/usr/bin/env python3
"""Make an isolated owner ceremony fixture; never starts with real owner state.

Example:
  python3 scripts/acceptance/owner-fixture.py /tmp/maestro-owner-dogfood-UNIQUE \
    --validator /absolute/workspace/scripts/ask_ledger.py --binary ./target/debug/maestro
  # Run the printed command, then follow signed-owner.js in T3 preview.

The real validator is copied unchanged. Maestro itself creates its isolated answer
worktree. Local mode forbids pushes/landing, and the ask names no agent session.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess

import yaml


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--validator", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--serve", action="store_true")
    args = parser.parse_args()
    root = args.root.absolute()
    # Fixed fixture origin is also enforced in signed-owner.js.
    if root.parent.resolve() not in (Path("/tmp").resolve(),) or not root.name.startswith("maestro-owner-dogfood-"):
        parser.error("root must be a new /tmp/maestro-owner-dogfood-* directory")
    if root.exists():
        parser.error("root already exists; choose a new fixture directory")
    validator = args.validator.resolve(strict=True)
    binary = args.binary.resolve(strict=True)
    if validator.name != "ask_ledger.py" or not os.access(binary, os.X_OK):
        parser.error("supply the canonical ask_ledger.py and an executable Maestro binary")
    root.mkdir(mode=0o700)
    workspace = root / "workspace"
    for path in [workspace / ".control/asks", workspace / ".control/initiatives", workspace / ".control/loops", workspace / "scripts", root / "work", root / "fleet-state", root / "fleet-meta", root / "machine", root / "locks", root / "owner"]:
        path.mkdir(parents=True, exist_ok=True)
    # The production surgical editor requires block YAML, not an inline JSON mapping.
    ask = {
        "arc": "owner-dogfood-fixture", "tz": "America/Bogota", "opened": "2026-11-23", "lanes": ["fixture"],
        "asks": [{
            "id": "signed-fixture", "ask": "Dogfood fixture: choose a test answer", "headline": "Dogfood fixture: sign or hold a local test answer?",
            "stakes": "An isolated local acceptance answer, with no external effect.",
            "why_you": "This fixture verifies the owner ceremony without a real owner decision.",
            "class": "decision", "gates": ["fixture"], "blocking": False, "preanswerable": True, "default": "hold",
            "exhausted": ["read: this is an isolated acceptance fixture", "default: hold has no external effect"],
            "options": [
                {"id": "accept", "label": "Accept fixture", "consequence": "Writes only the isolated fixture ledger.", "reversible": True},
                {"id": "hold", "label": "Hold fixture", "consequence": "Records the local fixture hold.", "reversible": True, "recommended": True, "status_quo": True},
            ],
        }],
    }
    ledger = workspace / ".control/asks/owner-dogfood-fixture.yaml"
    ledger.write_text(yaml.safe_dump(ask, sort_keys=False, default_flow_style=False))
    (workspace / "DOGFOOD-FIXTURE").write_text("Isolated owner acceptance fixture. No real session, remote or credentials.\n")
    shutil.copyfile(validator, workspace / "scripts/ask_ledger.py")
    validation = subprocess.run(["python3", str(workspace / "scripts/ask_ledger.py"), "validate", str(ledger)], cwd=workspace, text=True, capture_output=True)
    (root / "validator-baseline.log").write_text(validation.stdout + validation.stderr)
    if validation.returncode:
        raise SystemExit("Fixture validator refused the baseline; inspect " + str(root / "validator-baseline.log"))
    def git(*arguments):
        environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        subprocess.run(["git", "-C", str(workspace), *arguments], check=True, capture_output=True, env=environment)
    git("init", "-q", "-b", "main")
    git("config", "user.name", "Maestro fixture")
    git("config", "user.email", "fixture@localhost")
    git("add", ".")
    git("commit", "-qm", "Isolated owner browser fixture")
    git("update-ref", "refs/remotes/origin/main", "HEAD")
    settings = {
        "BROOMVA_WORKSPACE": str(workspace), "MAESTRO_WORKSPACE": str(workspace), "MAESTRO_TREE": "origin/main",
        "BROOMVA_HOME": str(root / "machine"), "MAESTRO_WORK_ROOT": str(root / "work"),
        "MAESTRO_FLEET_STATE": str(root / "fleet-state"), "MAESTRO_FLEET_META": str(root / "fleet-meta"),
        "MAESTRO_PASEO_ASK_LOCKS": str(root / "locks"), "MAESTRO_OWNER_DIR": str(root / "owner"),
        "MAESTRO_ANSWERS_CHECKOUT": str(root / "answers-checkout"), "MAESTRO_ANSWERS_LOCAL": "1",
        "MAESTRO_WEB_ORIGIN": "http://localhost:7190", "MAESTRO_T3_BIN": "/usr/bin/false",
    }
    command = [str(binary), "serve", "--http", "--bind", "127.0.0.1:7190"]
    env_file = root / "fixture-env.sh"
    env_file.write_text("unset MAESTRO_USAGE MAESTRO_ANSWER_THREAD\n" + "\n".join(f"export {key}={shlex.quote(value)}" for key, value in settings.items()) + "\n" + "exec " + shlex.join(command) + "\n")
    os.chmod(env_file, 0o700)
    print(json.dumps({"fixture_root": str(root), "start": "bash " + shlex.quote(str(env_file)), "owner_url": "http://localhost:7190/owner", "decision_url": "http://localhost:7190/decisions/owner-dogfood-fixture.yaml/signed-fixture", "setup_code_file_after_start": str(root / "owner/setup-code"), "validator": "baseline passed", "no_remote": True, "no_agent_session": True}))
    if args.serve:
        environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        for key in ("MAESTRO_USAGE", "MAESTRO_ANSWER_THREAD"):
            environment.pop(key, None)
        environment.update(settings)
        os.execve(binary, command, environment)


if __name__ == "__main__":
    main()
