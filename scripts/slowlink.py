#!/usr/bin/env python3
"""A slow link in user space: UDP from a client on LISTEN to the host on TARGET and back.
Host-to-client traffic is paced to RATE bit/s through a queue of at most QUEUE bytes (what
doesn't fit is dropped, as a router would). Client-to-host passes straight through.
usage: slowlink.py LISTEN_PORT TARGET_PORT RATE_BPS QUEUE_BYTES SECONDS"""
import asyncio, sys, time, collections

listen, target, rate, qmax, secs = int(sys.argv[1]), int(sys.argv[2]), float(sys.argv[3]), int(sys.argv[4]), float(sys.argv[5])
stats = {"sent": 0, "dropped": 0, "qsum": 0, "qn": 0}


async def main():
    loop = asyncio.get_running_loop()
    client = {"addr": None}
    queue = collections.deque()
    qbytes = [0]
    wake = asyncio.Event()

    class Up(asyncio.DatagramProtocol):  # facing the client
        def connection_made(self, t):
            self.t = t

        def datagram_received(self, data, addr):
            client["addr"] = addr
            down.t.sendto(data)

    class Down(asyncio.DatagramProtocol):  # facing the host
        def connection_made(self, t):
            self.t = t

        def datagram_received(self, data, addr):
            if qbytes[0] + len(data) > qmax:
                stats["dropped"] += 1
                return
            queue.append(data)
            qbytes[0] += len(data)
            stats["qsum"] += qbytes[0]
            stats["qn"] += 1
            wake.set()

    _, up = await loop.create_datagram_endpoint(Up, local_addr=("127.0.0.1", listen))
    _, down = await loop.create_datagram_endpoint(Down, remote_addr=("127.0.0.1", target))

    async def pace():
        next_t = time.monotonic()
        while True:
            if not queue:
                wake.clear()
                await wake.wait()
                next_t = max(next_t, time.monotonic())
            data = queue.popleft()
            qbytes[0] -= len(data)
            next_t += (len(data) + 28) * 8 / rate
            delay = next_t - time.monotonic()
            if delay > 0:
                await asyncio.sleep(delay)
            if client["addr"]:
                up.t.sendto(data, client["addr"])
                stats["sent"] += 1

    task = asyncio.create_task(pace())
    await asyncio.sleep(secs)
    task.cancel()
    print(f"slowlink: {stats['sent']} datagrams passed, {stats['dropped']} dropped at the queue, {stats['qsum'] / max(stats['qn'], 1) / 1024:.0f} KiB queued on average ({stats['qsum'] / max(stats['qn'], 1) * 8 / rate * 1000:.0f} ms)", flush=True)


asyncio.run(main())
