#!/usr/bin/env python3
"""Disposable local-model/trap probes. No corpus and no downloads."""
import argparse
import base64
import hashlib
import http.server
import json
from pathlib import Path
import socket
import struct
import subprocess
import threading
import time
import urllib.error
import urllib.request
import uuid
import wave
import zlib


def port():
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


def png():
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 32, 32, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress((b'\0' + b'\x80' * 96) * 32)) + chunk(b'IEND', b'')


def main():
    parser = argparse.ArgumentParser()
    for name in ('server', 'weights', 'projector', 'scratch'):
        parser.add_argument('--' + name, required=True, type=Path)
    parser.add_argument('--require-hard-disable', action='store_true')
    parser.add_argument('--whisper', type=Path)
    parser.add_argument('--speech-model', type=Path)
    args = parser.parse_args()
    args.scratch.mkdir(parents=True, exist_ok=True)
    captures = []

    class Trap(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            captures.append(self.path)
            self.send_response(404)
            self.end_headers()
        def log_message(self, *_):
            pass

    trap = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Trap)
    thread = threading.Thread(target=trap.serve_forever, daemon=True)
    thread.start()
    trap_url = f'http://127.0.0.1:{trap.server_port}/forbidden'
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPHandler())
    report = {'binary': str(args.server), 'binarySha256': hashlib.sha256(args.server.read_bytes()).hexdigest(), 'generatedFixturesOnly': True, 'trapRequests': captures}
    try:
        if args.require_hard_disable:
            with (args.scratch / 'outbound-download.log').open('wb') as log:
                result = subprocess.run([str(args.server), '-mu', trap_url, '-m', str(args.scratch / 'never-downloaded.gguf'), '--host', '127.0.0.1'], stdout=log, stderr=log, timeout=20)
            assert result.returncode != 0, 'Remote model acquisition unexpectedly succeeded'
            assert not captures, 'Outbound native model request reached trap'
            assert not (args.scratch / 'never-downloaded.gguf').exists()
            report['hardDisabledModelAcquisition'] = True
        key = 'fileid-' + uuid.uuid4().hex
        base = f'http://127.0.0.1:{port()}'
        command = [str(args.server), *([] if args.require_hard_disable else ['--offline']), '-m', str(args.weights), '--mmproj', str(args.projector), '--host', '127.0.0.1', '--port', base.rsplit(':', 1)[1], '--api-key', key, '-ngl', '0', '--no-mmproj-offload', '-c', '4096', '-t', '8', '--no-warmup']
        with (args.scratch / 'local-server.log').open('wb') as log:
            child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 120
                while True:
                    assert child.poll() is None, 'Native server exited during load'
                    try:
                        with opener.open(urllib.request.Request(base + '/health', headers={'Authorization': 'Bearer ' + key}), timeout=2) as response:
                            if response.status == 200:
                                break
                    except (OSError, urllib.error.URLError):
                        pass
                    assert time.monotonic() < deadline, 'Native server startup timeout'
                    time.sleep(.25)
                try:
                    opener.open(urllib.request.Request(base + '/v1/chat/completions', data=b'{"messages":[{"role":"user","content":"ok"}],"max_tokens":1}', headers={'Content-Type': 'application/json'}), timeout=5)
                    raise AssertionError('Unauthenticated inference API was accepted')
                except urllib.error.HTTPError as error:
                    assert error.code == 401
                def request(content, streaming=False):
                    body = {'messages': [{'role': 'user', 'content': content}], 'max_tokens': 8, 'temperature': 0, 'stream': streaming}
                    request = urllib.request.Request(base + '/v1/chat/completions', data=json.dumps(body).encode(), headers={'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
                    return opener.open(request, timeout=300).read(1024 * 1024 + 1)
                text = json.loads(request('Reply with exactly: ok'))['choices'][0]['message']['content']
                assert text.strip(), 'Local text generation was empty'
                streamed = request('Reply with exactly: ok', True)
                assert b'data:' in streamed and b'[DONE]' in streamed and len(streamed) <= 1024 * 1024
                image = 'data:image/png;base64,' + base64.b64encode(png()).decode()
                caption = json.loads(request([{'type': 'text', 'text': 'Describe the color briefly.'}, {'type': 'image_url', 'image_url': {'url': image}}]))['choices'][0]['message']['content']
                assert caption.strip(), 'Local generated-image caption was empty'
                if args.require_hard_disable:
                    for remote in ('image_url', 'audio_url', 'video_url'):
                        try:
                            request([{'type': 'text', 'text': 'Describe briefly.'}, {'type': remote, remote: {'url': trap_url}}])
                            raise AssertionError('Remote media request unexpectedly accepted')
                        except urllib.error.HTTPError as error:
                            assert 400 <= error.code < 600
                    assert not captures, 'Remote media helper reached HTTP trap'
                    report['hardDisabledRemoteMedia'] = True
                report.update(localText=text, localCaption=caption, sseDone=True, authenticatedLoopback=True)
            finally:
                child.kill()
                child.wait(timeout=10)
                report['serverReaped'] = child.returncode is not None
        if args.whisper:
            assert args.speech_model
            fixture = args.scratch / 'generated-silence.wav'
            with wave.open(str(fixture), 'wb') as audio:
                audio.setparams((1, 2, 16000, 0, 'NONE', 'not compressed'))
                audio.writeframes(b'\0' * 16000 * 2 * 2)
            with (args.scratch / 'whisper.log').open('wb') as log:
                result = subprocess.run([str(args.whisper), '-m', str(args.speech_model), '-f', str(fixture), '-ng', '-t', '4', '-oj', '-of', str(args.scratch / 'speech')], stdout=log, stderr=log, timeout=120)
            assert result.returncode == 0
            assert (args.scratch / 'speech.json').is_file()
            report['whisperGeneratedSilence'] = True
        assert not captures
        (args.scratch / 'runtime-validation.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(report, indent=2))
    finally:
        trap.shutdown()
        trap.server_close()
        thread.join(timeout=5)


if __name__ == '__main__':
    main()
