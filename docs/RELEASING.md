# Releasing and installing

Users install OpsTrellis with one command:

```sh
curl -fsSL https://raw.githubusercontent.com/mtr1920/opstrellis/main/install.sh | bash
opstrellis --version
```

They update in the app (Settings → About → Download update, then Restart to update), or by running
the command again. A `.deb` install is updated by installing the new `.deb` from the release page:
`install.sh` has no `.deb` path and would install a second copy (the AppImage) beside it.

## Cutting a release

1. Set `version` in `desktop/src-tauri/Cargo.toml` and commit it with the `Cargo.lock` change
   from the next build. It's the only place the app version is kept: `tauri.conf.json` takes it
   from there, and so does `opstrellis --version`.
2. Merge to `main`, then tag that commit and push the tag:
   ```sh
   git tag v0.2.0 && git push origin v0.2.0
   ```
3. CI (`.github/workflows/ci.yml`) runs the tests and builds as usual. Then the `release` job checks
   the tag matches `Cargo.toml` and publishes a GitHub release with:
   - `opstrellis-linux-x86_64.AppImage` and `opstrellis-macos-aarch64.app.tar.gz`, which
     `install.sh` and the in-app updater install;
   - their `.sig` files and `latest.json`, for the in-app updater (see [In-app updates](#in-app-updates));
   - the `.deb` and `.dmg`, for people who prefer to install by hand;
   - `SHA256SUMS`, which `install.sh` checks every download against (it lists every file above).
4. On every PR and tag build, each native desktop runner first installs and reruns its actual bundle
   through a fixture GitHub transport, checks CLI versions and runs installer failure regressions.
   Publication waits for these checks. The `install-test` job then installs the published release with `install.sh` on Ubuntu 22.04 and
   macOS 15, runs it a second time, and checks `opstrellis --version`.

`install.sh` installs the latest release, which GitHub defines as the newest one that isn't a draft
or a pre-release. Tags containing a prerelease suffix (such as `v0.2.0-beta.1`) are published as
prereleases automatically. To hold another release back from users, mark it as a pre-release; testers can still
install it with `OPSTRELLIS_VERSION=v0.2.0`.

Builds exist for Linux x86_64 and Apple-silicon Macs. On other platforms `install.sh` stops with a
clear message. Adding another platform requires a matching native runner, target name, architecture
check and installer smoke test in CI; `install.sh` selects artifacts by OS and architecture.

## In-app updates

The app uses Tauri's updater plugin. It reads
`https://github.com/mtr1920/opstrellis/releases/latest/download/latest.json`, downloads the build
for its platform, and installs it only if the build's signature matches the public key compiled
into the app. GitHub serves `releases/latest/download/` from the newest release that isn't a
pre-release, so a pre-release's `latest.json` never reaches users.

- **Public key:** `plugins.updater.pubkey` in `desktop/src-tauri/tauri.conf.json`, a copy of
  `opstrellis-updater.key.pub` (made with `npx tauri signer generate`). Public; safe to commit.
- **Private key:** the repository secret `TAURI_SIGNING_PRIVATE_KEY`, holding the contents of the
  private key file. The key has no password (CI sets `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` to "").
  Only tag builds see it. Every build runs with `createUpdaterArtifacts` (through `--config`), which
  makes Tauri sign the AppImage and write `OpsTrellis.app.tar.gz` with its `.sig` on macOS, and the
  desktop job checks each `.sig` with `latest_json.py --check`. A tag build without the secret
  fails. Other builds (PRs, forks, `main`) generate a throwaway key pair in the job, sign with it
  and build in its public key instead; the key is never kept or uploaded, so those builds can't
  install real releases in-app.
- `"requireSignedVersion": true`: the app refuses an update whose signature doesn't name the
  version `latest.json` announces, so an old release can't be passed off as a new one.
- **Keep an offline backup of the private key.** If it is lost, installed apps can never be updated
  in-app again: a release signed with a new key is refused, and every user has to update once with
  the install command to a build carrying the new public key. If it leaks, anyone who can publish a
  release in this repository can ship an update; replace the key the same way.
- **`latest.json`** is written by the release job with `.github/scripts/latest_json.py` (tested by
  `tests/test_release.py`): the version without the `v`, `pub_date`, and for `linux-x86_64` and
  `darwin-aarch64` the asset's download URL for this tag and the contents of its `.sig`. It refuses
  a missing asset or signature, or a signature made for another version (the Tauri CLI writes the
  version into each signature, and the app checks it).
- On macOS the published `opstrellis-macos-aarch64.app.tar.gz` is Tauri's own archive, since the
  signature covers its exact bytes; it holds `OpsTrellis.app` at its top, as `install.sh` requires,
  and the native install test checks it before publishing, along with `opstrellis --can-self-update`
  (a hidden flag) answering `yes` for the installed AppImage or app.
- Restarting into an update leaves an `update-restart` file in the app data dir; the next launch
  removes it, opens the window and ignores the repeated launch arguments (`--background`,
  `--toggle`, …) once.
- On Linux only the AppImage updates itself. When it is the one `install.sh` manages
  (`~/.local/share/opstrellis/OpsTrellis.AppImage`), the app also rewrites install.sh's `VERSION`
  marker, so a later `install.sh` run sees the new version.
- The first release that contains the updater can't be reached in-app: older versions have no
  updater, so their users update with the install command once. In-app updates work from the
  release after it.
- Where the app can't update itself (a `.deb`, a read-only location, a failed download or a
  release without `latest.json`), Settings → About shows the error, the release page link (where a
  `.deb` user gets the new `.deb`) and the install command.

## What install.sh does

- Detects the OS and CPU (including a Rosetta shell on Apple silicon) and checks the minimum macOS
  or glibc version.
- Looks up the release through the GitHub API and downloads the build for this platform into a
  temporary directory, then checks it against `SHA256SUMS`.
- **macOS:** unpacks `OpsTrellis.app` next to the installed one and swaps it in, into
  `/Applications`, or `~/Applications` if `/Applications` isn't writable.
- **Linux:** moves the AppImage into `~/.local/share/opstrellis/` in one rename and adds an
  app-menu entry. The path never changes, so "Launch at login" keeps working after updates.
- Writes the `~/.local/bin/opstrellis` launcher and adds `~/.local/bin` to `PATH` in your shell
  profile (`~/.zshrc`, `~/.bashrc` or `~/.bash_profile`, fish's `config.fish`, else `~/.profile`)
  if it isn't on `PATH` yet. `OPSTRELLIS_NO_MODIFY_PATH=1` turns that off.
- If that version is already installed, it skips the payload download and rewrites the launcher and
  menu entry. On Linux it also verifies the installed image against its saved digest, so a damaged
  image or stale version marker triggers a repair.

It never touches the app's data directory (`~/Library/Application Support/app.opstrellis.desktop`
on macOS, `~/.local/share/app.opstrellis.desktop` on Linux), where settings, history, models and
the key fallback file live; API keys themselves are in the Keychain or Secret Service. It needs no
`sudo`. Payload download and verification failures leave the previous version intact. A failed or
interrupted macOS swap restores the previous app; if restoration itself fails, the installer prints
and preserves the backup path for manual recovery. Menu/profile failures after a successful payload
swap can be repaired by rerunning. Concurrent installers are rejected. After a power loss or SIGKILL,
check for a `.OpsTrellis.*` backup in the application directory before removing a stale
`.opstrellis-install.lock` directory and rerunning.

## Private forks and rate limits

The repository is public, so `install.sh` needs no token. A token is only needed to install from a
private fork (`OPSTRELLIS_REPO=owner/repo`) or when GitHub's anonymous API rate limit is hit:
set `GITHUB_TOKEN` (or `GH_TOKEN`) to a token with read-only **Contents** access. `install.sh`
passes it to curl from a file, so it doesn't appear in the process list.

To try the installer from a branch, run `bash install.sh` from a checkout of that branch.

A quick check on each OS before announcing a release:

1. **Fresh install:** run the command, then `opstrellis --version` in a new terminal, and open
   OpsTrellis from Launchpad or the app menu.
2. **Rerun:** run it again. It says the version is already installed and changes nothing.
3. **Update/downgrade:** install an older release with `OPSTRELLIS_VERSION=v0.1.0`, change a setting and add
   a dictionary entry, then run the command without `OPSTRELLIS_VERSION`. The version moves to
   the latest and the setting and dictionary entry are still there.
4. **In-app update:** install the previous release (one that has the updater), open Settings →
   About → Check for updates → Download update → Restart to update. The app reopens on the new
   version (`opstrellis --version` agrees; on Linux a rerun of the command says it's installed).

## Trust and verification limits

The pipe command executes the script served by the repository's `main` branch. To inspect it first,
download it to a file, review it, then run `bash install.sh`; for repeatable installs use a reviewed
commit URL. Checksums detect a corrupt or substituted artifact relative to `SHA256SUMS`; they do not
protect against a compromised GitHub account or release that replaces both. Builds are not
code-signed and macOS builds are not notarized, so Gatekeeper may require explicit approval in Privacy & Security.
Native CI verifies its runner OS, not every supported macOS/glibc version, desktop launch, microphone
or Accessibility permission flow. Exercise those manually on the minimum supported systems.

## Uninstalling

```sh
# macOS
rm -rf /Applications/OpsTrellis.app ~/Applications/OpsTrellis.app ~/.local/bin/opstrellis
# Linux
rm -rf ~/.local/share/opstrellis ~/.local/bin/opstrellis ~/.local/share/applications/opstrellis.desktop
```

Delete the data directory above as well to remove settings, history and downloaded models.
