//! Reading a file aloud without opening the window (`speechout --read FILE`),
//! as "Read aloud" on the right-click menu in File Explorer does.
//!
//! It uses the voice, speed, wordlists and other choices saved on the
//! Settings tab. One file is read at a time: choosing Read aloud for another
//! file stops the first, and choosing it again for the file being read
//! stops reading. Errors, and the question before text is sent to a cloud
//! voice, are shown in message boxes, which screen readers read out, since
//! there is no window to show them in.

use crate::i18n::{self, t, tf};
use crate::settings::Settings;
use crate::speech::{self, Provider};
use crate::wordlist;
use crate::worker::{self, Control, Msg, Reporter, SpeechJob};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc::channel;

pub fn run(path: &Path) -> ExitCode {
    // The debug log is left to the window, which starts a fresh one each
    // time it opens.
    crate::logging::init();
    let settings = Settings::load();
    i18n::activate_saved(&crate::paths::languages_dir(), &settings.language);
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let reading = crate::paths::background_reading_file();
    let ours = path.display().to_string();

    // Choosing Read aloud for the file being read stops it.
    let before = std::fs::read_to_string(&reading).ok();
    if crate::platform::stop_background_reader() && before.as_deref() == Some(ours.as_str()) {
        log::info!("stopped reading in the background");
        return ExitCode::SUCCESS;
    }
    let control = Arc::new(Control::default());
    let stopper = control.clone();
    if let Err(e) = crate::platform::become_background_reader(move || stopper.stop()) {
        return fail(format!("{e:#}"));
    }
    if let Err(e) = crate::paths::write_private_file(&reading, ours.as_bytes()) {
        log::warn!("could not note which file is being read: {e}");
    }
    let code = read(&settings, &path, &name, control);
    // Unless another file has started being read since.
    if std::fs::read_to_string(&reading).ok().as_deref() == Some(ours.as_str()) {
        let _ = std::fs::remove_file(&reading);
    }
    code
}

fn read(settings: &Settings, path: &Path, name: &str, control: Arc<Control>) -> ExitCode {
    log::info!("reading a file in the background");
    let (text, system_voices_only) = match worker::read_file(
        path,
        &settings.vision_model,
        settings.resolve_location,
        settings.read_camera,
        settings.document_reading,
    ) {
        Ok(loaded) => (loaded.text, loaded.system_voices_only),
        Err(e) => return fail(tf("file.load_failed", &[("name", &name), ("error", &e)])),
    };
    if control.is_stopped() {
        return ExitCode::SUCCESS;
    }
    // A scanned PDF, say, has no text, and reading it would just be silent.
    if text.trim().is_empty() {
        return fail(tf("background.nothing", &[("name", &name)]));
    }
    let wordlists = wordlist::list_installed(&crate::paths::wordlist_dir());
    let text = crate::spoken::prepare(&text, &wordlists, &settings.disabled_wordlists);
    let job = match job_for(settings, system_voices_only, text) {
        Ok(job) => job,
        Err(e) => return fail(tf("background.failed", &[("name", &name), ("error", &e)])),
    };
    if job.provider != Provider::System && !confirm_sending(&job, name) {
        log::info!("not sending the file to {}", job.provider.short_name());
        return ExitCode::SUCCESS;
    }

    let (tx, rx) = channel();
    // Nothing is drawn, so the context only stands in for the window's.
    let rep = Reporter::new(tx, egui::Context::default());
    let speaker = rep.clone();
    std::thread::spawn(move || worker::speak(speaker, control, job));
    for msg in rx {
        match msg {
            Msg::Status(text) => log::info!("status: {text}"),
            Msg::Done(..) => break,
            Msg::Failed(e) => return fail(tf("background.failed", &[("name", &name), ("error", &e)])),
            _ => {}
        }
    }
    ExitCode::SUCCESS
}

/// A job for the saved service and voice, as the window would make it.
/// `system_voices_only` is for a file, such as a PNG screenshot, that must not
/// be read with a cloud voice.
fn job_for(settings: &Settings, system_voices_only: bool, text: String) -> Result<SpeechJob, String> {
    // A key is only looked up for a file a cloud voice may read.
    let key = settings.provider.key_name().filter(|_| !system_voices_only).and_then(crate::secrets::get);
    let provider = settings.provider.or_system(key.is_some(), system_voices_only);
    let voice = match settings.voice_for(provider) {
        Some(voice) => voice.to_owned(),
        None if provider == Provider::System => String::new(),
        // The window uses the first voice until one is chosen.
        None => speech::list_voices(provider, key.as_deref())
            .map_err(|e| format!("{e:#}"))?
            .first()
            .map(|v| v.id.clone())
            .ok_or_else(|| t("voice.choose_first"))?,
    };
    Ok(SpeechJob {
        provider,
        key,
        voice,
        speed: settings.speed_for(provider),
        text,
        announce_parts: settings.announce_parts,
        preview: false,
    })
}

/// Cloud voices charge by the character. The window says how much will be
/// sent as reading starts, but here nothing else would, so ask first.
fn confirm_sending(job: &SpeechJob, name: &str) -> bool {
    let count = speech::billable_chars(job.provider, &job.text, job.announce_parts);
    let key = if count == 1 { "usage.up_to.one" } else { "usage.up_to.other" };
    let usage = tf(key, &[("count", &speech::format_count(count)), ("service", &job.provider.short_name())]);
    rfd::MessageDialog::new()
        .set_title(crate::APP_TITLE)
        .set_description(tf("background.confirm_cloud", &[("name", &name), ("usage", &usage)]))
        .set_buttons(rfd::MessageButtons::YesNo)
        .set_level(rfd::MessageLevel::Info)
        .show()
        == rfd::MessageDialogResult::Yes
}

fn fail(message: String) -> ExitCode {
    log::warn!("reading in the background failed: {message}");
    rfd::MessageDialog::new()
        .set_title(crate::APP_TITLE)
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .set_level(rfd::MessageLevel::Error)
        .show();
    ExitCode::FAILURE
}
