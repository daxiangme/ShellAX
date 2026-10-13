#!/usr/bin/env python3
"""Scripted fake LLM endpoint used only to drive real agent CLIs (Claude Code, Codex, Grok)
far enough to call their PreToolUse/PermissionRequest hooks, without any API key.

  - Anthropic Messages API:  POST /v1/messages        (stream and non-stream)
  - OpenAI Responses API:    POST /v1/responses       (stream)
  - OpenAI Chat Completions: POST /v1/chat/completions (stream and non-stream)

The first turn of a request that offers a shell tool answers with one tool call that runs the
command written after "RUN:" in the user prompt. Once a tool result comes back it answers with
text that echoes the result. Each request is logged to /tmp/mock_agent_llm.log.
Usage: python3 tools/mock_agent_llm.py [port=18090]
"""
import json, re, sys, time, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = open('/tmp/mock_agent_llm.log', 'a', buffering=1)

def text_of(x):
    if isinstance(x, str): return x
    if isinstance(x, list): return ' '.join(text_of(i) for i in x)
    if isinstance(x, dict):
        return ' '.join(text_of(v) for k, v in x.items() if k in ('text', 'content', 'input', 'output'))
    return ''

def find_cmd(blob):
    m = re.search(r'RUN:\s*(.+?)(?:"|\\n|\n|$)', blob)
    return m.group(1).strip() if m else None

class H(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def body(self):
        n = int(self.headers.get('content-length') or 0)
        return json.loads(self.rfile.read(n) or b'{}')
    def send_json(self, obj, code=200):
        b = json.dumps(obj).encode()
        self.send_response(code); self.send_header('content-type', 'application/json'); self.send_header('content-length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def sse_start(self):
        self.send_response(200); self.send_header('content-type', 'text/event-stream'); self.send_header('cache-control', 'no-cache'); self.send_header('connection', 'close'); self.end_headers()
        self.close_connection = True
    def ev(self, name, data):
        self.wfile.write(f'event: {name}\ndata: {json.dumps(data)}\n\n'.encode()); self.wfile.flush()
    def do_GET(self):
        if self.path.rstrip('/').endswith('/models'):
            return self.send_json({'data': [{'id': 'mock-model', 'object': 'model', 'type': 'model', 'display_name': 'mock'}], 'object': 'list', 'has_more': False})
        self.send_json({'ok': True})
    def do_HEAD(self): self.send_response(200); self.send_header('content-length', '0'); self.end_headers()
    def do_POST(self):
        req = self.body()
        raw = json.dumps(req)
        path = self.path.split('?')[0]
        tools = [t.get('name') or (t.get('function') or {}).get('name') or t.get('type') for t in req.get('tools', []) or []]
        LOG.write(json.dumps({'ts': time.strftime('%H:%M:%S'), 'path': path, 'tools': tools[:40], 'stream': req.get('stream'), 'tail': raw[-300:]}) + '\n')
        if path.endswith('/count_tokens'): return self.send_json({'input_tokens': 10})
        if path.endswith('/v1/messages'): return self.anthropic(req, raw, tools)
        if path.endswith('/responses'): return self.oai_responses(req, raw, tools)
        if path.endswith('/chat/completions'): return self.chat(req, raw, tools)
        self.send_json({'error': 'not found'}, 404)

    # ---- Anthropic
    def anthropic(self, req, raw, tools):
        msgs = req.get('messages', [])
        last = msgs[-1] if msgs else {}
        has_result = isinstance(last.get('content'), list) and any(isinstance(c, dict) and c.get('type') == 'tool_result' for c in last['content'])
        cmd = find_cmd(raw)
        if has_result:
            res = text_of([c for c in last['content'] if isinstance(c, dict) and c.get('type') == 'tool_result'])
            blocks = [{'type': 'text', 'text': 'MOCK-DONE tool result: ' + res[:300]}]; stop = 'end_turn'
        elif cmd and any(t in tools for t in ('Bash', 'run_terminal_command')):
            name = 'Bash' if 'Bash' in tools else 'run_terminal_command'
            blocks = [{'type': 'tool_use', 'id': 'toolu_' + uuid.uuid4().hex[:20], 'name': name, 'input': {'command': cmd, 'description': 'mock'}}]; stop = 'tool_use'
        else:
            blocks = [{'type': 'text', 'text': 'ok'}]; stop = 'end_turn'
        model = req.get('model', 'mock')
        if not req.get('stream'):
            return self.send_json({'id': 'msg_' + uuid.uuid4().hex[:12], 'type': 'message', 'role': 'assistant', 'model': model, 'content': blocks, 'stop_reason': stop, 'stop_sequence': None, 'usage': {'input_tokens': 10, 'output_tokens': 5}})
        self.sse_start()
        self.ev('message_start', {'type': 'message_start', 'message': {'id': 'msg_' + uuid.uuid4().hex[:12], 'type': 'message', 'role': 'assistant', 'model': model, 'content': [], 'stop_reason': None, 'stop_sequence': None, 'usage': {'input_tokens': 10, 'output_tokens': 1}}})
        for i, b in enumerate(blocks):
            if b['type'] == 'text':
                self.ev('content_block_start', {'type': 'content_block_start', 'index': i, 'content_block': {'type': 'text', 'text': ''}})
                self.ev('content_block_delta', {'type': 'content_block_delta', 'index': i, 'delta': {'type': 'text_delta', 'text': b['text']}})
            else:
                self.ev('content_block_start', {'type': 'content_block_start', 'index': i, 'content_block': {'type': 'tool_use', 'id': b['id'], 'name': b['name'], 'input': {}}})
                self.ev('content_block_delta', {'type': 'content_block_delta', 'index': i, 'delta': {'type': 'input_json_delta', 'partial_json': json.dumps(b['input'])}})
            self.ev('content_block_stop', {'type': 'content_block_stop', 'index': i})
        self.ev('message_delta', {'type': 'message_delta', 'delta': {'stop_reason': stop, 'stop_sequence': None}, 'usage': {'output_tokens': 5}})
        self.ev('message_stop', {'type': 'message_stop'})

    # ---- OpenAI Responses (Codex)
    def oai_responses(self, req, raw, tools):
        items = req.get('input', [])
        last = items[-1] if items else {}
        has_result = isinstance(last, dict) and last.get('type') in ('function_call_output', 'custom_tool_call_output', 'local_shell_call_output')
        cmd = find_cmd(raw)
        rid = 'resp_' + uuid.uuid4().hex[:12]
        self.sse_start()
        self.ev('response.created', {'type': 'response.created', 'response': {'id': rid}})
        if has_result or not cmd:
            txt = ('MOCK-DONE tool result: ' + str(last.get('output'))[:300]) if has_result else 'ok'
            item = {'type': 'message', 'role': 'assistant', 'id': 'msg_1', 'content': [{'type': 'output_text', 'text': txt}]}
        elif 'exec_command' in tools:
            item = {'type': 'function_call', 'id': 'fc_1', 'call_id': 'call_' + uuid.uuid4().hex[:8], 'name': 'exec_command', 'arguments': json.dumps({'cmd': cmd})}
        elif 'shell_command' in tools:
            item = {'type': 'function_call', 'id': 'fc_1', 'call_id': 'call_' + uuid.uuid4().hex[:8], 'name': 'shell_command', 'arguments': json.dumps({'command': cmd})}
        elif 'shell' in tools:
            item = {'type': 'function_call', 'id': 'fc_1', 'call_id': 'call_' + uuid.uuid4().hex[:8], 'name': 'shell', 'arguments': json.dumps({'command': ['bash', '-lc', cmd]})}
        else:
            item = {'type': 'local_shell_call', 'id': 'ls_1', 'call_id': 'call_' + uuid.uuid4().hex[:8], 'status': 'completed', 'action': {'type': 'exec', 'command': ['bash', '-lc', cmd]}}
        self.ev('response.output_item.done', {'type': 'response.output_item.done', 'output_index': 0, 'item': item})
        self.ev('response.completed', {'type': 'response.completed', 'response': {'id': rid, 'usage': {'input_tokens': 10, 'input_tokens_details': None, 'output_tokens': 5, 'output_tokens_details': None, 'total_tokens': 15}}})

    # ---- OpenAI Chat Completions
    def chat(self, req, raw, tools):
        msgs = req.get('messages', [])
        last = msgs[-1] if msgs else {}
        cmd = find_cmd(raw)
        shell = next((t for t in tools if t and any(k in t.lower() for k in ('bash', 'shell', 'terminal', 'command', 'exec'))), None)
        if last.get('role') == 'tool' or not cmd or not shell:
            msg = {'role': 'assistant', 'content': 'MOCK-DONE tool result: ' + text_of(last.get('content'))[:300] if last.get('role') == 'tool' else 'ok'}; fin = 'stop'
        else:
            msg = {'role': 'assistant', 'content': None, 'tool_calls': [{'id': 'call_' + uuid.uuid4().hex[:8], 'type': 'function', 'function': {'name': shell, 'arguments': json.dumps({'command': cmd})}}]}; fin = 'tool_calls'
        if not req.get('stream'):
            return self.send_json({'id': 'c1', 'object': 'chat.completion', 'created': int(time.time()), 'model': req.get('model'), 'choices': [{'index': 0, 'message': msg, 'finish_reason': fin}], 'usage': {'prompt_tokens': 1, 'completion_tokens': 1, 'total_tokens': 2}})
        self.sse_start()
        delta = dict(msg)
        if 'tool_calls' in delta: delta['tool_calls'] = [dict(tc, index=0) for tc in delta['tool_calls']]
        for chunk in ({'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]}, {'choices': [{'index': 0, 'delta': {}, 'finish_reason': fin}]}):
            chunk.update({'id': 'c1', 'object': 'chat.completion.chunk', 'created': int(time.time()), 'model': req.get('model')})
            self.wfile.write(f'data: {json.dumps(chunk)}\n\n'.encode())
        self.wfile.write(b'data: [DONE]\n\n'); self.wfile.flush()

port = int(sys.argv[1]) if len(sys.argv) > 1 else 18090
ThreadingHTTPServer(('127.0.0.1', port), H).serve_forever()
