#!/usr/bin/env python3
"""Nova Nexo async smoke/integration test (package implementation).

Build the Nova package first:
  cargo run -- build examples/packages/nexo_async_counter
Then run:
  python3 examples/packages/nexo_async_counter/tests/test_server.py \
    examples/packages/nexo_async_counter/target/nova-app
"""
import http.client
import json
import pathlib
import socket
import subprocess
import sys
import time

PORT = 7888


def request(method, path, body=None, headers=None):
    con = http.client.HTTPConnection("127.0.0.1", PORT, timeout=4)
    try:
        con.request(method, path, body=body, headers=headers or {})
        res = con.getresponse()
        return res.status, res.getheader("Content-Type"), res.read().decode("utf-8")
    finally:
        con.close()


def ready(proc):
    for _ in range(300):
        if proc.poll() is not None:
            raise AssertionError(f"server exited early with {proc.returncode}")
        try:
            with socket.create_connection(("127.0.0.1", PORT), timeout=0.1):
                return
        except (OSError, TimeoutError):
            time.sleep(0.01)
    raise AssertionError("server did not start")


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: test_server.py <compiled-Nova-server-binary>")
    server = pathlib.Path(sys.argv[1]).resolve()
    p = subprocess.Popen([str(server)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        ready(p)
        code, typ, body = request("GET", "/unknown")
        assert code == 404 and body == "Not Found", (code, typ, body)
        code, typ, body = request("GET", "/user")
        assert code == 200 and typ.startswith("application/json") and json.loads(body) == {"id": 2}, (code, typ, body)
        code, _, body = request("GET", "/health")
        assert code == 200 and body == "ok", (code, body)
        code, _, body = request("GET", "/user?anything=1")
        assert code == 200 and json.loads(body) == {"id": 4}, (code, body)
        code, _, body = request("POST", "/echo", '{"name":"nova"}', {"Content-Type": "application/json"})
        assert code == 200 and body == '{"name":"nova"}', (code, body)
        stdout, stderr = p.communicate(timeout=8)
        assert p.returncode == 0, (p.returncode, stdout, stderr)
        print("OK: GET/POST, routes, 404, JSON, query, async future, counter 1→5 (includes the 404)")
        print(stdout.strip(), stderr.strip())
    finally:
        if p.poll() is None:
            p.terminate()
            try:
                p.wait(timeout=2)
            except subprocess.TimeoutExpired:
                p.kill()
                p.wait(timeout=2)


if __name__ == "__main__":
    main()
