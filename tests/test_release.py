#!/usr/bin/env python3
"""Execute the workflow's release shell blocks without calling GitHub or publishing."""
import base64
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = (ROOT / '.github/workflows/ci.yml').read_text()


def tauri_signature(version, file='OpsTrellis.app.tar.gz'):
    """A .sig file's contents as the Tauri CLI writes them: a base64 minisign box (fake key)."""
    box = ('untrusted comment: signature from tauri secret key\nRUTfakesignature==\n'
           f'trusted comment: timestamp:1794568220\tfile:{file}\tversion:{version}\nZmFrZWdsb2JhbA==\n')
    return base64.b64encode(box.encode()).decode()


def workflow_script(step):
    block = WORKFLOW.split('      - name: '+step+'\n', 1)[1]
    block = block.split('        run: |\n', 1)[1]
    return textwrap.dedent(re.split(r'\n(?=\S| {1,8}\S)', block, maxsplit=1)[0])


# The release job runs on Ubuntu; macOS runners have no sha256sum.
@unittest.skipUnless(shutil.which('sha256sum'), 'needs sha256sum, like the Ubuntu release job')
class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='opstrellis-release-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = dict(os.environ, GITHUB_REF_NAME='v0.1.0',
                        PATH=str(self.root / 'bin')+':'+os.environ['PATH'])
        (self.root / 'bin').mkdir()
        gh = self.root / 'bin/gh'
        gh.write_text('#!/usr/bin/env python3\nimport json, sys\nfrom pathlib import Path\nPath("gh-call").write_text(json.dumps(sys.argv[1:]))\n')
        gh.chmod(0o755)
        (self.root / 'desktop/src-tauri').mkdir(parents=True)
        (self.root / 'desktop/src-tauri/Cargo.toml').write_text('[package]\nname = "opstrellis"\nversion = "0.1.0"\n')
        self.names = ['opstrellis-linux-x86_64.AppImage', 'opstrellis-macos-aarch64.app.tar.gz', 'OpsTrellis_0.1.0_amd64.deb', 'OpsTrellis_0.1.0_aarch64.dmg']
        for name in self.names:
            path = self.root / 'artifacts/bundle' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('artifact '+name)
        shutil.copytree(ROOT / '.github/scripts', self.root / '.github/scripts')
        self.sign('0.1.0')

    def sign(self, version):
        for name in self.names[:2]:
            (self.root / 'artifacts/bundle' / (name+'.sig')).write_text(tauri_signature(version))

    def run_step(self, step, success=True):
        result = subprocess.run(['bash', '-euo', 'pipefail', '-c', workflow_script(step)], cwd=self.root, env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode == 0, success, result.stdout+result.stderr)
        return result

    def test_matching_and_mismatched_tags(self):
        self.run_step('Check the tag matches the app version')
        self.env['GITHUB_REF_NAME'] = 'v0.2.0'
        self.run_step('Check the tag matches the app version', False)

    def test_release_contains_exact_checksums(self):
        self.run_step('Publish the release')
        sums = (self.root / 'dist/SHA256SUMS').read_text().splitlines()
        published = self.names + [n+'.sig' for n in self.names[:2]] + ['latest.json']
        self.assertEqual(sorted(line.split()[1] for line in sums), sorted(published))
        for line in sums:
            digest, name = line.split()
            self.assertEqual(digest, hashlib.sha256((self.root / 'dist' / name).read_bytes()).hexdigest())
        call = json.loads((self.root / 'gh-call').read_text())
        self.assertEqual(call[:3], ['release', 'create', 'v0.1.0'])
        self.assertNotIn('--prerelease', call)
        self.assertIn('--verify-tag', call)

    def test_missing_artifact_cannot_publish(self):
        (self.root / 'artifacts/bundle/opstrellis-macos-aarch64.app.tar.gz').unlink()
        self.run_step('Publish the release', False)
        self.assertFalse((self.root / 'gh-call').exists())

    def test_updater_manifest_points_at_this_release(self):
        self.run_step('Publish the release')
        manifest = json.loads((self.root / 'dist/latest.json').read_text())
        self.assertEqual(manifest['version'], '0.1.0')
        datetime.strptime(manifest['pub_date'], '%Y-%m-%dT%H:%M:%SZ')
        base = 'https://github.com/mtr1920/opstrellis/releases/download/v0.1.0/'
        self.assertEqual(manifest['platforms'], {
            'linux-x86_64': {'url': base+'opstrellis-linux-x86_64.AppImage', 'signature': tauri_signature('0.1.0')},
            'darwin-aarch64': {'url': base+'opstrellis-macos-aarch64.app.tar.gz', 'signature': tauri_signature('0.1.0')},
        })

    def test_unsigned_or_wrongly_signed_updates_cannot_publish(self):
        (self.root / 'artifacts/bundle/opstrellis-linux-x86_64.AppImage.sig').unlink()
        self.run_step('Publish the release', False)
        for version, sig, error in (('0.0.9', None, 'was not signed for version 0.1.0'), ('0.1.0', 'not a signature', "isn't a Tauri signature")):
            shutil.rmtree(self.root / 'dist')
            self.sign(version)
            if sig: (self.root / 'artifacts/bundle/opstrellis-macos-aarch64.app.tar.gz.sig').write_text(sig)
            self.assertIn(error, self.run_step('Publish the release', False).stderr)
        self.assertFalse((self.root / 'gh-call').exists())

    def test_prerelease_does_not_become_stable(self):
        self.env['GITHUB_REF_NAME'] = 'v0.2.0-beta.1'
        self.sign('0.2.0-beta.1')
        self.run_step('Publish the release')
        self.assertIn('--prerelease', json.loads((self.root / 'gh-call').read_text()))


def load_generator():
    spec = importlib.util.spec_from_file_location('latest_json', ROOT / '.github/scripts/latest_json.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ManifestTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='opstrellis-manifest-')
        self.addCleanup(self.temp.cleanup)
        self.dir = Path(self.temp.name)
        self.gen = load_generator()
        for name in self.gen.ASSETS.values():
            (self.dir / name).write_text('payload')
            (self.dir / (name+'.sig')).write_text(tauri_signature('1.2.3')+'\n')

    def test_platform_keys_and_date_match_what_the_updater_reads(self):
        now = datetime(2026, 11, 13, 10, 0, tzinfo=timezone.utc)
        manifest = self.gen.manifest('v1.2.3', self.dir, now)
        self.assertEqual(manifest['pub_date'], '2026-11-13T10:00:00Z')
        self.assertEqual(sorted(manifest['platforms']), ['darwin-aarch64', 'linux-x86_64'])
        self.assertEqual(manifest['platforms']['linux-x86_64']['signature'], tauri_signature('1.2.3'))

    def test_bad_input_is_refused(self):
        now = datetime.now(timezone.utc)
        for tag in ('1.2.3', 'v1.2', 'latest', 'v1.2.3/../x', 'v1.2.3\n'):
            with self.subTest(tag=tag), self.assertRaises(SystemExit):
                self.gen.manifest(tag, self.dir, now)
        self.gen.manifest('v1.2.3', self.dir, now)
        (self.dir / 'opstrellis-macos-aarch64.app.tar.gz').write_text('')
        with self.assertRaises(SystemExit):
            self.gen.manifest('v1.2.3', self.dir, now)


    def test_check_mode_validates_one_build(self):
        asset = str(self.dir / 'opstrellis-linux-x86_64.AppImage')
        self.gen.main(['--check', '1.2.3', asset])
        for version in ('1.2.4', '1.2.3-beta.1'):
            with self.subTest(version=version), self.assertRaises(SystemExit):
                self.gen.main(['--check', version, asset])
        (self.dir / 'opstrellis-linux-x86_64.AppImage.sig').unlink()
        with self.assertRaises(SystemExit):
            self.gen.main(['--check', '1.2.3', asset])

if __name__ == '__main__':
    unittest.main(verbosity=2)
