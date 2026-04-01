use std::path::Path;
use std::path::PathBuf;

use codex_protocol::protocol::SkillsListEntry;

pub(crate) fn skills_entry_for_cwd<'a>(
    cwd: &Path,
    skills_entries: &'a [SkillsListEntry],
) -> Option<&'a SkillsListEntry> {
    let normalized_cwd = normalize_path(cwd);
    if let Some(entry) = skills_entries.iter().find(|entry| {
        entry.cwd.as_path() == cwd || paths_match(normalized_cwd.as_deref(), entry.cwd.as_path())
    }) {
        return Some(entry);
    }

    match skills_entries {
        [entry] => Some(entry),
        _ => None,
    }
}

fn paths_match(normalized_expected: Option<&Path>, candidate: &Path) -> bool {
    let Some(normalized_expected) = normalized_expected else {
        return false;
    };
    normalize_path(candidate)
        .as_deref()
        .is_some_and(|normalized_candidate| normalized_candidate == normalized_expected)
}

fn normalize_path(path: &Path) -> Option<PathBuf> {
    dunce::canonicalize(path).ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::skills_entry_for_cwd;

    fn entry(cwd: &str, skill_name: &str) -> codex_protocol::protocol::SkillsListEntry {
        codex_protocol::protocol::SkillsListEntry {
            cwd: PathBuf::from(cwd),
            skills: vec![codex_protocol::protocol::SkillMetadata {
                name: skill_name.to_string(),
                description: format!("{skill_name} description"),
                short_description: None,
                interface: None,
                dependencies: None,
                path: PathBuf::from(format!("/skills/{skill_name}/SKILL.md")),
                scope: codex_protocol::protocol::SkillScope::User,
                enabled: true,
            }],
            errors: Vec::new(),
        }
    }

    #[test]
    fn skills_entry_for_cwd_prefers_exact_match() {
        let entries = vec![
            entry("/tmp/other", "other"),
            entry("/tmp/project", "project"),
        ];

        let selected = skills_entry_for_cwd(PathBuf::from("/tmp/project").as_path(), &entries)
            .expect("expected exact cwd match");

        assert_eq!(selected.cwd, PathBuf::from("/tmp/project"));
        assert_eq!(selected.skills[0].name, "project");
    }

    #[test]
    fn skills_entry_for_cwd_falls_back_to_single_entry_when_no_match_exists() {
        let entries = vec![entry("/srv/remote-project", "remote-only")];

        let selected =
            skills_entry_for_cwd(PathBuf::from("/work/local-project").as_path(), &entries)
                .expect("expected sole entry fallback");

        assert_eq!(selected.cwd, PathBuf::from("/srv/remote-project"));
        assert_eq!(selected.skills[0].name, "remote-only");
    }

    #[test]
    fn skills_entry_for_cwd_avoids_ambiguous_multi_entry_fallback() {
        let entries = vec![
            entry("/srv/project-a", "alpha"),
            entry("/srv/project-b", "beta"),
        ];

        let selected =
            skills_entry_for_cwd(PathBuf::from("/work/local-project").as_path(), &entries);

        assert!(selected.is_none());
    }
}
