#!/usr/bin/env python3
"""Cached-only, headless native build evidence; never invoked by the application."""
import hashlib
import json
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def verify(path, expected):
    if digest(path) != expected:
        raise RuntimeError(f'Pinned SHA256 mismatch: {path}')


def check_build_graph(scratch, pins):
    configurations, graphs = {}, {}
    for name in ('llama', 'whisper'):
        build = scratch / (name + '-build')
        cache = (build / 'CMakeCache.txt').read_text()
        values = {line.split(':', 1)[0]: line.split('=', 1)[1] for line in cache.splitlines() if ':' in line and '=' in line and not line.startswith('//')}
        required = {'GGML_CUDA': 'OFF', 'GGML_VULKAN': 'OFF', 'GGML_NATIVE': 'OFF', 'BUILD_SHARED_LIBS': 'OFF'}
        if name == 'llama':
            required.update(LLAMA_OPENSSL='OFF', LLAMA_BUILD_NUMBER='9254', LLAMA_BUILD_COMMIT=pins[name]['commit'], LLAMA_BUILD_UI='OFF', LLAMA_USE_PREBUILT_UI='OFF', LLAMA_LLGUIDANCE='OFF', LLAMA_BUILD_BORINGSSL='OFF', LLAMA_BUILD_LIBRESSL='OFF', FETCHCONTENT_FULLY_DISCONNECTED='ON')
        else:
            required.update(WHISPER_CURL='OFF', WHISPER_BUILD_SERVER='OFF')
        if any(values.get(key) != value for key, value in required.items()):
            raise RuntimeError('Unexpected native build configuration: ' + name)
        configurations[name] = required
        for path in (build / 'build.ninja', build / 'CMakeFiles/rules.ninja'):
            text = path.read_text()
            if re.search(r'ui-download\.cmake|Building/provisioning UI|HF Bucket|\b(?:npm|pnpm|yarn|curl|wget)\b|llguidance_ext|FetchContent.*(?:populate|download)', text, re.IGNORECASE):
                raise RuntimeError('Unexpected install/download provisioning in build graph: ' + str(path))
            graphs[str(path)] = digest(path)
    return {'configuration': configurations, 'graphSha256': graphs, 'uiProvisioningTargets': False}


def main():
    action, repo, scratch = sys.argv[1:]
    repo, scratch = Path(repo), Path(scratch)
    pins = json.loads((repo / 'shared/models/native-runtime-sources.json').read_text())
    if action == 'prepare':
        for name in ('llama', 'whisper'):
            archive = scratch / (name + '.tar.gz')
            if not archive.is_file():
                raise RuntimeError('Missing cached approved source archive; acquisition is disabled: ' + str(archive))
            verify(archive, pins[name]['archiveSha256'])
        for name in ('llama', 'whisper'):
            pin = pins[name]
            archive = scratch / (name + '.tar.gz')
            source = scratch / (name + '-source')
            if source.exists():
                raise RuntimeError(f'Refusing to overwrite source tree: use fresh isolated scratch: {source}')
            source.mkdir()
            with tarfile.open(archive) as tar:
                for member in tar.getmembers():
                    parts = Path(member.name).parts
                    if len(parts) < 2:
                        continue
                    member.name = str(Path(*parts[1:]))
                    tar.extract(member, source, filter='data')
            verify(source / 'LICENSE', pin['licenseSha256'])
        llama = pins['llama']
        for relative, sha in llama['upstreamFiles'].items():
            verify(scratch / 'llama-source' / relative, sha)
        patch = repo / llama['patch']
        verify(patch, llama['patchSha256'])
        result = subprocess.run(['patch', '--batch', '--fuzz=0', '-p1', '-i', str(patch)], cwd=scratch / 'llama-source', capture_output=True, text=True, check=True)
        (scratch / 'patch-application.log').write_text(result.stdout + result.stderr)
        if 'offset' in result.stdout.lower() or 'fuzz' in result.stdout.lower():
            raise RuntimeError('Unexpected source: patch required an offset/fuzz')
        for relative, sha in llama['patchedFiles'].items():
            verify(scratch / 'llama-source' / relative, sha)
    elif action == 'check-graph':
        check = check_build_graph(scratch, pins)
        (scratch / 'build-graph-evidence.json').write_text(json.dumps(check, indent=2) + '\n')
        print(json.dumps(check, indent=2))
    elif action == 'record':
        for name in ('llama', 'whisper'):
            verify(scratch / (name + '.tar.gz'), pins[name]['archiveSha256'])
            verify(scratch / (name + '-source') / 'LICENSE', pins[name]['licenseSha256'])
        for relative, sha in pins['llama']['patchedFiles'].items():
            verify(scratch / 'llama-source' / relative, sha)
        verify(repo / pins['llama']['patch'], pins['llama']['patchSha256'])
        evidence = {'sources': pins, 'patchedSources': {}, 'binaries': {}, 'backend': 'CPU; native-specific instruction tuning OFF; CUDA/Vulkan OFF', 'logs': [str(scratch / name) for name in ('patch-application.log', 'llama-configure.log', 'llama-build.log', 'whisper-configure.log', 'whisper-build.log')]}
        evidence.update(check_build_graph(scratch, pins))
        evidence['acquisition'] = 'Cached approved archives only; no acquisition code path'
        evidence['headless'] = True
        for name in ('llama', 'whisper'):
            for phase in ('configure', 'build'):
                log = scratch / (name + '-' + phase + '.log')
                if re.search(r'\bnpm\b|UI:.*(?:building|downloading|provisioning)|Fetching (?:BoringSSL|LibreSSL)', log.read_text(), re.IGNORECASE):
                    raise RuntimeError('Unexpected provisioning in build log: ' + str(log))
        if (scratch / 'llama-source/tools/ui/node_modules').exists() or (scratch / 'llama-source/build/tools/ui/dist').exists():
            raise RuntimeError('Unexpected installed/generated UI artifacts')
        evidence['uiProvisioningObserved'] = False
        for relative in pins['llama']['upstreamFiles']:
            evidence['patchedSources'][relative] = digest(scratch / 'llama-source' / relative)
        for name, build in [('llama-server', 'llama'), ('llama-mtmd-cli', 'llama'), ('whisper-cli', 'whisper')]:
            path = scratch / (build + '-build') / 'bin' / name
            evidence['binaries'][name] = {'path': str(path), 'sha256': digest(path)}
            shutil.copyfile(scratch / (build + '-source') / 'LICENSE', path.parent / (build + '-LICENSE'))
        evidence['licenseArtifacts'] = {}
        for name in ('llama', 'whisper'):
            source = scratch / (name + '-source')
            licenses = sorted(path for path in source.rglob('*') if path.is_file() and (path.name.upper().startswith('LICENSE') or path.name.upper().startswith('COPYING')))
            for path in licenses:
                relative = path.relative_to(source)
                notice = scratch / 'runtime-notices' / name / relative
                notice.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(path, notice)
                evidence['licenseArtifacts'][str(notice)] = digest(notice)
        notices = '\n'.join(f"{pins[n]['repository']} {pins[n]['version']} {pins[n]['commit']} — MIT; see {n}-LICENSE; source/vendor license files retained in runtime-notices/{n}/" for n in ('llama', 'whisper'))
        (scratch / 'THIRD-PARTY-NOTICES.txt').write_text(notices + '\nFileID llama patch disables outbound helpers/client sockets, not its loopback server.\n')
        (scratch / 'native-build-evidence.json').write_text(json.dumps(evidence, indent=2) + '\n')
    else:
        raise RuntimeError('Unknown action')


if __name__ == '__main__':
    main()
