use std::sync::{Arc, mpsc};

use crate::{Registry, ServerSettings};

use super::Outbox;

#[test]
fn a_phone_that_connects_first_never_receives_desktop_notifications() {
    let (sender, _events) = mpsc::channel();
    let registry = Registry::new(ServerSettings::default(), sender);
    let session = muxy_protocol::SessionId::new(1).expect("nonzero");
    registry.activity.update(
        muxy_protocol::AgentActivity {
            session,
            project: registry.home_project(),
            provider: muxy_protocol::AgentProvider::Claude,
            state: muxy_protocol::AgentState::Blocked,
        },
        false,
    );
    let events: Vec<_> = registry
        .activity
        .snapshot()
        .events
        .iter()
        .map(|event| event.id)
        .collect();
    let changes = Arc::clone(&registry.attachment_changes);
    let phone = Arc::new(Outbox::for_device(
        Arc::clone(&changes),
        muxy_protocol::DeviceId::new(),
        muxy_protocol::DeviceKind::Phone,
    ));
    registry.register_connection(&phone);
    let desktop = Arc::new(Outbox::new(changes));
    desktop.identify(muxy_protocol::ClientKind::Desktop);
    registry.register_connection(&desktop);
    assert!(!events.is_empty());
    assert!(registry.claim_activity(&phone, &events).is_empty());
    assert_eq!(registry.claim_activity(&desktop, &events), events);
}

#[test]
fn phones_get_no_text_icons_that_stock_apps_reject() {
    let (sender, _events) = mpsc::channel();
    let registry = Registry::new(ServerSettings::default(), sender);
    let mut page = registry.read_catalog(None, None).expect("catalog");
    page.projects[0].icon = Some("AVP".into());
    page.projects.push(page.projects[0].clone());
    page.projects[1].icon = Some("🚀".into());
    let changes = Arc::clone(&registry.attachment_changes);
    let phone = Outbox::for_device(
        Arc::clone(&changes),
        muxy_protocol::DeviceId::new(),
        muxy_protocol::DeviceKind::Phone,
    );
    let icons = |page: &muxy_protocol::CatalogPage| -> Vec<_> {
        page.projects
            .iter()
            .map(|project| project.icon.clone())
            .collect()
    };
    assert_eq!(
        icons(&super::reader::catalog_for(&phone, page.clone())),
        [None, Some("🚀".into())]
    );
    let desktop = Outbox::new(changes);
    assert_eq!(
        icons(&super::reader::catalog_for(&desktop, page.clone())),
        icons(&page)
    );
}
