#!/usr/bin/env python3
"""Probe the running desktop app's local control API from INSIDE its container.

Proves the whole vertical slice booted: process up, webview loaded (the state
snapshot only exists once the React app mounted and invoked control_state),
control server listening, token auth working. Exit 0 = healthy.
"""
import asyncio
import json
import pathlib
import sys

import websockets


async def main() -> int:
    cfg_path = pathlib.Path.home() / ".config" / "org.raumdock.subraum" / "control.json"
    cfg = json.loads(cfg_path.read_text())

    async with websockets.connect(f"ws://127.0.0.1:{cfg['port']}") as ws:
        # Wrong token first: must be rejected, connection must survive.
        await ws.send(json.dumps({"t": "auth", "token": "wrong"}))
        reply = json.loads(await asyncio.wait_for(ws.recv(), 10))
        assert reply["t"] == "auth-failed", f"expected auth-failed, got {reply}"

        await ws.send(json.dumps({"t": "auth", "token": cfg["token"]}))
        hello = json.loads(await asyncio.wait_for(ws.recv(), 10))
        assert hello["t"] == "hello" and hello["app"] == "subraum", f"bad hello: {hello}"

        # The webview reports its state on mount; poll briefly until the
        # snapshot carries the connected flag (False — nobody joined a room).
        state = hello.get("state") or {}
        for _ in range(20):
            if "connected" in state:
                break
            await ws.send(json.dumps({"t": "volume", "value": 100}))  # benign nudge
            try:
                msg = json.loads(await asyncio.wait_for(ws.recv(), 1))
                if msg.get("t") == "state":
                    state = msg["state"]
            except asyncio.TimeoutError:
                pass
        assert state.get("connected") is False, f"webview never reported state: {state}"
        assert "volume" in state and "channel" in state, f"snapshot incomplete: {state}"

    print(f"PROBE OK: control API up, webview alive, state={json.dumps(state, ensure_ascii=False)}")
    return 0


sys.exit(asyncio.run(main()))
