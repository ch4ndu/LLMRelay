#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
case "${1:-}" in
    '') ;;
    --verified-receipt) [ "$#" -eq 1 ] || exit 2 ;;
    *) echo "Usage: package.sh [--verified-receipt]" >&2; exit 2 ;;
esac
python3 - "$root" "${1:-}" <<'PY'
import fcntl, hashlib, json, os, pathlib, re, shutil, subprocess, sys, tempfile

root = pathlib.Path(sys.argv[1]).resolve()
version = re.search(r'^version = "([0-9.]+)"', (root / 'Cargo.toml').read_text(), re.M).group(1)
dist = root / '.local/dist'
for directory in (dist.parent, dist):
    if directory.is_symlink():
        raise SystemExit(f'Package output path must not be a symlink: {directory}')
    directory.mkdir(exist_ok=True)
stage = dist / f'llmrelay-{version}-macos'
archive = dist / f'{stage.name}.zip'
ownership = dist / f'.{stage.name}.owned.json'
publication = dist / f'.{stage.name}.publishing.json'
work_prefix = f'.{stage.name}.package-'


def present(path):
    return os.path.lexists(path)


def digest(path):
    if path.is_symlink() or not path.is_file():
        raise SystemExit(f'Expected a regular package file: {path}')
    return hashlib.sha256(path.read_bytes()).hexdigest()


def inventory(directory):
    if not present(directory):
        return None
    if directory.is_symlink() or not directory.is_dir():
        raise SystemExit(f'Expected a package directory: {directory}')
    result = {}
    for path in directory.iterdir():
        if path.is_dir() and not path.is_symlink():
            result[path.name + '/'] = 'directory'
            result.update({f'{path.name}/{name}': value for name, value in inventory(path).items()})
        else:
            result[path.name] = digest(path)
    return result


def read_record(path):
    if path.is_symlink() or not path.is_file():
        raise SystemExit(f'Expected a regular package record: {path}')
    try:
        record = json.loads(path.read_text())
    except (ValueError, OSError) as error:
        raise SystemExit(f'Cannot read package record {path}: {error}')
    if not isinstance(record, dict):
        raise SystemExit(f'Invalid package record: {path}')
    return record


def receipt_state(record):
    if record is None:
        return None, None, None
    if (not isinstance(record, dict) or record.get('stage') != stage.name
            or record.get('archive') != archive.name or not isinstance(record.get('files'), dict)
            or not isinstance(record.get('archive_sha256'), str)
            or not re.fullmatch(r'[a-f0-9]{64}', record['archive_sha256'])):
        raise SystemExit('Invalid distribution ownership receipt; existing files were retained')
    return record['files'], record['archive_sha256'], record


def distribution_state(directory):
    stage_path, zip_path, receipt_path = (directory / path.name for path in (stage, archive, ownership))
    return (inventory(stage_path), digest(zip_path) if present(zip_path) else None,
            read_record(receipt_path) if present(receipt_path) else None)


def resume_publication():
    record = read_record(publication)
    work_name = record.get('work')
    if (record.get('schema') != 1 or 'before' not in record or record.get('after') is None
            or not isinstance(work_name, str)
            or not re.fullmatch(re.escape(work_prefix) + r'[a-z0-9_]{8}', work_name)):
        raise SystemExit(f'Invalid package publication record: {publication}')
    work = dist / work_name
    backup = work / 'previous-stage'
    pending = work / 'publish.json'
    allowed = {stage.name, archive.name, ownership.name, backup.name, pending.name}
    if work.is_symlink() or not work.is_dir() or {path.name for path in work.iterdir()} - allowed:
        raise SystemExit(f'Package publication directory is missing or changed: {work}')
    if pending.is_symlink() or not pending.is_file() or not pending.samefile(publication):
        raise SystemExit(f'Package publication record does not belong to its working directory: {publication}')

    before = receipt_state(record['before'])
    after = receipt_state(record['after'])
    outputs = (stage, archive, ownership)
    candidates = tuple(work / path.name for path in outputs)
    moves = list(zip(candidates, outputs))
    if record['before'] is not None:
        moves.insert(0, (stage, backup))
    paths = outputs + candidates + (backup,)
    expected = dict(zip(paths, before + after + (None,)))
    current = dict(zip(paths, distribution_state(dist) + distribution_state(work) + (inventory(backup),)))

    # Only a prefix of these atomic moves may have run. Validate both copies
    # and the ownership records before performing any of the remaining moves.
    for index, (source, destination) in enumerate(moves):
        if current == expected:
            for remaining_source, remaining_destination in moves[index:]:
                os.replace(remaining_source, remaining_destination)
            break
        expected[destination], expected[source] = expected[source], None
    else:
        if current != expected:
            raise SystemExit('Interrupted distribution changed; package outputs and publication record were retained')

    # Retire the publication before cleanup so partial backup removal cannot
    # leave a completed distribution dependent on that backup for recovery.
    publication.unlink()
    shutil.rmtree(work)


def prepare_package(work, bundle):
    candidate = work / stage.name
    (candidate / 'bin').mkdir(parents=True)
    share = candidate / 'share/llmrelay'
    share.mkdir(parents=True)
    for name in ('llmrelay', 'agenticjira'):
        shutil.copy2(root / 'target/release' / name, candidate / 'bin' / name)
    for name in ('README.md', 'RESEARCH_AND_PLAN.md', 'THIRD_PARTY_NOTICES.md'):
        shutil.copy2(root / name, share / name)
    shutil.copytree(root / 'docs', share / 'docs')
    shutil.copytree(bundle, candidate / 'LLMRelay.app')
    files = inventory(candidate)
    zip_path = work / archive.name
    subprocess.run(['/usr/bin/ditto', '-c', '-k', '--sequesterRsrc', '--keepParent',
                    str(candidate), str(zip_path)], check=True)
    record = {'stage': stage.name, 'archive': archive.name, 'files': files,
              'archive_sha256': digest(zip_path)}
    (work / ownership.name).write_text(json.dumps(record, sort_keys=True))
    return record


def publish(bundle):
    if present(publication):
        resume_publication()
    current = distribution_state(dist)
    previous = current[2]
    if current != receipt_state(previous):
        raise SystemExit('Existing distribution is unowned or changed; move it aside before packaging')
    work = pathlib.Path(tempfile.mkdtemp(prefix=work_prefix, dir=dist))
    try:
        candidate = prepare_package(work, bundle)
        pending = work / 'publish.json'
        pending.write_text(json.dumps({'schema': 1, 'work': work.name, 'before': previous,
                                       'after': candidate}, sort_keys=True))
        # Link the complete record into place without overwriting an entry.
        os.link(pending, publication)
    finally:
        if not present(publication):
            shutil.rmtree(work)
    resume_publication()


# Hold the output directory lock across verification and recovery so packaging
# invocations cannot race on either the verified products or the publication.
lock = os.open(dist, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
try:
    fcntl.flock(lock, fcntl.LOCK_EX)
    if not sys.argv[2]:
        subprocess.run(['sh', str(root / 'scripts/verify.sh')], check=True)
    bundle = subprocess.check_output(['sh', str(root / 'scripts/build-macos-menu.sh'), 'validate'],
                                     text=True).rstrip('\n')
    publish(bundle)
finally:
    os.close(lock)
print(archive)
PY
