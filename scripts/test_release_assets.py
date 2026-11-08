"""Prevent evidence files, stale versions and partial matrices leaking into downloads."""
import hashlib
from pathlib import Path
import tempfile
import unittest
from release_assets import package_names, stage_downloads


class DownloadContract(unittest.TestCase):
    def test_only_complete_current_installers_and_portables_are_published(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);source=root/'native';source.mkdir()
            names=package_names('1.1.0-alpha.1')
            self.assertEqual(len(set(names)),8)
            for name in names:(source/name).write_bytes(name.encode())
            for name in ['test.json','screenshot.png','rescue-old-setup.exe','SHA256SUMS.txt']:
                (source/name).write_text('not a release download')
            stage_downloads(source,root/'release','1.1.0-alpha.1')
            self.assertEqual({p.name for p in (root/'release').iterdir()},set(names)|{'SHA256SUMS.txt'})
            checks=(root/'release/SHA256SUMS.txt').read_text().splitlines()
            self.assertEqual(len(checks),8)
            for line in checks:
                digest,name=line.split('  ',1)
                self.assertEqual(digest,hashlib.sha256((root/'release'/name).read_bytes()).hexdigest())
            with self.assertRaises(ValueError):stage_downloads(source,root/'release','1.1.0-alpha.1')

    def test_missing_architecture_aborts_before_creating_release_directory(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            for name in package_names('1.1.0')[:-1]:(root/name).write_bytes(b'package')
            with self.assertRaises(ValueError):stage_downloads(root,root/'release','1.1.0')
            self.assertFalse((root/'release').exists())


if __name__=='__main__':unittest.main()
