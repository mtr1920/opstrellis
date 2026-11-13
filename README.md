<p align="center">
  <img src="desktop/src-tauri/icons/128x128@2x.png" width="96" alt="OpsTrellis icon">
</p>

<h1 align="center">OpsTrellis</h1>

<p align="center">
  <b>Voice dictation for macOS and Linux.</b><br>
  Hold a key, speak, let go: clean, formatted text appears in any app. Offline by default.
</p>

<p align="center">
  <a href="https://github.com/mtr1920/opstrellis/releases/latest"><img src="https://img.shields.io/github/v/release/mtr1920/opstrellis?label=release" alt="Latest release"></a>
  <a href="https://github.com/mtr1920/opstrellis/actions/workflows/ci.yml"><img src="https://github.com/mtr1920/opstrellis/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/mtr1920/opstrellis/releases"><img src="https://img.shields.io/github/downloads/mtr1920/opstrellis/total?label=downloads" alt="Downloads"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-blue" alt="macOS | Linux">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-green" alt="MIT license"></a>
</p>

<p align="center">
  <a href="https://mtr1920.github.io/opstrellis/">Website</a> ·
  <a href="#install">Install</a> ·
  <a href="#first-five-minutes">Get started</a> ·
  <a href="#features">Features</a> ·
  <a href="#privacy">Privacy</a> ·
  <a href="#license">License</a>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="site/img/pill-dark.png">
    <img src="site/img/pill-light.png" width="560" alt="The OpsTrellis recording pill showing live text while dictating">
  </picture>
</p>

- **Works in every app:** your editor, a terminal, a browser, email, chat or an AI assistant, with
  live text in a small recording pill while you speak.
- **Private by default:** transcribes offline on your computer out of the box. Cloud providers
  (OpenAI, Deepgram, AssemblyAI and more) are optional and use your own keys.
- **Writes like you would:** removes "um"s, follows voice commands, keeps code names and paths
  intact, and adapts its tone per app. Optional AI polish, offline too, also fixes grammar and
  self-corrections.
- **Built for developers:** knows `kubectl` from "cube control", writes `main.rs` when you say
  "main dot rs", and cleans up prompts for ChatGPT, Claude, Cursor or Claude Code. Add your
  project folders and it also knows your own code: their names are hinted to the transcriber and
  spelled right by AI polish, and "at sign pipeline dot rs" becomes `@src/pipeline.rs` in AI prompts.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="site/img/home-dark.png">
    <img src="site/img/home-light.png" width="49%" alt="Home: shortcut reminder, dictation stats and searchable history">
  </picture>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="site/img/settings-dark.png">
    <img src="site/img/settings-light.png" width="49%" alt="Settings: appearance, recording pill, sounds and shortcuts">
  </picture>
</p>

Free and open source under the [MIT License](#license).
If OpsTrellis saves you some typing, a ⭐ helps others find it.

## Install

macOS (Apple silicon, macOS 11 or later) or Linux (x86_64, glibc 2.35 or later, e.g. Ubuntu
22.04+):

```sh
curl -fsSL https://raw.githubusercontent.com/mtr1920/opstrellis/main/install.sh | bash
```

That's it. Run the same command again any time to update; your settings, history, models and keys
are kept. To check the installed version, run `opstrellis --version` or open Settings → About.

<details>
<summary>What the installer does, and other options</summary>

- macOS: installs `OpsTrellis.app` into `/Applications`. Linux: installs the AppImage into
  `~/.local/share/opstrellis` with an app-menu entry.
- Adds an `opstrellis` command in `~/.local/bin`.
- Downloads the latest [release](https://github.com/mtr1920/opstrellis/releases) and checks it
  against the release's SHA-256 checksums before installing anything.
- `OPSTRELLIS_VERSION=v0.2.0` installs a specific release.
- You can also download the `.dmg` (macOS) or `.deb` / AppImage (Linux) from the
  [releases page](https://github.com/mtr1920/opstrellis/releases) yourself.

</details>

## First five minutes

1. **Open OpsTrellis.** Onboarding suggests the offline *Moonshine Tiny* model (about 100 MB), so
   you can dictate without any account or API key.
2. **Allow permissions (macOS).** Microphone; **Accessibility** to paste into other apps; and
   **Input Monitoring** only if you use a single key such as 🌐 fn as your shortcut. You can
   check them any time in Settings → Permissions.
3. **Dictate.** Click into any text field, **hold `Ctrl+Shift+Space`**, speak, and let go.

> **macOS note:** the app isn't signed with an Apple Developer ID yet. After an update, macOS may
> ask for Accessibility and Input Monitoring again. If pasting stops working, remove OpsTrellis in
> System Settings → Privacy & Security → Accessibility and add it again.

## Everyday use

| Shortcut (default) | What it does |
|---|---|
| Hold **Ctrl+Shift+Space** | Talk while held; the text is inserted when you let go. |
| Tap **Ctrl+Shift+Space** | Hands-free: keeps listening until you tap again. |
| **Esc** | Cancels the current recording. |
| Hold **Ctrl+Alt+Space** | Command mode: select text and say how to change it ("make this more formal"). |
| **Enter** / **Esc** | While a command-mode rewrite is shown for review: insert it, or discard it. |
| **Ctrl+Alt+V** | Pastes your last transcript again. |

- **Use a single key (macOS):** in Settings → Shortcuts, set dictation to **🌐 fn**, Right ⌘,
  Right ⌥, Right ⌃ or Right ⇧. Hold it to talk; double-tap for hands-free and press again to
  finish. With 🌐 fn, macOS's own 🌐 action is switched off while it's your shortcut and restored
  afterwards.
- **Always-on fallback:** if you change the dictation shortcut, **Ctrl+Shift+Space** keeps working
  as a backup. Press it to start, press it again to finish.
- **Light or dark:** Settings → General → **Appearance** (System, Light or Dark), or the sun/moon
  button at the bottom of the sidebar. The recording pill follows it.
- **In the app:** ⌘1–⌘5 open the sidebar pages, ⌘, opens Settings, ⌘F searches the current page
  (Ctrl on Linux). Esc closes dialogs.
- **Menu bar only:** turn off Settings → General → **Show in Dock** and OpsTrellis disappears from
  the Dock and ⌘Tab (Linux: the dock and taskbar). It keeps running and opens from its menu bar /
  tray icon.
- **Voice commands while dictating:** "new line", "new paragraph", "scratch that", "press enter".
- **Wayland:** apps can't grab keys there, so OpsTrellis asks your desktop for the shortcuts
  instead. See [Linux on Wayland](#linux-on-wayland).

### Linux on Wayland

- **Shortcuts:** on GNOME 48 or later and KDE Plasma, OpsTrellis asks the desktop for its shortcuts
  when it starts. Click **Add** in the desktop's dialog; hold-to-talk then works as usual. Change
  the keys in GNOME Settings → Apps → OpsTrellis (KDE: System Settings → Shortcuts). GNOME needs
  the app-menu entry that `install.sh` (or the .deb) adds to allow this.
- **Older GNOME** (Ubuntu 24.04 and earlier): Settings → Shortcuts → **Add shortcuts** creates GNOME
  custom shortcuts that run `opstrellis --toggle`, `--command` and `--paste-last`. They start and
  finish with a press each; holding to talk isn't possible that way.
- **Other desktops:** bind `opstrellis --toggle`, `--command`, `--paste-last` and `--cancel` to keys
  in your desktop's keyboard settings.
- **Pasting** needs a tool that sends keys. GNOME doesn't support `wtype`, so it needs `ydotool`,
  whose service needs access to `/dev/uinput`:

  ```sh
  sudo apt install ydotool
  sudo usermod -aG input $USER
  # then restart the computer: logging out isn't enough, because the user session keeps running
  ```

  On wlroots desktops (Sway, Hyprland) and KDE, `sudo apt install wtype` is enough. Settings →
  Permissions → **Text insertion** says what's missing.

## Features

<details open>
<summary><b>Dictation</b></summary>

| | |
|---|---|
| System-wide dictation | Works in any app: push-to-talk or hands-free (hands-free stops after 2 minutes of silence or at 30 minutes). A shortcut pastes the last transcript, also with history off. |
| Recording pill | Shows when the mic is actually listening (the start sound plays then, so no words are lost on slow Bluetooth mics), a live waveform, the stage (transcribing, polishing), a timer in hands-free mode, and how it ended: "Inserted · 0.6 s", or what went wrong and where the text went. |
| Insertion | Pastes through the clipboard, then restores your clipboard. Uses Ctrl+Shift+V in terminals and `wtype`/`ydotool` on Wayland, or can type instead of paste. If you switch windows while it's processing, the text goes to your clipboard with a notice instead of into the wrong window. If insertion fails, the text is left on the clipboard. |
| Real-time | Streams audio to Deepgram, AssemblyAI or OpenAI Realtime while you speak, with live text in the pill. Local models show live text too. The final text is inserted once, on release. |
| Cleanup | Removes fillers, handles voice commands, applies your dictionary (`nest dev → feature/nest_dev`) and snippets. Dictionary entries keep their exact spelling through capitalization, tone and AI (`kubectl` stays lowercase at a sentence start). Spoken symbols become characters where they're clearly code: file and domain names everywhere (`main dot rs` → `main.rs`), identifiers, paths and flags in editors, terminals and AI prompts (`src slash app`, `user underscore id`, `dash dash watch`), never next to ordinary words ("the dot product"). |
| AI polish | Optional: grammar, self-corrections ("2pm, actually 3pm"), lists, while keeping code identifiers intact in editors and terminals. Use any OpenAI-compatible endpoint, Anthropic, OpsTrellis Cloud, or the offline model below. Your dictionary is respected: cloud models are told your terms, and if any AI (cloud or offline) rewords or drops a dictionary word you said, your plain text is inserted instead. |
| Offline AI polish | Models that run on your computer with llama.cpp, downloaded on demand (pinned and checksum-verified). **Spokenly Cleanup 1** (639 MB, 7 languages) and **S1-mini by Superwhisper** (484 MB, English) only clean up dictation, fast; **Qwen3.5 4B** (2.7 GB) follows instructions, so auto actions such as Prompt engineer, command mode, translation and meeting summaries work offline too, alone or paired with a cleanup model. Code editors and terminals keep the rule-based text with a cleanup model. If a model is slow (10 s cap) or returns something implausible, the rule-based text is inserted and the pill says why. |
| Style per app | Personal, work, email, AI prompts and other categories, each formal, casual, very casual or excited, with optional "how you write here" notes for AI polish. The category comes from the focused app and window title; Style → Apps lets you correct it. |
| AI prompts | Dictating to ChatGPT, Claude, Cursor or an AI agent in a terminal (Claude Code, Codex, Aider, Gemini CLI) gets prompt cleanup: instructions kept, file names, paths and flags written as code, and optional light structure (Goal, Context, Constraints) for long prompts. |
| Auto action | Optional, per app category: run an action such as "Polish" or "Make formal" on every dictation before it's inserted. Off by default; never in a plain terminal shell. |
| Command mode | Select text, hold the command shortcut and say what to do, or name a saved action (Library → Actions). With nothing selected, it writes new text; a built-in action named alone ("polish") asks you to select the text first. Actions can copy their result instead: select a message and say "reply saying Thursday works" or "summarize". By default, before a rewrite of long text (over 280 characters) replaces your selection, the pill shows it: **Enter** inserts it, **Esc** discards it (Paste last still inserts it); Settings → AI polish → Review AI rewrites (only long text, always or never; not on Wayland). |
| Snippets | A spoken trigger expands to saved text, with `{date}`, `{weekday}`, `{time}` and `{clipboard}` placeholders. |
| Auto-learn dictionary | Optional: correct a dictation in History and misheard words become dictionary entries (`Karl → Karol`). Changes that don't sound like mis-hearings are learned only when you make them twice; common words are never turned into replacements; Undo restores the dictionary. |
| Tech terms and term packs | About 330 developer and workplace terms (`kubectl`, `PostgreSQL`, `Next.js`, `Jira`, `OKR`, `Claude Code`, `Codex`). Common mis-hearings are fixed (`cube control → kubectl`); your own dictionary and snippets always win. Optional packs for data and ML, product and design, and finance, or import your own (Style → Term packs, with a preview of what it replaces). A pack's terms are sent as hints, so the transcriber prefers their spellings: a built-in pack's even before you say them, an imported pack's once you have. The tech pack's distinctive product names (Claude, Codex, Gemini, DeepSeek, Supabase, Vercel…) are hints from the first dictation, and local Parakeet models are steered toward hints as they decode. [Contribute a pack](CONTRIBUTING.md#contributing-a-term-pack). |
| Workspaces (repo-aware dictation) | Add project folders (Settings → Workspaces). In code editors, terminals and AI prompts, the workspace named in the window title (or the only one you added; the pill shows it) helps in three ways: up to 40 of its file and code names go to your transcription provider as hints; with AI polish (not the offline model) the code names you said are listed for the model, which spells them exactly only where you mean that code (`retranscribe_history`) and leaves ordinary phrases alone; and in AI prompts "at sign pipeline dot rs" or "@pipeline.rs" becomes `@desktop/src-tauri/src/pipeline.rs` when exactly one file has that name. Optional, off by default: write names said as words as code without AI ("user session store" → `UserSessionStore`), in editors and terminals only. That rule can't tell a name from a phrase, so it also rewrites ordinary words that match a name in your code ("plain text" → `plain_text`); your dictionary and snippets always win. Indexing reads folders directly and never runs git or anything else: it skips hidden, dependency and build folders and what simple `.gitignore` rules exclude (names, `*` globs, `/anchored`, `**/name`; negations and other patterns are skipped), keeps at most 20 000 files and only file names and the type, function and constant names declared in source files (not local variables or tests), never contents. File names with spaces or punctuation are never sent or used. Re-indexed in the background when 10 minutes old. |
| Notetaker | Records meetings from the mic, optionally plus system audio (PulseAudio monitor or BlackHole) transcribed on its own, so the transcript reads `Me:` / `Them:`. Transcribes while recording and saves a note with an AI summary (General, Sales call, Interview, 1:1, Standup). Then drafts a follow-up email or a CRM note, or copies the action items. |
| History and Insights | Search and correct past dictations, compare the original transcript with the polished text, polish again with AI. If every speech provider fails, the recording is kept so you can retry it from History or the tray; optionally recordings are kept on disk for about a day so any recent dictation can be transcribed again. Insights shows words, WPM, fixes, streaks, a heatmap and app categories. |
| Multilingual | Auto-detect or a fixed language. Multilingual local models: Whisper (99 languages), Parakeet v3 (25 European languages) and SenseVoice. |
| Speak one language, write another | Optional, per app category (Style → Write in): speak Hindi, Hinglish or any mix and AI polish writes English, or any of 27 languages. Names, code, URLs and your dictionary terms aren't translated; History keeps what you said. Needs a cloud or OpenAI-compatible AI provider; never in a plain terminal shell. If the AI is unavailable, the untranslated text is inserted and the pill says so. |

</details>

<details>
<summary><b>Speech-to-text and AI providers</b></summary>

| Speech-to-text provider | Batch | Streaming |
|---|---|---|
| Local models (sherpa-onnx): Moonshine, Parakeet, Whisper, SenseVoice | Yes | Live text |
| OpenAI and any OpenAI-compatible endpoint (Groq, Fireworks, speaches, whisper.cpp, vLLM) | Yes | Yes |
| Deepgram | Yes | Yes |
| AssemblyAI | Yes | Yes |
| Google Cloud Speech | Yes | No |
| Azure Speech | Yes | No |
| ElevenLabs Scribe | Yes | No |
| OpsTrellis Cloud | Yes | No |

- Providers form an ordered **fallback chain**: if one fails, the next one takes over, and a
  failed live stream falls back to sending the recording in one piece.
- Your dictionary is passed to each provider as vocabulary hints.
- **AI polish / command mode:** OpenAI, Anthropic, Google Gemini, Groq, OpenRouter, Mistral,
  DeepSeek, xAI, Together AI, Fireworks, Cerebras, Ollama, LM Studio, any other OpenAI-compatible
  endpoint, OpsTrellis Cloud, or a model on your computer. Each service keeps its own saved key.
- API keys are stored in the macOS Keychain or the Linux Secret Service.

</details>

## Use OpsTrellis from your AI agent

AI coding agents such as Claude Code or Cursor can run OpsTrellis as an
[MCP](https://modelcontextprotocol.io) server, so you can ask "what did I dictate about the deploy
this morning?" or "use the action items from today's standup". It is off until you turn it on in
**Settings → Agents**, where you also choose what agents can use.

**Set up.** For Claude Code:

```sh
claude mcp add opstrellis -- opstrellis mcp
```

Other agents: add an MCP server with `{ "command": "opstrellis", "args": ["mcp"] }`. `opstrellis`
is the command `install.sh` creates. Installed from the `.dmg`? Use
`/Applications/OpsTrellis.app/Contents/MacOS/opstrellis` instead. The `.deb` puts `opstrellis` in
`/usr/bin`; a downloaded AppImage works by its full path. OpsTrellis doesn't need to be running.

**Tools** (each one only while its switch is on):

| Tool | Switch | What it does |
|---|---|---|
| `search_dictations` | Dictation history | Your recent successful dictations, newest first (`query`, `limit` up to 50) |
| `get_last_dictation` | Dictation history | Your last dictation |
| `list_dictionary` | Dictation history | Your dictionary words and replacements |
| `list_notes` | Notes | Your notes and meeting notes (`kind`: `note` or `meeting`) |
| `get_note` | Notes | One note; a meeting note with its summary and transcript (cut at 100,000 characters) |
| `add_dictionary_term` | Add dictionary words (off by default) | Adds a technical term or name as a spelling hint, only if some word in it mixes capital and small letters after its first letter (gRPC, PostgreSQL), has a digit (k8s), has + or # after its first character (C++) or . _ / between letters or digits (Node.js). Ordinary words, all-capital words and plain Titlecase names (Meeting, EMAIL, Kubernetes) are refused, as are words you already have in another capitalization and single words under 3 characters. Agents can't add replacements or change or delete entries; added words show an **Agent** badge in Library → Dictionary |
| `ask_user` | Questions by voice (off by default) | Shows the whole question in a card above your screen's bottom edge ("Agent asks") and waits for your answer: 60 seconds for you to start, longer while you answer, 5 minutes at most |

The same data is available in a terminal: `opstrellis last`, `opstrellis history [--search <text>]
[--limit <n>]`, `opstrellis notes [--kind note|meeting]`, `opstrellis note <id>` and
`opstrellis ask "<question>"`, as plain text or with `--json`. `opstrellis --help` lists every command.

**Answering an agent by voice.** OpsTrellis never starts recording because an agent asked. The
question waits in a card where the pill appears; to answer, dictate with your usual shortcut once you have had the question
on screen for at least 2 seconds. A question that arrives while you are dictating shows up when you are
done, and only counts from then. While you speak the pill says **Answering the agent**, and your words go
to the agent instead of into the app in front; they are cleaned up like an AI prompt and saved in
History as "Answer to an agent". A command, or a dictation started sooner, works as usual. Esc while
recording keeps the question; to decline it, use the tray's **Decline agent question** or
`opstrellis --cancel`. Unanswered after 60 seconds, the agent is told you didn't answer; an answer you
are still speaking or that is being processed holds the question, and after a cancelled or empty attempt
you get at least 15 more seconds (5 minutes at most in all). Turning voice questions, agent access or the
pill off withdraws a waiting question, and an answer is only sent while they are on. OpsTrellis must be
running, with the recording pill on.

**Privacy.** Nothing leaves your computer through OpsTrellis: the agent reads your data and may send
it to its own AI provider, as with anything else you show it. Turning a switch off applies to the
agent's next request; after turning one on, restart the agent so it sees the new tools. With History
off, agents get no dictations. Every tool tells the agent that what it returns is your content, not
instructions, but meeting transcripts include other people's words, so keep an eye on what your agent
does with them. The switches are your consent for what agents get through OpsTrellis, not a lock:
other software running as you can read OpsTrellis's files directly.

## Privacy

- Out of the box, transcription runs **on your computer**; nothing is sent anywhere.
- Your API keys never leave your device except in requests to the provider you chose.
- The microphone is open only while recording. By default recordings are never written to disk;
  the last few whose transcription failed stay in memory until you retry them or quit. Keeping recordings on
  disk (Settings → Privacy → Keep recordings: failed ones or all) is opt-in, local only and needs
  History. A kept file is deleted within an hour of turning 24 hours old while OpsTrellis runs, or
  at its next launch, and at once when you choose Never or turn History off.
- History can be kept forever, 30 days, 7 days, or turned off.
- Workspaces (off until you add a folder) read only file names and the names declared in source
  files, keep them in memory, never run a program in the folder, and never send file contents
  anywhere. With a cloud transcription or AI provider, up to 40 of those names (and the code names
  you say, at most 30) go to it with each dictation, like your dictionary.
- AI agents get nothing from OpsTrellis unless you turn on Settings → Agents
  ([details](#use-opstrellis-from-your-ai-agent)).
- The log (Settings → About → Show logs) has timings and errors, never what you said or typed.
- Optional usage sync to OpsTrellis Cloud sends counts and durations only, never text, and can be
  turned off.
- Optional update check (off by default, Settings → About): once a day OpsTrellis asks github.com
  for the latest version number. The request carries no account, device or usage data (GitHub sees
  your IP address, as with any request), and nothing is installed automatically.

## OpsTrellis Cloud (optional, self-hosted)

`server/` is a server you can run yourself to give a team one managed gateway:

- An OpenAI-compatible speech-to-text and LLM gateway (`/v1/audio/transcriptions`,
  `/v1/chat/completions`) with quotas, metering, a provider fallback chain and a self-hosted
  local model.
- Accounts, device sign-in from the desktop app, and API tokens.
- Usage and cost tracking, Stripe subscriptions with metered overage, admin pages and a web
  dashboard.
- Teams with a shared library: dictionary entries, snippets and voice actions that team admins edit
  on the dashboard. Every member's desktop app syncs it down read-only, and their own entries always
  win over the team's (**Settings → Account → Team library**).

```sh
cd server
cp .env.example .env          # set MASTER_KEY, provider keys, optional Stripe keys
docker compose up --build     # Postgres + server on :8787
```

In the desktop app, open **Settings → Account**, set the server URL (https://, or http:// only on
your own computer or local network) and sign in, then add
"OpsTrellis Cloud" as a transcription or AI provider. Full details:
[server/README.md](server/README.md).

## Troubleshooting

| Problem | Fix |
|---|---|
| Text isn't pasted; it ends up on the clipboard | Grant **Accessibility** (macOS). After an update, remove OpsTrellis from the list and add it again. |
| 🌐 fn or another single-key shortcut does nothing | Grant **Input Monitoring**, then quit and reopen OpsTrellis. **Ctrl+Shift+Space** works meanwhile. |
| "You switched to …", text on the clipboard | The focused window changed while processing, so OpsTrellis didn't paste into the wrong place. Paste with ⌘V / Ctrl+V. |
| Slow or wrong results | Settings → Transcription: try another model or provider. Settings → About → **Show logs** shows the time each step took. |
| Shortcuts don't work on Wayland | Settings → Shortcuts shows what your desktop supports and sets it up. See [Linux on Wayland](#linux-on-wayland). |
| Text isn't pasted on Wayland | Settings → Permissions → **Text insertion** says what's missing (usually `ydotool` and the `input` group, then a restart). |

Found a bug or have an idea? [Open an issue](https://github.com/mtr1920/opstrellis/issues).

## Development

```sh
# Linux build dependencies (Ubuntu/Debian)
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev \
  libasound2-dev libxdo-dev libssl-dev libsecret-1-dev cmake clang
# optional, for Wayland text insertion: wtype (wlroots/KDE) or ydotool (GNOME)
# macOS: Xcode command line tools and `brew install cmake`

cd desktop
npm install
npx tauri dev                          # run
npx tauri build --bundles app,dmg      # macOS app (Linux: --bundles deb,appimage)
```

- **Repository layout:** `desktop/` is the Tauri 2 app (Rust core for audio, shortcuts, providers,
  the text pipeline and insertion; React UI). `server/` is OpsTrellis Cloud. `docs/` has the
  [architecture](docs/ARCHITECTURE.md), [design system](docs/DESIGN.md),
  [desktop IPC contract](docs/DESKTOP_IPC.md), [testing](docs/TESTING.md),
  [releasing](docs/RELEASING.md) and the [roadmap](docs/PRODUCT_ROADMAP.md).
- **Tests:** `cd desktop/src-tauri && cargo test` (unit and integration tests, plus live tests that need a
  microphone or API keys), `python3 tests/test_installer.py` and `python3 tests/test_release.py`
  for the installer, and `cd server && npm test` for the server. End-to-end suites with real
  shortcuts, a virtual microphone and real insertion are described in
  [docs/TESTING.md](docs/TESTING.md).
  A reproducible developer-speech accuracy benchmark is in [docs/BENCHMARK.md](docs/BENCHMARK.md).

## Known limitations

- **Unsigned macOS app:** distributing a signed, notarized build needs an Apple Developer ID; until
  then macOS may ask for permissions again after each update.
- **Live cloud providers:** OpenAI, Deepgram, AssemblyAI, Google, Azure, ElevenLabs, Groq,
  Anthropic and Stripe are implemented against their current APIs and tested on request and
  response shapes; live tests run when keys are set ([docs/TESTING.md](docs/TESTING.md)).
- **Wayland:** hold-to-talk needs the desktop's shortcut portal (GNOME 48+, KDE Plasma); the pill may take focus on some
  compositors (it can be turned off in Settings), and some Wayland docks ignore **Show in Dock**.
- **Not yet:** learning from edits you make inside other apps after insertion, Windows and mobile
  apps. Plans are in
  [docs/PRODUCT_ROADMAP.md](docs/PRODUCT_ROADMAP.md).

## License

OpsTrellis is free and open source under the [MIT License](LICENSE). Use it, change it and share it
however you like.

---

Made by [Mrityunjay](https://github.com/mtr1920). Questions, ideas or bugs:
[open an issue](https://github.com/mtr1920/opstrellis/issues/new/choose).
