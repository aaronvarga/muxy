use std::num::NonZeroU64;

use crate::{
    AttachSnapshot, ChannelId, Color, Cursor, CursorShape, ErrorCode, ErrorReply, ExitReason,
    HistoryCursor, HistoryPage, InputModes, Message, MetadataEvent, Modes, Modifiers, MouseAction,
    MouseButton, MouseEvent, ReplyBody, RequestBody, RequestId, Row, Run, ScreenFrame, SearchMatch,
    SearchPage, SearchSource, ServerPath, SessionId, Size, Style, TerminalColors, V2,
};

impl Message {
    pub fn samples() -> Vec<Self> {
        let session = SessionId::from(NonZeroU64::MIN);
        let channel = ChannelId(1);
        let size = Size { cols: 4, rows: 1 };
        let directory = ServerPath(b"/tmp".to_vec());
        let rows = vec![Row {
            index: 0,
            runs: vec![Run {
                text: "Muxy".to_owned(),
                width: 4,
                style: Style {
                    fg: Color::Rgb(80, 180, 240),
                    bg: Color::Indexed(0),
                    bold: true,
                    ..Style::default()
                },
            }],
        }];
        let cursor = sample_cursor();
        let modes = Modes {
            application_cursor_keys: true,
            bracketed_paste: true,
        };

        let mut samples = vec![
            Self::CellSize(crate::CellSize::default()),
            hello_sample(),
            create_session_sample(directory.clone(), size),
            Self::FrameAck { channel, seq: 1 },
            hello_reply_sample(),
            Self::VersionUnsupported,
            Self::Reply {
                id: RequestId(2),
                body: ReplyBody::Attached {
                    snapshot: Box::new(AttachSnapshot {
                        links: None,
                        graphics: crate::Graphics::default(),
                        prompts: vec![0],
                        channel,
                        size,
                        rows: rows.clone(),
                        cursor,
                        modes,
                        title: "Muxy".to_owned(),
                        directory,
                        history: Vec::new(),
                        history_cursor: None,
                        history_total: 0,
                    }),
                    process: None,
                },
            },
            Self::SessionEnded {
                session,
                reason: ExitReason::Exited(0),
            },
            Self::Fatal(ErrorReply {
                code: ErrorCode::BadRequest,
                message: "expected hello".to_owned(),
            }),
            Self::Input(b"pwd\r".to_vec()),
            Self::Frame(ScreenFrame {
                size,
                graphics: None,
                seq: 1,
                reset: true,
                rows,
                cursor,
                modes,
            }),
            Self::Metadata(MetadataEvent::Title("Muxy".to_owned())),
        ];
        let snapshot = samples.iter().find_map(|message| match message {
            Self::Reply {
                body: ReplyBody::Attached { snapshot, .. },
                ..
            } => Some(*snapshot.clone()),
            _ => None,
        });
        samples.extend(exec_samples());
        samples.extend(git_samples());
        samples.extend(git_review_samples());
        samples.extend(files_samples());
        samples.extend(snapshot.into_iter().flat_map(history_samples));
        samples.extend(input_samples().into_iter().chain(terminal_input_samples()));
        samples.extend(acknowledged_input_samples());
        samples.extend(search_samples(session, channel));
        samples.extend(image_samples(channel));
        samples.extend(color_samples());
        samples.extend(settings_samples());
        samples.extend(project_samples());
        samples.extend(close_samples());
        terminal_metadata_samples(&mut samples);
        samples.extend(activity_samples(session));
        samples.extend(remote_samples());
        samples.extend(push_samples());
        samples.extend(chat_samples());
        samples
    }
}

fn push_samples() -> Vec<Message> {
    let device = crate::DeviceId::from_u128(3);
    let requests = [
        RequestBody::RegisterPush(crate::PushToken {
            platform: crate::PushPlatform::Apns,
            token: "8f1c".repeat(16),
            environment: Some(crate::PushEnvironment::Production),
        }),
        RequestBody::RegisterPush(crate::PushToken {
            platform: crate::PushPlatform::Fcm,
            token: "fcm-token:APA91b_".repeat(4),
            environment: None,
        }),
        RequestBody::UnregisterPush,
        RequestBody::SetPushNotifications(false),
        RequestBody::ResetPush(device),
    ];
    let replies = [
        ReplyBody::PushRegistered(crate::PushRegistration {
            status: crate::PushStatus::Active,
            key: Some([9; crate::PUSH_KEY_BYTES]),
        }),
        ReplyBody::PushRegistered(crate::PushRegistration {
            status: crate::PushStatus::Disabled,
            key: None,
        }),
        ReplyBody::PushUnregistered,
        ReplyBody::RemoteAccess(crate::RemoteAccessState {
            revision: 8,
            settings: crate::RemoteAccessSettings {
                enabled: true,
                port: crate::DEFAULT_REMOTE_PORT,
            },
            status: crate::ListenerStatus::Listening,
            pairing_expires_at: None,
            devices: vec![crate::PairedDevice {
                id: device,
                name: "Phone".into(),
                paired_at: 100,
                last_seen: Some(200),
                connected: false,
                kind: crate::DeviceKind::Phone,
                push: Some(crate::PushStatus::Refused),
            }],
            push_notifications: true,
            push_kept_until: None,
        }),
        ReplyBody::RemoteAccess(crate::RemoteAccessState {
            revision: 9,
            settings: crate::RemoteAccessSettings {
                enabled: true,
                port: crate::DEFAULT_REMOTE_PORT,
            },
            status: crate::ListenerStatus::Listening,
            pairing_expires_at: None,
            devices: vec![crate::PairedDevice {
                id: device,
                name: "Phone".into(),
                paired_at: 100,
                last_seen: Some(200),
                connected: false,
                kind: crate::DeviceKind::Phone,
                push: Some(crate::PushStatus::Disabled),
            }],
            push_notifications: false,
            push_kept_until: Some(7_776_300),
        }),
    ];
    requests
        .into_iter()
        .map(|body| Message::Request {
            id: RequestId(48),
            body,
        })
        .chain(replies.into_iter().map(|body| Message::Reply {
            id: RequestId(48),
            body,
        }))
        .collect()
}

fn remote_samples() -> Vec<Message> {
    let device = crate::DeviceId::from_u128(3);
    let credential = crate::DeviceCredential {
        device,
        token: [7; 32],
    };
    let settings = crate::RemoteAccessSettings {
        enabled: true,
        port: crate::DEFAULT_REMOTE_PORT,
    };
    let mut messages = vec![Message::Changed {
        topic: crate::Topic::RemoteAccess,
        revision: 7,
    }];
    for body in [
        RequestBody::IdentifyClient(crate::ClientKind::Mobile),
        RequestBody::Authenticate(credential),
        RequestBody::Pair(crate::PairRequest {
            secret: [5; 16],
            name: "Studio".into(),
            kind: crate::DeviceKind::Computer,
        }),
        RequestBody::ReadRemoteAccess,
        RequestBody::WriteRemoteAccess(settings),
        RequestBody::StartPairing,
        RequestBody::StartPairingWithHosts(vec!["box.example.com".into(), "203.0.113.7".into()]),
        RequestBody::CancelPairing,
        RequestBody::RevokeDevice(device),
    ] {
        messages.push(Message::Request {
            id: RequestId(40),
            body,
        });
    }
    for body in [
        ReplyBody::Authenticated,
        ReplyBody::Paired(crate::Paired {
            credential,
            server: crate::ServerIdentity::from_u128(4),
            name: "Mac".into(),
        }),
        ReplyBody::RemoteAccess(crate::RemoteAccessState {
            revision: 7,
            settings,
            status: crate::ListenerStatus::Listening,
            pairing_expires_at: Some(300),
            devices: vec![crate::PairedDevice {
                id: device,
                name: "Phone".into(),
                paired_at: 100,
                last_seen: Some(200),
                connected: true,
                kind: crate::DeviceKind::Computer,
                push: None,
            }],
            push_notifications: false,
            push_kept_until: None,
        }),
        ReplyBody::Pairing(crate::PairingOffer {
            invite: crate::PairingInvite {
                hosts: vec!["192.168.1.5".into(), "mac.local".into()],
                port: crate::DEFAULT_REMOTE_PORT,
                fingerprint: [1; 32],
                secret: [5; 16],
            },
            expires_at: 300,
        }),
        ReplyBody::Error(ErrorReply {
            code: ErrorCode::Unauthorized,
            message: "unauthorized".into(),
        }),
    ] {
        messages.push(Message::Reply {
            id: RequestId(40),
            body,
        });
    }
    messages
}

fn search_samples(session: SessionId, channel: ChannelId) -> Vec<Message> {
    vec![
        Message::Request {
            id: RequestId(6),
            body: RequestBody::Search {
                source: SearchSource::Live(channel),
                query: "Muxy".into(),
                ignore_case: false,
                before: HistoryCursor(0),
                max_results: 500,
            },
        },
        Message::Request {
            id: RequestId(7),
            body: RequestBody::Search {
                source: SearchSource::Saved(session),
                query: "muxy".into(),
                ignore_case: true,
                before: HistoryCursor(42),
                max_results: 100,
            },
        },
        Message::Reply {
            id: RequestId(6),
            body: ReplyBody::SearchPage(SearchPage {
                matches: vec![SearchMatch {
                    row: 100,
                    start: 2,
                    end: 6,
                }],
                next: Some(HistoryCursor(42)),
                total_rows: 100,
                scanned_rows: 2000,
            }),
        },
    ]
}

fn image_samples(channel: ChannelId) -> Vec<Message> {
    let frame = |key| crate::GraphicFrame { key, gap: 40 };
    let pixels: std::sync::Arc<[u8]> = vec![0, 255, 0, 255].into();
    let graphics = crate::Graphics {
        images: vec![crate::GraphicImage {
            id: 7,
            generation: 3,
            width: 1,
            height: 1,
            rgba: pixels.clone(),
            frames: vec![frame(11), frame(12)],
            playback: crate::Playback {
                state: crate::PlaybackState::Running,
                frame: 1,
                loops: 0,
            },
        }],
        placements: vec![crate::GraphicPlacement {
            image: 7,
            id: 0,
            column: 0,
            row: 0,
            offset: [0, 0],
            size: [8, 16],
            source: [0, 0, 1, 1],
            z: 0,
        }],
        ..crate::Graphics::default()
    };
    vec![
        Message::Request {
            id: RequestId(8),
            body: RequestBody::DeferImages,
        },
        Message::Reply {
            id: RequestId(8),
            body: ReplyBody::ImagesDeferred,
        },
        Message::Request {
            id: RequestId(10),
            body: RequestBody::StreamImages,
        },
        Message::Reply {
            id: RequestId(10),
            body: ReplyBody::ImagesStreamed,
        },
        Message::Request {
            id: RequestId(9),
            body: RequestBody::ImageFrames {
                channel,
                image: 7,
                keys: vec![11, 12],
            },
        },
        Message::Reply {
            id: RequestId(9),
            body: ReplyBody::ImageFrames(vec![crate::ImageFrame {
                key: 11,
                width: 1,
                height: 1,
                rgba: pixels,
            }]),
        },
        Message::Frame(ScreenFrame {
            size: Size { cols: 80, rows: 24 },
            graphics: Some(graphics),
            seq: 2,
            reset: false,
            rows: Vec::new(),
            cursor: Cursor {
                shape: CursorShape::default(),
                row: 0,
                col: 0,
                visible: true,
            },
            modes: Modes::default(),
        }),
    ]
}

fn history_samples(mut snapshot: AttachSnapshot) -> Vec<Message> {
    snapshot.history.clone_from(&snapshot.rows);
    snapshot.history_total = 2;
    snapshot.links = Some(vec![crate::LinkRow {
        row: 0,
        spans: vec![crate::LinkSpan {
            start: 0,
            end: 4,
            uri: "https://example.com/history".into(),
        }],
    }]);
    snapshot.history_cursor = Some(HistoryCursor(42));
    vec![
        Message::Metadata(MetadataEvent::ScreenPrompts {
            seq: 1,
            rows: vec![0],
        }),
        Message::Metadata(MetadataEvent::History { total_rows: 5000 }),
        Message::Request {
            id: RequestId(3),
            body: RequestBody::HistoryPage {
                channel: snapshot.channel,
                before: HistoryCursor(42),
                max_rows: 500,
            },
        },
        Message::Request {
            id: RequestId(41),
            body: RequestBody::ClearScreen(snapshot.channel),
        },
        Message::Reply {
            id: RequestId(41),
            body: ReplyBody::ScreenCleared,
        },
        Message::Request {
            id: RequestId(4),
            body: RequestBody::SavedHistoryPage {
                session: SessionId::from(NonZeroU64::MIN),
                before: HistoryCursor(0),
                max_rows: 200,
            },
        },
        Message::Reply {
            id: RequestId(3),
            body: ReplyBody::HistoryPage(HistoryPage {
                links: snapshot.links.clone(),
                prompts: vec![0],
                rows: snapshot.history.clone(),
                next: None,
                total_rows: 2,
                screen: None,
            }),
        },
        Message::Reply {
            id: RequestId(5),
            body: ReplyBody::Attached {
                snapshot: Box::new(snapshot),
                process: None,
            },
        },
    ]
}

fn color_samples() -> [Message; 4] {
    let colors = TerminalColors {
        foreground: [0xc9, 0xc2, 0xd9],
        background: [0x19, 0x17, 0x1f],
        cursor: [0xc3, 0x70, 0xd3],
        ansi: [[0x12, 0x34, 0x56]; 16],
        palette: std::collections::BTreeMap::from([(196, [0xab, 0xcd, 0xef])]),
        cursor_style: Some(CursorShape::Bar),
        cursor_blink: Some(false),
    };
    [
        Message::Request {
            id: RequestId(8),
            body: RequestBody::SetTerminalColors(colors.clone()),
        },
        Message::Reply {
            id: RequestId(8),
            body: ReplyBody::TerminalColorsSet,
        },
        Message::Request {
            id: RequestId(9),
            body: RequestBody::SetSessionColors {
                session: SessionId::from(NonZeroU64::MIN),
                colors: Some(colors),
            },
        },
        Message::Reply {
            id: RequestId(9),
            body: ReplyBody::SessionColorsSet,
        },
    ]
}

fn input_samples() -> [Message; 2] {
    [
        Message::Mouse(MouseEvent {
            action: MouseAction::Press,
            button: Some(MouseButton::Left),
            column: 3,
            row: 1,
            scroll: None,
            modifiers: Modifiers {
                shift: false,
                alt: true,
                ctrl: false,
            },
        }),
        Message::Metadata(MetadataEvent::InputModes(InputModes {
            mouse_tracking: true,
            alternate_scroll: false,
            focus_events: true,
        })),
    ]
}

fn terminal_input_samples() -> [Message; 4] {
    [
        crate::TerminalInput::Key(crate::KeyEvent {
            key: "a".into(),
            text: "A".into(),
            action: crate::KeyAction::Repeat,
            modifiers: crate::KeyModifiers::SHIFT,
            consumed_modifiers: crate::KeyModifiers::SHIFT,
            unshifted_codepoint: u32::from('a'),
            option_as_alt: true,
        }),
        crate::TerminalInput::Paste(b"one\ntwo".to_vec()),
        crate::TerminalInput::Focus(true),
        crate::TerminalInput::ClearScreen,
    ]
    .map(Message::TerminalInput)
}

fn settings_samples() -> Vec<Message> {
    let settings = crate::ServerSettingsDoc {
        default_shell: Some(ServerPath(b"/bin/bash".to_vec())),
        history_budget_bytes: 16 * 1024 * 1024,
        shell_integration: true,
    };
    let mut messages = vec![Message::ServerRestarting];
    for body in [
        RequestBody::ReadServerSettings,
        RequestBody::WriteServerSettings(settings.clone()),
        RequestBody::StopServer,
        RequestBody::StopServerIfIdle,
    ] {
        messages.push(Message::Request {
            id: RequestId(25),
            body,
        });
    }
    for body in [
        ReplyBody::ServerSettings(settings),
        ReplyBody::ServerSettingsWritten,
        ReplyBody::ServerStopping,
        ReplyBody::ServerBusy,
    ] {
        messages.push(Message::Reply {
            id: RequestId(25),
            body,
        });
    }
    messages
}

fn hello_reply_sample() -> Message {
    Message::HelloReply {
        versions: vec![V2],
        server: crate::ServerInfo {
            build: crate::BuildInfo {
                version: "fixture".into(),
                compatibility: crate::COMPATIBILITY,
                protocol: crate::SUPPORTED.to_vec(),
            },
            instance: 1,
            unreadable: vec![crate::UnreadableFile {
                kept_as: ServerPath::from("/state/catalog.json.unreadable-1"),
                error: "expected value at line 1 column 1".into(),
            }],
        },
        features: vec![crate::Feature(1)],
    }
}

fn create_session_sample(directory: ServerPath, size: Size) -> Message {
    Message::Request {
        id: RequestId(1),
        body: RequestBody::CreateSession {
            project: crate::ProjectId::from_u128(1),
            operation: crate::OperationId::from_u128(2),
            directory,
            size,
            environment: [("http_proxy".to_owned(), "http://proxy:8080".to_owned())].into(),
            colors: Some(TerminalColors {
                background: [0xf0, 0xf0, 0xf5],
                ..TerminalColors::default()
            }),
        },
    }
}

fn hello_sample() -> Message {
    Message::Hello { versions: vec![V2] }
}

fn sample_cursor() -> Cursor {
    Cursor {
        shape: CursorShape::default(),
        row: 0,
        col: 3,
        visible: true,
    }
}

fn terminal_metadata_samples(samples: &mut Vec<Message>) {
    samples.push(Message::SessionMetadata {
        session: NonZeroU64::MIN.into(),
        metadata: crate::SessionMetadata {
            title: "Finished task".into(),
            directory: ServerPath(b"/tmp".to_vec()),
            process: None,
        },
    });
    samples.push(Message::Progress {
        session: SessionId::from(NonZeroU64::MIN),
        progress: crate::SessionProgress {
            progress: Some(crate::TerminalProgress {
                state: crate::ProgressState::Running,
                percent: Some(42),
            }),
            completed: 2,
        },
    });
    samples.push(Message::Metadata(MetadataEvent::CursorBlinking(true)));
    samples.push(Message::Metadata(MetadataEvent::ClipboardWrite(
        "copied text".into(),
    )));
    samples.push(Message::Metadata(MetadataEvent::AgentSession {
        provider: crate::AgentProvider::Claude,
        id: "bde3d35b-d3c0-49d7-93c1-9a0391781702".into(),
        arguments: vec!["--enable-auto-mode".into()],
    }));
    samples.push(Message::Metadata(MetadataEvent::Links {
        seq: 1,
        rows: vec![crate::LinkRow {
            row: 0,
            spans: vec![crate::LinkSpan {
                start: 0,
                end: 4,
                uri: "https://example.com".into(),
            }],
        }],
    }));
}

fn changed_samples() -> [Message; 2] {
    [(crate::Topic::Catalog, 1), (crate::Topic::Sessions, 2)]
        .map(|(topic, revision)| Message::Changed { topic, revision })
}

fn project_session_samples(project: crate::ProjectId, directory: ServerPath) -> [Message; 7] {
    use crate::{ProjectSession, ProjectSessions, SessionInfo, SessionStatus};
    [
        Message::Request {
            id: RequestId(7),
            body: RequestBody::WatchSessionMetadata,
        },
        Message::Reply {
            id: RequestId(7),
            body: ReplyBody::SessionMetadataWatched,
        },
        Message::Reply {
            id: RequestId(3),
            body: ReplyBody::ProjectSessions(ProjectSessions {
                revision: 1,
                sessions: vec![ProjectSession {
                    info: SessionInfo {
                        id: SessionId::from(NonZeroU64::MIN),
                        project,
                        directory: directory.clone(),
                    },
                    status: SessionStatus::Live,
                    owner: Some(crate::SessionClient {
                        id: crate::ClientId::from_u128(4),
                        kind: crate::ClientKind::Desktop,
                    }),
                    attached: true,
                    name: None,
                }],
                next: None,
            }),
        },
        Message::Request {
            id: RequestId(5),
            body: RequestBody::SetSessionName {
                session: SessionId::from(NonZeroU64::MIN),
                name: Some("API server".into()),
                replacing: None,
            },
        },
        Message::Reply {
            id: RequestId(5),
            body: ReplyBody::SessionNameSet,
        },
        Message::Request {
            id: RequestId(8),
            body: RequestBody::SetSessionName {
                session: SessionId::from(NonZeroU64::MIN),
                name: None,
                replacing: Some("API server".into()),
            },
        },
        Message::Reply {
            id: RequestId(6),
            body: ReplyBody::ProjectSessions(ProjectSessions {
                revision: 2,
                sessions: vec![ProjectSession {
                    info: SessionInfo {
                        id: SessionId::from(NonZeroU64::MIN),
                        project,
                        directory,
                    },
                    status: SessionStatus::Live,
                    owner: None,
                    attached: false,
                    name: Some("API server".into()),
                }],
                next: None,
            }),
        },
    ]
}

#[allow(
    clippy::too_many_lines,
    reason = "Keep every project message sample together"
)]
fn project_samples() -> Vec<Message> {
    use crate::{
        CatalogPage, OperationId, ProjectDescriptor, ProjectId, ProjectIntent, ProjectMutation,
        ServerIdentity,
    };
    let project = ProjectDescriptor {
        id: ProjectId::from_u128(1),
        home: false,
        name: "Example".into(),
        directory: ServerPath(b"/tmp".to_vec()),
        icon: None,
        logo: None,
        color: "#808080".into(),
        kind: None,
        parent_id: None,
        folders: vec![ServerPath(b"/srv/shared".to_vec())],
        folder_entries: Vec::new(),
        auto_subfolders: false,
    };
    let operation = OperationId::from_u128(2);
    let mut samples = vec![
        Message::Request {
            id: RequestId(5),
            body: RequestBody::IdentifyClient(crate::ClientKind::Desktop),
        },
        Message::Reply {
            id: RequestId(5),
            body: ReplyBody::ClientIdentified(crate::SessionClient {
                id: crate::ClientId::from_u128(4),
                kind: crate::ClientKind::Desktop,
            }),
        },
        Message::Request {
            id: RequestId(1),
            body: RequestBody::ReadCatalog {
                after: None,
                revision: None,
            },
        },
        Message::Request {
            id: RequestId(2),
            body: RequestBody::MutateProject(ProjectIntent {
                operation,
                mutation: ProjectMutation::Create(project.clone()),
            }),
        },
        Message::Request {
            id: RequestId(3),
            body: RequestBody::ListProjectSessions {
                project: project.id,
                after: None,
                revision: None,
            },
        },
        Message::Request {
            id: RequestId(4),
            body: RequestBody::CancelCreation(operation),
        },
        Message::Reply {
            id: RequestId(1),
            body: ReplyBody::Catalog(CatalogPage {
                server: ServerIdentity::from_u128(3),
                home: project.id,
                revision: 1,
                projects: vec![ProjectDescriptor {
                    home: true,
                    folders: Vec::new(),
                    folder_entries: Vec::new(),
                    auto_subfolders: false,
                    ..project.clone()
                }],
                next: None,
                legacy_home: Some(project.id),
            }),
        },
        Message::Reply {
            id: RequestId(2),
            body: ReplyBody::ProjectMutated { revision: 1 },
        },
        Message::Reply {
            id: RequestId(4),
            body: ReplyBody::CreationCancelled,
        },
    ];
    samples.extend(project_session_samples(project.id, project.directory));
    samples.extend(changed_samples());
    samples.push(prune_worktree_sample());
    samples.extend(folder_group_samples());
    samples
}

fn folder_group_samples() -> Vec<Message> {
    let entry = crate::FolderEntry {
        id: crate::FolderEntryId::from_u128(3),
        name: "Client".into(),
        directory: ServerPath(b"/tmp/client".to_vec()),
        command: "claude \"Review this client\"".into(),
    };
    let mut group = crate::ProjectDescriptor::new(ServerPath(b"/tmp".to_vec()), []);
    group.id = crate::ProjectId::from_u128(1);
    group.name = "Clients".into();
    group.color = "#808080".into();
    group.kind = Some(crate::ProjectKind::FolderGroup);
    group.auto_subfolders = true;
    group.folder_entries = vec![entry.clone()];
    [
        crate::ProjectMutation::Create(group),
        crate::ProjectMutation::Patch {
            project: crate::ProjectId::from_u128(1),
            patch: crate::ProjectPatch::FolderEntries(vec![entry]),
        },
        crate::ProjectMutation::Patch {
            project: crate::ProjectId::from_u128(1),
            patch: crate::ProjectPatch::HomeDirectory(ServerPath(b"/tmp/home".to_vec())),
        },
        crate::ProjectMutation::Patch {
            project: crate::ProjectId::from_u128(1),
            patch: crate::ProjectPatch::Directory(ServerPath(b"/tmp/project".to_vec())),
        },
        crate::ProjectMutation::Patch {
            project: crate::ProjectId::from_u128(1),
            patch: crate::ProjectPatch::FolderSource {
                directory: ServerPath(b"/tmp/clients".to_vec()),
                auto_subfolders: true,
            },
        },
    ]
    .into_iter()
    .map(|mutation| Message::Request {
        id: RequestId(7),
        body: RequestBody::MutateProject(crate::ProjectIntent {
            operation: crate::OperationId::from_u128(2),
            mutation,
        }),
    })
    .collect()
}

fn prune_worktree_sample() -> Message {
    Message::Request {
        id: RequestId(6),
        body: RequestBody::MutateProject(crate::ProjectIntent {
            operation: crate::OperationId::from_u128(2),
            mutation: crate::ProjectMutation::PruneWorktree(crate::ProjectId::from_u128(1)),
        }),
    }
}

fn close_samples() -> Vec<Message> {
    let operation = crate::OperationId::from_u128(2);
    vec![
        Message::Request {
            id: RequestId(41),
            body: RequestBody::AttachWithoutResize(SessionId::from(NonZeroU64::MIN)),
        },
        Message::Request {
            id: RequestId(99),
            body: RequestBody::SyncSessionReferences {
                owner: Some(operation),
                revision: 1,
                sessions: vec![SessionId::from(NonZeroU64::MIN)],
            },
        },
        Message::Request {
            id: RequestId(99),
            body: RequestBody::CloseSession {
                session: SessionId::from(NonZeroU64::MIN),
                operation,
            },
        },
        Message::Reply {
            id: RequestId(99),
            body: ReplyBody::SessionReferencesSynced,
        },
        Message::Reply {
            id: RequestId(99),
            body: ReplyBody::SessionClosed,
        },
        Message::Request {
            id: RequestId(99),
            body: RequestBody::MoveSession {
                session: SessionId::from(NonZeroU64::MIN),
                project: crate::ProjectId::from_u128(1),
            },
        },
        Message::Reply {
            id: RequestId(99),
            body: ReplyBody::SessionMoved,
        },
    ]
}

fn git_samples() -> Vec<Message> {
    vec![
        Message::GitChanged {
            project: crate::ProjectId::from_u128(1),
        },
        Message::Request {
            id: RequestId(100),
            body: RequestBody::Git(crate::GitRequest {
                project: crate::ProjectId::from_u128(1),
                action: crate::GitAction::Summary,
            }),
        },
        Message::Request {
            id: RequestId(101),
            body: RequestBody::Git(crate::GitRequest {
                project: crate::ProjectId::from_u128(1),
                action: crate::GitAction::BranchDiff {
                    base: "main".into(),
                    line_limit: Some(800),
                },
            }),
        },
        Message::Request {
            id: RequestId(104),
            body: RequestBody::Git(crate::GitRequest {
                project: crate::ProjectId::from_u128(1),
                action: crate::GitAction::Status {
                    local: false,
                    fresh: true,
                },
            }),
        },
        Message::Request {
            id: RequestId(103),
            body: RequestBody::Git(crate::GitRequest {
                project: crate::ProjectId::from_u128(1),
                action: crate::GitAction::PullRequest(crate::GitPullRequestAction::UpdateBranch {
                    number: 42,
                    expected_head: "def456".into(),
                }),
            }),
        },
        Message::Reply {
            id: RequestId(100),
            body: ReplyBody::Git(crate::GitReply::Changes(vec![crate::GitFile {
                path: ServerPath(b"file\xff".to_vec()),
                original_path: None,
                index: b'?',
                worktree: b'?',
                added: None,
                removed: None,
            }])),
        },
        Message::Reply {
            id: RequestId(104),
            body: ReplyBody::Git(crate::GitReply::Diff(crate::GitDiff {
                rows: Vec::new(),
                additions: 0,
                deletions: 0,
                truncated: false,
                binary: true,
            })),
        },
    ]
}

/// Samples for reviewing and applying AI-drafted commits and pull requests.
fn git_review_samples() -> Vec<Message> {
    let destination = crate::GitPushDestination {
        remote: "origin".into(),
        branch: "feature".into(),
    };
    vec![
        Message::Request {
            id: RequestId(102),
            body: RequestBody::Git(crate::GitRequest {
                project: crate::ProjectId::from_u128(1),
                action: crate::GitAction::ChangesPreview {
                    line_limit: Some(800),
                },
            }),
        },
        Message::Reply {
            id: RequestId(102),
            body: ReplyBody::Git(crate::GitReply::ChangesPreview(Box::new(
                crate::GitChangesPreview {
                    branch: Some("feature".into()),
                    head: Some("abc123".into()),
                    tree: "def456".into(),
                    diff: crate::GitRawDiff {
                        diff: "diff --git a/file b/file\n".into(),
                        truncated: false,
                    },
                    files: vec![crate::GitPreviewFile {
                        path: ServerPath(b"file".to_vec()),
                        added: Some(1),
                        removed: Some(0),
                        untracked: true,
                    }],
                    destination: Some(destination.clone()),
                },
            ))),
        },
        Message::Request {
            id: RequestId(104),
            body: RequestBody::Git(crate::GitRequest {
                project: crate::ProjectId::from_u128(1),
                action: crate::GitAction::CommitAll {
                    message: "Message".into(),
                    expected_head: Some("abc123".into()),
                    expected_tree: "def456".into(),
                },
            }),
        },
        Message::Request {
            id: RequestId(107),
            body: RequestBody::Git(crate::GitRequest {
                project: crate::ProjectId::from_u128(1),
                action: crate::GitAction::StagedPreview {
                    line_limit: Some(800),
                },
            }),
        },
        Message::Request {
            id: RequestId(108),
            body: RequestBody::Git(crate::GitRequest {
                project: crate::ProjectId::from_u128(1),
                action: crate::GitAction::CommitStaged {
                    message: "Message".into(),
                    expected_head: Some("abc123".into()),
                    expected_tree: "def456".into(),
                },
            }),
        },
        Message::Request {
            id: RequestId(105),
            body: RequestBody::Git(crate::GitRequest {
                project: crate::ProjectId::from_u128(1),
                action: crate::GitAction::PublishBranch {
                    branch: "feature".into(),
                    destination,
                },
            }),
        },
        Message::Request {
            id: RequestId(106),
            body: RequestBody::Git(crate::GitRequest {
                project: crate::ProjectId::from_u128(1),
                action: crate::GitAction::SwitchToBase("main".into()),
            }),
        },
        Message::Reply {
            id: RequestId(106),
            body: ReplyBody::Git(crate::GitReply::BaseSwitch(
                crate::GitBaseSwitch::CheckedOutElsewhere(ServerPath(b"/tmp/main".to_vec())),
            )),
        },
    ]
}

fn activity_samples(session: SessionId) -> Vec<Message> {
    vec![
        Message::Changed {
            topic: crate::Topic::Activity,
            revision: 42,
        },
        Message::Request {
            id: RequestId(1),
            body: RequestBody::ReadActivity,
        },
        Message::Request {
            id: RequestId(2),
            body: RequestBody::AcknowledgeActivity(vec![1]),
        },
        Message::Request {
            id: RequestId(3),
            body: RequestBody::ClaimActivity(vec![1]),
        },
        Message::Reply {
            id: RequestId(1),
            body: ReplyBody::Activity(crate::ActivitySnapshot {
                revision: 42,
                agents: vec![crate::AgentActivity {
                    session,
                    project: crate::ProjectId::from_u128(1),
                    provider: crate::AgentProvider::Claude,
                    state: crate::AgentState::Blocked,
                }],
                events: vec![crate::ActivityEvent {
                    id: 1,
                    session,
                    project: crate::ProjectId::from_u128(1),
                    provider: crate::AgentProvider::Claude,
                    kind: crate::ActivityKind::Attention,
                    read: false,
                    timestamp: 123,
                }],
            }),
        },
        Message::Reply {
            id: RequestId(2),
            body: ReplyBody::ActivityAcknowledged,
        },
        Message::Reply {
            id: RequestId(3),
            body: ReplyBody::ActivityClaimed(vec![1]),
        },
    ]
}

fn files_samples() -> Vec<Message> {
    vec![
        Message::Request {
            id: RequestId(9),
            body: RequestBody::ListFolders(ServerPath(b"/home/dev".to_vec())),
        },
        Message::Reply {
            id: RequestId(9),
            body: ReplyBody::Folders(vec![ServerPath(b"code".to_vec())]),
        },
        Message::Request {
            id: RequestId(10),
            body: RequestBody::Upload(crate::UploadChunk {
                session: SessionId::from(NonZeroU64::MIN),
                upload: crate::OperationId::from_u128(1),
                name: "screenshot.png".into(),
                offset: 0,
                bytes: b"\x89PNG".to_vec(),
                last: true,
            }),
        },
        Message::Reply {
            id: RequestId(10),
            body: ReplyBody::Uploaded(Some(ServerPath(
                b"/home/dev/.local/state/muxy/uploads/1/screenshot.png".to_vec(),
            ))),
        },
        Message::Request {
            id: RequestId(1),
            body: RequestBody::Files(crate::FilesRequest {
                project: crate::ProjectId::from_u128(1),
                action: crate::FilesAction::List(ServerPath(Vec::new())),
                root: Some(ServerPath(b"/srv/shared".to_vec())),
            }),
        },
        Message::Reply {
            id: RequestId(1),
            body: ReplyBody::Files(crate::FilesReply::Entries(vec![crate::FileEntry {
                name: ServerPath(b"file".to_vec()),
                path: ServerPath(b"file".to_vec()),
                is_directory: false,
                is_ignored: false,
            }])),
        },
        Message::FilesChanged {
            project: crate::ProjectId::from_u128(1),
            changes: crate::FileChanges {
                paths: vec![ServerPath(b"file".to_vec())],
                rescan: false,
                root: Some(ServerPath(b"/srv/shared".to_vec())),
            },
        },
        Message::Request {
            id: RequestId(2),
            body: RequestBody::Files(crate::FilesRequest {
                project: crate::ProjectId::from_u128(1),
                action: crate::FilesAction::ReadBytes(ServerPath(b"logo.png".to_vec())),
                root: None,
            }),
        },
        Message::Reply {
            id: RequestId(2),
            body: ReplyBody::Files(crate::FilesReply::Bytes(crate::FileBytes {
                path: ServerPath(b"logo.png".to_vec()),
                bytes: vec![0x89, b'P', b'N', b'G', 0xff],
            })),
        },
        Message::Request {
            id: RequestId(3),
            body: RequestBody::Files(crate::FilesRequest {
                project: crate::ProjectId::from_u128(1),
                action: crate::FilesAction::WriteBytes {
                    path: ServerPath(b"logo.png".to_vec()),
                    bytes: vec![0x89, b'P', b'N', b'G', 0xff],
                },
                root: None,
            }),
        },
    ]
}

fn acknowledged_input_samples() -> Vec<Message> {
    let channel = ChannelId(1);
    vec![
        Message::Request {
            id: RequestId(1),
            body: RequestBody::WriteInput {
                channel,
                bytes: b"pwd\r".to_vec(),
            },
        },
        Message::Reply {
            id: RequestId(1),
            body: ReplyBody::InputWritten,
        },
    ]
}

fn exec_samples() -> Vec<Message> {
    vec![
        Message::Request {
            id: RequestId(90),
            body: RequestBody::Exec(crate::ExecRequest {
                job: 1,
                project: crate::ProjectId::from_u128(1),
                argv: vec!["git".into(), "status".into()],
                shell: None,
                cwd: None,
                env: std::collections::BTreeMap::new(),
                stdin: Vec::new(),
                timeout_ms: 30000,
            }),
        },
        Message::Request {
            id: RequestId(91),
            body: RequestBody::CancelExec(1),
        },
        Message::Reply {
            id: RequestId(90),
            body: ReplyBody::Exec(crate::ExecResult {
                stdout: "output".into(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                truncated: false,
                cancelled: false,
            }),
        },
        Message::Reply {
            id: RequestId(91),
            body: ReplyBody::ExecCancelled,
        },
    ]
}

fn chat_samples() -> Vec<Message> {
    use crate::{ChatBatch, ChatEvent, ChatItemKind, ChatMode, PermissionRemoved};

    let session = SessionId::from(NonZeroU64::MIN);
    let generation = 1;
    let item = chat_item;
    let state = chat_state();
    let tool_prompt = chat_tool_prompt();
    let snapshot = chat_snapshot(state.clone(), tool_prompt.clone());
    let events = [
        ChatEvent::Upsert(item("msg_02:0", ChatItemKind::User { text: "go".into() })),
        ChatEvent::Delta {
            item_id: "msg_02:0".into(),
            text: " on".into(),
        },
        ChatEvent::State(state),
        ChatEvent::PermissionAdded(tool_prompt),
        ChatEvent::PermissionRemoved {
            request_id: "3f2f5616-2a4c-4504-8c51-5bc270050845".into(),
            reason: PermissionRemoved::Cancelled,
        },
        ChatEvent::Resync,
        ChatEvent::Error {
            text: "write failed".into(),
        },
        ChatEvent::Exited {
            reason: "exit 1: boom".into(),
        },
        ChatEvent::Blocked {
            reason: "Claude did not exit".into(),
        },
    ];
    let mut messages = vec![Message::Chat(ChatBatch {
        session,
        generation,
        from_rev: 0,
        to_rev: 1,
        events: Vec::new(),
        snapshot: Some(Box::new(snapshot)),
    })];
    messages.extend(events.into_iter().zip(1..).map(|(event, from_rev)| {
        Message::Chat(ChatBatch {
            session,
            generation,
            from_rev,
            to_rev: from_rev + 1,
            events: vec![event],
            snapshot: None,
        })
    }));
    messages.push(Message::Metadata(MetadataEvent::ChatMode {
        mode: ChatMode::Chat,
        generation,
    }));
    messages.extend(chat_request_samples(session, generation));
    messages
}

#[allow(
    clippy::too_many_lines,
    reason = "Wire fixture covers each request variant in one table"
)]
fn chat_request_samples(session: SessionId, generation: u64) -> Vec<Message> {
    use crate::{
        ChatAnswer, ChatImage, ChatImageType, ChatRequest, PermissionResponse, PlanDecision,
        ToolDecision,
    };

    let mut messages = Vec::new();
    let operation = crate::OperationId::from_u128(7);
    let features = vec![
        crate::Feature::CHAT_PROVIDERS,
        crate::Feature::CHAT_OPERATIONS,
        crate::Feature::CHAT_NATIVE_APPROVALS,
        crate::Feature::CHAT_QUESTIONS,
    ];
    let selection = crate::ChatSelection {
        provider: crate::ChatProvider::Codex,
        model: Some("native-model".into()),
        effort: Some("high".into()),
        target: None,
        launch_context: Some(8),
    };
    let requests = [
        ChatRequest::Discover {
            session,
            provider: crate::ChatProvider::Codex,
            features: features.clone(),
        },
        ChatRequest::PrepareSubmission { session },
        ChatRequest::Begin {
            session,
            host_epoch: 3,
            operation,
            selection: Box::new(selection.clone()),
            text: "hello".into(),
            images: Vec::new(),
            features: features.clone(),
        },
        ChatRequest::EnterProvider {
            session,
            selection: Box::new(selection),
            features,
        },
        ChatRequest::Submit {
            session,
            generation,
            operation,
            text: "hello again".into(),
            images: Vec::new(),
        },
        ChatRequest::OperationStatus { session, operation },
        ChatRequest::LeaveProvider {
            session,
            generation,
            interrupt: false,
        },
        ChatRequest::Respond {
            session,
            generation,
            request_id: "native-request".into(),
            response: PermissionResponse::NativeChoice { choice: "0".into() },
        },
        ChatRequest::Enter {
            session,
            model: Some("opus".into()),
            effort: None,
        },
        ChatRequest::Start {
            session,
            model: None,
            effort: Some("high".into()),
        },
        ChatRequest::Leave {
            session,
            generation,
        },
        ChatRequest::Send {
            session,
            generation,
            text: "look".into(),
            images: vec![ChatImage {
                mime: ChatImageType::Png,
                bytes: b"\x89PNG\r\n\x1a\n".to_vec(),
            }],
        },
        ChatRequest::Interrupt {
            session,
            generation,
        },
        ChatRequest::Respond {
            session,
            generation,
            request_id: "3f2f5616-2a4c-4504-8c51-5bc270050845".into(),
            response: PermissionResponse::Tool(ToolDecision::Deny {
                message: "not now".into(),
                interrupt: true,
            }),
        },
        ChatRequest::Respond {
            session,
            generation,
            request_id: "dismiss-request".into(),
            response: PermissionResponse::Dismiss {
                message: "not supported here".into(),
            },
        },
        ChatRequest::Respond {
            session,
            generation,
            request_id: "question-request".into(),
            response: PermissionResponse::Questions(vec![ChatAnswer {
                question: "Which color do you pick?".into(),
                answers: vec!["Red".into(), "Blue".into()],
            }]),
        },
        ChatRequest::Respond {
            session,
            generation,
            request_id: "plan-request".into(),
            response: PermissionResponse::Plan(PlanDecision::KeepPlanning {
                feedback: "smaller steps".into(),
            }),
        },
        ChatRequest::Respond {
            session,
            generation,
            request_id: "plan-approve-request".into(),
            response: PermissionResponse::Plan(PlanDecision::Approve {
                mode: Some(crate::PermissionMode::AcceptEdits),
            }),
        },
        ChatRequest::SetModel {
            session,
            generation,
            model: "sonnet".into(),
        },
        ChatRequest::SetEffort {
            session,
            generation,
            effort: "max".into(),
        },
        ChatRequest::Subscribe {
            session,
            subscription: 3,
            features: vec![crate::Feature::TERMINAL_INPUT],
        },
        ChatRequest::Unsubscribe { session },
    ];
    messages.extend(requests.into_iter().map(|request| Message::Request {
        id: RequestId(60),
        body: RequestBody::Chat(request),
    }));
    messages.extend(chat_reply_samples(generation));
    messages
}

fn chat_reply_samples(generation: u64) -> Vec<Message> {
    use crate::{ChatRefusal, ChatReply};

    let replies = [
        ChatReply::Discovered {
            host_epoch: 3,
            provider: crate::ChatProvider::Codex,
            capabilities: crate::ChatCapabilities::default(),
            model: None,
            effort: None,
            threads: vec![crate::ChatThread {
                target: crate::ChatTarget {
                    provider: crate::ChatProvider::Codex,
                    thread_id: "bde3d35b-d3c0-49d7-93c1-9a0391781702".into(),
                    home: ServerPath(b"/tmp/codex".to_vec()),
                    endpoint: Some(ServerPath(b"/tmp/codex/engine.sock".to_vec())),
                    executable: ServerPath(b"/usr/local/bin/codex".to_vec()),
                    engine: Some(crate::ChatEngine {
                        pid: 42,
                        started_seconds: 7,
                        started_micros: 8,
                    }),
                },
                title: "Native conversation".into(),
                updated_at: 7,
                busy: false,
            }],
            launch_context: Some(8),
        },
        ChatReply::Prepared {
            host_epoch: 3,
            operation: crate::OperationId::from_u128(7),
        },
        ChatReply::Submission(crate::ChatSubmission {
            operation: crate::OperationId::from_u128(7),
            outcome: crate::SubmissionOutcome::Uncertain,
            generation,
            detail: Some("Check native history".into()),
        }),
        ChatReply::Entered { generation },
        ChatReply::Left {
            resumed: false,
            detail: Some("the folder no longer exists".into()),
        },
        ChatReply::Ok,
        ChatReply::Subscribed {
            generation,
            subscription: 3,
        },
        ChatReply::Refused {
            reason: ChatRefusal::NotChat,
            detail: None,
        },
    ];
    replies
        .into_iter()
        .map(|reply| Message::Reply {
            id: RequestId(60),
            body: ReplyBody::Chat(reply),
        })
        .collect()
}

fn chat_item(id: &str, kind: crate::ChatItemKind) -> crate::ChatItem {
    crate::ChatItem {
        id: id.into(),
        turn_id: "turn-1".into(),
        seq: 1,
        kind,
    }
}

fn chat_state() -> crate::ChatState {
    crate::ChatState {
        provider: None,
        shared_engine: false,
        target: None,
        busy: true,
        quiescent: false,
        model: Some("opus".into()),
        effort: Some("high".into()),
        permission_mode: crate::PermissionMode::AcceptEdits,
        context_percent: Some(12),
    }
}

fn chat_tool_prompt() -> crate::PermissionPrompt {
    crate::PermissionPrompt {
        request_id: "3f2f5616-2a4c-4504-8c51-5bc270050845".into(),
        tool_use_id: Some("toolu_01".into()),
        kind: crate::PermissionKind::Tool {
            tool: "Bash".into(),
            title: "Run a command".into(),
            detail: "rm -rf build".into(),
            always: Some(crate::AlwaysAllow {
                label: "Bash(rm:*)".into(),
                saved_to_settings: true,
            }),
        },
    }
}

fn chat_snapshot(
    state: crate::ChatState,
    tool_prompt: crate::PermissionPrompt,
) -> crate::ChatSnapshot {
    use crate::{
        ChatCapabilities, ChatItemKind, ChatModel, ChatQuestion, ChatQuestionOption, ChatSnapshot,
        PermissionKind, PermissionPrompt,
    };

    let item = chat_item;
    ChatSnapshot {
        subscription: 3,
        items: vec![
            item("user-uuid", ChatItemKind::User { text: "hi".into() }),
            item(
                "msg_01:0",
                ChatItemKind::Assistant {
                    markdown: "**Hello**".into(),
                    final_in_turn: false,
                },
            ),
            item("msg_01:1", ChatItemKind::Thinking { text: "hmm".into() }),
            item(
                "toolu_01",
                ChatItemKind::Tool {
                    name: "Read".into(),
                    summary: "Read lib.rs".into(),
                    input_preview: "{\"file_path\":\"lib.rs\"}".into(),
                    result: Some("//! Docs".into()),
                    subagent_count: 0,
                    is_error: true,
                },
            ),
            item("folded-1", ChatItemKind::Folded { count: 4 }),
            item(
                "turn-1:end",
                ChatItemKind::TurnEnd {
                    duration_ms: 1200,
                    is_error: false,
                    changed: vec![crate::FileChange {
                        path: "/repo/src/lib.rs".into(),
                        added: 4,
                        removed: 1,
                    }],
                },
            ),
            item(
                "notice-1",
                ChatItemKind::Notice {
                    text: "Untested Claude Code version".into(),
                },
            ),
        ],
        state,
        capabilities: ChatCapabilities {
            models: vec![ChatModel {
                images: None,
                default_effort: None,
                alias: "opus".into(),
                label: "Opus".into(),
                efforts: vec!["low".into(), "high".into(), "max".into()],
            }],
            commands: vec![
                crate::ChatCommand {
                    name: "compact".into(),
                    description: "Free up context".into(),
                    skill: false,
                },
                crate::ChatCommand {
                    name: "codex:review".into(),
                    description: "Review with Codex".into(),
                    skill: true,
                },
            ],
        },
        pending: vec![
            tool_prompt,
            PermissionPrompt {
                request_id: "question-request".into(),
                tool_use_id: None,
                kind: PermissionKind::Questions {
                    questions: vec![ChatQuestion {
                        question: "Which color do you pick?".into(),
                        header: "Color".into(),
                        options: vec![ChatQuestionOption {
                            label: "Red".into(),
                            description: "Pick red".into(),
                            preview: "+-----+\n| red |\n+-----+".into(),
                        }],
                        multi_select: false,
                        allow_other: None,
                    }],
                },
            },
            PermissionPrompt {
                request_id: "plan-request".into(),
                tool_use_id: None,
                kind: PermissionKind::Plan {
                    markdown: "1. Do it".into(),
                },
            },
        ],
        has_earlier: true,
    }
}
