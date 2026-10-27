#!/usr/bin/env python3
"""Two queued TCP connections are handled by Nova sequentially."""
import json
import pathlib
import socket
import subprocess
import sys
import time

HOST, PORT = '127.0.0.1', 7888


def wait_listening(proc):
    for _ in range(300):
        if proc.poll() is not None:
            raise AssertionError(f'server exited with {proc.returncode}')
        try:
            conn=socket.create_connection((HOST,PORT),timeout=0.05)
            conn.close()
            return
        except OSError:
            time.sleep(0.01)
    raise AssertionError('server was not ready')


def connect(path):
    c=socket.create_connection((HOST,PORT),timeout=4)
    c.sendall(f'GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n'.encode())
    return c


def receive(c):
    blocks=[]
    while True:
        part=c.recv(65536)
        if not part:break
        blocks.append(part)
    c.close()
    data=b''.join(blocks)
    headers,sep,body=data.partition(b'\r\n\r\n')
    assert sep,repr(data)
    return int(headers.split(b' ')[1]),body


def main():
    if len(sys.argv)!=2:raise SystemExit('usage: test_concurrent.py <compiled-server>')
    binary=pathlib.Path(sys.argv[1]).resolve()
    p=subprocess.Popen([str(binary)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    try:
        wait_listening(p)
        # Both sockets are sent before either response is read.
        first,second=connect('/user'),connect('/user')
        r1,r2=receive(first),receive(second)
        assert r1[0]==200 and r2[0]==200,(r1,r2)
        assert {json.loads(r1[1])['id'],json.loads(r2[1])['id']}=={1,2},(r1,r2)
        third=connect('/health')
        assert receive(third)==(200,b'ok')
        fourth=connect('/user')
        assert json.loads(receive(fourth)[1])=={'id':4}
        fifth=connect('/user')
        assert json.loads(receive(fifth)[1])=={'id':5}
        stdout,stderr=p.communicate(timeout=8)
        assert p.returncode==0,(p.returncode,stdout,stderr)
        print('OK: queued sockets, increment 1..5, all responses flushed')
    finally:
        if p.poll() is None:
            p.terminate()
            try:p.wait(timeout=2)
            except subprocess.TimeoutExpired:p.kill();p.wait(timeout=2)

if __name__=='__main__':main()
