import json
import os
import sys
import time

mode = os.environ.get("FAKE_REPL_MODE", "worker")
if mode == "eof":
    sys.stderr.write("fixture eof")
    sys.exit(0)
if mode == "timeout":
    time.sleep(10)
    sys.exit(0)
if mode == "oversize":
    print(json.dumps({"env": 0, "padding": "x" * 4096}), flush=True)
    print(flush=True)
    time.sleep(10)
    sys.exit(0)
if mode == "unterminated":
    sys.stdout.write("x" * 100000)
    sys.stdout.flush()
    time.sleep(10)
    sys.exit(0)
if mode == "malformed":
    print("{not-json}", flush=True)
    print(flush=True)
    time.sleep(10)
    sys.exit(0)

env = 0
while True:
    lines = []
    for line in sys.stdin:
        if line.strip() == "":
            break
        lines.append(line)
    if not lines:
        break
    request = json.loads("".join(lines))
    command = request["cmd"]
    if "trigger_transport_failure" in command and (
        mode == "worker-timeout-validation"
        or (mode == "worker-timeout-extraction" and "#mls_extract" in command)
    ):
        time.sleep(10)
        sys.exit(0)
    env += 1
    messages = []
    if command.startswith("#mls_validate_source"):
        messages = [{"severity":"info", "data":"MLS_VALIDATE:{\"declaration\":\"mls_demo\",\"kind\":\"theorem\"}"}]
    elif "#mls_extract" in command:
        if "badType" in command:
            messages = [{"severity":"error", "pos":{"line":2,"column":15}, "data":"unknown identifier"}]
        else:
            messages = [{"severity":"info", "data":"MLS_EXTRACT:{\"declaration\":\"mls_demo\",\"kind\":\"theorem\",\"binders\":[],\"goal\":\"True\",\"goal_outer_head\":\"constant\",\"axioms\":[\"sorryAx\"],\"has_sorry\":true}"}]
            if mode == "duplicate":
                messages.append(messages[0])
    print(json.dumps({"env": env, "messages": messages}))
    print()
    sys.stdout.flush()
