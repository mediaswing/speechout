//! The Wordlist tab: importing wordlists, and turning them on, off or removing
//! them. They change how words are spoken, for pronunciation or to keep what is
//! read aloud suitable for a classroom.

use egui::Ui;

use super::widgets::{dropdown, full_button, full_checkbox, heading};
use super::App;
use crate::wordlist;
use crate::worker::{display_name, sentence};

impl App {
    fn reload_wordlists(&mut self) {
        match self.store.wordlists() {
            Ok(lists) => {
                self.wordlists = lists;
                if self.wordlist_to_remove >= self.wordlists.len() {
                    self.wordlist_to_remove = 0;
                }
            }
            Err(e) => {
                log::error!("Could not read the wordlists: {e:#}");
                self.set_error("The wordlists could not be read.");
            }
        }
    }

    fn import_wordlist(&mut self) {
        let Some(path) = self.dialogs.pick_file("Choose a wordlist to import", "XML wordlists", &["xml"]) else {
            return;
        };
        let result = wordlist::read_file(&path).and_then(|xml| self.store.import_wordlist(&xml));
        match result {
            Ok(name) => {
                self.reload_wordlists();
                self.set_info(format!("Imported the wordlist {name}. It is turned on."));
            }
            Err(e) => {
                log::warn!("Importing a wordlist failed: {e:#}");
                self.set_error(format!("{} could not be imported: {}", display_name(&path), sentence(&e)));
            }
        }
    }

    fn remove_wordlist(&mut self) {
        let Some(item) = self.wordlists.get(self.wordlist_to_remove) else {
            return self.set_error("There are no wordlists to remove.");
        };
        let (id, name) = (item.id, item.list.name.clone());
        if !self.dialogs.confirm("Remove the wordlist", &format!("Remove the wordlist {name}? This can't be undone.")) {
            return;
        }
        match self.store.remove_wordlist(id) {
            Ok(()) => {
                self.reload_wordlists();
                // With none left, the dropdown and this button go away.
                if self.wordlists.is_empty() {
                    self.focus_wanted = Some("import");
                }
                self.set_info(format!("Removed the wordlist {name}."));
            }
            Err(e) => {
                log::error!("Could not remove a wordlist: {e:#}");
                self.set_error(format!("The wordlist {name} could not be removed."));
            }
        }
    }

    pub(super) fn wordlist_tab(&mut self, ui: &mut Ui) {
        heading(ui, "Wordlist", 2);
        ui.label(
            "Wordlists change how words are spoken, to fix pronunciation or to keep what is read aloud suitable for a \
             classroom. The text you see isn't changed, only what is spoken and saved.",
        );
        let import = full_button(ui, "import", "Import a wordlist", None);
        if self.wants_focus("import") {
            import.request_focus();
        }
        if import.clicked() {
            self.import_wordlist();
        }

        heading(ui, "Installed Wordlists", 3);
        if self.wordlists.is_empty() {
            ui.label("No wordlists are installed.");
        }
        let mut toggled = None;
        for item in &mut self.wordlists {
            let mut text = item.list.name.clone();
            if !item.list.description.is_empty() {
                text.push_str(": ");
                text.push_str(&item.list.description);
            }
            text.push_str(&format!(" ({} entries)", item.list.entries.len()));
            if full_checkbox(ui, &format!("wordlist_{}", item.id), &mut item.enabled, &text).changed() {
                toggled = Some((item.id, item.enabled, item.list.name.clone()));
            }
        }
        if let Some((id, enabled, name)) = toggled {
            let state = if enabled { "on" } else { "off" };
            match self.store.set_wordlist_enabled(id, enabled) {
                Ok(()) => self.set_info(format!("Turned the wordlist {name} {state}.")),
                Err(e) => {
                    log::error!("Could not change a wordlist: {e:#}");
                    self.set_error(format!("The wordlist {name} could not be turned {state}."));
                }
            }
        }

        if !self.wordlists.is_empty() {
            ui.add_space(8.0);
            let names: Vec<String> = self.wordlists.iter().map(|w| w.list.name.clone()).collect();
            dropdown(ui, "remove_wordlist", "Wordlist to remove", &names, &mut self.wordlist_to_remove);
            if full_button(ui, "remove", "Remove this wordlist", None).clicked() {
                self.remove_wordlist();
            }
        }
    }
}
