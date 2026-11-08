#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
# Only verify.sh writes the receipt after its complete matrix succeeds.
python3 - "$root" "${1:-compile}" <<'PY'
import hashlib, json, os, pathlib, plistlib, platform, shutil, subprocess, sys, time
root = pathlib.Path(sys.argv[1]).resolve()
mode = sys.argv[2]
if mode not in ('prepare', 'compile', 'receipt', 'validate'):
    raise SystemExit('Expected prepare, compile, receipt, or validate')
if any(name in os.environ for name in ('AGENTICJIRA_FRONTEND_DIST', 'AGENTICJIRA_ALLOW_FOUNDATION')):
    raise SystemExit('Normal distribution verification requires the standard dashboard. Unset AGENTICJIRA_FRONTEND_DIST and AGENTICJIRA_ALLOW_FOUNDATION.')
if platform.system() != 'Darwin': raise SystemExit('Native bundle checks require macOS')
receipt_path = root / '.local/native-menu/verification.json'
started_path = receipt_path.with_name('verification-inputs.json')
for directory in (root / '.local', receipt_path.parent):
    if directory.is_symlink(): raise SystemExit('Native output directory must not be a symlink')
    directory.mkdir(exist_ok=True)

def digest(path):
    if path.is_symlink() or not path.is_file(): raise SystemExit(f'Expected regular input: {path.relative_to(root)}')
    return hashlib.sha256(path.read_bytes()).hexdigest()

def inputs():
    files = []
    for folder in ('src', 'tests', 'migrations', 'resources', 'frontend/src', 'frontend/dist', 'macos', 'scripts', 'docs', '.cargo'):
        files.extend(path for path in (root / folder).rglob('*') if path.is_file() or path.is_symlink())
    for name in ('Cargo.toml', 'Cargo.lock', 'build.rs', 'rust-toolchain.toml', 'README.md', 'RESEARCH_AND_PLAN.md', 'THIRD_PARTY_NOTICES.md', 'frontend/package.json', 'frontend/deno.json', 'frontend/deno.lock', 'frontend/index.html', 'frontend/vite.config.ts', 'frontend/tsconfig.json'):
        if (root / name).exists(): files.append(root / name)
    return {str(path.relative_to(root)): digest(path) for path in sorted(set(files))}

def tool_identity(name, arguments, directory=root):
    executable = shutil.which(name)
    if executable is None: raise SystemExit(f'Required build tool is unavailable: {name}')
    path = pathlib.Path(executable).absolute()
    version = subprocess.run([str(path), *arguments], cwd=directory, check=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=10)
    if not version.stdout.strip(): raise SystemExit(f'Build tool returned no version: {name}')
    return {'path': str(path), 'resolved_path': str(path.resolve(strict=True)),
        'version': version.stdout.strip(), 'version_stderr': version.stderr.strip()}

swiftc = subprocess.run(['/usr/bin/xcrun', '--find', 'swiftc'], check=True,
    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=10).stdout.strip()
source = inputs()
build_config = {'architecture': platform.machine(), 'swift_target': f'{platform.machine()}-apple-macosx12.0',
    'tools': {'rustc': tool_identity(os.environ.get('RUSTC') or 'rustc', ['--version', '--verbose']),
        'cargo': tool_identity('cargo', ['--version', '--verbose']),
        'deno': tool_identity('deno', ['--version'], root / 'frontend'),
        'swiftc': tool_identity(swiftc, ['--version'])},
    'environment_hashes': {name: hashlib.sha256(os.environ.get(name, '').encode()).hexdigest()
        for name in ('SDKROOT', 'DEVELOPER_DIR', 'MACOSX_DEPLOYMENT_TARGET', 'RUSTUP_TOOLCHAIN', 'RUSTC', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER')}}
release_hashes = {} if mode == 'prepare' else {name: digest(root / 'target/release' / name) for name in ('llmrelay', 'agenticjira')}
candidate = hashlib.sha256(json.dumps([source, build_config, release_hashes], sort_keys=True).encode()).hexdigest()
bundle = root / '.local/native-menu' / candidate / 'LLMRelay.app'
products = [bundle / 'Contents/MacOS/LLMRelayMenu', bundle / 'Contents/MacOS/llmrelay', bundle / 'Contents/Info.plist', root / 'target/release/llmrelay', root / 'target/release/agenticjira']
stable_source = {name: value for name, value in source.items() if not name.startswith('frontend/dist/')}
for path in (bundle.parent, bundle, bundle / 'Contents', bundle / 'Contents/MacOS'):
    if path.is_symlink(): raise SystemExit('Native bundle output path must not be a symlink')

def valid_timestamp(value):
    # Timestamps record provenance; unchanged verified inputs do not expire.
    return type(value) in (int, float) and 0 < value <= time.time()

def checked_verification_start():
    if started_path.is_symlink(): raise SystemExit('Verification start record must not be a symlink')
    started = json.loads(started_path.read_text())
    if started.get('root') != str(root) or not valid_timestamp(started.get('started_at')):
        raise SystemExit('Verification start is absent, foreign, or invalid; rerun verify.sh')
    if started.get('source') != stable_source or started.get('build_configuration') != build_config:
        raise SystemExit('Source or build configuration changed during verification; rerun verify.sh')

def inspect_bundle():
    actual = {path.relative_to(bundle).as_posix() for path in bundle.rglob('*') if not path.is_dir()}
    if actual != {'Contents/MacOS/LLMRelayMenu', 'Contents/MacOS/llmrelay', 'Contents/Info.plist'}: raise SystemExit('Unexpected bundle content')
    for path in bundle.rglob('*'):
        if path.is_symlink(): raise SystemExit('Bundle symlink refused')
    info = plistlib.loads((bundle / 'Contents/Info.plist').read_bytes())
    if info.get('CFBundleExecutable') != 'LLMRelayMenu' or info.get('LSUIElement') is not True or info.get('CFBundleIdentifier') != 'com.llmrelay.menu': raise SystemExit('Native bundle contract mismatch')
    if digest(products[1]) != digest(root / 'target/release/llmrelay'): raise SystemExit('Bundled CLI differs from the verified release executable')
    if digest(products[2]) != digest(root / 'macos/Info.plist'): raise SystemExit('Bundled plist differs from the reviewed resource')
    if not all(os.access(path, os.X_OK) for path in (products[0], products[1])): raise SystemExit('Bundled programs are not executable')

if mode == 'prepare':
    if started_path.is_symlink(): raise SystemExit('Verification start record must not be a symlink')
    started_path.write_text(json.dumps({'root': str(root), 'started_at': time.time(),
        'source': stable_source, 'build_configuration': build_config}, sort_keys=True))
elif mode == 'compile':
    checked_verification_start()
    directory = bundle / 'Contents/MacOS'
    if bundle.exists(): inspect_bundle()
    directory.mkdir(parents=True, exist_ok=True)
    subprocess.run(['/usr/bin/xcrun', 'swiftc', '-swift-version', '5', '-parse-as-library', '-target', build_config['swift_target'], '-O', '-framework', 'AppKit', '-framework', 'UserNotifications', '-framework', 'CryptoKit', str(root / 'macos/LLMRelayMenu.swift'), '-o', str(products[0])], check=True)
    shutil.copy2(root / 'target/release/llmrelay', products[1])
    shutil.copy2(root / 'macos/Info.plist', products[2])
    inspect_bundle()
elif mode == 'receipt':
    checked_verification_start()
    inspect_bundle()
    receipt = {'schema': 1, 'verified_at': time.time(), 'root': str(root), 'candidate': candidate,
        'source_build_config': source, 'build_configuration': build_config, 'bundle': str(bundle.relative_to(root)),
        'artifacts': {str(path.relative_to(root)): digest(path) for path in products}}
    temporary = receipt_path.with_suffix('.pending')
    with temporary.open('x') as output:
        json.dump(receipt, output, sort_keys=True); output.flush(); os.fsync(output.fileno())
    os.replace(temporary, receipt_path)
    fd = os.open(receipt_path.parent, os.O_RDONLY)
    try: os.fsync(fd)
    finally: os.close(fd)
else:
    if receipt_path.is_symlink(): raise SystemExit('Verification receipt must not be a symlink')
    receipt = json.loads(receipt_path.read_text())
    if receipt.get('schema') != 1 or receipt.get('root') != str(root) or not valid_timestamp(receipt.get('verified_at')):
        raise SystemExit('Verification receipt is absent, foreign, or invalid; rerun verify.sh')
    if receipt.get('candidate') != candidate or receipt.get('source_build_config') != source or receipt.get('build_configuration') != build_config or receipt.get('bundle') != str(bundle.relative_to(root)): raise SystemExit('Source/build/configuration changed after verification; rerun verify.sh')
    expected = {str(path.relative_to(root)): digest(path) for path in products}
    if receipt.get('artifacts') != expected: raise SystemExit('Verified products changed; rerun verify.sh')
    inspect_bundle()
    print(bundle)
PY
