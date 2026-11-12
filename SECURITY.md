# Security Policy

## Reporting a vulnerability

**Please don't report security problems in public issues or pull requests.**

Report them privately through the repository's
[**Security** tab](https://github.com/mediaswing/speechout/security), by choosing
**Report a vulnerability**. Please include your version and operating system, what
the problem is, and what an attacker could do with it.

Security fixes go into the latest release only.

## How your data is protected

**API keys are stored unencrypted** in the SQLite database, by design, so that the
database can be moved between computers and read by other tools. The database is
only protected by the permissions on it:

- On macOS and Linux, the file is readable only by you, and its folder only opens
  for you.
- On Windows, it is in `%APPDATA%`, which is private to your account by default.

So anyone using your account, any program running as you, and any backup that
copies the file can read your keys. Use keys with spending limits where the service
offers them, and remove them when you no longer need them. Removed keys are
overwritten in the database.

## What the app assumes about files

Documents, images and wordlists can come from anyone, so they are treated as
untrusted:

- Sizes are limited before files are read or unpacked, including what a DOCX can
  expand to and how large an image can claim to be.
- Wordlists containing a DOCTYPE are refused, which rules out entity expansion, and
  matches are plain text, so they can't be slow regular expressions.
- Names shown from wordlists, cloud services and models have control characters and
  text-direction overrides removed, so they can't disguise themselves on screen or
  to a screen reader.
- Text can't control the voices: macOS's embedded speech commands and eSpeak NG's
  phoneme input are neutralised, and nothing is interpreted as SSML.
- A panic while parsing a PDF is contained, and reported as a damaged file. A PDF
  built to exhaust the stack could still close the app, though it couldn't do more.
- If a wordlist that is turned on can't be used, reading stops with a message,
  rather than going ahead with the original words.
- Other programs are started directly and never through a shell, so a file's name
  can't be run as a command. Text goes through standard input, so it isn't visible
  in the process list.

A description is only as trustworthy as the model that wrote it, and a photo with
text in it can influence what the model says. Descriptions are only ever shown and
spoken, and the model has no way to act on anything.

## Network

- Cloud requests only use HTTPS and don't follow redirects, so a key can't be sent
  to a different host. A key only goes to its own service.
- Local AI addresses must be on this computer. Proxies are ignored for them and
  redirects aren't followed, so images can't leave it. Images are re-encoded first,
  which removes their metadata, GPS included.
- The update check asks GitHub's API for the latest release's version number, over
  HTTPS without following redirects, and sends nothing else. Only a plain version
  number is accepted from the reply, and the download link the app shows is built
  into it, never taken from the reply. The app never downloads or installs updates
  itself. The check when the app starts can be turned off on the Settings tab.
- Location lookup is off by default. It sends only coordinates rounded to about 100
  metres, and only when you describe a geotagged photo.

Another program on your computer could listen on the local AI address and receive
the images sent to it. Nothing here can tell it apart from a real AI system.

## Transcribing audio

- Only the first two hours of an audio file are decoded, which bounds how much
  memory a long or hostile file can take.
- Audio never leaves the computer. whisper.cpp is run directly, by absolute path,
  from where it is usually installed or where you chose it, and never through a
  shell. If you choose a program on the Settings tab, it is run as you, so choose only
  whisper.cpp's `whisper-cli`.
- Nothing is installed or downloaded without a yes from you. The only download is the
  speech recognition model, over HTTPS from Hugging Face. Hugging Face redirects it to
  its download network, so the file's size and SHA-256 are checked against the values
  built into the app, and it is thrown away unless both match.
- What whisper.cpp writes is shown only as text, with control characters and
  text-direction overrides removed.

## Debug logs

Logs never contain API keys or the text being read, and record only warnings and
errors from libraries. Replies from cloud services aren't logged, as they could
repeat part of a request. Logs do contain the names of files and of AI models, so the
log file is private to you on macOS and Linux. It is moved aside when it reaches 5 MB.

## Downloads

The packages aren't signed with a paid certificate. The macOS package has an ad hoc
signature, which Apple Silicon needs in order to run it, but which doesn't say who
made it. Check you are downloading from this repository's releases page. The release
workflow pins its actions to exact commits.
