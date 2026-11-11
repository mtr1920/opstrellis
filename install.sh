#!/usr/bin/env bash
# OpsTrellis installer and updater for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/mtr1920/opstrellis/main/install.sh | bash
#
# Run it again at any time to update. Settings, history, models and keys live in the app's data
# directory, which this script never touches.
#
# Environment:
#   OPSTRELLIS_VERSION=v0.2.0      install this release instead of the latest one
#   GITHUB_TOKEN (or GH_TOKEN)     a token that can read the repository, while it's private
#   OPSTRELLIS_NO_MODIFY_PATH=1    don't add ~/.local/bin to PATH in your shell profile
#   OPSTRELLIS_REPO=owner/repo     install from a fork
#
# Installs:
#   macOS  /Applications/OpsTrellis.app (~/Applications if /Applications isn't writable)
#   Linux  ~/.local/share/opstrellis/OpsTrellis.AppImage, plus an app-menu entry
#   both   ~/.local/bin/opstrellis, the command-line launcher (`opstrellis --version`, `opstrellis mcp`)

set -euo pipefail

repo="${OPSTRELLIS_REPO:-mtr1920/opstrellis}"
token="${GITHUB_TOKEN:-${GH_TOKEN:-}}"
bin_dir="$HOME/.local/bin"
data_dir="$HOME/.local/share/opstrellis"
appimage="$data_dir/OpsTrellis.AppImage"
app_dir="/Applications"
if [ -d "$HOME/Applications/OpsTrellis.app" ] || [ ! -w /Applications ]; then app_dir="$HOME/Applications"; fi
app="$app_dir/OpsTrellis.app"
tmp=""
staging=""
launcher_staging=""
lock=""

# Everything runs from main(), at the very end, so a download cut short runs nothing.
main() {
  need curl
  # Private staging and auth files, even with a permissive caller umask.
  umask 077
  need tar
  if command -v sha256sum >/dev/null; then sha256=(sha256sum)
  elif command -v shasum >/dev/null; then sha256=(shasum -a 256)
  else die "Need sha256sum or shasum to verify the download."; fi

  # --- Platform -------------------------------------------------------------------------------
  local os arch asset
  case "$(uname -s)" in
    Darwin) os=macos ;;
    Linux) os=linux ;;
    *) die "Unsupported OS: $(uname -s). OpsTrellis runs on macOS and Linux." ;;
  esac
  case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
    *) die "Unsupported CPU architecture: $(uname -m)." ;;
  esac
  if [ "$os" = macos ]; then
    # A shell running under Rosetta reports x86_64 on Apple silicon; install the native build.
    if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null)" = 1 ]; then arch=aarch64; fi
    local macos
    macos="$(sw_vers -productVersion)"
    [ "${macos%%.*}" -ge 11 ] || die "OpsTrellis needs macOS 11 or later (this is $macos)."
    asset="opstrellis-macos-$arch.app.tar.gz"
  else
    # The AppImage is built on Ubuntu 22.04, so it needs glibc 2.35 or newer.
    local glibc
    glibc="$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}')" ||
      die "OpsTrellis needs a glibc-based Linux distribution (musl is not supported)."
    version_ge "$glibc" 2.35 || die "OpsTrellis needs glibc 2.35 or newer (this system has $glibc)."
    asset="opstrellis-linux-$arch.AppImage"
  fi

  # --- Release --------------------------------------------------------------------------------
  local destination="$data_dir"
  if [ "$os" = macos ]; then
    destination="$app_dir"
    if [ "$app_dir" != /Applications ] && [ ! -d "$app" ] && [ -d /Applications/OpsTrellis.app ]; then
      die "An existing /Applications/OpsTrellis.app cannot be updated here. Use an account that can write to /Applications."
    fi
  fi
  mkdir -p "$destination"
  local candidate="$destination/.opstrellis-install.lock"
  mkdir "$candidate" 2>/dev/null || die "Another installer may be running. If none is running, remove $candidate and retry."
  lock="$candidate"
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM HUP
  tmp="$(mktemp -d)"
  curl_args=(-q -sSL --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 --connect-timeout 20)
  if [ -n "$token" ]; then
    # From a file, so the token doesn't show up in the process list.
    printf 'Authorization: Bearer %s\n' "$token" >"$tmp/auth"
    curl_args+=(-H "@$tmp/auth")
  fi

  local wanted="${OPSTRELLIS_VERSION:-latest}" url status
  if [ "$wanted" = latest ]; then
    url="https://api.github.com/repos/$repo/releases/latest"
  else
    wanted="v${wanted#v}"
    url="https://api.github.com/repos/$repo/releases/tags/$wanted"
  fi
  say "Looking up the $wanted OpsTrellis release..."
  status="$(curl "${curl_args[@]}" -H 'Accept: application/vnd.github+json' -o "$tmp/release.json" \
    -w '%{http_code}' "$url")" || die "Could not reach api.github.com. Check your network connection."
  case "$status" in
    200) ;;
    401) die "GitHub rejected the token in GITHUB_TOKEN. Check that it's valid and not expired." ;;
    403 | 429) die "GitHub refused the request (HTTP $status): the API rate limit, or a token without access. Try again later, or set GITHUB_TOKEN." ;;
    404) if [ -z "$token" ]; then die "No $wanted release found in $repo. If the repository is private, set GITHUB_TOKEN."
         else die "No $wanted release found in $repo, or the token can't read the repository."; fi ;;
    *) die "GitHub API returned HTTP $status for $url." ;;
  esac

  local tag version asset_url sums_url installed
  tag="$(grep -oE '"tag_name": *"[^"]*"' "$tmp/release.json" | sed -n '1p' | cut -d'"' -f4)"
  [ -n "$tag" ] || die "Could not read the release from the GitHub API response."
  version="${tag#v}"
  asset_url="$(asset_url "$asset" <"$tmp/release.json")"
  sums_url="$(asset_url SHA256SUMS <"$tmp/release.json")"
  [ -n "$asset_url" ] || die "Release $tag has no build for $os/$arch ($asset)."
  [ -n "$sums_url" ] || die "Release $tag has no SHA256SUMS file to verify the download with."
  installed="$(installed_version)"

  if [ "$installed" = "$version" ]; then
    say "OpsTrellis $version is already installed."
  else
    # --- Download and verify ------------------------------------------------------------------
    say "Downloading OpsTrellis $version for $os/$arch..."
    download "$asset_url" "$tmp/$asset"
    download "$sums_url" "$tmp/SHA256SUMS"
    local expected actual
    expected="$(awk -v n="$asset" '$2 == n || $2 == "*" n {print $1}' "$tmp/SHA256SUMS")"
    actual="$("${sha256[@]}" "$tmp/$asset" | awk '{print $1}')"
    [ -n "$expected" ] || die "SHA256SUMS has no entry for $asset."
    [ "$expected" = "$actual" ] || die "Checksum mismatch for $asset: the download is corrupt or was tampered with."

    # --- Install ------------------------------------------------------------------------------
    # The new copy is staged next to the old one and moved into place, so an interrupted
    # install leaves the previous version working.
    if [ "$os" = macos ]; then
      staging="$(mktemp -d "$app_dir/.OpsTrellis.XXXXXX")"
      # Only extract entries inside the bundle; tar also rejects symlink traversal.
      tar -tzf "$tmp/$asset" >"$tmp/entries"
      if ! awk '$0 !~ /^OpsTrellis[.]app(\/|$)/ || $0 ~ /(^|\/)\.\.(\/|$)/ {bad=1} END {exit bad}' "$tmp/entries"; then
        die "The download contains paths outside OpsTrellis.app."
      fi
      (umask 022; tar -xzf "$tmp/$asset" -C "$staging")
      [ ! -L "$staging/OpsTrellis.app" ] && [ -x "$staging/OpsTrellis.app/Contents/MacOS/opstrellis" ] || die "The download doesn't contain OpsTrellis.app."
      if [ -e "$app" ]; then
        mv "$app" "$staging/OpsTrellis.app.old" || die "Can't replace $app. Remove it and run this again."
      fi
      if ! mv "$staging/OpsTrellis.app" "$app"; then
        die "Can't move the new OpsTrellis.app into $app_dir."
      fi
      rm -rf "$staging"
    else
      staging="$(mktemp -d "$data_dir/.OpsTrellis.XXXXXX")"
      cp "$tmp/$asset" "$staging/OpsTrellis.AppImage"
      chmod 755 "$staging/OpsTrellis.AppImage"
      [ ! -d "$appimage" ] || die "$appimage is a directory; refusing to replace it."
      mv -f "$staging/OpsTrellis.AppImage" "$appimage"
      printf '%s\n%s\n' "$version" "$actual" >"$staging/VERSION"
      mv -f "$staging/VERSION" "$data_dir/VERSION"
    fi
    if [ -n "$installed" ]; then say "Installed OpsTrellis $version (replacing $installed)."
    else say "Installed OpsTrellis $version."; fi
    if pgrep -x opstrellis >/dev/null 2>&1; then
      say "OpsTrellis is running: quit it from the tray icon and open it again to use the new version."
    fi
  fi

  # Rewritten on every run, which also repairs a launcher or menu entry that was removed.
  if [ "$os" = linux ]; then
    install_desktop_entry
    check_fuse
  fi
  install_launcher
  ensure_path
  say "Run 'opstrellis --version' to check the installed version, or open OpsTrellis from your apps."
}

# The installed version, or "" if none. On macOS it's read from the app itself, so an app
# updated another way (from the DMG, say) is still detected correctly.
installed_version() {
  if [ "$(uname -s)" = Darwin ]; then
    if [ -x "$app/Contents/MacOS/opstrellis" ] && [ -f "$app/Contents/Info.plist" ]; then defaults read "$app/Contents/Info" CFBundleShortVersionString 2>/dev/null || true; fi
  elif [ -x "$appimage" ] && [ -f "$data_dir/VERSION" ]; then
    # A stale version marker must not hide an interrupted update or a damaged payload.
    local digest
    digest="$("${sha256[@]}" "$appimage" | awk '{print $1}')"
    if [ "$digest" = "$(sed -n '2p' "$data_dir/VERSION")" ]; then sed -n '1p' "$data_dir/VERSION"; fi
  fi
}

# Prints the API download URL of the release asset named $1, from the release JSON on stdin.
# Each asset object lists its "url" before its "name"; no other "url" field matches the pattern.
asset_url() {
  grep -oE '"(url|name)": *"[^"]*"' | awk -F'"' -v want="$1" '
    $2 == "url" && $4 ~ /\/releases\/assets\/[0-9]+$/ { url = $4; next }
    $2 == "name" && url != "" { if ($4 == want) { found = url } url = "" }
    END { print found }'
}

# The API URL works for private repositories too. It redirects to a signed storage URL, and curl
# doesn't send the token on to that other host.
download() {
  local id="${1##*/}"
  case "$id" in ''|*[!0-9]*) die "Invalid GitHub release asset URL." ;; esac
  [ "$1" = "https://api.github.com/repos/$repo/releases/assets/$id" ] || die "Invalid GitHub release asset URL."
  curl "${curl_args[@]}" -f -H 'Accept: application/octet-stream' -o "$2" "$1" || die "Download failed: $1"
}

install_launcher() {
  local target="$appimage"
  if [ "$(uname -s)" = Darwin ]; then target="$app/Contents/MacOS/opstrellis"; fi
  mkdir -p "$bin_dir"
  launcher_staging="$(mktemp -d "$bin_dir/.opstrellis.XXXXXX")"
  if [ "$(uname -s)" = Darwin ]; then
    # Without arguments, start the app through Launch Services: run directly from a terminal, macOS
    # would check the terminal's Accessibility, microphone and Input Monitoring permissions instead.
    printf '#!/usr/bin/env bash\n[ $# -gt 0 ] || exec open %q\nexec %q "$@"\n' "$app" "$target" >"$launcher_staging/opstrellis"
  else
    printf '#!/usr/bin/env bash\nexec %q "$@"\n' "$target" >"$launcher_staging/opstrellis"
  fi
  chmod 755 "$launcher_staging/opstrellis"
  [ ! -d "$bin_dir/opstrellis" ] || die "$bin_dir/opstrellis is a directory; refusing to replace it."
  mv -f "$launcher_staging/opstrellis" "$bin_dir/opstrellis"
}

install_desktop_entry() {
  local apps="$HOME/.local/share/applications" icon="$data_dir/opstrellis.png"
  # The AppImage carries its own icon; extracting a file from it doesn't need FUSE.
  if (cd "$tmp" && "$appimage" --appimage-extract OpsTrellis.png >/dev/null 2>&1); then
    cp "$tmp/squashfs-root/OpsTrellis.png" "$icon" 2>/dev/null || true
  fi
  mkdir -p "$apps"
  local exec_path
  exec_path="$(printf '%s' "$appimage" | sed 's/\\/\\\\\\\\/g; s/"/\\\\"/g; s/[$`]/\\\\&/g; s/%/%%/g')"
  icon="$(printf '%s' "$icon" | sed 's/\\/\\\\/g')"
  cat >"$apps/opstrellis.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=OpsTrellis
Comment=Voice dictation for every app
Exec=/usr/bin/env "$exec_path"
Icon=$icon
Terminal=false
Categories=Office;
StartupWMClass=opstrellis
EOF
  if command -v update-desktop-database >/dev/null; then update-desktop-database "$apps" >/dev/null 2>&1 || true; fi
}

# Adds ~/.local/bin to PATH in the shell's profile, once.
# shellcheck disable=SC2016 # the profile line expands $HOME when the shell starts, not now
ensure_path() {
  case ":$PATH:" in *":$bin_dir:"*) return ;; esac
  local line='export PATH="$HOME/.local/bin:$PATH"' rc
  case "$(basename "${SHELL:-sh}")" in
    zsh) rc="${ZDOTDIR:-$HOME}/.zshrc" ;;
    bash)
      rc="$HOME/.bashrc"
      if [ "$(uname -s)" = Darwin ]; then
        rc="$HOME/.bash_profile"
        local profile
        for profile in .bash_profile .bash_login .profile; do
          if [ -f "$HOME/$profile" ]; then rc="$HOME/$profile"; break; fi
        done
      fi ;;
    fish) rc="$HOME/.config/fish/config.fish"; line='fish_add_path "$HOME/.local/bin"' ;;
    *) rc="$HOME/.profile" ;;
  esac
  if [ -n "${OPSTRELLIS_NO_MODIFY_PATH:-}" ]; then
    say "$bin_dir is not on your PATH. Add it with: $line"
    return
  fi
  if ! grep -qsF "$line" "$rc"; then
    mkdir -p "$(dirname "$rc")"
    printf '\n# Added by the OpsTrellis installer\n%s\n' "$line" >>"$rc"
    say "Added $bin_dir to your PATH in $rc."
  fi
  say "Open a new terminal to use 'opstrellis', or run: $line"
}

# AppImages mount themselves with FUSE: they need fusermount and access to /dev/fuse.
check_fuse() {
  if ! command -v fusermount3 >/dev/null && ! command -v fusermount >/dev/null; then
    say "Note: AppImages need FUSE to start, and it isn't installed. Install it with your package"
    say "manager (Ubuntu/Debian: sudo apt install fuse3; Fedora: sudo dnf install fuse3)."
  elif [ ! -w /dev/fuse ]; then
    say "Note: /dev/fuse isn't accessible (common in containers), so the AppImage can't mount itself."
    say "Start it with APPIMAGE_EXTRACT_AND_RUN=1 set, or enable FUSE for this system."
  fi
}

need() { command -v "$1" >/dev/null || die "Need '$1' to install OpsTrellis. Install it and run this again."; }
# True if dotted version $1 >= $2.
version_ge() { [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -1)" = "$2" ]; }
say() { printf '%s\n' "$*"; }
die() { printf 'Error: %s\n' "$*" >&2; exit 1; }
cleanup() {
  # Restore a moved-aside app on errors/signals. Never delete the only remaining copy.
  if [ -n "$staging" ] && [ -e "$staging/OpsTrellis.app.old" ] && [ ! -e "$app" ]; then
    if ! mv "$staging/OpsTrellis.app.old" "$app"; then
      printf 'Error: Restore the previous app manually from %s\n' "$staging/OpsTrellis.app.old" >&2
      staging=""
    fi
  fi
  [ -z "$tmp" ] || rm -rf "$tmp"
  [ -z "$staging" ] || rm -rf "$staging"
  [ -z "$launcher_staging" ] || rm -rf "$launcher_staging"
  [ -z "$lock" ] || rmdir "$lock"
}

main "$@"
