import asyncio, json, urllib.request, sys
import websockets

BASE = "http://127.0.0.1:8787"
WS = "ws://127.0.0.1:8787"

def create_room():
    req = urllib.request.Request(BASE + "/api/rooms", method="POST")
    with urllib.request.urlopen(req) as r:
        return json.loads(r.read())

async def recv_json(ws, timeout=3):
    return json.loads(await asyncio.wait_for(ws.recv(), timeout))

async def main():
    room = create_room()
    rid, token, guest_token = room["room_id"], room["host_token"], room["guest_token"]
    ok = lambda name, cond: print(("PASS " if cond else "FAIL ") + name) or (cond or sys.exit(1))

    host = await websockets.connect(f"{WS}/ws/{rid}")
    await host.send(json.dumps({"t": "hello", "host_token": token}))
    w = await recv_json(host)
    ok("host welcome role=host", w["t"] == "welcome" and w["role"] == "host" and w["count"] == 1)

    # wrong token is rejected
    bad = await websockets.connect(f"{WS}/ws/{rid}")
    await bad.send(json.dumps({"t": "hello", "host_token": "nope"}))
    try:
        await bad.recv(); closed = False
    except websockets.ConnectionClosed as e:
        closed = e.code == 4403
    ok("bad host token closed 4403", closed)

    # second valid host is rejected
    dup = await websockets.connect(f"{WS}/ws/{rid}")
    await dup.send(json.dumps({"t": "hello", "host_token": token}))
    try:
        await dup.recv(); closed = False
    except websockets.ConnectionClosed as e:
        closed = e.code == 4409
    ok("duplicate host closed 4409", closed)

    g1 = await websockets.connect(f"{WS}/ws/{rid}")
    await g1.send(json.dumps({"t": "hello", "guest_token": guest_token}))
    w1 = await recv_json(g1)
    ok("guest welcome", w1["t"] == "welcome" and w1["role"] == "guest" and w1["count"] == 2)
    j = await recv_json(host)
    ok("host sees joined", j["t"] == "joined" and j["count"] == 2)

    g2 = await websockets.connect(f"{WS}/ws/{rid}")
    await g2.send(json.dumps({"t": "hello", "guest_token": guest_token}))
    w2 = await recv_json(g2)
    await recv_json(host); await recv_json(g1)  # joined events

    # relay: opaque payload reaches everyone except sender
    await g1.send(json.dumps({"t": "relay", "p": "b64cipher=="}))
    r_host = await recv_json(host); r_g2 = await recv_json(g2)
    ok("relay fanout", r_host["t"] == "relay" and r_host["p"] == "b64cipher==" and r_g2["from"] == r_host["from"])

    # guest cannot lock under default policy
    await g1.send(json.dumps({"t": "gen_start"}))
    e = await recv_json(g1)
    ok("guest lock denied by default", e["t"] == "error" and e["code"] == "not_allowed")

    # guest cannot change policy
    await g1.send(json.dumps({"t": "policy", "everyone": True}))
    e = await recv_json(g1)
    ok("guest policy change denied", e["t"] == "error")

    # host flips policy, everyone hears it
    await host.send(json.dumps({"t": "policy", "everyone": True}))
    p1 = await recv_json(host); p2 = await recv_json(g1); p3 = await recv_json(g2)
    ok("policy broadcast", all(x["t"] == "policy" and x["everyone"] for x in (p1, p2, p3)))

    # now the guest can lock; everyone gets locked event
    await g1.send(json.dumps({"t": "gen_start"}))
    l1 = await recv_json(host); l2 = await recv_json(g1); l3 = await recv_json(g2)
    ok("guest lock granted", all(x["t"] == "locked" for x in (l1, l2, l3)))

    # while locked: guests muted, host may stream
    await g2.send(json.dumps({"t": "relay", "p": "xx"}))
    e = await recv_json(g2)
    ok("guest muted while locked", e["t"] == "error" and e["code"] == "locked")
    await host.send(json.dumps({"t": "relay", "p": "tokchunk"}))
    t1 = await recv_json(g1); t2 = await recv_json(g2)
    ok("host streams while locked", t1["p"] == "tokchunk" and t2["p"] == "tokchunk")

    # host ends generation -> unlocked everywhere
    await host.send(json.dumps({"t": "gen_end"}))
    u = [await recv_json(x) for x in (host, g1, g2)]
    ok("unlock broadcast", all(x["t"] == "unlocked" for x in u))

    # host disconnect kills the room with a reason
    await host.close()
    msgs, code = [], None
    try:
        while True:
            msgs.append(await recv_json(g1, timeout=3))
    except websockets.ConnectionClosed as e:
        code = e.code
    except asyncio.TimeoutError:
        pass
    reasons = [m for m in msgs if m["t"] == "room_closed"]
    ok("room_closed reason=host_left", bool(reasons) and reasons[0]["reason"] == "host_left")
    ok("guest closed with 4001", code == 4001)

    # room is gone
    g3 = await websockets.connect(f"{WS}/ws/{rid}")
    await g3.send(json.dumps({"t": "hello", "guest_token": guest_token}))
    try:
        await g3.recv(); gone = False
    except websockets.ConnectionClosed as e:
        gone = e.code == 4404
    ok("room removed 4404", gone)

    print("ALL PASS")

asyncio.run(main())
