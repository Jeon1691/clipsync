# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Enforce the complete regression ledger and verify its registered test references.

This checks bookkeeping, source references, and test discovery. It cannot prove
that assertions reproduce the named historical defect; that requires source
review and the separately retained historical RED/GREEN evidence. Discovery is
not execution: CI runs the tests separately, and platform/release-only checks
are explicitly reported as registrations rather than local execution claims.
"""

import argparse
import ast
import json
from pathlib import Path
import re
import shlex
import subprocess
import sys
import unittest


BASELINE_IDS = frozenset(f"F{number:02}" for number in range(1, 85))
PLATFORMS = frozenset({"linux", "macos", "windows"})


class PolicyError(Exception):
    """Invalid ledger or unavailable verification evidence."""


def source_path(root, name):
    if not isinstance(name, str) or not name:
        raise PolicyError("source must be a repository-relative path")
    relative = Path(name)
    root = root.resolve()
    path = (root / relative).resolve()
    if relative.is_absolute() or ".." in relative.parts or not path.is_relative_to(root):
        raise PolicyError(f"source escapes repository: {name}")
    if not path.is_file():
        raise PolicyError(f"missing source: {name}")
    return path


def string_list(value, label, allow_empty=False):
    if not isinstance(value, list) or (not value and not allow_empty):
        raise PolicyError(f"{label} must be a {'possibly empty ' if allow_empty else 'nonempty '}list")
    if any(not isinstance(item, str) or not item for item in value):
        raise PolicyError(f"{label} must contain nonempty strings")
    return value


def run(command, root):
    try:
        result = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=600)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise PolicyError(f"cannot run {command[0]}: {error}") from error
    if result.returncode:
        raise PolicyError(f"{' '.join(map(str, command))} failed: {result.stderr[-2000:]}")
    return result.stdout


def parse_test_list(text):
    return {match.group(1) for line in text.splitlines()
            if (match := re.fullmatch(r"(.+): test", line.strip()))}


def discover_rust_tests(root):
    metadata = json.loads(run(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], root))
    members = set(metadata["workspace_members"])
    packages = {package["id"]: package["name"] for package in metadata["packages"]
                if package["id"] in members}
    output = run(["cargo", "test", "--workspace", "--locked", "--no-run", "--message-format=json"], root)
    inventory = {}
    for line in output.splitlines():
        try:
            artifact = json.loads(line)
        except json.JSONDecodeError:
            continue
        if artifact.get("reason") != "compiler-artifact" or not artifact.get("executable"):
            continue
        if not artifact.get("profile", {}).get("test") or artifact.get("package_id") not in packages:
            continue
        target = artifact["target"]
        library_types = {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"}
        kind = ("lib" if library_types.intersection(target["kind"])
                else next((kind for kind in target["kind"] if kind in {"test", "bin"}), None))
        if kind is None:
            continue
        executable = artifact["executable"]
        listed = parse_test_list(run([executable, "--list", "--format", "terse"], root))
        ignored = parse_test_list(run([executable, "--list", "--format", "terse", "--ignored"], root))
        key = (packages[artifact["package_id"]], f"{kind}:{target['name']}")
        inventory.setdefault(key, set()).update(listed - ignored)
    if not inventory:
        raise PolicyError("Cargo returned no workspace test executables")
    return inventory


def discover_python_tests(root):
    suite = unittest.TestLoader().discover(str(root / "scripts/tests"), pattern="test_*.py")
    found = set()

    def visit(node):
        if isinstance(node, unittest.TestSuite):
            for child in node:
                visit(child)
            return
        if node.__class__.__name__ == "_FailedTest":
            raise PolicyError(f"Python unittest discovery failed: {node.id()}")
        method = getattr(node, node._testMethodName)
        if (getattr(type(node), "__unittest_skip__", False)
                or getattr(method, "__unittest_skip__", False)
                or getattr(method, "__unittest_expecting_failure__", False)):
            return
        found.add(node.id())

    visit(suite)
    return found


def load_workflow(path):
    # safe_load uses only standard Ruby/Psych classes, with no arbitrary YAML objects.
    program = "puts JSON.generate(YAML.safe_load(File.read(ARGV[0]), permitted_classes: [Date, Time], aliases: true))"
    return json.loads(run(["ruby", "-ryaml", "-rjson", "-rdate", "-e", program, str(path)], path.parent))


def shell_commands(text):
    commands = []
    for line in text.replace("\\\n", " ").splitlines():
        try:
            tokens = shlex.split(line, comments=True)
        except ValueError:
            continue  # A heredoc/body fragment is not a standalone command.
        while tokens and re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*=.*", tokens[0]):
            tokens.pop(0)
        if tokens:
            commands.append(tokens)
    return commands


def matches_command(commands, requirement):
    prefix = string_list(requirement.get("prefix"), "command prefix")
    required = string_list(requirement.get("required", []), "required arguments", allow_empty=True)
    for tokens in commands:
        if tokens[:len(prefix)] != prefix:
            continue
        remaining = iter(tokens[len(prefix):])
        if all(any(actual == expected for actual in remaining) for expected in required):
            return True
    return False


def is_disabled(value):
    return value is False or (isinstance(value, str) and value.replace(" ", "") in {"false", "${{false}}"})


def verify_workflow(check, workflow):
    if not isinstance(workflow, dict) or not isinstance(workflow.get("jobs"), dict):
        raise PolicyError("workflow must contain a jobs mapping")
    jobs = workflow.get("jobs", {})
    job = jobs.get(check.get("job"))
    if not isinstance(job, dict) or is_disabled(job.get("if")):
        raise PolicyError("workflow job is missing or disabled")
    steps = [step for step in job.get("steps", []) if step.get("name") == check.get("step")]
    if len(steps) != 1 or is_disabled(steps[0].get("if")) or not isinstance(steps[0].get("run"), str):
        raise PolicyError("workflow step must identify one enabled run step")
    requirements = check.get("commands")
    if not isinstance(requirements, list) or not requirements:
        raise PolicyError("workflow commands must be a nonempty list")
    commands = shell_commands(steps[0]["run"])
    for requirement in requirements:
        if not isinstance(requirement, dict) or not matches_command(commands, requirement):
            raise PolicyError(f"required workflow command not found: {requirement}")
    if check.get("execution") == "release-before-publish":
        ancestors = set()

        def visit(name):
            needs = jobs.get(name, {}).get("needs", [])
            for dependency in [needs] if isinstance(needs, str) else needs:
                if dependency not in ancestors:
                    ancestors.add(dependency)
                    visit(dependency)

        visit("publish")
        if "publish" not in jobs or check["job"] not in ancestors:
            raise PolicyError("release check is not a dependency of publication")


def validate_check(check, root, platform, rust_tests, python_tests, workflow_loader):
    if not isinstance(check, dict):
        raise PolicyError("check must be an object")
    platforms = string_list(check.get("platforms"), "platforms")
    if not set(platforms) <= PLATFORMS or len(set(platforms)) != len(platforms):
        raise PolicyError("platform scope must name supported, distinct CI platforms")
    path = source_path(root, check.get("source"))
    kind = check.get("kind")
    applies = platform in platforms
    status = "registered" if applies else "other-platform"
    if kind == "rust":
        package, target, name = check.get("package"), check.get("target"), check.get("test")
        if not isinstance(package, str) or not check["source"].startswith(f"crates/{package}/"):
            raise PolicyError("Rust source must belong to the named package")
        if not isinstance(target, str) or not re.fullmatch(r"(?:lib|test|bin):[A-Za-z0-9_-]+", target):
            raise PolicyError("Rust target must use kind:name")
        if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*(?:::[A-Za-z_][A-Za-z_0-9]*)*", name):
            raise PolicyError("Rust test must be a qualified test name")
        leaf = re.escape(name.split("::")[-1])
        if not re.search(rf"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+{leaf}\s*\(", path.read_text()):
            raise PolicyError(f"Rust test function missing in source: {name}")
        if applies and rust_tests is not None:
            if name not in rust_tests.get((package, target), set()):
                raise PolicyError(f"Rust test not discovered (or ignored) in {package}/{target}: {name}")
            status = "discovered"
    elif kind == "python":
        name = check.get("test")
        if not isinstance(name, str) or len(name.split(".")) != 3:
            raise PolicyError("Python test must use module.Class.method")
        module, class_name, method = name.split(".")
        if module != path.stem or not check["source"].startswith("scripts/tests/"):
            raise PolicyError("Python source must identify its scripts/tests module")
        tree = ast.parse(path.read_text())
        if not any(isinstance(node, ast.ClassDef) and node.name == class_name
                   and any(isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)) and child.name == method
                           for child in node.body) for node in tree.body):
            raise PolicyError(f"Python test method missing in source: {name}")
        if applies and python_tests is not None:
            if name not in python_tests:
                raise PolicyError(f"Python unittest not discovered (or skipped): {name}")
            status = "discovered"
    elif kind == "workflow":
        if not check["source"].startswith(".github/workflows/"):
            raise PolicyError("workflow source must be under .github/workflows")
        if not all(isinstance(check.get(key), str) and check[key] for key in ["job", "step"]):
            raise PolicyError("workflow check requires a job and named step")
        if applies:
            verify_workflow(check, workflow_loader(path))
            status = "workflow-registration"
    else:
        raise PolicyError(f"unsupported check kind: {kind}")
    return {"kind": kind, "source": check["source"], "test": check.get("test"),
            "status": status, "platforms": platforms, "execution": check.get("execution", "routine-ci")}


def evaluate(document, root, platform, rust_tests=None, python_tests=None, workflow_loader=load_workflow):
    errors = []
    report = {"passed": False, "covered": 0, "total": 0, "percent": 0,
              "minimum_percent": 50, "platform": platform, "errors": errors, "defects": []}
    if not isinstance(document, dict) or document.get("schema_version") != 1:
        errors.append("manifest must use schema_version 1")
        return report
    minimum = document.get("minimum_percent", 50)
    if isinstance(minimum, bool) or not isinstance(minimum, (int, float)) or not 50 <= minimum <= 100:
        errors.append("minimum_percent must be between 50 and 100")
    else:
        report["minimum_percent"] = minimum
    rows = document.get("defects")
    if not isinstance(rows, list):
        errors.append("defects must be a list")
        return report
    seen = set()
    for row in rows:
        if not isinstance(row, dict):
            errors.append("defect must be an object")
            continue
        identity = row.get("id")
        if not isinstance(identity, str) or not re.fullmatch(r"F[0-9]{2,}", identity) or identity != f"F{int(identity[1:]):02}":
            errors.append(f"invalid stable defect ID: {identity}")
            continue
        if identity in seen:
            errors.append(f"duplicate defect ID: {identity}")
            continue
        seen.add(identity)
        result = {"id": identity, "registered": False, "checks": []}
        report["defects"].append(result)
        row_errors = len(errors)
        if "covered" in row:
            errors.append(f"{identity}: manual covered flags are forbidden; register concrete checks")
        if not isinstance(row.get("description"), str) or not row["description"].strip():
            errors.append(f"{identity}: defect description is required")
        try:
            string_list(row.get("fixes"), "fixes")
        except PolicyError as error:
            errors.append(f"{identity}: {error}")
        checks = row.get("checks")
        if not isinstance(checks, list):
            errors.append(f"{identity}: checks must be a list")
            continue
        for check in checks:
            try:
                result["checks"].append(validate_check(check, root, platform, rust_tests, python_tests, workflow_loader))
            except (PolicyError, OSError, SyntaxError, ValueError, TypeError) as error:
                errors.append(f"{identity}: {error}")
        result["registered"] = bool(checks) and len(errors) == row_errors
    missing = BASELINE_IDS - seen
    if missing:
        errors.append("baseline defect IDs cannot be removed: " + ", ".join(sorted(missing)))
    if sorted(int(item[1:]) for item in seen) != list(range(1, len(seen) + 1)):
        errors.append("new stable defect IDs must be appended without gaps")
    report["total"] = len(seen)
    report["covered"] = sum(row["registered"] for row in report["defects"])
    if report["total"]:
        report["percent"] = 100 * report["covered"] / report["total"]
    if report["covered"] * 100 < report["minimum_percent"] * report["total"]:
        errors.append(f"registered regressions are below {report['minimum_percent']}% of the complete ledger")
    report["passed"] = not errors
    return report


def current_platform():
    return "windows" if sys.platform == "win32" else "macos" if sys.platform == "darwin" else "linux"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", default="docs/regressions.json")
    parser.add_argument("--verify-tests", action="store_true", help="Discover compiled non-ignored Cargo tests and Python unittests")
    parser.add_argument("--platform", choices=sorted(PLATFORMS), default=current_platform())
    parser.add_argument("--report", type=Path, help="Write the complete JSON policy report")
    args = parser.parse_args(argv)
    root = Path(__file__).resolve().parents[1]
    try:
        if args.verify_tests and args.platform != current_platform():
            raise PolicyError("--verify-tests must use the actual host platform; other scopes support static registration checks only")
        document = json.loads(source_path(root, args.manifest).read_text())
        if not isinstance(document, dict) or not isinstance(document.get("defects"), list):
            raise PolicyError("manifest must contain an object with a defects list")
        rust_tests = discover_rust_tests(root) if args.verify_tests else None
        applicable_python = any(check.get("kind") == "python" and args.platform in check.get("platforms", [])
                                for row in document["defects"] if isinstance(row, dict)
                                for check in row.get("checks", []) if isinstance(check, dict))
        python_tests = discover_python_tests(root) if args.verify_tests and applicable_python else None
        report = evaluate(document, root, args.platform, rust_tests, python_tests)
        report["verify_tests"] = args.verify_tests
    except (PolicyError, OSError, ValueError, TypeError, KeyError) as error:
        report = {"passed": False, "errors": [str(error)]}
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Registered regression checks: {report.get('covered', 0)}/{report.get('total', 0)} "
          f"({report.get('percent', 0):.2f}%; minimum {report.get('minimum_percent', 50)}%)")
    for error in report["errors"]:
        print(f"error: {error}", file=sys.stderr)
    print("Mechanical validation does not establish assertion quality or historical reproduction; review the regression audit and owning CI runs.")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
