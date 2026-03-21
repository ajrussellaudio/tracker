/// Placeholder data model for the tracker project.
pub struct Project {
    pub name: String,
    pub bpm: u32,
}

impl Project {
    pub fn new(name: impl Into<String>, bpm: u32) -> Self {
        Self { name: name.into(), bpm }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_project_new() {
        let p = Project::new("My Song", 120);
        assert_eq!(p.name, "My Song");
        assert_eq!(p.bpm, 120);
    }
}
