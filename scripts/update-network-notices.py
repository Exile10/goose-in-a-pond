#!/usr/bin/env python3
"""Regenerate redistribution notices for the pinned embedded networking module."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
MODULE = ROOT / 'native/pondnet'
TOOL = 'github.com/google/go-licenses/v2@v2.0.1'
IGNORE = 'github.com/Exile10/goose-in-a-pond/native/pondnet'


def go_env():
    """The environment for every go call: at least the module's own Go, which go-licenses needs.

    go-licenses finds the standard library through the toolchain that built it; built by an older
    Go than the module's go line, it reports every standard package as lacking module info.
    """
    env = dict(os.environ)
    if 'GOTOOLCHAIN' not in env:
        for line in (MODULE / 'go.mod').read_text().splitlines():
            if line.startswith('go '):
                env['GOTOOLCHAIN'] = f'go{line.split()[1]}+auto'
    return env


def collect(output, directories):
    notices = {}
    for directory in directories:
        for path in sorted(directory.rglob('*')):
            if path.is_file():
                name = path.relative_to(directory).as_posix()
                content = path.read_text()
                if name in notices and notices[name] != content:
                    raise RuntimeError(f'Platform notice disagreement: {name}')
                notices[name] = content
    goroot = subprocess.check_output(['go', 'env', 'GOROOT'], text=True, env=go_env()).strip()
    notices['Go runtime/LICENSE'] = (Path(goroot) / 'LICENSE').read_text()
    checksum = hashlib.sha256((MODULE / 'go.sum').read_bytes()).hexdigest()
    text = ['Embedded networking third-party notices',
            f'Generated using {TOOL}.', f'go.sum SHA-256: {checksum}',
            'Coverage: macOS, Linux ARM64, Android ARM64 and iOS ARM64.',
            'The first-party module is excluded; its third-party dependencies are included.',
            'Dependency versions and checksums are pinned in go.mod and go.sum.\n']
    for name, content in sorted(notices.items()):
        text.extend([f'--- {name} ---\n', content.rstrip(), ''])
    output.write_text('\n'.join(text))
    print(f'Wrote {len(notices)} third-party notices to {output}')


def main():
    with tempfile.TemporaryDirectory(prefix='pond-notices-') as temp:
        temp = Path(temp)
        tools = temp / 'tools'
        tools.mkdir()
        subprocess.run(['go', 'install', TOOL], env={**go_env(), 'GOBIN': str(tools)}, check=True)
        directories = []
        for platform, cgo, packages in [
            ('darwin', '0', ['./mobile', './cmd/pondnet', './cmd/pond-enrollment']),
            ('linux', '0', ['./mobile', './cmd/pondnet', './cmd/pond-enrollment']),
            ('android', '0', ['./mobile']), ('ios', '1', ['./mobile']),
        ]:
            destination = temp / platform
            subprocess.run([str(tools / 'go-licenses'), 'save', *packages,
                            '--ignore=' + IGNORE, '--save_path=' + str(destination)],
                           cwd=MODULE, env={**go_env(), 'GOOS': platform, 'GOARCH': 'arm64', 'CGO_ENABLED': cgo}, check=True)
            directories.append(destination)
        collect(MODULE / 'THIRD_PARTY_NOTICES.txt', directories)


if __name__ == '__main__':
    main()
