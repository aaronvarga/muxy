use muxy_app_core::{AppState, ServerId, store};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn dividers_hold_their_place_while_projects_move_and_go() -> Result {
    let directory = tempfile::tempdir()?;
    let mut state = AppState::bootstrap()?;
    let [a, b, c] = [(); 3].map(|()| state.add_project(ServerId::local(), directory.path().into()));
    let (a, b, c) = (a?, b?, c?);
    // Home, A, B | C
    let divider = state.add_divider(b)?;
    state.set_divider_label(divider, "  Clients  ")?;

    // Dragging A down over B swaps them; the divider stays below the pair.
    state.move_project(a, 2)?;
    assert_eq!(state.dividers()[0].after, a);
    // Dragging C up over A puts C above A; the divider stays below A.
    state.move_project(c, 2)?;
    assert_eq!(state.dividers()[0].after, a);
    // Removing the project above it hands the divider to the next one up.
    state.remove_project(a)?;
    assert_eq!(state.dividers()[0].after, c);

    let path = directory.path().join("state.json");
    store::save(&path, &state)?;
    let saved = store::load(&path)?;
    assert_eq!(saved.dividers(), state.dividers());
    assert_eq!(saved.dividers()[0].label.as_deref(), Some("Clients"));

    state.remove_divider(divider)?;
    store::save(&path, &state)?;
    assert!(!std::fs::read_to_string(&path)?.contains("dividers"));
    Ok(())
}

#[test]
fn dividers_below_one_project_keep_their_order() -> Result {
    let directory = tempfile::tempdir()?;
    let mut state = AppState::bootstrap()?;
    let a = state.add_project(ServerId::local(), directory.path().into())?;
    let b = state.add_project(ServerId::local(), directory.path().into())?;
    let lower = state.add_divider(a)?;
    let upper = state.add_divider(a)?;
    // B crosses only the lower divider: A, upper, B, lower.
    state.place_dividers(&[lower], b, true)?;
    let order: Vec<_> = state.dividers().iter().map(|d| (d.id, d.after)).collect();
    assert_eq!(order, [(upper, a), (lower, b)]);
    Ok(())
}
