# Changelog

All notable changes to the Speech Output Engine are recorded here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the
project uses [Semantic Versioning](https://semver.org/).

When a version tag is pushed, the release workflow copies the section for that
version onto the GitHub release page, so write each entry for people who use
the app.

## [Unreleased]

## [2.3.0] - 2026-10-09

### Added

- Keyboard shortcuts can now be changed or turned off, one at a time or all
  at once, under "Keyboard shortcuts" on the Settings tab. The lists only
  offer keys that don't clash with screen readers, the operating system or
  typing, and button labels and spoken hints follow your choices.
- Press F1 to see a list of the keyboard shortcuts.
- On a Mac, Cmd+R reads aloud or plays, and Cmd+. stops, because the F keys
  need Fn on most Mac keyboards.
- While a tab has focus, the Left and Right arrows, Home and End move between
  tabs.
- Alt+Down (Option+Down on a Mac) opens a focused list, and Alt+Up closes it.
- The General tab has a new "Text to read" list. Choose "Paste Text" to type
  or paste text into a box and read it aloud or save it as audio, without
  making a file first. Choose "Choose File" to read a file as before. Choosing
  a file with Ctrl+O (Cmd+O on a Mac) switches back to "Choose File". Pasted
  text is limited to 200,000 characters, and invisible characters that could
  hide or reorder words are removed before it is read.

### Fixed

- Shortcuts no longer go off when extra keys are held. Typing a character
  with AltGr, such as ś (AltGr+S) or ó (AltGr+O) on a Polish keyboard, could
  open the Save or Open dialog, and AltGr with a number could switch tabs.
- Pressing Escape when nothing was running, or to close a list, took focus
  away from the control, so screen readers lost their place. Now focus stays
  where it was.
- On a Mac, text between double square brackets, such as `[[volm 0]]`, was
  taken by the system voice as a command, so a document could silence the
  voice, add long pauses or change how it spoke. Now it is read as ordinary
  text.

## [2.2.0] - 2026-10-09

### Added

- The Audio Player tab can now open a zip file of WAV and MP3 files and play
  them as a playlist, one after another. A `listing.txt` file in the zip sets
  the order, with one file name on each line; without one, the files play in
  order of name. Choose a track from the new "Track" list, or move between
  them with the "Previous track" and "Next track" buttons.

### Fixed

- A damaged PowerPoint 97–2003 (PPT) file could make the app use up all the
  computer's memory and close. Now the app says the presentation has more text
  than it can read.
- A damaged file that made the app fail while opening or playing it could
  leave the app waiting for ever, so no other file could be opened until it
  was restarted. Now the app says the file may be damaged.
- If the debug log folder is one that other people on the computer can write
  to, the app no longer follows a link left there in place of the log, which
  could have overwritten another file.

## [2.1.0] - 2026-10-08

### Added

- PowerPoint presentations (PPTX, and PPT from PowerPoint 97 to 2003) can now
  be read aloud. Each slide is read in the order it is shown, starting with
  its number ("Slide 1."), and slides with no text are skipped. Speaker notes
  are left out. In PPTX files, the text in SmartArt graphics is read box by
  box, and charts are read out as their title, then each series with its
  value for each category, such as "North. Q1: 10. Q2: 20."
- The app now plays a short sound when something succeeds, such as a file
  opening, reading aloud reaching the end or audio being saved, and a
  different sound when something fails. The sounds never play over speech or
  an audio file that is playing. Turn them off under "Sounds" on the Settings
  tab.

## [2.0.0] - 2026-10-08

### Added

- OpenDocument files from LibreOffice and other office apps can now be read
  aloud. ODT documents are read like DOCX files, and ODS spreadsheets are read
  as a table, row by row, like CSV files. When a spreadsheet has more than one
  sheet, each sheet is read in turn, starting with its name.
- Transcripts on the Audio player tab can now say who is speaking, starting
  each paragraph with "Speaker 1:", "Speaker 2:" and so on. Choose this under
  "Label who is speaking", either letting the app work out how many people
  there are or choosing the number yourself, which is more accurate. Like
  Whisper, this runs on your computer, so the audio isn't sent anywhere. The
  first time, the app asks to download the two small AI models it needs
  (about 46 MB).
