//! Words the window shows or says for things, such as a file's name or how
//! long a recording is.

use super::*;

pub(super) fn file_name(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| t("file.fallback_name"))
}

/// The suggested model's download size, such as "about 3.3 GB".
pub(super) fn model_size() -> String {
    tf("model.size", &[("size", &crate::vision::SUGGESTED_MODEL_SIZE)])
}

/// A voice's name in the list. The default system voice is named by the app,
/// so it follows the interface language.
pub(super) fn voice_name(voice: &Voice) -> String {
    if voice.id.is_empty() { t("voice.default") } else { voice.name.clone() }
}

/// A length of time as it is said, such as "1 minute 5 seconds".
pub(super) fn spoken_duration(duration: Duration) -> String {
    let total = duration.as_secs_f64().round() as u64;
    let units = [
        (total / 3600, "time.hour", "time.hours"),
        (total / 60 % 60, "time.minute", "time.minutes"),
        (total % 60, "time.second", "time.seconds"),
    ];
    let parts: Vec<String> = units
        .into_iter()
        .filter(|(count, _, _)| *count > 0)
        .map(|(count, one, other)| if count == 1 { t(one) } else { tf(other, &[("count", &count)]) })
        .collect();
    if parts.is_empty() { tf("time.seconds", &[("count", &0)]) } else { parts.join(" ") }
}

/// A choice in the "Label who is speaking" list on the Audio Player tab.
pub(super) fn speaker_choice_label(choice: SpeakerLabels) -> String {
    match choice {
        SpeakerLabels::Off => t("speakers.off"),
        SpeakerLabels::Auto => t("speakers.auto"),
        SpeakerLabels::Count(n) => tf("speakers.count", &[("count", &n)]),
    }
}

/// A Whisper model as it is listed on the Settings tab.
pub(super) fn whisper_label(model: WhisperModel) -> String {
    let size = tf("model.size", &[("size", &model.size())]);
    let key = match model {
        WhisperModel::Base => "settings.whisper_base",
        WhisperModel::Small => "settings.whisper_small",
    };
    let label = tf(key, &[("model", &model.name()), ("size", &size)]);
    if model.is_downloaded() { format!("{label} {}", t("settings.whisper_downloaded")) } else { label }
}

pub(super) fn display_name(item: &Installed) -> String {
    match &item.list {
        Ok(list) => format!("{} ({})", list.name, item.file_name),
        Err(_) => item.file_name.clone(),
    }
}
