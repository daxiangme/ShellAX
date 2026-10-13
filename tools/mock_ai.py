# Local mock of an OpenAI-compatible /v1/chat/completions endpoint (no real model).
import json, time
from http.server import BaseHTTPRequestHandler, HTTPServer
LOG = open('/tmp/mock_ai.log', 'a')
def pick(text):
    t = text.lower()
    for keys, cmd, ex in [
        (['big file', '大文件', 'largest'], 'find . -type f -size +100M -exec ls -lh {} +', 'List files larger than 100 MB under the current directory'),
        (['disk', '磁盘'], 'df -h', 'Show disk usage of mounted filesystems'),
        (['delete', '删除', 'clean'], 'rm -rf ./build', 'Remove the build directory'),
        (['port', '端口'], 'ss -tlnp', 'List listening TCP ports'),
    ]:
        if any(k in t for k in keys): return cmd, ex
    return 'ls -la', 'List files'
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get('content-length', 0))).decode()
        auth = self.headers.get('authorization', '')
        LOG.write(json.dumps({'ts': time.strftime('%H:%M:%S'), 'path': self.path,
                              'auth_present': auth.startswith('Bearer '), 'auth_last4': auth[-4:],
                              'body': json.loads(body)}, ensure_ascii=False) + '\n'); LOG.flush()
        if not auth.startswith('Bearer sk-'):
            self.send_response(401); self.end_headers(); self.wfile.write(b'{"error":{"message":"missing or invalid API key"}}'); return
        req = json.loads(body)
        user = req['messages'][-1]['content']
        request_line = next((l for l in user.splitlines() if l.startswith('Request:')), user)
        cmd, ex = pick(request_line)
        time.sleep(0.15)
        out = {'id': 'mock', 'model': 'mock-gpt', 'choices': [{'index': 0, 'message': {'role': 'assistant',
               'content': json.dumps({'command': cmd, 'explanation': ex})}, 'finish_reason': 'stop'}]}
        b = json.dumps(out).encode()
        self.send_response(200); self.send_header('content-type', 'application/json'); self.send_header('content-length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def log_message(self, *a): pass
HTTPServer(('127.0.0.1', 18080), H).serve_forever()
