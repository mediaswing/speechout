//! Checking for a new version: when the app starts, unless that is turned off,
//! and whenever the button on the Settings tab is pressed. A new version is
//! announced in the status bar and linked to from the Settings tab. One found
//! when the app starts is also shown in a dialog, which offers to open the
//! download page. Nothing is downloaded or installed by the app itself.

use egui::Ui;

use super::App;
use super::widgets::{dropdown, full_button, heading};
use crate::update::{self, Version};
use crate::worker;

const CHECK_ON: &str = "Yes, ask GitHub for the latest version number when the app starts";
const CHECK_OFF: &str = "No, only check when I press the button";

impl App {
    /// The check when the app starts, if it is turned on. Its answer is only
    /// announced if there is a new version.
    pub fn check_for_updates_at_startup(&mut self) {
        if self.settings.check_updates {
            self.checking_for_updates = true;
            worker::check_for_update(&self.rep, false);
        }
    }

    pub(super) fn check_for_updates_now(&mut self) {
        if self.checking_for_updates {
            return self.set_info("Already checking for updates.");
        }
        self.checking_for_updates = true;
        self.set_info("Checking for updates.");
        worker::check_for_update(&self.rep, true);
    }

    pub(super) fn update_checked(&mut self, result: Result<Option<Version>, String>, requested: bool) {
        self.checking_for_updates = false;
        let current = Version::current();
        match result {
            Ok(Some(newer)) => {
                self.newer_version = Some(newer);
                // Checking by hand happens on the Settings tab, beside the link.
                if !requested
                    && self.dialogs.confirm(
                        "Update available",
                        &format!(
                            "Version {newer} of {} is available. You have {current}.\n\nOpen the download page in \
                             your web browser?",
                            crate::APP_TITLE
                        ),
                    )
                {
                    self.open_downloads = true;
                    return self.set_info(format!("Opening the download page for version {newer} in your web browser."));
                }
                // At startup, a problem already in the status bar matters more.
                if requested || !self.status.error {
                    self.set_info(format!(
                        "Version {newer} of {} is available. You have {current}. The Updates section of the \
                         Settings tab has a link to download it.",
                        crate::APP_TITLE
                    ));
                }
            }
            Ok(None) => {
                self.newer_version = None;
                if requested {
                    self.set_info(format!("You have the latest version, {current}."));
                }
            }
            // At startup, a failed check isn't worth interrupting anyone for.
            Err(e) if requested => self.set_error(format!("Could not check for updates. {e}")),
            Err(_) => {}
        }
    }

    pub(super) fn updates_section(&mut self, ui: &mut Ui) {
        heading(ui, "Updates", 3);
        let current = Version::current();
        let about = match self.newer_version {
            Some(newer) => format!("This is version {current}. Version {newer} is available."),
            None => format!("This is version {current}."),
        };
        ui.label(about);
        if let Some(newer) = self.newer_version {
            // Opens in the web browser. Hovering shows where it goes.
            ui.vertical_centered(|ui| ui.hyperlink_to(format!("Download version {newer} from GitHub"), update::RELEASES_URL));
        }
        let options = [CHECK_ON.to_owned(), CHECK_OFF.to_owned()];
        let mut index = usize::from(!self.settings.check_updates);
        if dropdown(ui, "check_updates", "Check for updates when the app starts", &options, &mut index) {
            self.settings.check_updates = index == 0;
            self.save_settings();
        }
        if full_button(ui, "check_now", "Check for updates now", None).clicked() {
            self.check_for_updates_now();
        }
    }
}
