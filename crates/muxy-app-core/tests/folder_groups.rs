use std::os::unix::ffi::OsStrExt;

use muxy_app_core::{AppState, ServerId, store};
use muxy_protocol::{FolderEntry, FolderEntryId, ServerPath};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn automatic_mode_preserves_manual_commands_and_survives_a_missing_parent() -> Result {
    let root = tempfile::tempdir()?;
    let parent = root.path().join("Clients");
    let child = parent.join("Acme");
    let nested = child.join("Nested");
    let manual = root.path().join("Manual client");
    std::fs::create_dir_all(&nested)?;
    std::fs::create_dir(&manual)?;
    let mut state = AppState::bootstrap()?;
    let group = state.add_folder_group("Clients")?;
    let entry = FolderEntry {
        id: FolderEntryId::new(),
        name: "Manual review".into(),
        directory: ServerPath(manual.as_os_str().as_bytes().to_vec()),
        command: "claude \"Review this client\"".into(),
    };
    state.set_folder_entries(group, vec![entry.clone()])?;
    state.set_folder_group_source(group, parent.clone(), true)?;
    assert!(state.open_folder_entry(group, entry.id).is_err());
    assert!(state.open_subfolder(group, parent.clone()).is_err());
    assert!(
        state
            .open_subfolder(
                group,
                nested
                    .join("..")
                    .join("..")
                    .join("..")
                    .join("Manual client")
            )
            .is_err()
    );
    state.open_subfolder(group, nested)?;
    state.open_subfolder(group, child.clone())?;
    let pane = state.window().active_pane.ok_or("pane")?;
    assert_eq!(
        state.prepare_creation(pane, &parent),
        ServerPath(child.as_os_str().as_bytes().to_vec())
    );
    assert_eq!(state.startup_command(pane), None);
    let path = root.path().join("state.json");
    store::save(&path, &state)?;
    let mut loaded = store::load(&path)?;
    assert!(loaded.project(group).ok_or("group")?.auto_subfolders);
    let backup = loaded.restore_configuration(&AppState::bootstrap()?)?;
    assert!(
        backup
            .project(group)
            .ok_or("restored group")?
            .auto_subfolders
    );
    assert_eq!(backup.project(group).ok_or("group")?.directory, parent);
    std::fs::remove_dir_all(&parent)?;
    assert!(loaded.open_subfolder(group, child).is_err());
    loaded.remove_project(group)?;
    let restored = loaded.restore_removed_project(0)?;
    assert!(
        loaded
            .project(restored)
            .ok_or("restored group")?
            .auto_subfolders
    );
    assert_eq!(
        loaded.project(restored).ok_or("group")?.folder_entries,
        std::slice::from_ref(&entry)
    );
    loaded.set_folder_group_source(restored, parent, false)?;
    loaded.open_folder_entry(restored, entry.id)?;
    let pane = loaded.window().active_pane.ok_or("manual pane")?;
    assert_eq!(loaded.startup_command(pane), Some(entry.command.as_str()));
    assert_eq!(loaded.prepare_creation(pane, &manual), entry.directory);
    Ok(())
}

#[test]
fn folder_group_entries_artwork_and_launch_intent_survive_reload_and_restore() -> Result {
    let directory = tempfile::tempdir()?;
    let folder = directory.path().join("Client's $(literal) folder");
    std::fs::create_dir(&folder)?;
    let mut state = AppState::bootstrap()?;
    let group = state.add_folder_group("Clients")?;
    let entry = FolderEntry {
        id: FolderEntryId::new(),
        name: "Review client".into(),
        directory: ServerPath(folder.as_os_str().as_bytes().to_vec()),
        command: "claude \"Review this client\"".into(),
    };
    state.set_folder_entries(group, vec![entry.clone()])?;
    state.set_project_icon(group, Some("sf:briefcase".into()))?;
    state.set_project_color(group, "#e5484d".parse()?)?;
    let logo: std::sync::Arc<[u8]> =
        include_bytes!("../../muxy-protocol/tests/fixtures/project-logo.png")
            .as_slice()
            .into();
    state.set_project_logo(group, Some(logo.clone()))?;
    let tab = state.open_folder_entry(group, entry.id)?;
    let pane = state.window().active_pane.ok_or("missing pane")?;
    let path = directory.path().join("state.json");
    store::save(&path, &state)?;
    let mut loaded = store::load(&path)?;
    assert_eq!(
        loaded.prepare_creation(pane, &loaded.home().directory.clone()),
        entry.directory
    );
    assert_eq!(loaded.startup_command(pane), Some(entry.command.as_str()));
    let project = loaded.project(group).ok_or("missing group")?;
    assert!(project.is_folder_group());
    assert_eq!(project.tabs[0].id, tab);
    assert_eq!(project.folder_entries, std::slice::from_ref(&entry));
    assert_eq!(project.icon.as_deref(), Some("sf:briefcase"));
    assert_eq!(project.logo, Some(logo.clone()));
    assert_eq!(project.color.as_str(), "#e5484d");

    // Removing a launcher does not remove its already-open terminal.
    loaded.set_folder_entries(group, vec![])?;
    assert_eq!(loaded.project(group).ok_or("group")?.tabs[0].id, tab);
    loaded.set_folder_entries(group, vec![entry.clone()])?;
    loaded.remove_project(group)?;
    let restored = loaded.restore_removed_project(0)?;
    let project = loaded.project(restored).ok_or("restored group")?;
    assert!(project.is_folder_group());
    assert_eq!(project.folder_entries, [entry]);
    assert_eq!(project.logo, Some(logo));
    Ok(())
}

#[test]
fn unavailable_or_stale_entries_do_not_create_tabs_and_groups_do_not_merge_by_home_path() -> Result
{
    let directory = tempfile::tempdir()?;
    let mut state = AppState::bootstrap()?;
    let first = state.add_folder_group("Clients")?;
    let second = state.add_folder_group("Personal")?;
    let entry = FolderEntry {
        id: FolderEntryId::new(),
        name: "Offline drive".into(),
        directory: ServerPath(
            directory
                .path()
                .join("missing")
                .as_os_str()
                .as_bytes()
                .to_vec(),
        ),
        command: String::new(),
    };
    state.set_folder_entries(first, vec![entry.clone()])?;
    assert!(state.open_folder_entry(first, entry.id).is_err());
    assert!(
        state
            .open_folder_entry(first, FolderEntryId::new())
            .is_err()
    );
    assert!(state.project(first).ok_or("group")?.tabs.is_empty());
    let ordinary = state.add_project(ServerId::local(), directory.path().to_owned())?;
    assert!(state.set_folder_entries(ordinary, vec![entry]).is_err());
    let merged = state.merge_configuration(&AppState::bootstrap()?)?;
    assert!(
        merged
            .project(first)
            .ok_or("first group")?
            .is_folder_group()
    );
    assert!(
        merged
            .project(second)
            .ok_or("second group")?
            .is_folder_group()
    );
    state.remove_project(first)?;
    state.remove_project(second)?;
    state.restore_removed_project(0)?;
    while let Some(intent) = state.project_intents(ServerId::local()).first().cloned() {
        state.complete_project_intent(ServerId::local(), intent.operation, true)?;
    }
    assert_eq!(state.removed_projects().len(), 1);
    assert_eq!(state.removed_projects()[0].name, "Clients");
    Ok(())
}
