# Contributing

Thank you for your interest in the Speech Output Engine. Contributions are
welcome, but please read this first.

## A personal project

This is a personal project that I maintain in my spare time. I read issues and
pull requests when I can, but I can't promise to reply to them, review them or
merge them, or to do any of that quickly. Please don't take silence as a
judgement on your work.

If you'd like to make a large change, open an issue to describe it before you
write the code. That way you won't spend time on something that doesn't fit
the project.

## Security issues

Please don't report security problems in a public issue. Use GitHub's
[private vulnerability reporting](https://github.com/mediaswing/speechout/security/advisories/new)
instead, from the **Security** tab of the repository. Only the maintainer can
see a private report.

## Bug reports

A good bug report says:

- which version of the app you're using, and on which operating system
- which screen reader you use, if any, and its version
- what you did, what you expected, and what happened instead

The debug log (`speechout.log`, in the folder chosen on the Settings tab) often
helps. It covers only the current session; if the problem happened before you
restarted the app, attach `speechout.previous.log` instead. It never records API
keys or the text being read, but please check it before attaching it.

## Design principles

Please keep these in mind for any change. A change that goes against them is
unlikely to be merged, however useful it is.

### 1. Accessibility comes first

The app is for people who rely on a keyboard, a screen reader or both, so every
feature must work for them from the start, not be added later.

- **Everything works from the keyboard.** Every control can be reached with
  Tab and Shift+Tab and used with Space or Enter. New shortcuts must not clash
  with common screen reader keys, and they belong in the keyboard table in the
  README.
- **Everything is exposed to screen readers.** Each control has a visible label
  that's linked to it, headings are marked as headings, and the status line is
  a polite live region. Use the helpers in `src/app.rs`, such as the labelled
  dropdown and the heading widget, rather than plain egui widgets.
- **The layout stays simple.** Each control is on its own full-width line, in a
  sensible reading order. The focus ring must stay thick and clearly visible in
  light and dark mode.
- **The user is kept informed.** Progress, results and errors are reported in
  the status line, in words that make sense when spoken aloud.

If you can, try your change with a screen reader (Narrator or NVDA on Windows,
VoiceOver on macOS, Orca on Linux) and say in the pull request which one you
used.

### 2. Privacy by default

- Documents and photos stay on the user's computer unless they've chosen a
  service that needs them. Photos go only to the local Ollama model.
- Send as little as possible over the network. For example, photo coordinates
  are rounded before a place name is looked up.
- Every network request must be something the user can understand and, where it
  isn't essential, turn off. Describe any new one in the README's Privacy
  section.
- API keys are only sent to the service they belong to. Neither API keys nor the
  text being read may ever be written to the debug log.

### 3. The window never freezes

Anything slow, such as network requests, speech synthesis or reading a file,
runs on a background thread (see `src/worker.rs`) and reports back to the
window through a message. A frozen window looks like a crash to someone using a
screen reader.

### 4. Each platform is self-contained

Windows, macOS and Linux code lives in `src/platform/`, one file per operating
system, and each file provides the same set of functions (listed at the top of
`src/platform/mod.rs`). The rest of the app shouldn't need to know which
platform it's running on. The app packages itself (`--package-macos`,
`--package-deb`), so please don't add shell scripts for building or packaging.

### 5. Plain language

Status messages, labels, the README and the changelog are written for the
people who use the app, in plain British English. Prefer short sentences and
everyday words, and avoid jargon. The same goes for code comments.

Every label and message the app shows lives in the `ENGLISH` list in
`src/i18n.rs`, under a key, so that it can be translated. Use `t("key")` or
`tf("key", &[("name", &value)])` rather than writing the text in place, and
write values as `{name}` rather than building sentences from pieces, since
word order differs between languages. A test checks that every key used in
the code has English text.

### 6. Few dependencies

Add a new crate only when it's clearly worth it, and turn off the features you
don't need. Smaller dependency trees build faster on all three platforms and
are easier to keep secure.

## Making a change

1. Fork the repository and create a branch for your change.
2. Build and test it:

   ```
   cargo build
   cargo test
   ```

   Tests that use the real speech synthesiser are skipped by default; run them
   with `cargo test -- --ignored`. See the README for the Ubuntu build
   dependencies, and for building HEIC support into the Windows app with the
   `bundled-heif` feature.
3. Add tests for new behaviour where you can, especially in code that doesn't
   depend on the window, such as wordlists and document reading.
4. If the change affects people who use the app, add a line to the
   `[Unreleased]` section of `CHANGELOG.md`. Write it for them, not for
   developers.
5. Update the README if you've changed what the app does, its keyboard
   shortcuts or what it sends over the network.
6. Open a pull request that explains what the change does and why, and how you
   tested it.

Keep each pull request to one change. Small pull requests are much easier to
review.

## Wordlists

New or improved example wordlists are welcome. They're in
[`wordlists/`](wordlists), and the format is described in the README. Please
explain where an entry's pronunciation comes from if it isn't obvious.

## Licence

The Speech Output Engine is released under the
[GNU General Public License, version 3 or later](LICENSE). By contributing, you
agree that your contribution will be released under the same licence.
