# Changelog

## [3.6.0] - 2026-10-29

### Added

- A new Web Reader choice under "Text to read" reads the article on a web page. Type or paste the page's address and press Enter, or choose "Get text from the page". The app downloads the page and reads the headline and then the article, leaving out menus, adverts, related stories, image credits and embedded social media posts. Your computer fetches the page directly from the site, without cookies, and only the page itself, so its images, scripts and trackers never load. Only secure (https) pages on the internet can be read. If a page can't be read, for example because the site only sends its pages to web browsers, a message says so and suggests copying the text into Paste Text instead. Audio saved from a web page is named after its headline.

## [3.5.0] - 2026-10-22

### Added

- The Audio Player tab now plays and transcribes WMA files, the kind saved by Olympus and other voice recorders, such as the DM650. They're opened with ffmpeg, which runs on your computer, so your recordings aren't sent anywhere. If ffmpeg isn't installed, the app offers to install it, as it does for videos, and then opens the file. WMA files can be tracks in a zip playlist too. A long recording takes a little longer to open than an MP3 of the same length, and Escape still stops it.
- If Ollama can't use the chosen model to describe a photo or video, for example because it can't load it, or the model gives an empty description, the other vision models you have installed are tried in turn. The first one that works becomes the chosen model, and the app tells you which.

### Changed

- The Audio Player tab now shows the time remaining instead of the length, counting down as the file plays.
- Speech this app saved as MP3, by reading text aloud, is no longer transcribed when you open it on the Audio Player tab, since its words are known already. The app says so instead, and "Transcribe the speech" still transcribes it if you want.

### Fixed

- When Ollama can't describe a photo, the app now says why, such as a model it can't load, instead of giving only an error code.
- Installing ffmpeg for a video, or now a WMA file, can no longer be interrupted by the app looking for AI models in the meantime, which could lose the waiting file or offer to set up Ollama for it.

## [3.4.0] - 2026-10-22

### Added

- EPUB books can now be read aloud. A book starts with its title and authors, such as "The City of the Blind, by Murray Leinster.", then each chapter is read in turn, in the order the book is meant to be read, and the Content and Format settings for Word, OpenDocument and Markdown files apply to EPUB books too, so Format says headings, lists, tables, quotations, bold, italic and links. Pictures are read from their descriptions (alt text), such as "Image: A man with a lantern.", after the paragraph they're in; pictures with no description, or marked as decoration, are passed over. Footnotes, the numbers that refer to them and printed page numbers are left out. Books with copy protection (DRM) can't be read, and say so.

## [3.3.0] - 2026-10-21

### Added

- Speech saved as an MP3 file now has ID3 tags, so music players and File Explorer show more than the file name: the title is the saved file's name, the artist is the voice that read it, and the album is the document it was read from, so recordings of the same document are grouped together. Names in any language, such as "Café" or "日本語", are kept as they are.

### Fixed

- Saved MP3 files now say exactly how long they are, so players show the right length, can play them one after another without gaps, and no longer start each file with a blank frame.
- Text files saved on Windows are no longer read with a pause at the end of every line. Lines wrapped part-way through a sentence are joined up again, and only blank lines mark a new paragraph.
- Text files saved by older Windows programs no longer lose their curly quotes, dashes, euro signs and ellipses ("don’t" was read as "dont").

## [3.2.0] - 2026-10-21

First release from the new repository. Speech Output Engine reads documents aloud and describes photos. Written in Rust, it puts accessibility first: every feature is keyboard-accessible. 
