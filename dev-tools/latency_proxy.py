#!/usr/bin/env python3
"""A tiny TCP proxy that adds artificial latency, for benchmarking database
round-trip behaviour against something closer to a REMOTE Postgres (Neon)
than the local Docker `test-db` on loopback.

Why: a local Postgres answers in ~0.2 ms, which hides exactly the cost the
efficiency refactor is trying to remove -- extra round trips. Put this in
front of `test-db` with 10 ms each way (a plausible 20 ms round trip to a
cloud database) and a saved round trip shows up as a real, measurable 20 ms.

Usage (listens on 127.0.0.1:5434, forwards to the test-db on 5433):

    python3 dev-tools/latency_proxy.py --delay-ms 10

then point a benchmark's connection string at port 5434 instead of 5433.
Each chunk of bytes is delayed by --delay-ms in each direction, so one
request/response exchange costs about 2 x --delay-ms.

Dev tool only: never point anything real at it, and it has no TLS (the
local test-db has none either).
"""

import argparse
import asyncio


async def pump(reader, writer, delay):
    try:
        while True:
            data = await reader.read(65536)
            if not data:
                break
            await asyncio.sleep(delay)
            writer.write(data)
            await writer.drain()
    except (ConnectionError, asyncio.CancelledError):
        pass
    finally:
        try:
            writer.close()
        except Exception:
            pass


async def handle(client_reader, client_writer, upstream_host, upstream_port, delay):
    try:
        upstream_reader, upstream_writer = await asyncio.open_connection(
            upstream_host, upstream_port
        )
    except OSError:
        client_writer.close()
        return
    await asyncio.gather(
        pump(client_reader, upstream_writer, delay),
        pump(upstream_reader, client_writer, delay),
    )


async def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--listen-port", type=int, default=5434)
    parser.add_argument("--upstream-host", default="127.0.0.1")
    parser.add_argument("--upstream-port", type=int, default=5433)
    parser.add_argument(
        "--delay-ms", type=float, default=10.0, help="added latency each way"
    )
    args = parser.parse_args()

    delay = args.delay_ms / 1000.0
    server = await asyncio.start_server(
        lambda r, w: handle(r, w, args.upstream_host, args.upstream_port, delay),
        "127.0.0.1",
        args.listen_port,
    )
    print(
        f"latency proxy on 127.0.0.1:{args.listen_port} -> "
        f"{args.upstream_host}:{args.upstream_port}, +{args.delay_ms} ms each way",
        flush=True,
    )
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
