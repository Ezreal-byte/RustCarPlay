import hashlib
import importlib.util
import io
import pathlib
import tarfile
import tempfile
import unittest
import zstandard

spec = importlib.util.spec_from_file_location('extract_usb', pathlib.Path(__file__).with_name('extract-usb-runtime.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class UsbArchiveTests(unittest.TestCase):
    def fixture(self, root, names, link=None):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode='w') as stream:
            for name in names:
                entry = tarfile.TarInfo(name)
                entry.size = 4
                stream.addfile(entry, io.BytesIO(b'test'))
            if link:
                entry = tarfile.TarInfo(link)
                entry.type = tarfile.SYMTYPE
                entry.linkname = '../../outside'
                stream.addfile(entry)
        archive = root / 'runtime.pkg.tar.zst'
        archive.write_bytes(zstandard.ZstdCompressor().compress(data.getvalue()))
        return archive, hashlib.sha256(archive.read_bytes()).hexdigest()

    def test_selected_runtime_files_and_licenses_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            names = ['.PKGINFO', 'ucrt64/bin/libtest.dll', 'ucrt64/bin/idevice_id.exe',
                     'ucrt64/share/licenses/libtest/LICENSE', 'ucrt64/bin/unwanted.exe', '.INSTALL']
            archive, digest = self.fixture(root, names)
            module.extract(archive, root / 'out', digest)
            self.assertEqual({p.relative_to(root/'out').as_posix() for p in (root/'out').rglob('*') if p.is_file()}, set(names[:4]))

    def test_rejects_unsafe_paths_links_and_duplicates_before_writing(self):
        for names, link in [(['../escape'], None), (['/absolute'], None),
                            (['ucrt64/bin/x.dll']*2, None), ([], 'ucrt64/share/licenses/x/LICENSE')]:
            with self.subTest(names=names, link=link), tempfile.TemporaryDirectory() as tmp:
                root = pathlib.Path(tmp)
                archive, digest = self.fixture(root, names, link)
                with self.assertRaises(ValueError): module.extract(archive, root/'out', digest)
                self.assertFalse(any((root/'out').rglob('*')))

    def test_rejects_wrong_hash_and_truncated_frame(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            archive, digest = self.fixture(root, ['ucrt64/bin/x.dll'])
            with self.assertRaises(ValueError): module.extract(archive, root/'out', '0'*64)
            archive.write_bytes(archive.read_bytes()[:-2])
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            with self.assertRaises(RuntimeError): module.extract(archive, root/'out', digest)
