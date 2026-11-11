#!/bin/sh
# Renders every `spoken` sentence in sentences.tsv to <out>/<voice>-<nn>.wav (16 kHz mono 16-bit)
# with the macOS `say` command. Usage: synthesize.sh [OUT_DIR]   (default: a new temp directory)
# Voices can be overridden: VOICES="Samantha Daniel" synthesize.sh out
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tsv="${TSV:-$here/sentences.tsv}"
voices="${VOICES:-Samantha Daniel Karen}"
out="${1:-$(mktemp -d "${TMPDIR:-/tmp}/opstrellis-dev-speech.XXXXXX")}"
mkdir -p "$out"

command -v say >/dev/null 2>&1 || { echo "say not found: this script needs macOS" >&2; exit 1; }
installed=$(say -v '?')
tab=$(printf '\t')

for voice in $voices; do
  if ! printf '%s\n' "$installed" | grep -q "^$voice .*[a-z][a-z]_[A-Z][A-Z] "; then
    echo "skipping $voice: voice is not installed" >&2
    continue
  fi
  n=0
  # The first line is the header; tab is the only separator, so quotes in a sentence are harmless.
  while IFS="$tab" read -r spoken _expected _terms; do
    n=$((n + 1))
    [ "$n" -eq 1 ] && continue
    file=$(printf '%s/%s-%02d.wav' "$out" "$voice" $((n - 1)))
    say -v "$voice" -o "$file" --data-format=LEI16@16000 "$spoken"
  done < "$tsv"
  echo "$voice: $((n - 1)) clips"
done
echo "wrote WAVs to $out"
