#!/usr/bin/env python3
"""Executable installer regressions. Network transport is stubbed; filesystem operations are real.

OPSTRELLIS_TEST_ASSET optionally uses the native CI bundle instead of a tiny fixture.
Linux-hosted Darwin cases only test shell control flow, not macOS compatibility.
"""
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tarfile
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
INSTALLER = Path(os.environ.get('INSTALLER_UNDER_TEST', ROOT / 'install.sh'))
NATIVE = os.uname().sysname
REAL_ASSET = os.environ.get('OPSTRELLIS_TEST_ASSET')
REAL_VERSION = os.environ.get('OPSTRELLIS_TEST_VERSION', '0.1.0')


def executable(path, text):
    path.write_text(text)
    path.chmod(0o755)


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='opstrellis-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / 'home'
        self.home.mkdir()
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.assets = self.root / 'assets'
        self.assets.mkdir()
        self.env = dict(os.environ, HOME=str(self.home), SHELL='/bin/bash',
                        PATH=str(self.bin) + ':' + os.environ['PATH'],
                        TEST_ROOT=str(self.root), TEST_OS=NATIVE,
                        GITHUB_TOKEN='test-token', GH_TOKEN='', OPSTRELLIS_VERSION='',
                        OPSTRELLIS_REPO='mtr1920/opstrellis', OPSTRELLIS_NO_MODIFY_PATH='')
        # Never write to the runner's /Applications, even when it is writable.
        (self.home / 'Applications/OpsTrellis.app').mkdir(parents=True)
        executable(self.bin / 'curl', '''#!/usr/bin/env python3
import json, os, pathlib, shutil, sys, time
root = pathlib.Path(os.environ['TEST_ROOT'])
a = sys.argv[1:]
with (root/'requests').open('a') as f: f.write(json.dumps(a)+'\\n')
if os.environ.get('NETWORK_FAILURE'): sys.exit(6)
out = pathlib.Path(a[a.index('-o')+1])
if a[-1].endswith('/releases/latest') or '/releases/tags/' in a[-1]:
    if os.environ.get('SLOW_LOOKUP'):
        (root/'lookup-started').touch()
        time.sleep(0.7)
    out.write_text((root/'release.json').read_text())
    sys.stdout.write(os.environ.get('API_STATUS', '200'))
else:
    shutil.copyfile(root/'assets'/a[-1].rsplit('/',1)[1], out)
''')
        executable(self.bin / 'mv', '''#!/usr/bin/env python3
import os, signal, subprocess, sys
src, dst = sys.argv[-2:]
old = src.endswith('OpsTrellis.app') and dst.endswith('.old')
new = src.endswith('OpsTrellis.app') and '/.OpsTrellis.' in src
if os.environ.get('FAIL_SWAP') and new: sys.exit(1)
if os.environ.get('FAIL_RESTORE') and src.endswith('.old'): sys.exit(1)
if os.environ.get('FAIL_IMAGE') and dst.endswith('OpsTrellis.AppImage'): sys.exit(1)
r = subprocess.call(['/bin/mv'] + sys.argv[1:])
if old and os.environ.get('INTERRUPT_SWAP'):
    os.kill(os.getppid(), signal.SIGTERM)
sys.exit(r)
''')
        self.release(REAL_VERSION if REAL_ASSET else '0.1.0')
        self.data = self.home / '.local/share/app.opstrellis.desktop'
        self.data.mkdir(parents=True)
        (self.data / 'settings').write_text('preserve settings and keys')
        mac_data = self.home / 'Library/Application Support/app.opstrellis.desktop'
        mac_data.mkdir(parents=True)
        (mac_data / 'settings').write_text('preserve settings and keys')

    def mac(self):
        self.env['TEST_OS'] = 'Darwin'
        if NATIVE != 'Darwin':
            executable(self.bin / 'uname', '#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n')
            executable(self.bin / 'sw_vers', '#!/bin/sh\necho "${MAC_VERSION:-14.0}"\n')
            executable(self.bin / 'sysctl', '#!/bin/sh\necho "${TRANSLATED:-0}"\n')
            executable(self.bin / 'defaults', '''#!/usr/bin/env python3
import plistlib, sys
with open(sys.argv[2]+'.plist','rb') as f: print(plistlib.load(f)['CFBundleShortVersionString'])
''')
        self.release('0.1.0')

    def release(self, version, bad_sum=False, missing_sums=False):
        self.version = version
        mac = self.env['TEST_OS'] == 'Darwin'
        self.asset_name = 'opstrellis-macos-aarch64.app.tar.gz' if mac else 'opstrellis-linux-x86_64.AppImage'
        asset = self.assets / '1'
        if REAL_ASSET and self.env['TEST_OS'] == NATIVE:
            shutil.copyfile(REAL_ASSET, asset)
        elif mac:
            bundle = self.root / 'bundle/OpsTrellis.app'
            (bundle / 'Contents/MacOS').mkdir(parents=True, exist_ok=True)
            executable(bundle / 'Contents/MacOS/opstrellis', '#!/bin/sh\nprintf "opstrellis '+version+'\\n"\n')
            (bundle / 'Contents/Info.plist').write_bytes(plistlib.dumps({'CFBundleShortVersionString': version}))
            with tarfile.open(asset, 'w:gz') as tar: tar.add(bundle, arcname='OpsTrellis.app')
        else:
            asset.write_text('#!/bin/sh\ncase "$1" in\n--appimage-extract) mkdir -p squashfs-root; echo icon > squashfs-root/OpsTrellis.png;;\n*) printf "opstrellis '+version+'\\n";;\nesac\n')
        digest = hashlib.sha256(asset.read_bytes()).hexdigest()
        (self.assets / '2').write_text(('0'*64 if bad_sum else digest)+'  '+self.asset_name+'\n')
        assets = [{'url': 'https://api.github.com/repos/mtr1920/opstrellis/releases/assets/1', 'name': self.asset_name}]
        if not missing_sums: assets.append({'url': 'https://api.github.com/repos/mtr1920/opstrellis/releases/assets/2', 'name': 'SHA256SUMS'})
        (self.root / 'release.json').write_text(json.dumps({'tag_name': 'v'+version, 'assets': assets}))

    def run_install(self, success=True, **env):
        result = subprocess.run(['bash', str(INSTALLER)], env=dict(self.env, **env), text=True, capture_output=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        for relative in ('.local/share/app.opstrellis.desktop/settings', 'Library/Application Support/app.opstrellis.desktop/settings'):
            self.assertEqual((self.home / relative).read_text(), 'preserve settings and keys')
        return result

    def installed(self):
        return self.home / ('Applications/OpsTrellis.app/Contents/MacOS/opstrellis' if self.env['TEST_OS'] == 'Darwin' else '.local/share/opstrellis/OpsTrellis.AppImage')

    def check_version(self, version):
        for flag in ('--version', '-V'):
            r = subprocess.run([str(self.home / '.local/bin/opstrellis'), flag], env=dict(self.env, APPIMAGE_EXTRACT_AND_RUN='1'), text=True, capture_output=True)
            self.assertEqual((r.returncode, r.stdout.strip()), (0, 'opstrellis '+version), r.stderr)

    def test_fresh_repeat_and_launcher_repair(self):
        self.run_install()
        self.check_version(self.version)
        first = self.installed().stat().st_ino
        (self.home / '.local/bin/opstrellis').unlink()
        self.run_install()
        self.assertEqual(self.installed().stat().st_ino, first)
        self.check_version(self.version)
        requests = [json.loads(s) for s in (self.root / 'requests').read_text().splitlines()]
        self.assertEqual(len(requests), 4)  # 2 lookups, payload, checksums
        self.assertNotIn('test-token', json.dumps(requests))
        self.assertTrue(all(a[0] == '-q' for a in requests))
        self.assertTrue(all('--proto-redir' in a and '=https' in a for a in requests))
        self.assertEqual((self.home / ('.bash_profile' if NATIVE == 'Darwin' else '.bashrc')).read_text().count('# Added by'), 1)

    @unittest.skipIf(bool(REAL_ASSET), 'version fixtures differ from the native release')
    def test_upgrade_downgrade_and_bad_checksum(self):
        self.run_install()
        self.release('0.2.0')
        self.run_install()
        self.check_version('0.2.0')
        self.release('0.1.0', bad_sum=True)
        self.run_install(False)
        self.check_version('0.2.0')
        self.release('0.1.0')
        self.run_install(OPSTRELLIS_VERSION='v0.1.0')
        self.check_version('0.1.0')

    def test_missing_checksums_and_api_errors_preserve_install(self):
        self.run_install()
        before = self.installed().read_bytes()
        self.release('0.2.0', missing_sums=True)
        self.run_install(False)
        self.run_install(False, NETWORK_FAILURE='1')
        for status in ('401', '403', '404', '429', '500'):
            self.run_install(False, API_STATUS=status)
        self.assertEqual(self.installed().read_bytes(), before)

    @unittest.skipUnless(NATIVE == 'Linux', 'Linux version metadata')
    def test_corrupt_same_version_is_repaired(self):
        self.run_install()
        self.installed().write_text('#!/bin/sh\necho broken\n')
        self.run_install()
        self.check_version(self.version)

    @unittest.skipUnless(NATIVE == 'Linux', 'Linux publish path')
    def test_linux_failed_replace_keeps_old_image(self):
        self.run_install()
        before = self.installed().read_bytes()
        # A version no real build has, so a native run still sees an update to attempt.
        self.release('99.0.0')
        self.run_install(False, FAIL_IMAGE='1')
        self.assertEqual(self.installed().read_bytes(), before)

    def test_cleanup_does_not_remove_unowned_files(self):
        for relative in ('.local/share/opstrellis/OpsTrellis.AppImage.new', '.local/bin/opstrellis.new', 'Applications/.OpsTrellis.install/keep'):
            p = self.home / relative
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text('unrelated')
        self.run_install(False, API_STATUS='404')
        for p in self.home.rglob('*'):
            if p.name in ('keep', 'opstrellis.new', 'OpsTrellis.AppImage.new'): self.assertEqual(p.read_text(), 'unrelated')
        self.assertTrue((self.home / '.local/bin/opstrellis.new').exists())
        self.assertTrue((self.home / '.local/share/opstrellis/OpsTrellis.AppImage.new').exists())
        self.assertTrue((self.home / 'Applications/.OpsTrellis.install/keep').exists())

    def test_shell_metacharacters_in_home(self):
        original = self.home
        self.home = self.root / "home 'quoted' $TEST_EXPANSION `false`"
        original.rename(self.home)
        self.env['HOME'] = str(self.home)
        self.data = self.home / '.local/share/app.opstrellis.desktop'
        self.run_install()
        self.check_version(self.version)

    @unittest.skipUnless(NATIVE == 'Linux' and not REAL_ASSET and shutil.which('gio'), 'Linux menu launcher with a fixture executable')
    def test_desktop_menu_launch_with_special_home_path(self):
        original = self.home
        self.home = self.root / 'home "quoted" $expand `false` %f \\backslash'
        original.rename(self.home)
        self.env['HOME'] = str(self.home)
        self.data = self.home / '.local/share/app.opstrellis.desktop'
        self.run_install()
        desktop = str(self.home / '.local/share/applications/opstrellis.desktop')
        result = subprocess.run(['gio', 'launch', desktop], env=self.env, text=True, capture_output=True)
        self.assertEqual((result.returncode, result.stdout.strip()), (0, 'opstrellis 0.1.0'), result.stderr)

    def test_concurrent_installs(self):
        first = subprocess.Popen(['bash', str(INSTALLER)], env=dict(self.env, SLOW_LOOKUP='1'), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(lambda: first.poll() is None and first.kill())
        for _ in range(100):
            if (self.root / 'lookup-started').exists(): break
            time.sleep(.01)
        self.run_install(False)
        out, err = first.communicate(timeout=30)
        self.assertEqual(first.returncode, 0, out+err)
        self.check_version(self.version)

    @unittest.skipIf(bool(REAL_ASSET), 'failure injection uses version fixtures')
    def test_mac_failed_swap_restores_old_app(self):
        self.mac()
        self.run_install()
        self.release('0.2.0')
        self.run_install(False, FAIL_SWAP='1')
        self.check_version('0.1.0')

    @unittest.skipIf(bool(REAL_ASSET), 'failure injection uses version fixtures')
    def test_mac_interrupted_swap_restores_old_app(self):
        self.mac()
        self.run_install()
        self.release('0.2.0')
        self.run_install(False, INTERRUPT_SWAP='1')
        self.check_version('0.1.0')

    @unittest.skipIf(bool(REAL_ASSET), 'failure injection uses version fixtures')
    def test_mac_failed_rollback_retains_backup(self):
        self.mac()
        self.run_install()
        self.release('0.2.0')
        self.run_install(False, FAIL_SWAP='1', FAIL_RESTORE='1')
        self.assertTrue(list((self.home / 'Applications').glob('.OpsTrellis.*/OpsTrellis.app.old/Contents/MacOS/opstrellis')))

    def test_untrusted_asset_url_rejected_before_auth_is_sent(self):
        doc = json.loads((self.root / 'release.json').read_text())
        doc['assets'][0]['url'] = 'https://attacker.invalid/releases/assets/1'
        (self.root / 'release.json').write_text(json.dumps(doc))
        self.run_install(False)
        self.assertEqual(len((self.root / 'requests').read_text().splitlines()), 1)

    def test_many_assets_do_not_cause_sigpipe(self):
        doc = json.loads((self.root / 'release.json').read_text())
        doc['assets'] += [{'url': 'https://api.github.com/repos/mtr1920/opstrellis/releases/assets/'+str(i), 'name': 'unused-'+str(i)} for i in range(3, 3000)]
        (self.root / 'release.json').write_text(json.dumps(doc))
        self.run_install()
        self.check_version(self.version)

    @unittest.skipIf(bool(REAL_ASSET), 'malformed fixture archive')
    def test_mac_archive_cannot_extract_outside_bundle(self):
        self.mac()
        outside = self.root / 'outside'
        outside.write_text('unexpected')
        with tarfile.open(self.assets / '1', 'w:gz') as tar:
            tar.add(outside, arcname='../escaped')
        digest = hashlib.sha256((self.assets / '1').read_bytes()).hexdigest()
        (self.assets / '2').write_text(digest+'  '+self.asset_name+'\n')
        self.run_install(False)
        self.assertFalse((self.home / 'Applications/escaped').exists())

    @unittest.skipIf(bool(REAL_ASSET), 'platform fixture')
    def test_rosetta_and_minimum_macos_version(self):
        self.mac()
        executable(self.bin / 'uname', '#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo x86_64;; esac\n')
        executable(self.bin / 'sysctl', '#!/bin/sh\necho 1\n')
        executable(self.bin / 'sw_vers', '#!/bin/sh\necho "${MAC_VERSION:-14.0}"\n')
        self.run_install(False, MAC_VERSION='10.15')
        self.run_install()
        self.check_version('0.1.0')

    @unittest.skipIf(bool(REAL_ASSET), 'macOS fixture')
    def test_mac_missing_executable_and_existing_profile(self):
        self.mac()
        (self.home / '.profile').write_text('export EXISTING_PROFILE=preserved\n')
        self.run_install()
        self.installed().unlink()
        self.run_install()
        self.check_version('0.1.0')
        self.assertFalse((self.home / '.bash_profile').exists())
        self.assertIn('EXISTING_PROFILE=preserved', (self.home / '.profile').read_text())

    def test_mac_launcher_opens_the_app_without_arguments(self):
        # Started through Launch Services, macOS checks OpsTrellis's own permissions, not the terminal's.
        self.mac()
        self.run_install()
        executable(self.bin / 'open', '#!/bin/sh\nprintf "%s" "$*" > "$TEST_ROOT/opened"\n')
        r = subprocess.run([str(self.home / '.local/bin/opstrellis')], env=self.env, text=True, capture_output=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual((self.root / 'opened').read_text(), str(self.home / 'Applications/OpsTrellis.app'))
        # With arguments it still runs the binary. On a native Mac CI installs the real build, so
        # its version isn't the fixture's.
        r = subprocess.run([str(self.home / '.local/bin/opstrellis'), '--version'], env=self.env, text=True, capture_output=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(r.stdout.startswith('opstrellis '), r.stdout)

    def test_launcher_connects_subcommands_to_the_binary(self):
        # `opstrellis mcp` speaks over stdin/stdout, so the launcher must exec the binary itself.
        for mac in (False, True):
            with self.subTest(mac=mac):
                if mac: self.mac()
                self.run_install()
                executable(self.installed(), '#!/bin/sh\nprintf "args:%s\\n" "$*"\ncat\n')
                executable(self.bin / 'open', '#!/bin/sh\necho opened\n')
                for args in (['mcp'], ['history', '--search', 'a b']):
                    r = subprocess.run([str(self.home / '.local/bin/opstrellis')] + args, input='{"id":1}\n', env=self.env, text=True, capture_output=True)
                    self.assertEqual((r.returncode, r.stdout), (0, 'args:' + ' '.join(args) + '\n{"id":1}\n'), r.stderr)

    def test_no_modify_path(self):
        self.run_install(OPSTRELLIS_NO_MODIFY_PATH='1')
        self.assertFalse((self.home / '.bashrc').exists())
        self.assertFalse((self.home / '.bash_profile').exists())

    @unittest.skipUnless(REAL_ASSET, 'needs the native bundle')
    def test_native_bundle_can_update_itself(self):
        # The installed AppImage or .app must offer in-app updates; a .deb-style "no" would leave
        # users with the command only.
        self.run_install()
        r = subprocess.run([str(self.home / '.local/bin/opstrellis'), '--can-self-update'], env=dict(self.env, APPIMAGE_EXTRACT_AND_RUN='1'), text=True, capture_output=True)
        self.assertEqual((r.returncode, r.stdout.strip()), (0, 'yes'), r.stderr)

    @unittest.skipUnless(NATIVE == 'Darwin' and REAL_ASSET, 'native macOS system installation')
    def test_native_system_app_directory(self):
        app = Path('/Applications/OpsTrellis.app')
        if app.exists() or not os.access('/Applications', os.W_OK):
            self.skipTest('system application directory is not available for an isolated install')
        (self.home / 'Applications/OpsTrellis.app').rmdir()
        self.addCleanup(lambda: shutil.rmtree(app) if app.exists() else None)
        self.run_install()
        self.assertTrue((app / 'Contents/MacOS/opstrellis').is_file())
        self.assertEqual(app.stat().st_mode & 0o777, 0o755)
        self.check_version(self.version)

    @unittest.skipUnless(NATIVE == 'Darwin' and REAL_ASSET, 'requires an actual Apple-silicon bundle and Rosetta')
    def test_native_rosetta_installs_arm_bundle(self):
        probe = subprocess.run(['/usr/bin/arch', '-x86_64', '/bin/bash', '-c', 'uname -m'], text=True, capture_output=True)
        if probe.returncode:
            self.skipTest('Rosetta is unavailable on this runner')
        self.assertEqual(probe.stdout.strip(), 'x86_64')
        result = subprocess.run(['/usr/bin/arch', '-x86_64', '/bin/bash', str(INSTALLER)], env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        self.check_version(self.version)

    @unittest.skipUnless(NATIVE == 'Linux' and not REAL_ASSET, 'Linux platform guards')
    def test_unsupported_platform_cpu_and_libc(self):
        executable(self.bin / 'uname', '#!/bin/sh\necho FreeBSD\n')
        self.run_install(False)
        executable(self.bin / 'uname', '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo riscv64;; esac\n')
        self.run_install(False)
        (self.bin / 'uname').unlink()
        executable(self.bin / 'getconf', '#!/bin/sh\necho glibc 2.34\n')
        self.run_install(False)
        executable(self.bin / 'getconf', '#!/bin/sh\nexit 1\n')
        self.run_install(False)
        self.assertFalse((self.root / 'requests').exists())

    def test_truncated_script_never_installs(self):
        script = INSTALLER.read_text()
        for cut in (script.index('main()'), len(script)//2, script.rindex('main "$@"')):
            result = subprocess.run(['bash'], input=script[:cut], env=self.env, text=True, capture_output=True)
            self.assertFalse((self.home / '.local/bin/opstrellis').exists(), result.stdout + result.stderr)


if __name__ == '__main__':
    unittest.main(verbosity=2)
