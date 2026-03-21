use tracker_core::Song;

const MAX_HISTORY: usize = 1000;

pub struct History {
    pub undo_stack: Vec<(String, Song)>, // (description, snapshot before mutation)
    pub redo_stack: Vec<(String, Song)>, // (description, snapshot before undo)
}

impl History {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    /// Save a snapshot before a mutation. Call BEFORE applying the mutation.
    /// Clears the redo stack.
    pub fn push(&mut self, description: String, snapshot: Song) {
        if self.undo_stack.len() >= MAX_HISTORY {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push((description, snapshot));
        self.redo_stack.clear();
    }

    /// Undo: restore previous snapshot. Returns (restored_song, description).
    pub fn undo(&mut self, current_song: Song) -> Option<(Song, String)> {
        let (desc, snapshot) = self.undo_stack.pop()?;
        self.redo_stack.push((desc.clone(), current_song));
        Some((snapshot, desc))
    }

    /// Redo: re-apply the most recently undone mutation.
    pub fn redo(&mut self, current_song: Song) -> Option<(Song, String)> {
        let (desc, snapshot) = self.redo_stack.pop()?;
        self.undo_stack.push((desc.clone(), current_song));
        Some((snapshot, desc))
    }

    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }
}
