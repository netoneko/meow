#!/usr/bin/env python3
"""Timeline of a litter run: what the protocol did, and what each agent
spent its turns on.

Two clocks, because the yard keeps two:
  * the hub's event log is strictly ordered (epoch), so it is the
    protocol's timeline — open, assign, offer, claim, submit, clear;
  * the agents' console logs carry no absolute time but do carry each
    turn's duration, so they give the behavioural timeline — how long a
    model thought and what it did with the result.
"""
import json, re, subprocess

import os
AGENTS = os.environ.get("AGENTS", "sherlock hercules zenigata ressler").split()
RUN_DIR = os.environ.get("RUN_DIR", "/tmp/litter-run")
ANSI = re.compile(r"\x1b\[[0-9;]*m")

def dex(*a):
    return subprocess.run(["docker", "exec", *a], capture_output=True, text=True).stdout

print("=" * 78)
print("PROTOCOL TIMELINE (hub event log, in order)")
print("=" * 78)
raw = dex("-e", "MEOW_HOME=/operator", "litter-yard", "/bin/meow", "litter", "peers")
for line in raw.splitlines():
    if line.strip().startswith("{"):
        for i, e in enumerate(json.loads(line).get("events", [])):
            print(f"  {i:>3}. {e.replace('[event] ', '')}")
        break

print()
print("=" * 78)
print("AGENT TIMELINE (turns, what each cost, what it produced)")
print("=" * 78)
for a in AGENTS:
    log = ANSI.sub("", dex("litter-yard", "sh", "-c", f"cat /agents/{a}/log 2>/dev/null"))
    model = dex("litter-yard", "sh", "-c",
                f"grep -h current_model /agents/{a}/etc/meow/config 2>/dev/null").strip()
    print(f"\n-- {a}  ({model.split('=')[-1]}) --")
    turn = 0
    for line in log.splitlines():
        if "wakes on" in line:
            turn += 1
            n = re.search(r"wakes on (\d+)", line)
            print(f"   turn {turn}: woke on {n.group(1) if n else '?'} new message(s)")
        d = re.search(r"Duration: ([^|]+)$", line)
        if d and "Stream:" in line:
            print(f"            thought for {d.group(1).strip()}")
        t = re.search(r"ToolCalled: (\w+)(?: \| Arguments (.*))?", line)
        if t:
            args = " ".join((t.group(2) or "").split())[:100]
            print(f"            -> {t.group(1)} {args}")
        if "Unknown or unsupported tool" in line:
            print(f"            !! tool does not exist")
    if turn == 0:
        print("   (no turns yet)")
    # What the inference server itself measured, for the same agent — the
    # only way to tell "the model is slow" from "meow is waiting".
    try:
        with open(f"{RUN_DIR}/llama-{a}.log") as fh:
            times = [l for l in fh if "total time" in l]
        if times:
            last = times[-1].split("total time =")[-1].strip()
            print(f"            [server] {len(times)} request(s), last: {last}")
    except OSError:
        pass
