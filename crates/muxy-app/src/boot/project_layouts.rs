use muxy_app_core::project_layouts::{Config, DIRECTORY, Descriptor, discover};
use muxy_client::Client;
use muxy_protocol::{FileEntry, FilesAction, FilesReply, FilesRequest, ProjectId, ServerPath};
use muxy_ui::tr;

pub(super) fn list(client: &Client, project: ProjectId) -> Result<Vec<Descriptor>, String> {
    match entries(client, project, DIRECTORY) {
        Ok(entries) => Ok(discover(entries)),
        // A project without the folder simply has no layouts.
        Err(_) if !exists(client, project, DIRECTORY) => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn entries(client: &Client, project: ProjectId, path: &str) -> Result<Vec<FileEntry>, String> {
    match client
        .files(FilesRequest {
            project,
            action: FilesAction::List(ServerPath(path.as_bytes().to_vec())),
            root: None,
        })
        .map_err(|error| error.to_string())?
    {
        FilesReply::Entries(entries) => Ok(entries),
        _ => Err(tr!("Unexpected layout directory response").to_string()),
    }
}

/// Whether `path` is listed as a folder in its parent, checked from the project root down.
/// The server reports a missing folder only as a generic bad path.
fn exists(client: &Client, project: ProjectId, path: &str) -> bool {
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
    (parent.is_empty() || exists(client, project, parent))
        && entries(client, project, parent).is_ok_and(|entries| {
            entries
                .iter()
                .any(|entry| entry.is_directory && entry.name.0 == name.as_bytes())
        })
}

pub(super) fn load(
    client: &Client,
    project: ProjectId,
    layout: &Descriptor,
) -> Result<Config, String> {
    match client
        .files(FilesRequest {
            project,
            action: FilesAction::Read(layout.path.clone()),
            root: None,
        })
        .map_err(|error| error.to_string())?
    {
        FilesReply::Content(file) => Config::parse(&file.content),
        _ => Err(tr!("Unexpected layout file response").to_string()),
    }
}
