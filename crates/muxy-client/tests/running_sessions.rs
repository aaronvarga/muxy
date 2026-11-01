use muxy_client::Client;
use muxy_protocol::{
    CONTROL, Feature, Message, ProjectId, ProjectSession, ProjectSessions, ReplyBody, RequestBody,
    SUPPORTED, ServerInfo, ServerPath, SessionId, SessionInfo, SessionStatus,
    transport::socket_pair,
    wire::{Decoder, Encoder},
};
use std::{thread, time::Duration};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn paged_inventory_keeps_attached_and_starting_sessions_without_reopening_ended_ones() -> Result {
    let project = ProjectId::new();
    let sessions = [
        (1, SessionStatus::Live, true),
        (2, SessionStatus::Starting, false),
        (3, SessionStatus::Ended, false),
        (4, SessionStatus::Unavailable, false),
    ]
    .into_iter()
    .map(|(id, status, attached)| -> Result<ProjectSession> {
        Ok(ProjectSession {
            info: SessionInfo {
                project,
                id: SessionId::new(id).ok_or("invalid session")?,
                directory: ServerPath(b"/tmp".to_vec()),
            },
            status,
            attached,
            owner: attached.then(Default::default),
        })
    })
    .collect::<Result<Vec<_>>>()?;
    let (client, server) = socket_pair()?;
    server.set_read_timeout(Some(Duration::from_secs(5)))?;
    let worker = thread::spawn(move || -> Result {
        let mut input = Decoder::new(server.try_clone()?);
        let mut output = Encoder::new(server);
        assert!(matches!(input.next()?, (CONTROL, Message::Hello { .. })));
        output.send(
            CONTROL,
            &Message::HelloReply {
                versions: SUPPORTED.to_vec(),
                server: ServerInfo::current(),
                features: vec![Feature::PAIRED_COMPUTERS],
            },
        )?;
        for _ in 0..4 {
            let (channel, message) = input.next()?;
            assert_eq!(channel, CONTROL);
            let Message::Request {
                id,
                body:
                    RequestBody::ListProjectSessions {
                        project: owner,
                        after,
                        revision,
                    },
            } = message
            else {
                return Err("unexpected inventory request".into());
            };
            assert_eq!(owner, project);
            let page = if let Some(after) = after {
                assert_eq!(after, sessions[1].info.id);
                assert_eq!(revision, Some(7));
                ProjectSessions {
                    revision: 7,
                    sessions: sessions[2..].to_vec(),
                    next: None,
                }
            } else {
                assert_eq!(revision, None);
                ProjectSessions {
                    revision: 7,
                    sessions: sessions[..2].to_vec(),
                    next: Some(sessions[1].info.id),
                }
            };
            output.send(
                CONTROL,
                &Message::Reply {
                    id,
                    body: ReplyBody::ProjectSessions(page),
                },
            )?;
        }
        Ok(())
    });
    let client = Client::from_stream(Box::new(client))?.with_timeout(Duration::from_secs(5));
    assert!(
        !client.supports(Feature::PROJECT_FOLDERS),
        "upstream pairing cannot enable fork folder requests"
    );
    let running = client.running_project_sessions(project)?;
    assert_eq!(running.sessions.len(), 2);
    assert!(running.sessions[0].attached);
    assert_eq!(running.sessions[1].status, SessionStatus::Starting);
    let available = client.available_project_sessions(project)?;
    assert_eq!(available.sessions.len(), 1);
    assert_eq!(available.sessions[0].info.id, running.sessions[1].info.id);
    worker.join().map_err(|_| "inventory fixture panicked")??;
    Ok(())
}
