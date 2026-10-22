//! The Wordlists tab: importing, turning on and off, and removing wordlists.

use super::*;

impl SpeechApp {
    pub(super) fn reload_wordlists(&mut self) {
        self.wordlists = wordlist::list_installed(&crate::paths::wordlist_dir());
        if self.wordlist_to_remove >= self.wordlists.len() {
            self.wordlist_to_remove = 0;
        }
    }

    fn import_wordlist(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title(t("wordlists.import_title"))
            .add_filter(t("wordlists.filter"), &["xml"])
            .pick_file()
        else {
            return;
        };
        match wordlist::import(&path, &crate::paths::wordlist_dir()) {
            Ok(name) => {
                self.reload_wordlists();
                self.announce(tf("wordlists.imported", &[("name", &name)]));
                self.cue(Cue::Success);
            }
            Err(e) => {
                let error = format!("{e:#}");
                self.show_error(tf("wordlists.import_failed", &[("name", &file_name(&path)), ("error", &error)]));
            }
        }
    }

    fn remove_wordlist(&mut self) {
        let Some(item) = self.wordlists.get(self.wordlist_to_remove) else { return };
        let file = item.file_name.clone();
        let title = display_name(item);
        let confirmed = rfd::MessageDialog::new()
            .set_title(t("wordlists.remove_title"))
            .set_description(tf("wordlists.remove_question", &[("name", &title)]))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes;
        if !confirmed {
            return;
        }
        match wordlist::remove(&crate::paths::wordlist_dir(), &file) {
            Ok(()) => {
                self.settings.disabled_wordlists.remove(&file);
                self.settings.save();
                self.reload_wordlists();
                self.announce(tf("wordlists.removed", &[("name", &title)]));
            }
            Err(e) => self.show_error(format!("{e:#}")),
        }
    }

    pub(super) fn wordlists_tab(&mut self, ui: &mut Ui) {
        heading(ui, &t("wordlists.heading"));
        ui.label(t("wordlists.intro"));
        if full_button(ui, &t("wordlists.import"), true).clicked() {
            self.import_wordlist();
        }
        if full_button(ui, &t("wordlists.reload"), true).clicked() {
            self.reload_wordlists();
            self.announce(tf("wordlists.count", &[("count", &self.wordlists.len())]));
        }

        ui.add_space(8.0);
        heading(ui, &t("wordlists.installed"));
        if self.wordlists.is_empty() {
            ui.label(t("wordlists.none"));
        }
        let mut toggled = None;
        for item in &self.wordlists {
            match &item.list {
                Ok(list) => {
                    let mut enabled = !self.settings.disabled_wordlists.contains(&item.file_name);
                    let description = if list.description.is_empty() { &item.file_name } else { &list.description };
                    let text = tf(
                        "wordlists.item",
                        &[("name", &list.name), ("description", description), ("count", &list.entries.len())],
                    );
                    let resp = ui.add_sized(
                        [ui.available_width(), CONTROL_HEIGHT],
                        egui::Checkbox::new(&mut enabled, text),
                    );
                    if resp.changed() {
                        toggled = Some((item.file_name.clone(), enabled, list.name.clone()));
                    }
                }
                Err(e) => {
                    let text = tf("wordlists.unreadable", &[("file", &item.file_name), ("error", e)]);
                    ui.label(RichText::new(text).color(error_color(ui)));
                }
            }
        }
        if let Some((file, enabled, name)) = toggled {
            if enabled {
                self.settings.disabled_wordlists.remove(&file);
            } else {
                self.settings.disabled_wordlists.insert(file);
            }
            self.settings.save();
            let key = if enabled { "wordlists.enabled" } else { "wordlists.disabled" };
            self.announce(tf(key, &[("name", &name)]));
        }

        if !self.wordlists.is_empty() {
            ui.add_space(8.0);
            let names: Vec<String> = self.wordlists.iter().map(display_name).collect();
            let mut index = self.wordlist_to_remove;
            if dropdown(ui, "remove_wordlist", &t("wordlists.to_remove"), &names, &mut index, true) {
                self.wordlist_to_remove = index;
            }
            if full_button(ui, &t("wordlists.remove"), true).clicked() {
                self.remove_wordlist();
            }
        }
    }
}
