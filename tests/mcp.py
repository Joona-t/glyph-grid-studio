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
    # ITER-076: in_path is optional (a text source needs none).
    if required == {"out_path"} and "in_path" in schema.get("properties", {}) \
            and {"text", "srt_path", "text_options"} <= set(schema.get("properties", {})):
        ok("C3a render tool schema requires out_path; in_path optional, text/srt_path/text_options present")
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

# --- Test C13 (F25/ITER-059): failures are isError results with {code, path, message} ---
def tool_result(resp):
    res = resp.get("result", {}) if resp else {}
    text = (res.get("content") or [{}])[0].get("text", "")
    try:
        body = json.loads(text)
    except Exception:
        body = None
    return res, body

for rid, args, want_code, want_path in [
    (12, {"in_path": TEST_IMG, "out_path": os.path.join(SCRATCH, "c13a.gif"),
          "config": {"dither": {"mode": "STBN"}}}, "invalid_arguments", "/config/dither/mode"),
    (13, {"in_path": "/tmp/__nonexistent_test_image__.png",
          "out_path": os.path.join(SCRATCH, "c13b.gif")}, "invalid_arguments", "/in_path"),
    # ITER-062: blame what the caller sent — the preset file, or the named param.
    (30, {"in_path": TEST_IMG, "out_path": os.path.join(SCRATCH, "c13d.gif"),
          "preset": preset12}, "invalid_arguments", "/preset/dither/mode"),
    (31, {"in_path": TEST_IMG, "out_path": os.path.join(SCRATCH, "c13e.gif"),
          "dither": "STBN"}, "invalid_arguments", "/dither"),
    (32, {"in_path": TEST_IMG, "out_path": os.path.join(SCRATCH, "c13f.gif"),
          "selection_mode": "shapes", "config": {"seed": 3}}, "invalid_arguments", "/selection_mode"),
]:
    send(proc, {"jsonrpc": "2.0", "id": rid, "method": "tools/call",
                "params": {"name": "glyph_grid_render", "arguments": args}})
    res, body = tool_result(recv(proc, timeout=10))
    if res.get("isError") is True and isinstance(body, dict) \
            and body.get("code") == want_code and body.get("path") == want_path \
            and body.get("message") and res.get("structuredContent") == body:
        ok("C13 isError + {code: %s, path: %s}" % (want_code, want_path))
    else:
        bad("C13 structured error %s" % want_path, str(res)[:200])

# C13c: a child that times out → code "timeout" (exit 3), bounded wall time.
t0 = time.time()
send(proc, {"jsonrpc": "2.0", "id": 14, "method": "tools/call", "params": {
    "name": "glyph_grid_render",
    "arguments": {"in_path": TEST_IMG, "out_path": os.path.join(SCRATCH, "c13c.gif"),
                  "frames": 120, "timeout_s": 2},
}})
res, body = tool_result(recv(proc, timeout=60))
if res.get("isError") is True and isinstance(body, dict) and body.get("code") == "timeout" \
        and body.get("exit_code") == 3 and time.time() - t0 < 20 \
        and not os.path.exists(os.path.join(SCRATCH, "c13c.gif")):
    ok("C13c timeout_s → isError code=timeout exit_code=3 (%.1fs)" % (time.time() - t0))
else:
    bad("C13c timeout_s", str(res)[:200])

# --- Test C14 (F26/ITER-060): glyph_grid_render sends per-frame progress ---
out14 = os.path.join(SCRATCH, "c14_progress.gif")
send(proc, {"jsonrpc": "2.0", "id": 15, "method": "tools/call", "params": {
    "name": "glyph_grid_render", "_meta": {"progressToken": "c14"},
    "arguments": {"in_path": TEST_IMG, "out_path": out14, "frames": 4},
}})
progress14, r14, deadline = [], None, time.time() + 120
while time.time() < deadline:
    msg = recv(proc, timeout=max(1, deadline - time.time()))
    if msg is None:
        break
    if msg.get("method") == "notifications/progress":
        progress14.append(msg["params"])
    elif msg.get("id") == 15:
        r14 = msg
        break
steps14 = [(q.get("progress"), q.get("total")) for q in progress14 if q.get("progressToken") == "c14"]
if steps14 == [(1, 4), (2, 4), (3, 4), (4, 4)] and r14 and not r14.get("result", {}).get("isError"):
    ok("C14 render progress notifications 1/4..4/4 per captured frame")
else:
    bad("C14 render frame progress", str(steps14)[:200])

# --- Test C15 (F26/ITER-060): notifications/cancelled stops the render child ---
out15 = os.path.join(SCRATCH, "c15_cancel.gif")
send(proc, {"jsonrpc": "2.0", "id": 16, "method": "tools/call", "params": {
    "name": "glyph_grid_render", "_meta": {"progressToken": "c15"},
    "arguments": {"in_path": TEST_IMG, "out_path": out15, "frames": 120},
}})
deadline = time.time() + 60
while time.time() < deadline:            # wait until the child is capturing
    msg = recv(proc, timeout=max(1, deadline - time.time()))
    if msg is None or (msg.get("method") == "notifications/progress"
                       and msg["params"].get("progressToken") == "c15"):
        break
send(proc, {"jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": {"requestId": 16, "reason": "test"}})
t0 = time.time()
alive = True
while time.time() - t0 < 5:
    alive = subprocess.run(["pgrep", "-f", out15], capture_output=True).returncode == 0
    if not alive:
        break
    time.sleep(0.2)
parts15 = [f for f in os.listdir(SCRATCH) if f.startswith(".c15_cancel.gif.") and f.endswith(".part")]
if not alive and not os.path.exists(out15) and not parts15:
    ok("C15 cancel stops the render child in %.1fs (no output, no .part)" % (time.time() - t0))
else:
    bad("C15 cancel stops the render child", "alive=%s out=%s parts=%s" % (alive, os.path.exists(out15), parts15))
# Drain anything the cancelled call still emits, then confirm the server answers.
send(proc, {"jsonrpc": "2.0", "id": 17, "method": "ping"})
pong, deadline = None, time.time() + 10
while time.time() < deadline:
    msg = recv(proc, timeout=1)
    if msg and msg.get("id") == 17:
        pong = msg
        break
if pong is not None and "result" in pong:
    ok("C15b server still answers after a cancel")
else:
    bad("C15b server answers after cancel", str(pong))

# --- Test C16 (F27/ITER-061): a typo'd manifest key → isError with the job path, fast ---
t0 = time.time()
send(proc, {"jsonrpc": "2.0", "id": 18, "method": "tools/call", "params": {
    "name": "glyph_grid_batch",
    "arguments": {"in": TEST_IMG, "jobs": [
        {"name": "a", "out": os.path.join(SCRATCH, "c16.gif"), "capwidth": 720}]},
}})
res, body = tool_result(recv(proc, timeout=20))
if res.get("isError") is True and isinstance(body, dict) and body.get("code") == "invalid_arguments" \
        and body.get("path") == "/jobs/0" and "capwidth" in body.get("message", "") + body.get("stderr_tail", "") \
        and time.time() - t0 < 5 and not os.path.exists(os.path.join(SCRATCH, "c16.gif")):
    ok("C16 batch manifest typo → invalid_arguments at /jobs/0 (%.1fs)" % (time.time() - t0))
else:
    bad("C16 batch manifest typo", str(res)[:240])

# --- Test C17 (ITER-074): format "txt" writes text AND returns it inline ---
out17 = os.path.join(SCRATCH, "c17_mcp.txt")
if os.path.exists(out17):
    os.remove(out17)
send(proc, {"jsonrpc": "2.0", "id": 19, "method": "tools/call", "params": {
    "name": "glyph_grid_render",
    "arguments": {"in_path": TEST_IMG, "out_path": out17, "format": "txt", "cols": 80, "rows": 40},
}})
print("    (waiting up to 90s for MCP txt render…)")
r17 = recv(proc, timeout=90)
c17 = (r17 or {}).get("result", {}).get("content", []) or []
body17 = c17[1].get("text", "") if len(c17) > 1 else ""
rows17 = body17.split("\n")[:-1]
if "Rendered" in (c17[0].get("text", "") if c17 else "") and os.path.exists(out17) \
        and len(rows17) == 40 and all(len(r) == 80 for r in rows17) \
        and body17 == open(out17, encoding="utf-8").read():
    ok("C17 format txt: file written + 40x80 text returned inline (%d B)" % len(body17.encode()))
else:
    bad("C17 format txt inline", str(r17)[:240])

# C17b: an unknown format is refused in-process with its argument path.
send(proc, {"jsonrpc": "2.0", "id": 20, "method": "tools/call", "params": {
    "name": "glyph_grid_render",
    "arguments": {"in_path": TEST_IMG, "out_path": os.path.join(SCRATCH, "c17b.txt"), "format": "webm"},
}})
res, body = tool_result(recv(proc, timeout=10))
if res.get("isError") is True and isinstance(body, dict) and body.get("code") == "invalid_arguments" \
        and body.get("path") == "/format" and not os.path.exists(os.path.join(SCRATCH, "c17b.txt")):
    ok("C17b unknown format → invalid_arguments at /format")
else:
    bad("C17b unknown format", str(res)[:200])

# --- Test C18 (ITER-076): text source, no in_path → txt inline shows the word ---
out18 = os.path.join(SCRATCH, "c18_mcp.txt")
if os.path.exists(out18):
    os.remove(out18)
send(proc, {"jsonrpc": "2.0", "id": 21, "method": "tools/call", "params": {
    "name": "glyph_grid_render",
    "arguments": {"out_path": out18, "format": "txt", "cols": 80, "rows": 40, "text": "HI",
                  "text_options": {"size": 300}},
}})
print("    (waiting up to 90s for MCP text-source render…)")
r18 = recv(proc, timeout=90)
c18 = (r18 or {}).get("result", {}).get("content", []) or []
body18 = c18[1].get("text", "") if len(c18) > 1 else ""
inked18 = sum(1 for ch in body18 if not ch.isspace())
if "Rendered" in (c18[0].get("text", "") if c18 else "") and os.path.exists(out18) \
        and len(body18.split("\n")[:-1]) == 40 and 100 < inked18 < 80 * 40 * 0.6:
    ok("C18 text source without in_path: 40x80 txt, %d inked cells" % inked18)
else:
    bad("C18 text source", str(r18)[:240])

# C18b: no in_path and no text → invalid_arguments at /in_path, in-process.
send(proc, {"jsonrpc": "2.0", "id": 22, "method": "tools/call", "params": {
    "name": "glyph_grid_render",
    "arguments": {"out_path": os.path.join(SCRATCH, "c18b.gif"), "text_options": {"anim": "bounce"}},
}})
res, body = tool_result(recv(proc, timeout=10))
send(proc, {"jsonrpc": "2.0", "id": 23, "method": "tools/call", "params": {
    "name": "glyph_grid_render",
    "arguments": {"out_path": os.path.join(SCRATCH, "c18c.gif")},
}})
res2, body2 = tool_result(recv(proc, timeout=10))
if isinstance(body, dict) and body.get("path") == "/text_options/anim" \
        and isinstance(body2, dict) and body2.get("code") == "invalid_arguments" and body2.get("path") == "/in_path":
    ok("C18b bad text_options.anim → /text_options/anim; no source → /in_path")
else:
    bad("C18b text argument errors", str(body)[:160] + " | " + str(body2)[:160])

shutdown(proc)
print(f"  MCP: {PASS} passed, {FAIL} failed")
sys.exit(0 if FAIL == 0 else 1)
