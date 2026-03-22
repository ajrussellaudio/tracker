use crate::app::{App, BrowserEntry, BrowserMode, View};
use crate::wav_io::load_wav;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use vitakt_core::{audio::Command, model, storage};

/// List subdirectories and `.wav` files in `dir`, sorted alphabetically (case-insensitive).
pub(crate) fn list_browser_entries(dir: &std::path::Path) -> Vec<BrowserEntry> {
    list_browser_entries_ext(dir, "wav")
}

/// List subdirectories and files matching `file_ext` in `dir`, sorted alphabetically
/// (case-insensitive). All other file types are excluded.
pub(crate) fn list_browser_entries_ext(dir: &std::path::Path, file_ext: &str) -> Vec<BrowserEntry> {
    let mut entries = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                entries.push(BrowserEntry::Dir(name));
            } else if let Some(ext) = path.extension() {
                if ext.to_ascii_lowercase() == file_ext {
                    entries.push(BrowserEntry::Wav(name));
                }
            }
        }
    }
    entries.sort_by_key(|e| e.sort_key());
    if dir.parent().is_some() {
        entries.insert(0, BrowserEntry::ParentDir);
    }
    entries
}

impl App {
    /// Open the sample browser starting at the instrument's current sample directory,
    /// or the current working directory if no sample is set.
    pub(crate) fn open_sample_browser(&mut self) {
        let start_dir = self
            .song
            .instruments
            .get(self.active_instrument)
            .and_then(|instr| instr.sample.as_ref())
            .and_then(|sample| {
                let p = std::path::Path::new(&sample.path);
                p.parent().map(|parent| parent.to_path_buf())
            })
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            });
        self.browser_dir = start_dir;
        self.browser_mode = BrowserMode::Sample;
        self.browser_entries = list_browser_entries(&self.browser_dir);
        self.browser_cursor = 0;
        self.browser_scroll = 0;
        self.push_view(View::SampleBrowser);
    }

    /// Open the file browser scoped to `.trk` project files.
    pub(crate) fn open_project_browser(&mut self) {
        let start_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        self.browser_dir = start_dir;
        self.browser_mode = BrowserMode::Project;
        self.browser_entries = list_browser_entries_ext(&self.browser_dir, "trk");
        self.browser_cursor = 0;
        self.browser_scroll = 0;
        self.push_view(View::SampleBrowser);
    }

    /// Handle Enter in the sample browser.
    pub(crate) fn browser_enter(&mut self) {
        if let Some(entry) = self.browser_entries.get(self.browser_cursor).cloned() {
            match entry {
                BrowserEntry::ParentDir => {
                    self.browser_go_up();
                }
                BrowserEntry::Dir(name) => {
                    self.browser_dir = self.browser_dir.join(&name);
                    let ext = match self.browser_mode {
                        BrowserMode::Sample => "wav",
                        BrowserMode::Project => "trk",
                    };
                    self.browser_entries = list_browser_entries_ext(&self.browser_dir, ext);
                    self.browser_cursor = 0;
                    self.browser_scroll = 0;
                }
                BrowserEntry::Wav(name) => match self.browser_mode {
                    BrowserMode::Sample => {
                        let full_path = self.browser_dir.join(&name);
                        let path_str = full_path
                            .canonicalize()
                            .unwrap_or(full_path)
                            .to_string_lossy()
                            .to_string();
                        self.record("select sample");
                        self.ensure_instrument(self.active_instrument);
                        if let Some(instr) = self.song.instruments.get_mut(self.active_instrument) {
                            instr.sample = Some(model::Sample::from_path(path_str));
                        }
                        self.pop_view();
                        self.reload_instrument_sample();
                    }
                    BrowserMode::Project => {
                        let full_path = self.browser_dir.join(&name);
                        let path_str = full_path
                            .canonicalize()
                            .unwrap_or(full_path)
                            .to_string_lossy()
                            .to_string();
                        match storage::load_trk(&path_str) {
                            Ok(song) => {
                                self.song = model::migrate(song);
                                if self.song.phrases.is_empty() {
                                    self.song.phrases.push(model::Phrase::default());
                                }
                                self.sync_phrase_to_sequencer();
                                self.reload_instruments();
                                self.sync_song_to_sequencer();
                                self.sync_mixer_to_audio();
                                self.history.clear();
                                self.view_stack.clear();
                                self.view = View::SongView;
                                self.browser_mode = BrowserMode::Sample;
                                self.set_timed_status(format!("Loaded: {path_str}"));
                            }
                            Err(e) => {
                                self.set_timed_status(format!("Error: {e}"));
                            }
                        }
                    }
                },
            }
        }
    }

    /// Navigate to the parent directory in the sample browser.
    pub(crate) fn browser_go_up(&mut self) {
        if let Some(parent) = self.browser_dir.parent().map(|p| p.to_path_buf()) {
            self.browser_dir = parent;
            let ext = match self.browser_mode {
                BrowserMode::Sample => "wav",
                BrowserMode::Project => "trk",
            };
            self.browser_entries = list_browser_entries_ext(&self.browser_dir, ext);
            self.browser_cursor = 0;
            self.browser_scroll = 0;
        }
    }

    /// Handle the `b` key in the sample browser.
    pub(crate) fn browser_try_open_bookmarks(&mut self) {
        let has_valid = self
            .config
            .bookmarks
            .iter()
            .any(|p| std::path::Path::new(p.as_str()).is_dir());
        if has_valid {
            self.browser_bookmark_cursor = 0;
            self.browser_show_bookmarks = true;
        } else {
            self.set_timed_status(
                "No bookmarks set (or none exist on disk)  —  press B to add one".to_string(),
            );
        }
    }

    /// Handle the `B` key in the sample browser.
    pub(crate) fn browser_add_bookmark(&mut self) {
        let dir = self.browser_dir.to_string_lossy().to_string();
        if !self.config.bookmarks.contains(&dir) {
            self.config.bookmarks.push(dir.clone());
            if let Err(e) = self.config.save() {
                self.set_timed_status(format!("Error saving bookmark: {e}"));
            } else {
                self.set_timed_status(format!("Bookmarked: {dir}"));
            }
        } else {
            self.set_timed_status(format!("Already bookmarked: {dir}"));
        }
    }

    /// Handle the `e` key in the sample browser: suspend the TUI, launch the configured
    /// external file picker, and load the selected `.wav` into the current instrument.
    ///
    /// Does nothing if `config.file_browser` is `None`.
    pub(crate) fn browser_launch_external(&mut self) {
        let Some(cmd) = self.config.file_browser.clone() else {
            return;
        };

        let tmp_path = std::env::temp_dir()
            .join(format!("vitakt-chooser-{}.txt", std::process::id()));

        // Suspend TUI
        let _ = crossterm::terminal::disable_raw_mode();
        let mut stdout = std::io::stdout();
        let _ = crossterm::execute!(stdout, crossterm::terminal::LeaveAlternateScreen);

        // Launch external file picker via shell so that env-var substitution in the
        // command string (e.g. --chooser-file "$VITAKT_CHOOSER_FILE") is expanded.
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .env("VITAKT_CHOOSER_FILE", &tmp_path)
            .status();

        // Resume TUI (terminal.clear() is triggered via needs_terminal_clear in tui.rs)
        let _ = crossterm::terminal::enable_raw_mode();
        let _ = crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen);
        self.needs_terminal_clear = true;

        // Read and validate the selection
        let selected = std::fs::read_to_string(&tmp_path)
            .unwrap_or_default()
            .trim()
            .to_string();
        let _ = std::fs::remove_file(&tmp_path);

        if selected.to_lowercase().ends_with(".wav") && std::path::Path::new(&selected).exists() {
            self.record("select sample");
            self.ensure_instrument(self.active_instrument);
            if let Some(instr) = self.song.instruments.get_mut(self.active_instrument) {
                instr.sample = Some(vitakt_core::model::Sample::from_path(selected.clone()));
            }
            self.pop_view();
            self.reload_instrument_sample();
        }
    }

    /// Handle Enter on the bookmark overlay.
    pub(crate) fn browser_overlay_enter(&mut self) {
        let valid_bookmarks: Vec<String> = self
            .config
            .bookmarks
            .iter()
            .filter(|p| std::path::Path::new(p.as_str()).is_dir())
            .cloned()
            .collect();
        if let Some(path) = valid_bookmarks.get(self.browser_bookmark_cursor) {
            let dest = PathBuf::from(path);
            self.browser_dir = dest.clone();
            self.browser_entries = list_browser_entries(&dest);
            self.browser_cursor = 0;
            self.browser_scroll = 0;
        }
        self.browser_show_bookmarks = false;
        self.browser_bookmark_cursor = 0;
    }

    /// Handle Esc on the bookmark overlay.
    pub(crate) fn browser_overlay_esc(&mut self) {
        self.browser_show_bookmarks = false;
    }

    /// Adjust `browser_scroll` so that `browser_cursor` stays within the visible window.
    pub(crate) fn browser_clamp_scroll(&mut self, available: usize) {
        if available == 0 {
            return;
        }
        if self.browser_cursor < self.browser_scroll {
            self.browser_scroll = self.browser_cursor;
        } else if self.browser_cursor >= self.browser_scroll + available {
            self.browser_scroll = self.browser_cursor - available + 1;
        }
    }

    /// Handle Space in the sample browser: toggle preview playback of the highlighted .wav.
    pub(crate) fn browser_preview_toggle(&mut self) {
        self.is_previewing = self.preview_playing.load(Ordering::Relaxed);

        if self.is_previewing {
            self.send_cmd(Command::StopPreview);
            self.preview_playing.store(false, Ordering::Relaxed);
            self.is_previewing = false;
            return;
        }
        let Some(entry) = self.browser_entries.get(self.browser_cursor).cloned() else {
            return;
        };
        let BrowserEntry::Wav(name) = entry else {
            return;
        };
        let full_path = self.browser_dir.join(&name);
        let path_str = full_path
            .canonicalize()
            .unwrap_or(full_path)
            .to_string_lossy()
            .to_string();
        match load_wav(&path_str) {
            Ok((samples, channels)) => {
                self.send_cmd(Command::PreviewSample { samples, channels });
                self.preview_playing.store(true, Ordering::Relaxed);
                self.is_previewing = true;
            }
            Err(e) => self.set_timed_status(format!("Preview error: {e}")),
        }
    }
}
