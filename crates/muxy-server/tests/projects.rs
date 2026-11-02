use muxy_protocol::{
    ErrorCode, OperationId, ProjectDescriptor, ProjectId, ProjectIntent, ProjectMutation,
    ProjectPatch, ServerPath,
};
use muxy_server::{LegacyImport, Registry, ServerSettings};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("muxy-catalog-{}", ProjectId::new()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
    fn registry(&self, legacy: LegacyImport) -> Result<Registry> {
        let (events, _) = mpsc::channel();
        Ok(Registry::persistent_with_import(
            ServerSettings {
                default_shell: Some("/bin/sh".into()),
                ..ServerSettings::default()
            },
            events,
            &self.0.join("sessions"),
            legacy,
        )?)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn project(path: &Path) -> ProjectDescriptor {
    ProjectDescriptor {
        id: ProjectId::new(),
        home: false,
        name: "Test".into(),
        icon: None,
        logo: None,
        color: "#808080".into(),
        directory: ServerPath(path.as_os_str().as_bytes().into()),
        kind: None,
        parent_id: None,
        folders: Vec::new(),
        folder_entries: Vec::new(),
        auto_subfolders: false,
    }
}
fn intent(mutation: ProjectMutation) -> ProjectIntent {
    ProjectIntent {
        operation: OperationId::new(),
        mutation,
    }
}

#[test]
fn folder_groups_persist_entries_and_reject_entries_on_ordinary_projects() -> Result {
    use muxy_protocol::{FolderEntry, FolderEntryId, ProjectKind};
    let fixture = Fixture::new()?;
    let registry = fixture.registry(LegacyImport::default())?;
    let ordinary = project(&fixture.0);
    let mut group = project(&fixture.0);
    group.kind = Some(ProjectKind::FolderGroup);
    registry.mutate_project(&intent(ProjectMutation::Create(ordinary.clone())))?;
    registry.mutate_project(&intent(ProjectMutation::Create(group.clone())))?;
    let entries = vec![FolderEntry {
        id: FolderEntryId::new(),
        name: "Client".into(),
        directory: ordinary.directory.clone(),
        command: "codex \"Review this project\"".into(),
    }];
    let patch = |project, entries| {
        intent(ProjectMutation::Patch {
            project,
            patch: ProjectPatch::FolderEntries(entries),
        })
    };
    assert!(
        registry
            .mutate_project(&patch(ordinary.id, entries.clone()))
            .is_err()
    );
    registry.mutate_project(&patch(group.id, entries.clone()))?;
    let parent = fixture.0.join("clients");
    let child = parent.join("Acme");
    fs::create_dir_all(&child)?;
    let source = |project| {
        intent(ProjectMutation::Patch {
            project,
            patch: ProjectPatch::FolderSource {
                directory: ServerPath(parent.as_os_str().as_bytes().to_vec()),
                auto_subfolders: true,
            },
        })
    };
    assert!(registry.mutate_project(&source(ordinary.id)).is_err());
    registry.mutate_project(&source(group.id))?;
    assert!(
        registry
            .git(&muxy_protocol::GitRequest {
                project: group.id,
                action: muxy_protocol::GitAction::Init,
            })
            .is_err()
    );
    assert!(!fixture.0.join(".git").exists());
    let mut files = muxy_protocol::FilesRequest {
        project: group.id,
        root: None,
        action: muxy_protocol::FilesAction::List(ServerPath(Vec::new())),
    };
    assert!(registry.files(&files).is_err());
    files.root = Some(ordinary.directory.clone());
    assert!(registry.files(&files).is_ok());
    files.root = Some(ServerPath(child.as_os_str().as_bytes().to_vec()));
    assert!(registry.files(&files).is_ok());
    let mut invalid = entries.clone();
    invalid[0].command = "echo safe\nrm -rf something".into();
    assert!(registry.mutate_project(&patch(group.id, invalid)).is_err());
    drop(registry);
    let registry = fixture.registry(LegacyImport::default())?;
    let page = registry.read_catalog(None, None)?;
    let saved = page
        .projects
        .iter()
        .find(|project| project.id == group.id)
        .ok_or("group")?;
    assert!(saved.auto_subfolders);
    assert_eq!(saved.directory.0, parent.as_os_str().as_bytes());
    assert_eq!(
        page.projects
            .iter()
            .find(|project| project.id == group.id)
            .ok_or("group")?
            .folder_entries,
        entries
    );
    assert!(
        page.projects
            .iter()
            .find(|project| project.id == ordinary.id)
            .ok_or("ordinary")?
            .folder_entries
            .is_empty()
    );
    Ok(())
}

#[test]
fn exact_identity_duplicate_paths_private_catalog_and_idempotent_mutations() -> Result {
    let fixture = Fixture::new()?;
    let registry = fixture.registry(LegacyImport::default())?;
    let before = registry.read_catalog(None, None)?;
    let first = project(&fixture.0);
    let second = project(&fixture.0);
    let create = intent(ProjectMutation::Create(first.clone()));
    registry.mutate_project(&create)?;
    registry.mutate_project(&create)?;
    registry.mutate_project(&intent(ProjectMutation::Create(second.clone())))?;
    assert_eq!(registry.read_catalog(None, None)?.projects.len(), 3);
    let rename = intent(ProjectMutation::Patch {
        project: first.id,
        patch: ProjectPatch::Name("Shared".into()),
    });
    registry.mutate_project(&rename)?;
    let revision = registry.catalog_revision();
    registry.mutate_project(&rename)?;
    assert_eq!(registry.catalog_revision(), revision);
    assert_eq!(
        registry
            .read_catalog(None, Some(before.revision))
            .err()
            .ok_or("expected stale revision")?
            .code(),
        ErrorCode::CatalogChanged
    );
    assert_eq!(
        fs::metadata(fixture.0.join("sessions/catalog.json"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    drop(registry);
    let reopened = fixture.registry(LegacyImport::default())?;
    let after = reopened.read_catalog(None, None)?;
    assert_eq!((after.server, after.home), (before.server, before.home));
    assert!(
        after
            .projects
            .iter()
            .any(|project| project.id == first.id && project.name == "Shared")
    );
    assert!(
        reopened
            .mutate_project(&intent(ProjectMutation::Delete(after.home)))
            .is_err()
    );
    Ok(())
}

#[test]
fn import_is_once_preserves_home_and_rejects_missing_membership_without_committing() -> Result {
    let fixture = Fixture::new()?;
    let mut home = project(&fixture.0);
    home.home = true;
    let first = project(&fixture.0);
    let legacy = LegacyImport {
        projects: vec![first.clone(), home.clone()],
        sessions: std::collections::BTreeMap::new(),
    };
    let registry = fixture.registry(legacy.clone())?;
    assert_eq!(registry.home_project(), home.id);
    registry.mutate_project(&intent(ProjectMutation::Delete(first.id)))?;
    drop(registry);
    let registry = fixture.registry(legacy)?;
    assert!(
        !registry
            .read_catalog(None, None)?
            .projects
            .iter()
            .any(|project| project.id == first.id)
    );
    drop(registry);
    let invalid = Fixture::new()?;
    let session = muxy_protocol::SessionId::new(99).ok_or("ID")?;
    assert!(
        invalid
            .registry(LegacyImport {
                projects: vec![],
                sessions: [(session, first.id)].into()
            })
            .is_err()
    );
    assert!(!invalid.0.join("sessions/catalog.json").exists());
    Ok(())
}

#[test]
fn logo_heavy_catalog_pages_fit_the_wire_and_continue_without_losing_projects() -> Result {
    let (events, _) = mpsc::channel();
    let registry = Registry::new(ServerSettings::default(), events);
    let mut logo = include_bytes!("../../muxy-protocol/tests/fixtures/project-logo.png").to_vec();
    logo.resize(muxy_protocol::MAX_PROJECT_LOGO_BYTES, 0);
    let logo: Arc<[u8]> = logo.into();
    for _ in 0..60 {
        let mut project = project(Path::new("/tmp"));
        project.logo = Some(logo.clone());
        registry.mutate_project(&intent(ProjectMutation::Create(project)))?;
    }
    let mut after = None;
    let mut count = 0;
    loop {
        let page = registry.read_catalog(after, None)?;
        count += page.projects.len();
        after = page.next;
        let mut bytes = Vec::new();
        muxy_protocol::wire::encode(
            &muxy_protocol::Message::Reply {
                id: muxy_protocol::RequestId(1),
                body: muxy_protocol::ReplyBody::Catalog(page),
            },
            muxy_protocol::CONTROL,
            &mut bytes,
        )?;
        assert!(bytes.len() < 4 * 1024 * 1024);
        if after.is_none() {
            break;
        }
    }
    assert_eq!(count, 61);
    Ok(())
}
