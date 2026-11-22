"""Stands in for `ctrpass serve`, answering commands with the events the real helper emits."""

import json
import sys


def emit(**event):
    print(json.dumps(event), flush=True)


emit(event="hello", version="0", interface=sys.argv[-1])
emit(event="state", state="idle")
store = None
for line in sys.stdin:
    cmd = json.loads(line)
    kind = cmd["cmd"]
    if kind == "keys":
        emit(event="keys", titles=["00020800"], record="slot0x31" in cmd["text"])
    elif kind == "load":
        store = cmd["store"]
        emit(event="loaded", mac="02:00:00:00:00:01", titles=len(store["titles"]),
             messages=len(store["messages"]))
    elif kind == "exchange":
        if store is None:
            emit(event="error", message="no store loaded")
            continue
        emit(event="state", state="exchanging")
        emit(event="engaged", peer="96:58:cb:00:00:01")
        emit(event="pass", pass_=0, role=cmd["role"], peer="96:58:cb:00:00:01", sent=[],
             refused=[], received=[], not_filed=[], messages=[], peers={"filter": ["96:58:cb:00:00:01"]})
        if cmd.get("passes") == 1:
            emit(event="state", state="idle")
    elif kind == "stop":
        emit(event="state", state="idle")
    elif kind == "quit":
        break
    else:
        emit(event="error", message="unexpected " + kind)
