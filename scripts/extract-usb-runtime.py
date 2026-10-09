"""Extract selected files from a hash-pinned MSYS2 runtime without shell tar."""
import argparse
import hashlib
import pathlib
import shutil

from native_runtime_sources import source_tar


def extract(archive, destination, digest):
    archive, destination = pathlib.Path(archive), pathlib.Path(destination)
    with archive.open('rb') as source:
        actual_digest = hashlib.file_digest(source, 'sha256').hexdigest()
    if actual_digest != digest:
        raise ValueError('USB runtime archive digest mismatch')
    destination.mkdir(parents=True, exist_ok=True)
    root = destination.resolve()
    with source_tar(archive) as stream:
        selected = []
        seen = set()
        for member in stream:
            path = pathlib.PurePosixPath(member.name)
            if path.is_absolute() or '..' in path.parts or '\\' in member.name or ':' in member.name:
                raise ValueError('Unsafe USB runtime archive path')
            wanted = (member.name == '.PKGINFO' or
                      (len(path.parts) == 3 and path.parts[:2] == ('ucrt64', 'bin') and
                       (path.suffix == '.dll' or path.name in ('idevice_id.exe', 'ideviceinfo.exe', 'idevicepair.exe'))) or
                      (path.parts[:3] == ('ucrt64', 'share', 'licenses') and not member.isdir()))
            if not wanted:
                continue
            if not member.isfile() or path in seen:
                raise ValueError('USB runtime links or duplicate files are not supported')
            seen.add(path)
            selected.append((member, path))
        for member, path in selected:
            target = destination.joinpath(*path.parts)
            if not target.resolve().is_relative_to(root):
                raise ValueError('USB runtime destination escapes staging directory')
            target.parent.mkdir(parents=True, exist_ok=True)
            with stream.extractfile(member) as source, target.open('wb') as output:
                shutil.copyfileobj(source, output)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive')
    parser.add_argument('destination')
    parser.add_argument('--sha256', required=True)
    args = parser.parse_args()
    extract(args.archive, args.destination, args.sha256)
