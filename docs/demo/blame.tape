# dkod demo: per-line agent attribution (`dkod blame`) + the drift card.
#
# Run from the repo root:
#   vhs docs/demo/blame.tape
#
# Requirements:
#   * charmbracelet vhs (`brew install vhs` or `nix shell nixpkgs#vhs`)
#   * a dkod binary at target/release/dkod or target/debug/dkod
#
# The hidden setup builds a throwaway demo repo (docs/demo/setup-demo-repo.sh)
# with one human commit, one clean AI session, and one drifting AI session
# whose id is fixed (0196f8e2-2222-7000-8000-0000000000d2).

Output docs/demo/dkod-demo.gif

Set Shell "bash"
Set FontSize 14
Set Width 780
Set Height 460
Set Padding 14
Set Theme "Catppuccin Mocha"
Set TypingSpeed 35ms

Hide
Type `DKOD_BIN="$PWD/target/release"; [ -x "$DKOD_BIN/dkod" ] || DKOD_BIN="$PWD/target/debug"`
Enter
Type `export PATH="$DKOD_BIN:$PATH"`
Enter
Type `DEMO="$(./docs/demo/setup-demo-repo.sh 2>/dev/null | tail -1)"`
Enter
# Fresh HOME keeps local dkod setup-state warnings out of the recording.
Type `export HOME="$(mktemp -d)"`
Enter
Type `cd "$DEMO"`
Enter
Type "clear"
Enter
Show

Type "dkod blame src/server.js"
Sleep 500ms
Enter
Sleep 6s

Type "clear"
Enter
Type "dkod drift 0196f8e2-2222-7000-8000-0000000000d2 --card"
Sleep 500ms
Enter
Sleep 9s
