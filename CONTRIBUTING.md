# Contributing to OpsTrellis

Thanks for helping! Bug reports, ideas and pull requests are all welcome.

## Reporting bugs and ideas

- **Bugs:** [open a bug report](https://github.com/mtr1920/opstrellis/issues/new?template=bug_report.yml)
  with your version (Settings → About), OS and provider. Log lines (Settings → About → Show logs)
  help a lot; they contain timings and errors, never what you said.
- **Ideas:** [open a feature request](https://github.com/mtr1920/opstrellis/issues/new?template=feature_request.yml)
  describing the problem you want solved.
- **Security issues:** please report them privately; see [SECURITY.md](SECURITY.md).

## Pull requests

1. For anything larger than a small fix, open an issue first so we can agree on the approach.
2. Keep changes focused: one fix or feature per pull request.
3. Run the checks:
   ```sh
   cd desktop && npm run build                 # type-check and build the UI
   cd src-tauri && cargo test                  # Rust tests
   python3 ../../tests/test_installer.py       # if you touched install.sh
   ```
4. Describe what changed, why, and how you tested it (which OS, which provider).

Development setup is in the [README](README.md#development); architecture notes are in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Contributing a term pack

A term pack is a list of names for one field (medicine, law, a game engine…). With it turned on
(Style → Term packs), OpsTrellis replaces the mis-hearings the pack lists (its aliases) with the
term, and sends some of its terms to the transcriber and to AI polish as hints, which makes them
prefer those spellings. Built-in packs live in `desktop/src-tauri/terms/packs/<id>.json`
and are off by default. Anyone can also import a pack file without a new release (Style → Term packs →
Import pack…).

```json
{
  "id": "medical",
  "name": "Medical",
  "description": "Common drug names.",
  "terms": [
    { "term": "ibuprofen", "say": ["eye bew pro fen"], "aliases": ["eye be profen"] },
    { "term": "metformin" },
    { "term": "PRN", "say": ["P R N"], "techAliases": ["p r n"] }
  ]
}
```

- `id`: 1 to 40 lowercase letters, digits or hyphens, starting with a letter or digit (`tech` and the
  built-in ids are taken). It is also the file name.
- `name` up to 60 characters, `description` one sentence up to 200. Describe only what the pack
  holds.
- `terms`: at most 2000, **most important first**. Up to 30 terms in all are hints: first the ones
  the user says most, then, for a built-in pack, its first terms. An imported pack's terms are hints
  only once the user has said them.
- `term`: the spelling an alias is replaced with, up to 60 characters. A term with no aliases is
  fine: it can still become a hint, and how often it is said is counted.
- `aliases`: what a speech model writes instead of the term, replaced everywhere, prose included.
  `techAliases`: replaced only in code editors and terminals. At most 10 of each, up to 60 characters.
- `say`: how the term is spoken, used only by the alias miner below.

### Safety rules

An alias rewrites what the user said, so a wrong one silently breaks ordinary sentences, and a term
can end up in the transcriber's and the AI's prompt. These rules are checked by a test for built-in
packs, and an imported pack has every entry that breaks one dropped (the app says how many):

- A term is not one of the few hundred very common words in `learn::common` ("the", "make"…).
  Terms can still be ordinary words (Notion, Excel, Stripe); with the pack on, the transcriber then
  prefers that spelling, which is one reason packs are off by default.
- An alias is not itself a term, and no alias belongs to two terms, so a fix is never ambiguous.
- An `aliases` entry is not made only of words in `learn::common`: "type script" would rewrite prose,
  so it can only be a `techAliases` entry. Passing this check doesn't make an alias safe; leave out
  any phrase that could appear in an ordinary sentence.
- A `techAliases` entry has at least two words: a single dictionary word is too likely to be meant.
- A term is a name, not an instruction: letters, digits, spaces and `. + # & / - _ '`, at most four
  words.
- An alias sounds like its term (`learn::sounds_alike`), as a mis-hearing does: "approve" can never
  be an alias of "decline".

An alias next to a hyphen joined to another word ("co-op ex-president") is left alone in prose, and
next to any hyphen in code editors and terminals. Before an imported pack is saved, the app lists the
replacements it would make and asks the user to confirm.

Add an alias only when you are confident a speech model writes it (best: you have seen one do so)
and it is not a phrase anyone would say on purpose. A built-in pack must not repeat a term or alias
of `tech.json` or another built-in pack; between packs the earlier one wins (tech, then built-in packs, then imported ones by id) and
the user's own dictionary and snippets always win.

Mis-hearings can be mined from real models with `desktop/tools/mine_aliases.py`, which synthesizes
each `say` with several TTS voices, transcribes it with the app's local models and writes candidates
for review (nothing is applied automatically):

```sh
python3 desktop/tools/mine_aliases.py --terms desktop/src-tauri/terms/packs/<id>.json --tts DIR --models DIR --out aliases.json
```

The aliases of `tech.json` were mined this way. Those in the `data-ml`, `product-design` and `finance`
packs were not: they were written by hand and kept to a few likely ones, so mined corrections are
welcome.

To add a built-in pack, put the file in `desktop/src-tauri/terms/packs/`, add it to `BUILTIN` in
`desktop/src-tauri/src/terms.rs` (its position there is its precedence) and run the checks:

```sh
cd desktop/src-tauri && cargo test terms::   # includes pack_is_consistent, over every built-in pack
```

## License of contributions

OpsTrellis is licensed under the [MIT License](LICENSE). By submitting a contribution you confirm
that you wrote it (or have the right to submit it) and agree that it is licensed under the same
MIT License.
