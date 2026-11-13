# Security policy

## Reporting a vulnerability

Please **don't open a public issue** for security problems. Report them privately through
[GitHub's private vulnerability reporting](https://github.com/mtr1920/opstrellis/security/advisories/new).

Include what's affected (desktop app, installer or server), the version, and steps to reproduce.
You'll get a reply as soon as possible, and credit in the release notes if you'd like.

## Supported versions

Security fixes go into the latest release. Update from Settings → About, or with the install command
in the [README](README.md#install).

## How OpsTrellis protects you

- Transcription runs offline by default; audio is never stored.
- API keys live in the macOS Keychain or the Linux Secret Service and are only sent to the
  provider you chose.
- Releases are checked against SHA-256 checksums by the installer, and the offline AI model is
  pinned to one version and verified before it's loaded.
- In-app updates are signed: the app installs one only if its signature matches the public key
  built into the app.
- Logs contain timings and errors, never what you said or typed.
