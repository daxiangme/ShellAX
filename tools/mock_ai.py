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
OPENAI_MODELS = ['mock-gpt', 'mock-gpt-mini', 'mock-reasoner', 'deepseek-chat', 'gpt-4.1', 'gpt-4.1-mini',
                 'text-embedding-3-small', 'whisper-1', 'tts-1', 'dall-e-3', 'omni-moderation-latest']
ANTHROPIC_MODELS = [('claude-sonnet-4-5', 'Claude Sonnet 4.5'), ('claude-haiku-4-5', 'Claude Haiku 4.5'),
                    ('claude-opus-4-1', 'Claude Opus 4.1'), ('claude-3-5-haiku-latest', 'Claude Haiku 3.5'), ('claude-x-mock', 'Claude X (mock)')]
class H(BaseHTTPRequestHandler):
    def _json(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code); self.send_header('content-type', 'application/json'); self.send_header('content-length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        from urllib.parse import urlparse, parse_qs
        u = urlparse(self.path); q = parse_qs(u.query)
        LOG.write(json.dumps({'ts': time.strftime('%H:%M:%S'), 'GET': self.path, 'bearer': self.headers.get('authorization', '').startswith('Bearer '),
                              'x_api_key': bool(self.headers.get('x-api-key')), 'anthropic_version': self.headers.get('anthropic-version')}) + '\n'); LOG.flush()
        time.sleep(float(__import__('os').environ.get('MOCK_MODELS_DELAY', '0.3')))
        if u.path.startswith('/nomodels/'):
            return self._json(404, {'error': {'message': 'Not Found'}})
        if u.path == '/api/tags':  # Ollama native
            return self._json(200, {'models': [{'name': 'llama3.2:latest'}, {'name': 'qwen2.5-coder:7b'}, {'name': 'nomic-embed-text:latest'}]})
        if u.path.endswith('/v1/models'):
            if self.headers.get('x-api-key'):  # Anthropic style, 2 per page to exercise pagination
                if self.headers.get('anthropic-version') is None: return self._json(400, {'error': 'missing anthropic-version'})
                ids = [m[0] for m in ANTHROPIC_MODELS]
                start = ids.index(q['after_id'][0]) + 1 if 'after_id' in q else 0
                page = ANTHROPIC_MODELS[start:start + 2]
                return self._json(200, {'data': [{'type': 'model', 'id': i, 'display_name': n} for i, n in page],
                                        'has_more': start + 2 < len(ids), 'first_id': page[0][0] if page else None, 'last_id': page[-1][0] if page else None})
            if not self.headers.get('authorization', '').startswith('Bearer sk-'):
                return self._json(401, {'error': {'message': 'missing or invalid API key'}})
            return self._json(200, {'object': 'list', 'data': [{'id': i, 'object': 'model', 'owned_by': 'mock'} for i in OPENAI_MODELS]})
        self._json(404, {'error': 'not found'})

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
        system = next((m['content'] for m in req['messages'] if m['role'] == 'system'), '')
        if req.get('stream') or 'Complete the user' in system:
            if 'Complete the user' in system:
                part = user.split('Partial command:')[-1].strip()
                text = next((c for c in ['git status', 'git stash list', 'docker ps -a', 'systemctl status nginx', 'tail -f /var/log/syslog'] if c.startswith(part) and c != part), part)
            elif 'explain' in system.lower():
                text = ('The command exited with a non-zero status. Most likely cause: the file or directory does not exist '
                        '(see "No such file or directory" in the output).\n\nFix:\n```\nls -la\n```')
            elif user.startswith('Results of the commands'):
                text = 'Got the results (%d chars, %d step(s)). Everything looks fine.' % (len(user), user.count('\n$ '))
            elif any(k in (user.splitlines()[-1] if user else '').lower() for k in ['steps', '步骤', '排查']):
                text = ('Plan: check disk, then restart the service, then verify.\n\n```bash\ndf -h\nsystemctl restart nginx\nuptime\n```')
            else:
                last = user.splitlines()[-1] if user else ''
                text = ('Mock assistant (streamed). You asked: "%s". I received %d context chars, %d redacted secret(s), %d message(s) of history.\n\n```\ndf -h\n```'
                        % (last[:80], len(user), user.count('[REDACTED'), len(req['messages']) - 1))
            if not req.get('stream'):
                out = {'id': 'mock', 'model': 'mock-gpt', 'choices': [{'index': 0, 'message': {'role': 'assistant', 'content': text}, 'finish_reason': 'stop'}]}
                b = json.dumps(out).encode()
                self.send_response(200); self.send_header('content-type', 'application/json'); self.send_header('content-length', str(len(b))); self.end_headers(); self.wfile.write(b); return
            self.send_response(200); self.send_header('content-type', 'text/event-stream'); self.end_headers()
            words = text.split(' ')
            for i, w in enumerate(words):
                chunk = {'model': 'mock-gpt', 'choices': [{'index': 0, 'delta': {'content': w + (' ' if i < len(words) - 1 else '')}}]}
                self.wfile.write(('data: ' + json.dumps(chunk) + '\n\n').encode()); self.wfile.flush(); time.sleep(0.04)
            self.wfile.write(b'data: [DONE]\n\n'); self.wfile.flush(); return
        request_line = next((l for l in user.splitlines() if l.startswith('Request:')), user)
        cmd, ex = pick(request_line)
        time.sleep(0.15)
        out = {'id': 'mock', 'model': 'mock-gpt', 'choices': [{'index': 0, 'message': {'role': 'assistant',
               'content': json.dumps({'command': cmd, 'explanation': ex})}, 'finish_reason': 'stop'}]}
        b = json.dumps(out).encode()
        self.send_response(200); self.send_header('content-type', 'application/json'); self.send_header('content-length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def log_message(self, *a): pass
HTTPServer(('127.0.0.1', 18080), H).serve_forever()
