# Changelog

## [3.7.2] - 2026-11-01

### Added

- A Send feedback button at the end of the Settings tab opens the feedback form in your web browser. Its shortcut is F8, which you can change or turn off under Keyboard shortcuts.

### Fixed

- The Web Reader now reads addresses typed without "https://" that have another web address in them, such as `example.com/share?u=https://…`. Before, they were refused as not starting with https://.
- Web pages in other character sets, such as Shift_JIS, GBK, EUC-KR, Windows-1251 and ISO-8859-2, are now read correctly. Before, their text came out as nonsense.
- A headline with invisible characters in it is no longer read twice.
- Pages that a site sends with a success code other than 200, such as 203 from some proxies, can now be read.
- The Web Reader now connects only to the internet addresses it checked, so a site can't switch to an address on your computer or local network between the check and the download. Two more kinds of local IPv6 address are refused too.

## [3.7.1] - 2026-10-31

### Fixed

- When you choose Read aloud for another file while one is being read in the background, the new file now waits until the first has stopped, so you never hear the two at once. Before, if the first took more than a few seconds to stop, both were read together for a moment.
- On macOS and Linux, choosing Read aloud again now stops reading when your home folder is on a network drive that can't lock files. Before, it started reading the file a second time instead.
- Translating the app now finishes in one go when some text is the same in English, such as "Pause" in French. Text the AI model gives back in English is asked for again before translating ends, instead of being reported as not translated until you translate again.

## [3.7.0] - 2026-10-30

### Added

- Read aloud is now on the right-click menu on macOS and Linux too. Add it with the button under Right-click menu on the Settings tab. On a Mac, it is under Quick Actions when you right-click a document or image in Finder. On Linux, it is at the top of the menu in Dolphin and Nemo, under Scripts in Files, and under Open With in other file managers. As on Windows, it reads the file in the background with the voice chosen in the app, and choosing it again stops reading.

### Changed

- In a translated app, the sentences that tell you which key to press now say the key's name in that language too, such as "Échap" for Escape in French, as your screen reader would. Letters, numbers and function keys stay as they are, and buttons still show the keys as they're printed on the keyboard. Translations you already have keep working: translate again to add the key names, which stay in English until then.

### Fixed

- Translating the app no longer keeps reporting text it "could not translate" when the right translation is the same as the English, such as "Pause" in French and German. That text is now kept when the AI model gives the same answer a second time, so translating again finishes the job.
- A translation that the AI model ended with a stray line break is no longer thrown away.
- When a damaged translation file is set aside to make a new one, an earlier copy that was set aside is no longer overwritten. Copies are kept as `.json.bak`, `.json.bak2` and so on.

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
