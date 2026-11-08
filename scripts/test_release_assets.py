"""Prevent evidence files, stale versions and partial matrices leaking into downloads."""
import hashlib
import copy
from pathlib import Path
import tempfile
import unittest
from release_assets import package_names, stage_downloads
from installer_versions import versions
from ime_evidence import require_repeated_ime


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


class InstallerVersions(unittest.TestCase):
    def test_numeric_versions_increase_from_preview_to_stable_and_next_patch(self):
        sequence=['1.2.0-alpha.1','1.2.0-alpha.3','1.2.0','1.2.1-alpha.1','1.2.1','1.3.0-alpha.1','1.3.0','2.0.0']
        for platform in ['windows','macos']:
            numeric=[tuple(map(int,versions(v)[platform].split('.'))) for v in sequence]
            self.assertEqual(numeric,sorted(set(numeric)))
        self.assertGreater(tuple(map(int,versions('1.2.0')['macos'].split('.'))),(1,2,3))
        self.assertEqual(versions('1.2.0-alpha.3')['linux'],'1.2.0~alpha.3')
        for invalid in ['1.2.0-rc.1','1.2.0-alpha.0','1.2.0-alpha.99','100.0.0','1.2.100']:
            with self.assertRaises(ValueError):versions(invalid)


class RepeatedImeEvidence(unittest.TestCase):
    def test_rejects_missing_or_failed_middle_cycle(self):
        cycle={'cancel_start_value':'とりけし','cancelled_roman_keys':'torikeshi',
               'committed_roman_keys':'genbun','committed_value':'原文',
               'unicode_injection_used':False,'cancel_escape_presses':2,
               'cancel_trace':[{'escape':1,'value':'とりけし'}, {'escape':2,'value':''}]}
        proof={'uia':{'native_ime':{'cycles':[copy.deepcopy(cycle) for _ in range(8)]}},
               'ime_cycle_screenshots':[{'cycle':i,'sha256':'a'*64} for i in range(1,9)]}
        require_repeated_ime(proof)
        for field,value in [('committed_value',''),('unicode_injection_used',True),
                            ('cancel_trace',[{'escape':1,'value':''},{'escape':2,'value':'とりけし'}])]:
            bad=copy.deepcopy(proof);bad['uia']['native_ime']['cycles'][3][field]=value
            with self.subTest(field=field),self.assertRaises(ValueError):require_repeated_ime(bad)
        for path in [('uia','native_ime','cycles'),('ime_cycle_screenshots',)]:
            bad=copy.deepcopy(proof);items=bad
            for key in path:items=items[key]
            items.pop()
            with self.subTest(path=path),self.assertRaises(ValueError):require_repeated_ime(bad)


if __name__=='__main__':unittest.main()
