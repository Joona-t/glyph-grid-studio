#!/usr/bin/env python3
"""MCP protocol tests for Glyph Grid Studio (Phase C)."""

import json
import os
import subprocess
import sys
import time

BIN = os.environ["BIN"]
SCRATCH = os.environ["SCRATCH"]
TEST_IMG = os.environ["TEST_IMG"]

PASS = 0
FAIL = 0


def ok(name):
    global PASS
    print(f"  ✓ {name}")
    PASS += 1


def bad(name, msg):
    global FAIL
    print(f"  ✗ {name}: {msg}")
    FAIL += 1


def boot():
    """Spawn an MCP server and complete the initialize handshake. Return the proc."""
    proc = subprocess.Popen(
        [BIN, "mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        bufsize=0,
    )
    send(proc, {
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"},
        },
    })
    init = recv(proc, timeout=5)
    if not init:
        proc.kill()
        return None, None
    send(proc, {"jsonrpc": "2.0", "method": "notifications/initialized"})
    return proc, init


def send(proc, req):
    proc.stdin.write((json.dumps(req) + "\n").encode())
    proc.stdin.flush()


def recv(proc, timeout=60):
    """Read one JSON-RPC message from stdout (with a deadline)."""
    import select
    deadline = time.time() + timeout
    while time.time() < deadline:
        rlist, _, _ = select.select([proc.stdout], [], [], 0.5)
        if proc.stdout in rlist:
            line = proc.stdout.readline().decode().strip()
            if line:
                return json.loads(line)
    return None


def shutdown(proc):
    if proc and proc.stdin and not proc.stdin.closed:
        proc.stdin.close()
    if proc:
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()


# --- Test C1: initialize handshake ---
proc, init = boot()
if init and init.get("result", {}).get("protocolVersion") == "2024-11-05":
    ok("C1 initialize handshake (server version: %s)" % init["result"].get("serverInfo", {}).get("version", "?"))
else:
    bad("C1 initialize handshake", str(init))
    sys.exit(1)

# --- Test C2: tools/list returns 3 tools (F22/ITER-058 added glyph_grid_batch) ---
send(proc, {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
tools_resp = recv(proc, timeout=5)
tools = tools_resp.get("result", {}).get("tools", []) if tools_resp else []
tool_names = sorted(t["name"] for t in tools)
if tool_names == ["glyph_grid_batch", "glyph_grid_catalog", "glyph_grid_render"]:
    ok("C2 tools/list returns exactly 3 tools (%s)" % tool_names)
else:
    bad("C2 tools/list returns exactly 3 tools", str(tool_names))

# --- Test C3: schema validation ---
render_tool = next((t for t in tools if t["name"] == "glyph_grid_render"), None)
catalog_tool = next((t for t in tools if t["name"] == "glyph_grid_catalog"), None)
if render_tool and "inputSchema" in render_tool:
    schema = render_tool["inputSchema"]
    required = set(schema.get("required", []))
    if required == {"in_path", "out_path"}:
        ok("C3a render tool schema requires in_path + out_path")
    else:
        bad("C3a render tool schema required fields", str(required))
    # check at least 10 properties
    props = schema.get("properties", {})
    if len(props) >= 13:
        ok("C3b render tool schema has %d properties" % len(props))
    else:
        bad("C3b render tool schema property count", "only %d" % len(props))
else:
    bad("C3 render tool schema present", "missing")

batch_tool = next((t for t in tools if t["name"] == "glyph_grid_batch"), None)
if batch_tool and set(batch_tool.get("inputSchema", {}).get("required", [])) == {"in", "jobs"}:
    ok("C3d batch tool schema requires in + jobs (manifest shape)")
else:
    bad("C3d batch tool schema", str(batch_tool and batch_tool.get("inputSchema")))

if catalog_tool and "inputSchema" in catalog_tool:
    if not catalog_tool["inputSchema"].get("required"):
        ok("C3c catalog tool schema has no required fields")
    else:
        bad("C3c catalog tool schema required fields", "should be empty")

# --- Test C4: catalog tool returns same data as CLI ---
send(proc, {"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "glyph_grid_catalog", "arguments": {}}})
cat_resp = recv(proc, timeout=5)
cat_text = cat_resp.get("result", {}).get("content", [{}])[0].get("text", "") if cat_resp else ""
try:
    mcp_catalog = json.loads(cat_text)
    cli_catalog = json.loads(subprocess.check_output([BIN, "catalog"]).decode())
    if mcp_catalog == cli_catalog:
        ok("C4 MCP catalog == CLI catalog (%d palettes)" % len(mcp_catalog["palettes"]))
    else:
        bad("C4 MCP catalog == CLI catalog", "values differ")
    # AUDIT-2026-06-10: pin the palette count so catalog_json() drift vs the
    # front-end PALETTES registry (12 entries incl. kawaii-pink) fails loudly
    # instead of silently advertising an incomplete list.
    if len(mcp_catalog.get("palettes", [])) == 12:
        ok("C4b catalog palette count == 12")
    else:
        bad("C4b catalog palette count == 12", "got %d" % len(mcp_catalog.get("palettes", [])))
except Exception as e:
    bad("C4 MCP catalog == CLI catalog", str(e))

# --- Test C5: render tool produces valid GIF ---
out_path = os.path.join(SCRATCH, "c5_mcp_render.gif")
if os.path.exists(out_path):
    os.remove(out_path)
send(proc, {"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
    "name": "glyph_grid_render",
    "arguments": {
        "in_path": TEST_IMG,
        "out_path": out_path,
        "frames": 6,
        "palette": "spice",
        "color_mode": "gradient",
        "ramp": "blockAscend",
        "dither": "stbn",
        "selection_mode": "shape-edge-aware",
        "glyph_set": "octant",
        "sampling_strategy": "average",
        "postprocess": ["vignette", "crtBeam"],
        "cols": 200,
        "rows": 100,
    },
}})
print("    (waiting up to 90s for MCP render…)")
ren_resp = recv(proc, timeout=90)
if ren_resp and os.path.exists(out_path):
    size = os.path.getsize(out_path)
    text = ren_resp.get("result", {}).get("content", [{}])[0].get("text", "")
    if "Rendered" in text and 200_000 < size < 15_000_000:
        ok("C5 render tool produces valid GIF (%d KB)" % (size // 1024))
    else:
        bad("C5 render tool", f"size {size}, response: {text[:120]}")
else:
    bad("C5 render tool", "no output file or no response")

# --- Test C6: render with bad path returns error ---
send(proc, {"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {
    "name": "glyph_grid_render",
    "arguments": {
        "in_path": "/tmp/__nonexistent_test_image__.png",
        "out_path": os.path.join(SCRATCH, "c6_should_not_exist.gif"),
        "frames": 6,
    },
}})
err_resp = recv(proc, timeout=60)
err_text = err_resp.get("result", {}).get("content", [{}])[0].get("text", "") if err_resp else ""
if "fail" in err_text.lower() or "error" in err_text.lower() or not os.path.exists(os.path.join(SCRATCH, "c6_should_not_exist.gif")):
    ok("C6 render with bad path returns error or refuses to write (text: %s)" % err_text[:80])
else:
    bad("C6 render with bad path", err_text[:120])

# --- Test C7: server handles 2 sequential renders without restart ---
out2 = os.path.join(SCRATCH, "c7_second.gif")
if os.path.exists(out2):
    os.remove(out2)
send(proc, {"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {
    "name": "glyph_grid_render",
    "arguments": {"in_path": TEST_IMG, "out_path": out2, "frames": 4},
}})
print("    (waiting up to 90s for sequential render…)")
seq_resp = recv(proc, timeout=90)
if seq_resp and os.path.exists(out2):
    ok("C7 server handles 2 sequential renders (second: %d KB)" % (os.path.getsize(out2) // 1024))
else:
    bad("C7 server handles 2 sequential renders", "second render failed")

# --- Test C8 (ITER-057): `config` object reaches every CONFIG key ---
out8 = os.path.join(SCRATCH, "c8_config.gif")
if os.path.exists(out8):
    os.remove(out8)
send(proc, {"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {
    "name": "glyph_grid_render",
    "arguments": {"in_path": TEST_IMG, "out_path": out8, "frames": 2,
                  "config": {"seed": 7, "palette": "phosphor",
                             "postprocess": {"vignette": {"enabled": True, "strength": 0.9}}},
                  "palette": "spice"},
}})
print("    (waiting up to 90s for config-object render…)")
r8 = recv(proc, timeout=90)
t8 = r8.get("result", {}).get("content", [{}])[0].get("text", "") if r8 else ""
if "Rendered" in t8 and os.path.exists(out8):
    ok("C8 render with a `config` object (%d KB)" % (os.path.getsize(out8) // 1024))
else:
    bad("C8 render with a `config` object", t8[:160])

# --- Test C9 (ITER-056/057): invalid args fail in-process with a JSON path ---
t0 = time.time()
send(proc, {"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {
    "name": "glyph_grid_render",
    "arguments": {"in_path": TEST_IMG, "out_path": os.path.join(SCRATCH, "c9.gif"),
                  "config": {"dither": {"mode": "STBN"}}},
}})
r9 = recv(proc, timeout=10)
t9 = r9.get("result", {}).get("content", [{}])[0].get("text", "") if r9 else ""
if "invalid arguments" in t9 and "/dither/mode" in t9 and time.time() - t0 < 5 \
        and not os.path.exists(os.path.join(SCRATCH, "c9.gif")):
    ok("C9 invalid config rejected before spawning (%.1fs)" % (time.time() - t0))
else:
    bad("C9 invalid config rejected before spawning", t9[:160])

# --- Test C10 (F22/ITER-058): 3-job batch in one session, progress per job ---
outs10 = [os.path.join(SCRATCH, "c10_%s.gif" % n) for n in ("a", "b", "c")]
for o in outs10:
    if os.path.exists(o):
        os.remove(o)
send(proc, {"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {
    "name": "glyph_grid_batch",
    "_meta": {"progressToken": "c10"},
    "arguments": {"in": TEST_IMG, "frames": 2, "jobs": [
        {"name": "a", "out": outs10[0], "config": {"palette": "spice"}},
        {"name": "b", "out": outs10[1], "config": {"palette": "phosphor"}},
        {"name": "c", "out": outs10[2], "frames": 3, "border": {"width": 8}},
    ]},
}})
print("    (waiting up to 300s for the 3-job MCP batch…)")
progress10, r10, deadline = [], None, time.time() + 300
while time.time() < deadline:
    msg = recv(proc, timeout=max(1, deadline - time.time()))
    if msg is None:
        break
    if msg.get("method") == "notifications/progress":
        progress10.append(msg["params"])
    elif msg.get("id") == 9:
        r10 = msg
        break
t10 = r10.get("result", {}).get("content", [{}])[0].get("text", "") if r10 else ""
steps10 = [(q.get("progress"), q.get("total")) for q in progress10 if q.get("progressToken") == "c10"]
if "Batch complete: 3 jobs" in t10 and all(os.path.exists(o) for o in outs10):
    ok("C10 batch tool renders 3 jobs in one call (%s KB)"
       % "/".join(str(os.path.getsize(o) // 1024) for o in outs10))
else:
    bad("C10 batch tool renders 3 jobs", t10[:200])
if steps10 == [(1, 3), (2, 3), (3, 3)]:
    ok("C10b batch progress notifications 1/3, 2/3, 3/3 on the client token")
else:
    bad("C10b batch progress notifications", str(progress10)[:200])

# --- Test C11 (F22/ITER-058): `preset` path (+ `config` merged on top) ---
preset11 = os.path.join(SCRATCH, "c11_preset.json")
with open(preset11, "w") as f:
    json.dump({"palette": "phosphor", "dither": {"mode": "stbn"}}, f)
out11 = os.path.join(SCRATCH, "c11_preset.gif")
if os.path.exists(out11):
    os.remove(out11)
send(proc, {"jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": {
    "name": "glyph_grid_render",
    "arguments": {"in_path": TEST_IMG, "out_path": out11, "frames": 2,
                  "preset": preset11, "config": {"seed": 3}},
}})
print("    (waiting up to 90s for preset render…)")
r11 = recv(proc, timeout=90)
t11 = r11.get("result", {}).get("content", [{}])[0].get("text", "") if r11 else ""
if "Rendered" in t11 and os.path.exists(out11):
    ok("C11 render with a `preset` file + `config` (%d KB)" % (os.path.getsize(out11) // 1024))
else:
    bad("C11 render with a `preset` file", t11[:160])

# --- Test C12: a bad `preset` file is validated in-process (it is actually read) ---
preset12 = os.path.join(SCRATCH, "c12_bad_preset.json")
with open(preset12, "w") as f:
    json.dump({"dither": {"mode": "STBN"}}, f)
t0 = time.time()
send(proc, {"jsonrpc": "2.0", "id": 11, "method": "tools/call", "params": {
    "name": "glyph_grid_render",
    "arguments": {"in_path": TEST_IMG, "out_path": os.path.join(SCRATCH, "c12.gif"),
                  "preset": preset12},
}})
r12 = recv(proc, timeout=10)
t12 = r12.get("result", {}).get("content", [{}])[0].get("text", "") if r12 else ""
if "invalid arguments" in t12 and "/dither/mode" in t12 and time.time() - t0 < 5:
    ok("C12 invalid preset file rejected before spawning (%.1fs)" % (time.time() - t0))
else:
    bad("C12 invalid preset file rejected", t12[:160])

shutdown(proc)
print(f"  MCP: {PASS} passed, {FAIL} failed")
sys.exit(0 if FAIL == 0 else 1)
