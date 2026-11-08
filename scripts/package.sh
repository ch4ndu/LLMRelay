#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
case "${1:-}" in
    '') sh "$root/scripts/verify.sh" ;;
    --verified-receipt) [ "$#" -eq 1 ] || exit 2 ;;
    *) echo "Usage: package.sh [--verified-receipt]" >&2; exit 2 ;;
esac
bundle=$(sh "$root/scripts/build-macos-menu.sh" validate)
python3 - "$root" "$bundle" <<'PY'
import hashlib, json, os, pathlib, re, shutil, subprocess, sys, tempfile
root = pathlib.Path(sys.argv[1]).resolve()
bundle = pathlib.Path(sys.argv[2])
version = re.search(r'^version = "([0-9.]+)"', (root / 'Cargo.toml').read_text(), re.M).group(1)
dist = root / '.local/dist'
if (root / '.local').is_symlink(): raise SystemExit('Package parent directory must not be a symlink')
dist.mkdir(parents=True, exist_ok=True)
if dist.is_symlink(): raise SystemExit('Package output directory must not be a symlink')
stage = dist / f'llmrelay-{version}-macos'
archive = dist / f'{stage.name}.zip'
ownership = dist / f'.{stage.name}.owned.json'

def digest(path):
    if path.is_symlink() or not path.is_file(): raise SystemExit(f'Unrecognized package file: {path.name}')
    return hashlib.sha256(path.read_bytes()).hexdigest()

def inventory(directory):
    result = {}
    if directory.is_symlink(): raise SystemExit('Package staging directory is a symlink')
    for path in sorted(directory.rglob('*')):
        if path.is_symlink(): raise SystemExit('Package staging contains a symlink')
        if path.is_dir(): result[str(path.relative_to(directory)) + '/'] = 'directory'
        elif path.is_file(): result[str(path.relative_to(directory))] = digest(path)
        else: raise SystemExit('Unrecognized package output type')
    return result

# Remove only outputs whose complete prior inventory still matches.
if stage.exists() or archive.exists():
    if not ownership.is_file() or ownership.is_symlink(): raise SystemExit('Existing distribution has no ownership receipt; move it aside before packaging')
    previous = json.loads(ownership.read_text())
    if previous.get('stage') != stage.name or previous.get('archive') != archive.name or previous.get('files') != inventory(stage) or not archive.is_file() or previous.get('archive_sha256') != digest(archive):
        raise SystemExit('Existing distribution changed; it was retained')
temporary = pathlib.Path(tempfile.mkdtemp(prefix='.llmrelay-package-', dir=dist))
try:
    (temporary / 'bin').mkdir()
    share = temporary / 'share/llmrelay'
    share.mkdir(parents=True)
    for name in ('llmrelay', 'agenticjira'): shutil.copy2(root / 'target/release' / name, temporary / 'bin' / name)
    for name in ('README.md', 'RESEARCH_AND_PLAN.md', 'THIRD_PARTY_NOTICES.md'): shutil.copy2(root / name, share / name)
    shutil.copytree(root / 'docs', share / 'docs')
    shutil.copytree(bundle, temporary / 'LLMRelay.app')
    files = inventory(temporary)
    if stage.exists(): shutil.rmtree(stage)
    os.replace(temporary, stage)
    if archive.exists(): archive.unlink()
    subprocess.run(['/usr/bin/ditto', '-c', '-k', '--sequesterRsrc', '--keepParent', str(stage), str(archive)], check=True)
    record = {'stage': stage.name, 'archive': archive.name, 'files': files, 'archive_sha256': digest(archive)}
    pending = ownership.with_suffix('.pending')
    pending.write_text(json.dumps(record, sort_keys=True))
    os.replace(pending, ownership)
finally:
    if temporary.exists(): shutil.rmtree(temporary)
print(archive)
PY
