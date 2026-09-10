#!/usr/bin/env python3
"""Turns a Proctor agent brief on stdin into one prompt on stdout.

Kept apart from the adapter shell script so the prompt is readable and
testable on its own: `proctor run` cares only that the adapter edits the
worktree, so what an agent is told is the part worth reviewing.
"""
import json
import sys


def prompt(b):
    findings = b.get("findings") or ["(no assertion failed; see the measurements)"]
    observed = b.get("observed") or {}
    out = [
        "A payment-behavior contract is failing. Fix the code so it passes.",
        "",
        f"This is attempt {b['attempt']} of {b['attempts_allowed']}. "
        f"The previous attempt ended {b['outcome']}.",
        "",
        "The contract, which you must NOT edit:",
        "",
        b["task_toml"],
        "",
        "What failed:",
    ]
    out += [f"  - {f}" for f in findings]
    if observed:
        out += [
            "",
            "What Proctor measured independently, from the seller journal and",
            "the buyer ledger rather than the application's own output:",
        ]
        out += [f"  {k} = {v}" for k, v in sorted(observed.items())]
    out += [
        "",
        "Rules:",
        "  - Edit the implementation, never the task contract and never the",
        "    tests. A contract edited to turn a run green is a different task",
        "    and Proctor aborts the run.",
        "  - The observe half of a check is measured from evidence the",
        "    application does not write, so making it merely claim success",
        "    will not pass.",
        "  - This is a payment system. Never leave an authorization",
        "    outstanding to make a check pass.",
        "",
        "Make the change now.",
    ]
    return "\n".join(out)


def main():
    try:
        brief = json.load(sys.stdin)
    except json.JSONDecodeError as e:
        sys.exit(f"adapter: the brief was not JSON: {e}")
    for key in ("attempt", "attempts_allowed", "outcome", "task_toml"):
        if key not in brief:
            sys.exit(f"adapter: the brief has no {key}")
    print(prompt(brief))


if __name__ == "__main__":
    main()
