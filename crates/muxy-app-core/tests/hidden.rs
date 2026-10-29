use muxy_app_core::{AppState, ServerId, store};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn hidden_projects_stay_out_of_the_sidebar_until_shown_again() -> Result {
    let directory = tempfile::tempdir()?;
    let mut state = AppState::bootstrap()?;
    let a = state.add_project(ServerId::local(), directory.path().into())?;
    let b = state.add_project(ServerId::local(), directory.path().into())?;
    let workspace = state.create_workspace("Work")?;
    state.set_workspace_member(workspace, a, true)?;
    assert!(state.set_project_hidden(state.home().id, true).is_err());

    state.set_project_hidden(a, true)?;
    let listed = |state: &AppState, id| state.is_listed(state.project(id).unwrap());
    assert!(!listed(&state, a));
    state.select_workspace(Some(workspace))?;
    assert!(!listed(&state, a));
    state.select_workspace(None)?;

    let path = directory.path().join("state.json");
    store::save(&path, &state)?;
    let saved = store::load(&path)?;
    assert!(!listed(&saved, a));
    assert_eq!(saved.workspace(workspace).unwrap().projects.len(), 1);

    // Opening a hidden project, as from Switch Project, shows it again.
    state.select_project(a)?;
    state.reveal_current_project();
    assert!(listed(&state, a));

    state.set_project_hidden(b, true)?;
    state.remove_project(b)?;
    assert_eq!(state.hidden_projects().count(), 0);
    store::save(&path, &state)?;
    assert!(!std::fs::read_to_string(&path)?.contains("hidden"));
    Ok(())
}
