# Minimal HTTP CONNECT proxy for local tests (logs each tunnel to /tmp/mock_proxy.log).
import socket, threading, sys
PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 18111
def pipe(a, b):
    try:
        while True:
            d = a.recv(65536)
            if not d: break
            b.sendall(d)
    except OSError: pass
    finally:
        for s in (a, b):
            try: s.shutdown(socket.SHUT_RDWR)
            except OSError: pass
def handle(c):
    head = b''
    while b'\r\n\r\n' not in head:
        d = c.recv(1)
        if not d: return
        head += d
    line = head.split(b'\r\n')[0].decode()
    open('/tmp/mock_proxy.log', 'a').write(line + '\n')
    method, target = line.split()[0], line.split()[1]
    if method != 'CONNECT':  # absolute-form plain HTTP (AI / update over http://)
        hp, _, path = target[len('http://'):].partition('/')
        host, port = (hp.rsplit(':', 1) + ['80'])[:2]
        up = socket.create_connection((host, int(port)))
        rest = head.split(b'\r\n', 1)[1]
        up.sendall(f'{method} /{path} HTTP/1.1\r\n'.encode() + rest)
    else:
        host, port = target.rsplit(':', 1)
        up = socket.create_connection((host, int(port)))
        c.sendall(b'HTTP/1.1 200 Connection established\r\n\r\n')
    threading.Thread(target=pipe, args=(c, up), daemon=True).start()
    pipe(up, c)
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(('127.0.0.1', PORT)); s.listen(16)
while True:
    c, _ = s.accept(); threading.Thread(target=handle, args=(c,), daemon=True).start()
