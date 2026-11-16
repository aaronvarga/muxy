use std::collections::BTreeSet;
use std::num::NonZeroU64;

use muxy_protocol::{
    AttachSnapshot, CONTROL, ChannelId, ChannelKind, Cursor, ErrorCode, MAX_PATH, Message,
    MetadataEvent, Modes, ReplyBody, RequestBody, RequestId, Row, Run, ServerPath, SessionId, Size,
    Style, Topic, validate_absolute_path, validate_relative_path,
};

#[test]
fn paths_are_held_to_one_rule_for_where_they_point_nul_bytes_and_length() {
    let longest = |first: u8| ServerPath([vec![first], vec![b'a'; MAX_PATH - 1]].concat());
    let too_long = |first: u8| ServerPath([vec![first], vec![b'a'; MAX_PATH]].concat());
    assert_eq!(validate_absolute_path(&longest(b'/')), Ok(()));
    assert_eq!(validate_relative_path(&longest(b'a')), Ok(()));
    assert_eq!(validate_relative_path(&ServerPath(Vec::new())), Ok(()));
    for path in [
        ServerPath(b"relative".to_vec()),
        ServerPath(Vec::new()),
        ServerPath(b"/with\0nul".to_vec()),
        too_long(b'/'),
    ] {
        assert_eq!(validate_absolute_path(&path), Err(ErrorCode::BadPath));
    }
    for path in [
        ServerPath(b"/absolute".to_vec()),
        ServerPath(b"with\0nul".to_vec()),
        too_long(b'a'),
    ] {
        assert_eq!(validate_relative_path(&path), Err(ErrorCode::BadPath));
    }
}

#[test]
fn every_request_with_a_size_validates_it() {
    for (size, expected) in [
        (Size { cols: 1, rows: 1 }, Ok(())),
        (
            Size {
                cols: 4096,
                rows: 1024,
            },
            Ok(()),
        ),
        (Size { cols: 0, rows: 1 }, Err(ErrorCode::BadSize)),
        (
            Size {
                cols: 1,
                rows: 1025,
            },
            Err(ErrorCode::BadSize),
        ),
    ] {
        for body in [
            RequestBody::CreateSession {
                project: muxy_protocol::ProjectId::from_u128(1),
                operation: muxy_protocol::OperationId::from_u128(2),
                directory: directory(),
                size,
                environment: std::collections::BTreeMap::new(),
                colors: None,
            },
            RequestBody::Attach {
                session: session_id(),
                size,
            },
            RequestBody::Resize {
                channel: ChannelId(1),
                size,
            },
        ] {
            assert_eq!(request(body).validate(), expected);
        }
    }
}

#[test]
fn new_sessions_refuse_environment_a_process_cannot_hold() {
    let create = |environment: &[(&str, &str)]| {
        request(RequestBody::CreateSession {
            project: muxy_protocol::ProjectId::from_u128(1),
            operation: muxy_protocol::OperationId::from_u128(2),
            directory: directory(),
            size: Size { cols: 80, rows: 24 },
            environment: environment
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            colors: None,
        })
        .validate()
    };
    assert_eq!(create(&[]), Ok(()));
    assert_eq!(create(&[("http_proxy", "http://proxy:8080")]), Ok(()));
    for environment in [[("", "value")], [("A=B", "value")], [("NAME", "nul\0")]] {
        assert_eq!(create(&environment), Err(ErrorCode::BadRequest));
    }
    let large = "x".repeat(64 * 1024);
    assert_eq!(create(&[("LARGE", &large)]), Err(ErrorCode::BadRequest));
}

#[test]
#[allow(clippy::too_many_lines)]
fn samples_cover_every_message_variant_once_and_use_the_right_channel() {
    use muxy_protocol::{
        ChatEvent, ChatReply, ChatRequest, FilesAction, FilesReply, GitAction,
        GitPullRequestAction, GitReply, PermissionResponse,
    };

    let mut seen = BTreeSet::new();
    for message in Message::samples() {
        let (name, channel) = match &message {
            Message::Changed { topic, .. } => (
                match topic {
                    Topic::Catalog => "CatalogChanged",
                    Topic::Sessions => "SessionsChanged",
                    Topic::Activity => "ActivityChanged",
                    Topic::RemoteAccess => "RemoteAccessChanged",
                    Topic::Unrecognized(_) => "UnrecognizedChanged",
                },
                ChannelKind::Control,
            ),
            Message::UnsupportedRequest { .. } | Message::UnreadableReply { .. } => {
                panic!("samples are messages a peer sends, not decoding outcomes")
            }
            Message::Request {
                body: RequestBody::Chat(request),
                ..
            } => (
                match request {
                    ChatRequest::Discover { .. } => "ChatDiscover",
                    ChatRequest::PrepareSubmission { .. } => "ChatPrepare",
                    ChatRequest::Begin { .. } => "ChatBegin",
                    ChatRequest::EnterProvider { .. } => "ChatEnterProvider",
                    ChatRequest::Submit { .. } => "ChatSubmit",
                    ChatRequest::OperationStatus { .. } => "ChatOperationStatus",
                    ChatRequest::LeaveProvider { .. } => "ChatLeaveProvider",
                    ChatRequest::Enter { .. } => "ChatEnter",
                    ChatRequest::Start { .. } => "ChatStart",
                    ChatRequest::Leave { .. } => "ChatLeave",
                    ChatRequest::Send { .. } => "ChatSend",
                    ChatRequest::Interrupt { .. } => "ChatInterrupt",
                    ChatRequest::Respond { response, .. } => match response {
                        PermissionResponse::Tool(_) => "ChatRespondTool",
                        PermissionResponse::Questions(_) => "ChatRespondQuestions",
                        PermissionResponse::Plan(muxy_protocol::PlanDecision::Approve {
                            ..
                        }) => "ChatRespondPlanApprove",
                        PermissionResponse::Plan(_) => "ChatRespondPlan",
                        PermissionResponse::NativeChoice { .. } => "ChatNativeChoice",
                        PermissionResponse::Dismiss { .. } => "ChatRespondDismiss",
                    },
                    ChatRequest::SetModel { .. } => "ChatSetModel",
                    ChatRequest::SetEffort { .. } => "ChatSetEffort",
                    ChatRequest::Subscribe { .. } => "ChatSubscribe",
                    ChatRequest::Unsubscribe { .. } => "ChatUnsubscribe",
                },
                ChannelKind::Control,
            ),
            Message::Reply {
                body: ReplyBody::Chat(reply),
                ..
            } => (
                match reply {
                    ChatReply::Discovered { .. } => "ChatDiscovered",
                    ChatReply::Prepared { .. } => "ChatPrepared",
                    ChatReply::Submission(_) => "ChatSubmission",
                    ChatReply::Entered { .. } => "ChatEntered",
                    ChatReply::Left { .. } => "ChatLeft",
                    ChatReply::Ok => "ChatOk",
                    ChatReply::Subscribed { .. } => "ChatSubscribed",
                    ChatReply::Refused { .. } => "ChatRefused",
                },
                ChannelKind::Control,
            ),
            Message::Chat(batch) => (
                match (batch.snapshot.as_deref(), batch.events.as_slice()) {
                    (Some(_), []) => "ChatSnapshot",
                    (None, [ChatEvent::Upsert(_)]) => "ChatUpsert",
                    (None, [ChatEvent::Delta { .. }]) => "ChatDelta",
                    (None, [ChatEvent::State(_)]) => "ChatState",
                    (None, [ChatEvent::PermissionAdded(_)]) => "ChatPermissionAdded",
                    (None, [ChatEvent::PermissionRemoved { .. }]) => "ChatPermissionRemoved",
                    (None, [ChatEvent::Resync]) => "ChatResync",
                    (None, [ChatEvent::Error { .. }]) => "ChatError",
                    (None, [ChatEvent::Exited { .. }]) => "ChatExited",
                    (None, [ChatEvent::Blocked { .. }]) => "ChatBlocked",
                    _ => panic!("chat samples hold one event each"),
                },
                ChannelKind::Control,
            ),
            Message::Metadata(MetadataEvent::ChatMode { .. }) => ("ChatMode", ChannelKind::Session),
            Message::Request {
                body: RequestBody::IdentifyClient(muxy_protocol::ClientKind::Mobile),
                ..
            } => ("IdentifyMobile", ChannelKind::Control),
            Message::Request {
                body: RequestBody::Authenticate(_),
                ..
            } => ("Authenticate", ChannelKind::Control),
            Message::Request {
                body: RequestBody::Pair(_),
                ..
            } => ("Pair", ChannelKind::Control),
            Message::Request {
                body: RequestBody::ReadRemoteAccess,
                ..
            } => ("ReadRemoteAccess", ChannelKind::Control),
            Message::Request {
                body: RequestBody::WriteRemoteAccess(_),
                ..
            } => ("WriteRemoteAccess", ChannelKind::Control),
            Message::Request {
                body: RequestBody::StartPairing,
                ..
            } => ("StartPairing", ChannelKind::Control),
            Message::Request {
                body: RequestBody::StartPairingWithHosts(_),
                ..
            } => ("StartPairingWithHosts", ChannelKind::Control),
            Message::Request {
                body: RequestBody::CancelPairing,
                ..
            } => ("CancelPairing", ChannelKind::Control),
            Message::Request {
                body: RequestBody::ListFolders(_),
                ..
            } => ("ListFolders", ChannelKind::Control),
            Message::Request {
                body: RequestBody::Upload(_),
                ..
            } => ("Upload", ChannelKind::Control),
            Message::Request {
                body: RequestBody::RevokeDevice(_),
                ..
            } => ("RevokeDevice", ChannelKind::Control),
            Message::Request {
                body:
                    RequestBody::RegisterPush(muxy_protocol::PushToken {
                        platform: muxy_protocol::PushPlatform::Apns,
                        ..
                    }),
                ..
            } => ("RegisterApnsPush", ChannelKind::Control),
            Message::Request {
                body: RequestBody::RegisterPush(_),
                ..
            } => ("RegisterFcmPush", ChannelKind::Control),
            Message::Request {
                body: RequestBody::UnregisterPush,
                ..
            } => ("UnregisterPush", ChannelKind::Control),
            Message::Request {
                body: RequestBody::SetPushNotifications(_),
                ..
            } => ("SetPushNotifications", ChannelKind::Control),
            Message::Request {
                body: RequestBody::ResetPush(_),
                ..
            } => ("ResetPush", ChannelKind::Control),
            Message::Request {
                body: RequestBody::DeferImages,
                ..
            } => ("DeferImages", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::ImagesDeferred,
                ..
            } => ("ImagesDeferred", ChannelKind::Control),
            Message::Request {
                body: RequestBody::StreamImages,
                ..
            } => ("StreamImages", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::ImagesStreamed,
                ..
            } => ("ImagesStreamed", ChannelKind::Control),
            Message::Request {
                body: RequestBody::ImageFrames { .. },
                ..
            } => ("ImageFramesRequest", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::ImageFrames(_),
                ..
            } => ("ImageFramesReply", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::PushRegistered(registration),
                ..
            } if registration.key.is_some() => ("PushRegistered", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::PushRegistered(_),
                ..
            } => ("PushDisabled", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::PushUnregistered,
                ..
            } => ("PushUnregistered", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::RemoteAccess(state),
                ..
            } if state.push_notifications => ("RemoteAccessWithPush", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::RemoteAccess(state),
                ..
            } if state.push_kept_until.is_some() => {
                ("RemoteAccessPushPaused", ChannelKind::Control)
            }
            Message::Reply {
                body: ReplyBody::Authenticated,
                ..
            } => ("Authenticated", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::Paired(_),
                ..
            } => ("Paired", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::RemoteAccess(_),
                ..
            } => ("RemoteAccess", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::Pairing(_),
                ..
            } => ("Pairing", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::Error(error),
                ..
            } if error.code == ErrorCode::Unauthorized => ("Unauthorized", ChannelKind::Control),
            Message::Request {
                body: RequestBody::Exec(_),
                ..
            } => ("Exec", ChannelKind::Control),
            Message::Request {
                body: RequestBody::CancelExec(_),
                ..
            } => ("CancelExec", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::Exec(_),
                ..
            } => ("ExecResult", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::ExecCancelled,
                ..
            } => ("ExecCancelled", ChannelKind::Control),
            Message::Request {
                body: RequestBody::ReadActivity,
                ..
            } => ("ReadActivity", ChannelKind::Control),
            Message::Request {
                body: RequestBody::AcknowledgeActivity(_),
                ..
            } => ("AcknowledgeActivity", ChannelKind::Control),
            Message::Request {
                body: RequestBody::ClaimActivity(_),
                ..
            } => ("ClaimActivity", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::Activity(_),
                ..
            } => ("Activity", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::ActivityAcknowledged,
                ..
            } => ("ActivityAcknowledged", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::ActivityClaimed(_),
                ..
            } => ("ActivityClaimed", ChannelKind::Control),
            Message::SessionMetadata { .. } => ("SessionMetadata", ChannelKind::Control),
            Message::Request {
                body: RequestBody::IdentifyClient(_),
                ..
            } => ("IdentifyClient", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::ClientIdentified(_),
                ..
            } => ("ClientIdentified", ChannelKind::Control),
            Message::Request {
                body: RequestBody::ReadCatalog { .. },
                ..
            } => ("ReadCatalog", ChannelKind::Control),
            Message::Request {
                body:
                    RequestBody::MutateProject(muxy_protocol::ProjectIntent {
                        mutation: muxy_protocol::ProjectMutation::PruneWorktree(_),
                        ..
                    }),
                ..
            } => ("PruneWorktree", ChannelKind::Control),
            Message::Request {
                body:
                    RequestBody::MutateProject(muxy_protocol::ProjectIntent {
                        mutation: muxy_protocol::ProjectMutation::Create(project),
                        ..
                    }),
                ..
            } if project.kind == Some(muxy_protocol::ProjectKind::FolderGroup) => {
                ("CreateFolderGroup", ChannelKind::Control)
            }
            Message::Request {
                body:
                    RequestBody::MutateProject(muxy_protocol::ProjectIntent {
                        mutation:
                            muxy_protocol::ProjectMutation::Patch {
                                patch: muxy_protocol::ProjectPatch::FolderEntries(_),
                                ..
                            },
                        ..
                    }),
                ..
            } => ("FolderEntries", ChannelKind::Control),
            Message::Request {
                body:
                    RequestBody::MutateProject(muxy_protocol::ProjectIntent {
                        mutation:
                            muxy_protocol::ProjectMutation::Patch {
                                patch: muxy_protocol::ProjectPatch::FolderSource { .. },
                                ..
                            },
                        ..
                    }),
                ..
            } => ("FolderSource", ChannelKind::Control),
            Message::Request {
                body:
                    RequestBody::MutateProject(muxy_protocol::ProjectIntent {
                        mutation:
                            muxy_protocol::ProjectMutation::Patch {
                                patch: muxy_protocol::ProjectPatch::HomeDirectory(_),
                                ..
                            },
                        ..
                    }),
                ..
            } => ("HomeDirectory", ChannelKind::Control),
            Message::Request {
                body:
                    RequestBody::MutateProject(muxy_protocol::ProjectIntent {
                        mutation:
                            muxy_protocol::ProjectMutation::Patch {
                                patch: muxy_protocol::ProjectPatch::Directory(_),
                                ..
                            },
                        ..
                    }),
                ..
            } => ("ProjectDirectory", ChannelKind::Control),
            Message::Request {
                body: RequestBody::MutateProject(_),
                ..
            } => ("MutateProject", ChannelKind::Control),
            Message::Request {
                body: RequestBody::ListProjectSessions { .. },
                ..
            } => ("ListProjectSessions", ChannelKind::Control),
            Message::Request {
                body: RequestBody::CancelCreation(_),
                ..
            } => ("CancelCreation", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::Catalog(_),
                ..
            } => ("Catalog", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::ProjectMutated { .. },
                ..
            } => ("ProjectMutated", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::ProjectSessions(page),
                ..
            } if page.sessions.iter().any(|session| session.name.is_some()) => {
                ("NamedProjectSessions", ChannelKind::Control)
            }
            Message::Reply {
                body: ReplyBody::ProjectSessions(_),
                ..
            } => ("ProjectSessions", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::CreationCancelled,
                ..
            } => ("CreationCancelled", ChannelKind::Control),

            Message::FilesChanged { .. } => ("FilesChanged", ChannelKind::Control),
            Message::Request {
                body: RequestBody::Files(request),
                ..
            } => (
                match request.action {
                    FilesAction::ReadBytes(_) => "FilesReadBytes",
                    FilesAction::WriteBytes { .. } => "FilesWriteBytes",
                    _ => "FilesRequest",
                },
                ChannelKind::Control,
            ),
            Message::Reply {
                body: ReplyBody::Files(reply),
                ..
            } => (
                match reply {
                    FilesReply::Bytes(_) => "FilesBytesReply",
                    _ => "FilesReply",
                },
                ChannelKind::Control,
            ),
            Message::GitChanged { .. } => ("GitChanged", ChannelKind::Control),
            Message::Request {
                body: RequestBody::SyncSessionReferences { .. },
                ..
            } => ("SyncSessionReferences", ChannelKind::Control),
            Message::Request {
                body: RequestBody::CloseSession { .. },
                ..
            } => ("CloseSession", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::SessionReferencesSynced,
                ..
            } => ("SessionReferencesSynced", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::SessionClosed,
                ..
            } => ("SessionClosed", ChannelKind::Control),
            Message::Request {
                body: RequestBody::MoveSession { .. },
                ..
            } => ("MoveSession", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::SessionMoved,
                ..
            } => ("SessionMoved", ChannelKind::Control),
            Message::Request {
                body: RequestBody::StopServerIfIdle,
                ..
            } => ("StopServerIfIdle", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::ServerBusy,
                ..
            } => ("ServerBusy", ChannelKind::Control),
            Message::Request {
                body: RequestBody::ReadServerSettings,
                ..
            } => ("ReadServerSettings", ChannelKind::Control),
            Message::Request {
                body: RequestBody::WriteServerSettings(_),
                ..
            } => ("WriteServerSettings", ChannelKind::Control),
            Message::Request {
                body: RequestBody::StopServer,
                ..
            } => ("StopServer", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::ServerSettings(_),
                ..
            } => ("ServerSettings", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::ServerSettingsWritten,
                ..
            } => ("ServerSettingsWritten", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::ServerStopping,
                ..
            } => ("ServerStopping", ChannelKind::Control),
            Message::Request {
                body: RequestBody::SetTerminalColors(_),
                ..
            } => ("TerminalColorsRequest", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::TerminalColorsSet,
                ..
            } => ("TerminalColorsReply", ChannelKind::Control),
            Message::Request {
                body: RequestBody::SetSessionColors { .. },
                ..
            } => ("SessionColorsRequest", ChannelKind::Control),
            Message::Request {
                body:
                    RequestBody::SetSessionName {
                        replacing: Some(_), ..
                    },
                ..
            } => ("SessionNameClearRequest", ChannelKind::Control),
            Message::Request {
                body: RequestBody::SetSessionName { .. },
                ..
            } => ("SessionNameRequest", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::SessionNameSet,
                ..
            } => ("SessionNameReply", ChannelKind::Control),
            Message::Request {
                body: RequestBody::WatchSessionMetadata,
                ..
            } => ("WatchSessionMetadata", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::SessionMetadataWatched,
                ..
            } => ("SessionMetadataWatched", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::SessionColorsSet,
                ..
            } => ("SessionColorsReply", ChannelKind::Control),
            Message::Request {
                body:
                    RequestBody::Search {
                        source: muxy_protocol::SearchSource::Live(_),
                        ..
                    },
                ..
            } => ("SearchRequest", ChannelKind::Control),
            Message::Request {
                body:
                    RequestBody::Search {
                        source: muxy_protocol::SearchSource::Saved(_),
                        ..
                    },
                ..
            } => ("SavedSearchRequest", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::SearchPage(_),
                ..
            } => ("SearchReply", ChannelKind::Control),
            Message::ServerRestarting => ("ServerRestarting", ChannelKind::Control),
            Message::Hello { .. } => ("Hello", ChannelKind::Control),
            Message::Request {
                body: RequestBody::HistoryPage { .. },
                ..
            } => ("HistoryRequest", ChannelKind::Control),
            Message::Request {
                body: RequestBody::SavedHistoryPage { .. },
                ..
            } => ("SavedHistoryRequest", ChannelKind::Control),
            Message::Request {
                body: RequestBody::Git(request),
                ..
            } => (
                match &request.action {
                    GitAction::BranchDiff { .. } => "GitBranchDiff",
                    GitAction::ChangesPreview { .. } => "GitChangesPreview",
                    GitAction::CommitAll { .. } => "GitCommitAll",
                    GitAction::StagedPreview { .. } => "GitStagedPreview",
                    GitAction::CommitStaged { .. } => "GitCommitStaged",
                    GitAction::PublishBranch { .. } => "GitPublishBranch",
                    GitAction::SwitchToBase(_) => "GitSwitchToBase",
                    GitAction::Status { .. } => "GitStatus",
                    GitAction::PullRequest(GitPullRequestAction::UpdateBranch { .. }) => {
                        "GitUpdatePrBranch"
                    }
                    _ => "GitRequest",
                },
                ChannelKind::Control,
            ),
            Message::Request {
                body: RequestBody::WriteInput { .. },
                ..
            } => ("WriteInput", ChannelKind::Control),
            Message::Request {
                body: RequestBody::AttachWithoutResize(_),
                ..
            } => ("AttachWithoutResize", ChannelKind::Control),
            Message::Request {
                body: RequestBody::ClearScreen(_),
                ..
            } => ("ClearScreen", ChannelKind::Control),
            Message::Request { .. } => ("Request", ChannelKind::Control),
            Message::FrameAck { .. } => ("FrameAck", ChannelKind::Control),
            Message::HelloReply { .. } => ("HelloReply", ChannelKind::Control),
            Message::VersionUnsupported => ("VersionUnsupported", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::HistoryPage(_),
                ..
            } => ("HistoryReply", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::Attached { snapshot, .. },
                ..
            } if !snapshot.history.is_empty() => ("HistoryAttach", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::Git(reply),
                ..
            } => (
                match reply {
                    GitReply::ChangesPreview(_) => "GitChangesPreviewReply",
                    GitReply::BaseSwitch(_) => "GitBaseSwitchReply",
                    GitReply::Diff(_) => "GitDiffReply",
                    _ => "GitReply",
                },
                ChannelKind::Control,
            ),
            Message::Reply {
                body: ReplyBody::InputWritten,
                ..
            } => ("InputWritten", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::ScreenCleared,
                ..
            } => ("ScreenCleared", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::Folders(_),
                ..
            } => ("Folders", ChannelKind::Control),
            Message::Reply {
                body: ReplyBody::Uploaded(_),
                ..
            } => ("Uploaded", ChannelKind::Control),
            Message::Reply { .. } => ("Reply", ChannelKind::Control),
            Message::SessionEnded { .. } => ("SessionEnded", ChannelKind::Control),
            Message::Fatal(_) => ("Fatal", ChannelKind::Control),
            Message::Input(_) => ("Input", ChannelKind::Session),
            Message::TerminalInput(input) => (
                match input {
                    muxy_protocol::TerminalInput::Key(_) => "KeyInput",
                    muxy_protocol::TerminalInput::Paste(_) => "PasteInput",
                    muxy_protocol::TerminalInput::Focus(_) => "FocusInput",
                    muxy_protocol::TerminalInput::ClearScreen => "ClearInput",
                },
                ChannelKind::Session,
            ),
            Message::Mouse(_) => ("Mouse", ChannelKind::Session),
            Message::Frame(frame) if frame.graphics.is_some() => {
                ("FrameWithImages", ChannelKind::Session)
            }
            Message::Frame(_) => ("Frame", ChannelKind::Session),
            Message::Metadata(MetadataEvent::History { .. }) => {
                ("HistoryMetadata", ChannelKind::Session)
            }
            Message::Metadata(MetadataEvent::ScreenPrompts { .. }) => {
                ("ScreenPrompts", ChannelKind::Session)
            }
            Message::Metadata(MetadataEvent::Links { .. }) => ("Links", ChannelKind::Session),
            Message::Metadata(MetadataEvent::InputModes(_)) => ("InputModes", ChannelKind::Session),
            Message::Metadata(MetadataEvent::CursorBlinking(_)) => {
                ("CursorBlinking", ChannelKind::Session)
            }
            Message::Metadata(MetadataEvent::ClipboardWrite(_)) => {
                ("ClipboardWrite", ChannelKind::Session)
            }
            Message::Metadata(MetadataEvent::AgentSession { .. }) => {
                ("AgentSession", ChannelKind::Session)
            }
            Message::Progress { .. } => ("Progress", ChannelKind::Control),
            Message::Metadata(_) => ("Metadata", ChannelKind::Session),
            Message::CellSize(_) => ("CellSize", ChannelKind::Session),
        };
        assert!(seen.insert(name), "duplicate sample: {name}");
        assert_eq!(message.channel_kind(), channel, "{name}");
        assert_eq!(message.validate(), Ok(()), "{name}");
    }
    assert_eq!(
        seen,
        BTreeSet::from([
            "Exec",
            "CancelExec",
            "ExecResult",
            "ExecCancelled",
            "SessionMetadata",
            "ActivityChanged",
            "ReadActivity",
            "AcknowledgeActivity",
            "ClaimActivity",
            "Activity",
            "ActivityAcknowledged",
            "ActivityClaimed",
            "FilesChanged",
            "FilesRequest",
            "FilesReply",
            "FilesReadBytes",
            "FilesWriteBytes",
            "FilesBytesReply",
            "GitChanged",
            "GitRequest",
            "GitReply",
            "GitDiffReply",
            "GitBranchDiff",
            "GitChangesPreview",
            "GitChangesPreviewReply",
            "GitCommitAll",
            "GitStagedPreview",
            "GitCommitStaged",
            "GitPublishBranch",
            "GitSwitchToBase",
            "GitStatus",
            "GitBaseSwitchReply",
            "GitUpdatePrBranch",
            "CatalogChanged",
            "SessionsChanged",
            "IdentifyClient",
            "ClientIdentified",
            "SyncSessionReferences",
            "CloseSession",
            "SessionReferencesSynced",
            "SessionClosed",
            "MoveSession",
            "SessionMoved",
            "ReadCatalog",
            "MutateProject",
            "CreateFolderGroup",
            "FolderEntries",
            "FolderSource",
            "HomeDirectory",
            "ProjectDirectory",
            "PruneWorktree",
            "ListProjectSessions",
            "CancelCreation",
            "Catalog",
            "ProjectMutated",
            "ProjectSessions",
            "CreationCancelled",
            "CellSize",
            "ReadServerSettings",
            "WriteServerSettings",
            "AttachWithoutResize",
            "WriteInput",
            "InputWritten",
            "ClearScreen",
            "ScreenCleared",
            "StopServer",
            "StopServerIfIdle",
            "ServerBusy",
            "ServerSettings",
            "ServerSettingsWritten",
            "ServerStopping",
            "Hello",
            "ServerRestarting",
            "Request",
            "SearchRequest",
            "SavedSearchRequest",
            "SearchReply",
            "TerminalColorsRequest",
            "TerminalColorsReply",
            "SessionColorsRequest",
            "SessionColorsReply",
            "SessionNameRequest",
            "SessionNameClearRequest",
            "SessionNameReply",
            "WatchSessionMetadata",
            "SessionMetadataWatched",
            "NamedProjectSessions",
            "HistoryRequest",
            "SavedHistoryRequest",
            "HistoryReply",
            "HistoryAttach",
            "HistoryMetadata",
            "ScreenPrompts",
            "Links",
            "FrameAck",
            "HelloReply",
            "VersionUnsupported",
            "Reply",
            "SessionEnded",
            "Fatal",
            "Input",
            "KeyInput",
            "PasteInput",
            "FocusInput",
            "ClearInput",
            "Mouse",
            "InputModes",
            "CursorBlinking",
            "ClipboardWrite",
            "AgentSession",
            "Progress",
            "Frame",
            "Metadata",
            "RemoteAccessChanged",
            "IdentifyMobile",
            "Authenticate",
            "Pair",
            "ReadRemoteAccess",
            "WriteRemoteAccess",
            "StartPairing",
            "StartPairingWithHosts",
            "ListFolders",
            "Folders",
            "Upload",
            "Uploaded",
            "CancelPairing",
            "RevokeDevice",
            "RegisterApnsPush",
            "RegisterFcmPush",
            "UnregisterPush",
            "SetPushNotifications",
            "ResetPush",
            "RemoteAccessPushPaused",
            "PushRegistered",
            "PushDisabled",
            "PushUnregistered",
            "DeferImages",
            "ImagesDeferred",
            "StreamImages",
            "ImagesStreamed",
            "ImageFramesRequest",
            "ImageFramesReply",
            "FrameWithImages",
            "RemoteAccessWithPush",
            "Authenticated",
            "Paired",
            "RemoteAccess",
            "Pairing",
            "Unauthorized",
            "ChatDiscover",
            "ChatPrepare",
            "ChatBegin",
            "ChatEnterProvider",
            "ChatSubmit",
            "ChatOperationStatus",
            "ChatLeaveProvider",
            "ChatNativeChoice",
            "ChatDiscovered",
            "ChatPrepared",
            "ChatSubmission",
            "ChatEnter",
            "ChatStart",
            "ChatLeave",
            "ChatSend",
            "ChatInterrupt",
            "ChatRespondTool",
            "ChatRespondQuestions",
            "ChatRespondPlan",
            "ChatRespondPlanApprove",
            "ChatRespondDismiss",
            "ChatSetModel",
            "ChatSetEffort",
            "ChatSubscribe",
            "ChatUnsubscribe",
            "ChatEntered",
            "ChatLeft",
            "ChatOk",
            "ChatSubscribed",
            "ChatRefused",
            "ChatSnapshot",
            "ChatUpsert",
            "ChatDelta",
            "ChatState",
            "ChatPermissionAdded",
            "ChatPermissionRemoved",
            "ChatResync",
            "ChatError",
            "ChatExited",
            "ChatBlocked",
            "ChatMode",
        ])
    );
}

fn session_id() -> SessionId {
    SessionId::from(NonZeroU64::MIN)
}

fn directory() -> ServerPath {
    ServerPath(b"/tmp".to_vec())
}

fn request(body: RequestBody) -> Message {
    Message::Request {
        id: RequestId(1),
        body,
    }
}

fn reply(body: ReplyBody) -> Message {
    Message::Reply {
        id: RequestId(1),
        body,
    }
}

fn snapshot() -> AttachSnapshot {
    AttachSnapshot {
        links: None,
        graphics: muxy_protocol::Graphics::default(),
        prompts: Vec::new(),
        channel: ChannelId(1),
        size: Size { cols: 1, rows: 1 },
        rows: vec![Row {
            index: 0,
            runs: vec![Run {
                text: " ".to_owned(),
                width: 1,
                style: Style::default(),
            }],
        }],
        cursor: Cursor {
            shape: muxy_protocol::CursorShape::default(),
            row: 0,
            col: 0,
            visible: true,
        },
        modes: Modes::default(),
        title: String::new(),
        directory: directory(),
        history: vec![],
        history_cursor: None,
        history_total: 0,
    }
}

#[test]
fn history_limits_and_page_shape_are_validated() {
    use muxy_protocol::{HistoryCursor, HistoryPage};
    for max_rows in [0, 1, 200, 500, 501, u16::MAX] {
        let expected = if (1..=500).contains(&max_rows) {
            Ok(())
        } else {
            Err(ErrorCode::BadRequest)
        };
        for body in [
            RequestBody::HistoryPage {
                channel: ChannelId(1),
                before: HistoryCursor(0),
                max_rows,
            },
            RequestBody::SavedHistoryPage {
                session: session_id(),
                before: HistoryCursor(12),
                max_rows,
            },
        ] {
            assert_eq!(request(body).validate(), expected);
        }
    }
    assert_eq!(
        request(RequestBody::HistoryPage {
            channel: CONTROL,
            before: HistoryCursor(0),
            max_rows: 500
        })
        .validate(),
        Err(ErrorCode::UnknownChannel)
    );
    let valid = HistoryPage {
        links: None,
        prompts: Vec::new(),
        rows: snapshot().rows,
        next: Some(HistoryCursor(3)),
        total_rows: 5,
        screen: None,
    };
    assert_eq!(
        reply(ReplyBody::HistoryPage(valid.clone())).validate(),
        Ok(())
    );
    for page in [
        HistoryPage {
            next: Some(HistoryCursor(0)),
            ..valid.clone()
        },
        HistoryPage {
            total_rows: 0,
            ..valid.clone()
        },
        HistoryPage {
            prompts: Vec::new(),
            rows: vec![],
            ..valid.clone()
        },
        HistoryPage {
            prompts: Vec::new(),
            rows: vec![Row {
                index: 1,
                runs: vec![],
            }],
            ..valid.clone()
        },
        HistoryPage {
            prompts: Vec::new(),
            rows: vec![Row {
                index: 0,
                runs: vec![Run {
                    text: "x".into(),
                    width: 0,
                    style: Style::default(),
                }],
            }],
            ..valid
        },
    ] {
        assert_eq!(
            reply(ReplyBody::HistoryPage(page)).validate(),
            Err(ErrorCode::BadRequest)
        );
    }
}

#[test]
fn history_links_reject_out_of_page_rows_and_invalid_spans() {
    use muxy_protocol::{HistoryPage, LinkRow, LinkSpan};
    let link = LinkRow {
        row: 0,
        spans: vec![LinkSpan {
            start: 0,
            end: 1,
            uri: "https://example.com".into(),
        }],
    };
    let page = HistoryPage {
        links: Some(vec![link.clone()]),
        prompts: vec![],
        rows: snapshot().rows,
        next: None,
        total_rows: 1,
        screen: None,
    };
    assert!(
        reply(ReplyBody::HistoryPage(page.clone()))
            .validate()
            .is_ok()
    );
    for change in 0..6 {
        let mut invalid = link.clone();
        match change {
            0 => invalid.row = 1,
            1 => invalid.spans[0].end = 0,
            2 => invalid.spans[0].uri.push('\n'),
            3 => invalid.spans[0].uri = "x".repeat(muxy_protocol::MAX_LINK_URI + 1),
            4 => invalid.spans.push(invalid.spans[0].clone()),
            _ => invalid.spans.clear(),
        }
        assert_eq!(
            reply(ReplyBody::HistoryPage(HistoryPage {
                links: Some(vec![invalid.clone()]),
                ..page.clone()
            }))
            .validate(),
            Err(ErrorCode::BadRequest)
        );
        let mut snapshot = snapshot();
        snapshot.links = Some(vec![invalid]);
        assert_eq!(
            reply(ReplyBody::Attached {
                snapshot: Box::new(snapshot),
                process: None
            })
            .validate(),
            Err(ErrorCode::BadRequest)
        );
    }
    let mut page = page;
    page.rows = (0..500)
        .map(|index| Row {
            index,
            runs: vec![],
        })
        .collect();
    page.total_rows = 500;
    let mut screen = snapshot();
    screen.size.rows = muxy_protocol::MAX_ROWS;
    page.screen = Some(muxy_protocol::SavedScreen {
        graphics: screen.graphics,
        size: screen.size,
        rows: (0..screen.size.rows)
            .map(|index| Row {
                index,
                runs: vec![],
            })
            .collect(),
        cursor: screen.cursor,
        reason: None,
    });
    page.links = Some(vec![LinkRow { row: 1523, ..link }]);
    assert!(reply(ReplyBody::HistoryPage(page)).validate().is_ok());
}

#[test]
fn history_and_screen_link_budgets_are_independent_and_bounded()
-> Result<(), Box<dyn std::error::Error>> {
    use muxy_protocol::{HistoryPage, LinkRow, LinkSpan, MAX_LINK_SPANS, SavedScreen};
    let mut snapshot = snapshot();
    snapshot.history = snapshot.rows.clone();
    snapshot.history_total = 1;
    let spans = (0..=MAX_LINK_SPANS)
        .map(|column| {
            Ok(LinkSpan {
                start: u16::try_from(column)?,
                end: u16::try_from(column + 1)?,
                uri: "https://test".into(),
            })
        })
        .collect::<Result<Vec<_>, std::num::TryFromIntError>>()?;
    for (history, screen, expected) in [
        (MAX_LINK_SPANS, MAX_LINK_SPANS, Ok(())),
        (MAX_LINK_SPANS + 1, 1, Err(ErrorCode::BadRequest)),
        (1, MAX_LINK_SPANS + 1, Err(ErrorCode::BadRequest)),
    ] {
        snapshot.links = Some(vec![
            LinkRow {
                row: 0,
                spans: spans[..history].to_vec(),
            },
            LinkRow {
                row: 1,
                spans: spans[..screen].to_vec(),
            },
        ]);
        assert_eq!(
            reply(ReplyBody::Attached {
                snapshot: Box::new(snapshot.clone()),
                process: None,
            })
            .validate(),
            expected
        );
        let mut page = HistoryPage {
            links: snapshot.links.clone(),
            prompts: vec![],
            rows: snapshot.history.clone(),
            next: None,
            total_rows: 1,
            screen: Some(SavedScreen {
                graphics: snapshot.graphics.clone(),
                size: snapshot.size,
                rows: snapshot.rows.clone(),
                cursor: snapshot.cursor,
                reason: None,
            }),
        };
        assert_eq!(
            reply(ReplyBody::HistoryPage(page.clone())).validate(),
            expected
        );
        page.links.as_mut().ok_or("missing links")?.reverse();
        assert_eq!(
            reply(ReplyBody::HistoryPage(page)).validate(),
            Err(ErrorCode::BadRequest)
        );
    }
    Ok(())
}

#[test]
fn session_names_are_trimmed_bounded_and_free_of_control_characters() {
    let project = muxy_protocol::ProjectId::from_u128(1);
    for (name, expected) in [
        (None, Ok(())),
        (Some("API server".to_owned()), Ok(())),
        (
            Some("é".repeat(muxy_protocol::MAX_SESSION_NAME / 2)),
            Ok(()),
        ),
        (Some(String::new()), Err(ErrorCode::BadRequest)),
        (Some(" padded".into()), Err(ErrorCode::BadRequest)),
        (Some("two\nlines".into()), Err(ErrorCode::BadRequest)),
        (Some("bell\u{7}".into()), Err(ErrorCode::BadRequest)),
        (
            Some("x".repeat(muxy_protocol::MAX_SESSION_NAME + 1)),
            Err(ErrorCode::BadRequest),
        ),
    ] {
        assert_eq!(
            request(RequestBody::SetSessionName {
                session: session_id(),
                name: name.clone(),
                replacing: None,
            })
            .validate(),
            expected,
            "{name:?}"
        );
        assert_eq!(
            request(RequestBody::SetSessionName {
                session: session_id(),
                name: None,
                replacing: name.clone(),
            })
            .validate(),
            expected,
            "replacing {name:?}"
        );
        let listed = reply(ReplyBody::ProjectSessions(muxy_protocol::ProjectSessions {
            revision: 1,
            sessions: vec![muxy_protocol::ProjectSession {
                info: muxy_protocol::SessionInfo {
                    project,
                    id: session_id(),
                    directory: directory(),
                },
                status: muxy_protocol::SessionStatus::Live,
                owner: None,
                attached: false,
                name: name.clone(),
            }],
            next: None,
        }));
        assert_eq!(listed.validate(), expected, "{name:?}");
    }
}

#[test]
fn clipboard_writes_require_nonempty_bounded_text() {
    for (text, expected) in [
        ("copied text".into(), Ok(())),
        ("x".repeat(muxy_protocol::MAX_CLIPBOARD_TEXT), Ok(())),
        (String::new(), Err(ErrorCode::BadRequest)),
        (
            "x".repeat(muxy_protocol::MAX_CLIPBOARD_TEXT + 1),
            Err(ErrorCode::BadRequest),
        ),
    ] {
        assert_eq!(
            Message::Metadata(MetadataEvent::ClipboardWrite(text)).validate(),
            expected
        );
    }
}

#[test]
fn graphics_reject_overflow_truncation_missing_images_and_invalid_crops() {
    use muxy_protocol::{GraphicImage, GraphicPlacement, Graphics};
    let mut graphics = Graphics {
        images: vec![GraphicImage {
            id: 1,
            generation: 1,
            width: 1,
            height: 1,
            rgba: vec![255; 4].into(),
            ..GraphicImage::default()
        }],
        placements: vec![GraphicPlacement {
            image: 1,
            id: 1,
            column: 0,
            row: -1,
            offset: [0, 0],
            size: [8, 16],
            source: [0, 0, 1, 1],
            z: -1,
        }],
        ..Graphics::default()
    };
    assert!(graphics.validate().is_ok());
    graphics.images[0].width = u32::MAX;
    graphics.images[0].height = u32::MAX;
    assert!(graphics.validate().is_err());
    graphics.images[0].width = 2;
    graphics.images[0].height = 1;
    assert!(graphics.validate().is_err());
    graphics.images[0].width = 1;
    graphics.placements[0].source[0] = u32::MAX;
    assert!(graphics.validate().is_err());
    graphics.placements[0].source[0] = 0;
    graphics.placements[0].image = 2;
    assert!(graphics.validate().is_err());
}

#[test]
fn graphics_accept_deferred_pixels_and_check_animation_frames() {
    use muxy_protocol::{GraphicFrame, GraphicImage, Graphics, MAX_GRAPHICS_FRAMES};
    let frame = |key| GraphicFrame { key, gap: 40 };
    let mut graphics = Graphics {
        images: vec![GraphicImage {
            id: 1,
            generation: 9,
            width: 2,
            height: 2,
            frames: vec![frame(1), frame(2)],
            ..GraphicImage::default()
        }],
        ..Graphics::default()
    };
    assert!(graphics.validate().is_ok());
    assert_eq!(graphics.images[0].frame_keys().collect::<Vec<_>>(), [1, 2]);
    graphics.images[0].playback.frame = 2;
    assert!(graphics.validate().is_err());
    graphics.images[0].playback.frame = 1;
    graphics.images[0].frames = vec![frame(1); MAX_GRAPHICS_FRAMES + 1];
    assert!(graphics.validate().is_err());
    graphics.images[0].frames.clear();
    assert!(graphics.validate().is_err());
    graphics.images[0].playback.frame = 0;
    assert_eq!(graphics.images[0].frame_keys().collect::<Vec<_>>(), [9]);
    graphics.images[0].width = 4096;
    graphics.images[0].height = 4096;
    assert!(graphics.validate().is_err());
}

#[test]
fn image_frame_requests_and_replies_are_bounded() {
    use muxy_protocol::{ImageFrame, MAX_GRAPHICS_BYTES, MAX_GRAPHICS_FRAMES};
    let ask = |channel, keys| {
        request(RequestBody::ImageFrames {
            channel,
            image: 1,
            keys,
        })
        .validate()
    };
    assert!(ask(ChannelId(1), vec![7]).is_ok());
    assert_eq!(ask(CONTROL, vec![7]), Err(ErrorCode::UnknownChannel));
    assert_eq!(ask(ChannelId(1), Vec::new()), Err(ErrorCode::BadRequest));
    assert_eq!(
        ask(ChannelId(1), vec![7; MAX_GRAPHICS_FRAMES + 1]),
        Err(ErrorCode::BadRequest)
    );
    let frame = |width: u32, height: u32, bytes: usize| ImageFrame {
        key: 7,
        width,
        height,
        rgba: vec![0; bytes].into(),
    };
    let answer = |frames| reply(ReplyBody::ImageFrames(frames)).validate();
    assert!(answer(Vec::new()).is_ok());
    assert!(answer(vec![frame(1, 2, 8), frame(1, 1, 4)]).is_ok());
    assert!(answer(vec![frame(1, 2, 4)]).is_err());
    assert!(answer(vec![frame(0, 0, 0)]).is_err());
    let half = MAX_GRAPHICS_BYTES / 2 + 4;
    let side = u32::try_from(half / 4).unwrap_or_default();
    assert!(answer(vec![frame(side, 1, half), frame(side, 1, half)]).is_err());
}

#[test]
fn project_folders_must_be_separate_normalized_and_only_on_top_level_projects() {
    use muxy_protocol::{ProjectDescriptor, ProjectId, ProjectKind, ServerPath};
    let path = |text: &str| ServerPath(text.as_bytes().to_vec());
    let project = |folders: &[&str]| ProjectDescriptor {
        folders: folders.iter().map(|folder| path(folder)).collect(),
        ..ProjectDescriptor::new(path("/work/app"), [])
    };
    assert!(project(&["/work/lib", "/shared"]).validate().is_ok());
    for folders in [
        &["/work/app"][..],
        &["/work/app/src"],
        &["/work"],
        &["/"],
        &["/a", "/a/b"],
        &["/a", "/a"],
        &["relative"],
        &["/a/"],
        &["/a/../work/app"],
        &["/a/./b"],
    ] {
        assert!(project(folders).validate().is_err(), "{folders:?}");
    }
    assert!(project(&["/work/application"]).validate().is_ok());
    let mut worktree = project(&["/shared"]);
    worktree.kind = Some(ProjectKind::Worktree);
    worktree.parent_id = Some(ProjectId::new());
    assert!(worktree.validate().is_err());
}

fn chat(events: Vec<muxy_protocol::ChatEvent>) -> Message {
    Message::Chat(muxy_protocol::ChatBatch {
        session: session_id(),
        generation: 1,
        from_rev: 0,
        to_rev: 1,
        events,
        snapshot: None,
    })
}

fn chat_with_snapshot(
    snapshot: Box<muxy_protocol::ChatSnapshot>,
    events: Vec<muxy_protocol::ChatEvent>,
) -> Message {
    Message::Chat(muxy_protocol::ChatBatch {
        session: session_id(),
        generation: 1,
        from_rev: 0,
        to_rev: 1,
        events,
        snapshot: Some(snapshot),
    })
}

fn chat_snapshot(
    pending: Vec<muxy_protocol::PermissionPrompt>,
    models: usize,
) -> Box<muxy_protocol::ChatSnapshot> {
    Box::new(muxy_protocol::ChatSnapshot {
        subscription: 1,
        items: Vec::new(),
        state: muxy_protocol::ChatState::default(),
        capabilities: muxy_protocol::ChatCapabilities {
            models: (0..models)
                .map(|index| muxy_protocol::ChatModel {
                    images: None,
                    default_effort: None,
                    alias: format!("model-{index}"),
                    label: "Model".into(),
                    efforts: vec!["high".into()],
                })
                .collect(),
            commands: Vec::new(),
        },
        pending,
        has_earlier: false,
    })
}

fn prompt(kind: muxy_protocol::PermissionKind) -> muxy_protocol::PermissionPrompt {
    muxy_protocol::PermissionPrompt {
        request_id: "request".into(),
        tool_use_id: None,
        kind,
    }
}

#[test]
fn chat_items_and_approvals_over_their_caps_are_rejected() {
    use muxy_protocol::{
        ChatEvent, ChatItem, ChatItemKind, ChatQuestion, MAX_CHAT_PLAN, MAX_CHAT_PREVIEW,
        MAX_CHAT_QUESTIONS, MAX_CHAT_TEXT, MAX_CHAT_TOOL_PROMPT, PermissionKind,
    };

    let upsert = |kind| {
        chat(vec![ChatEvent::Upsert(ChatItem {
            id: "item".into(),
            turn_id: "turn".into(),
            seq: 1,
            kind,
        })])
        .validate()
    };
    let markdown = |length| ChatItemKind::Assistant {
        markdown: "x".repeat(length),
        final_in_turn: true,
    };
    let tool = |length| ChatItemKind::Tool {
        name: "Bash".into(),
        summary: String::new(),
        input_preview: String::new(),
        result: Some("x".repeat(length)),
        subagent_count: 0,
        is_error: false,
    };
    assert_eq!(upsert(markdown(MAX_CHAT_TEXT)), Ok(()));
    assert_eq!(
        upsert(markdown(MAX_CHAT_TEXT + 1)),
        Err(ErrorCode::BadRequest)
    );
    assert_eq!(upsert(tool(MAX_CHAT_PREVIEW)), Ok(()));
    assert_eq!(
        upsert(tool(MAX_CHAT_PREVIEW + 1)),
        Err(ErrorCode::BadRequest)
    );
    let changed = |count, length| ChatItemKind::TurnEnd {
        duration_ms: 1,
        is_error: false,
        changed: vec![
            muxy_protocol::FileChange {
                path: "x".repeat(length),
                added: 1,
                removed: 0,
            };
            count
        ],
    };
    let files = muxy_protocol::MAX_CHAT_FILE_CHANGES;
    assert_eq!(upsert(changed(files, MAX_CHAT_PREVIEW)), Ok(()));
    assert_eq!(upsert(changed(files + 1, 1)), Err(ErrorCode::BadRequest));
    assert_eq!(
        upsert(changed(1, MAX_CHAT_PREVIEW + 1)),
        Err(ErrorCode::BadRequest)
    );

    let added = |kind| chat(vec![ChatEvent::PermissionAdded(prompt(kind))]).validate();
    let plan = |length| PermissionKind::Plan {
        markdown: "x".repeat(length),
    };
    let questions = |length| PermissionKind::Questions {
        questions: vec![ChatQuestion {
            allow_other: None,
            question: "x".repeat(length),
            header: String::new(),
            options: Vec::new(),
            multi_select: false,
        }],
    };
    // Title and detail share one cap.
    let tool_prompt = |length| PermissionKind::Tool {
        tool: "Bash".into(),
        title: "x".repeat(length / 2),
        detail: "x".repeat(length - length / 2),
        always: None,
    };
    for (kind, cap) in [
        (plan as fn(usize) -> PermissionKind, MAX_CHAT_PLAN),
        (questions, MAX_CHAT_QUESTIONS),
        (tool_prompt, MAX_CHAT_TOOL_PROMPT),
    ] {
        assert_eq!(added(kind(cap)), Ok(()));
        assert_eq!(added(kind(cap + 1)), Err(ErrorCode::BadRequest));
    }
}

#[test]
fn question_previews_have_their_own_cap_and_count_toward_the_total() {
    use muxy_protocol::{ChatEvent, ChatQuestion, MAX_CHAT_QUESTIONS, PermissionKind};

    let added = |kind| chat(vec![ChatEvent::PermissionAdded(prompt(kind))]).validate();
    // A preview has its own cap and counts toward the questions' total.
    let preview = |length, options, questions| PermissionKind::Questions {
        questions: vec![
            ChatQuestion {
                allow_other: None,
                question: String::new(),
                header: String::new(),
                options: vec![
                    muxy_protocol::ChatQuestionOption {
                        label: String::new(),
                        description: String::new(),
                        preview: "x".repeat(length),
                    };
                    options
                ],
                multi_select: false,
            };
            questions
        ],
    };
    let cap = muxy_protocol::MAX_CHAT_QUESTION_PREVIEW;
    assert_eq!(added(preview(cap, 1, 1)), Ok(()));
    assert_eq!(added(preview(cap + 1, 1, 1)), Err(ErrorCode::BadRequest));
    // Each preview fits, but together they pass the questions' total.
    let options = MAX_CHAT_QUESTIONS / cap / 2 + 1;
    assert_eq!(added(preview(cap, options, 2)), Err(ErrorCode::BadRequest));
}

#[test]
fn chat_snapshots_bound_pending_approvals_and_models_and_travel_alone() {
    use muxy_protocol::{
        ChatEvent, MAX_CHAT_EVENTS, MAX_CHAT_MODELS, MAX_CHAT_PLAN, PermissionKind,
    };

    let plans = |count| {
        (0..count)
            .map(|_| {
                prompt(PermissionKind::Plan {
                    markdown: "x".repeat(MAX_CHAT_PLAN),
                })
            })
            .collect()
    };
    // Four plans at their cap fill the 1 MiB pending budget exactly.
    assert_eq!(
        chat_with_snapshot(chat_snapshot(plans(4), 1), Vec::new()).validate(),
        Ok(())
    );
    assert_eq!(
        chat_with_snapshot(chat_snapshot(plans(5), 1), Vec::new()).validate(),
        Err(ErrorCode::BadRequest)
    );
    assert_eq!(
        chat_with_snapshot(chat_snapshot(Vec::new(), MAX_CHAT_MODELS), Vec::new()).validate(),
        Ok(())
    );
    assert_eq!(
        chat_with_snapshot(chat_snapshot(Vec::new(), MAX_CHAT_MODELS + 1), Vec::new()).validate(),
        Err(ErrorCode::BadRequest)
    );
    assert_eq!(
        chat_with_snapshot(chat_snapshot(Vec::new(), 1), vec![ChatEvent::Resync]).validate(),
        Err(ErrorCode::BadRequest)
    );
    assert_eq!(
        chat(vec![ChatEvent::Resync; MAX_CHAT_EVENTS]).validate(),
        Ok(())
    );
    assert_eq!(
        chat(vec![ChatEvent::Resync; MAX_CHAT_EVENTS + 1]).validate(),
        Err(ErrorCode::BadRequest)
    );
    let backwards = Message::Chat(muxy_protocol::ChatBatch {
        session: session_id(),
        generation: 1,
        from_rev: 2,
        to_rev: 1,
        events: Vec::new(),
        snapshot: None,
    });
    assert_eq!(backwards.validate(), Err(ErrorCode::BadRequest));
}

#[test]
fn chat_subscriptions_bound_their_feature_list() {
    use muxy_protocol::{ChatRequest, Feature, MAX_CHAT_FEATURES};

    let subscribe = |count| {
        request(RequestBody::Chat(ChatRequest::Subscribe {
            session: session_id(),
            subscription: 1,
            features: vec![Feature::AGENT_CHAT; count],
        }))
        .validate()
    };
    assert_eq!(subscribe(MAX_CHAT_FEATURES), Ok(()));
    assert_eq!(subscribe(MAX_CHAT_FEATURES + 1), Err(ErrorCode::BadRequest));
}

#[test]
fn chat_prompts_refuse_too_many_large_or_mislabelled_images() {
    use muxy_protocol::{
        ChatImage, ChatImageType, ChatRequest, MAX_CHAT_IMAGE_BYTES, MAX_CHAT_IMAGES, MAX_CHAT_TEXT,
    };

    let png = |length: usize| {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.resize(length, 0);
        ChatImage {
            mime: ChatImageType::Png,
            bytes,
        }
    };
    let send = |text: String, images| {
        request(RequestBody::Chat(ChatRequest::Send {
            session: session_id(),
            generation: 1,
            text,
            images,
        }))
        .validate()
    };
    assert_eq!(
        send("x".repeat(MAX_CHAT_TEXT), vec![png(16); MAX_CHAT_IMAGES]),
        Ok(())
    );
    for (text, images) in [
        ("x".repeat(MAX_CHAT_TEXT + 1), Vec::new()),
        (String::new(), vec![png(16); MAX_CHAT_IMAGES + 1]),
        (String::new(), vec![png(MAX_CHAT_IMAGE_BYTES + 1)]),
        // Each within its own cap, but 15 MiB together.
        (String::new(), vec![png(MAX_CHAT_IMAGE_BYTES); 3]),
    ] {
        assert_eq!(send(text, images), Err(ErrorCode::BadRequest));
    }
    for (mime, bytes) in [
        (ChatImageType::Jpeg, b"\x89PNG\r\n\x1a\n".to_vec()),
        (ChatImageType::Png, b"\xff\xd8\xff\xe0".to_vec()),
        (ChatImageType::Webp, b"RIFF\0\0\0\0WAVE".to_vec()),
        (ChatImageType::Gif, b"GIF8".to_vec()),
    ] {
        assert_eq!(
            send(String::new(), vec![ChatImage { mime, bytes }]),
            Err(ErrorCode::BadRequest),
            "{mime:?}"
        );
    }
    for (mime, bytes) in [
        (ChatImageType::Jpeg, b"\xff\xd8\xff\xe0".to_vec()),
        (ChatImageType::Gif, b"GIF89a".to_vec()),
        (ChatImageType::Webp, b"RIFF\0\0\0\0WEBPVP8 ".to_vec()),
    ] {
        assert_eq!(
            send(String::new(), vec![ChatImage { mime, bytes }]),
            Ok(()),
            "{mime:?}"
        );
    }
}

#[test]
fn chat_answers_are_capped_by_count_not_only_by_bytes() {
    use muxy_protocol::{ChatAnswer, ChatRequest, MAX_CHAT_QUESTION_ENTRIES, PermissionResponse};

    let respond = |count: usize| {
        request(RequestBody::Chat(ChatRequest::Respond {
            session: session_id(),
            generation: 1,
            request_id: "request".into(),
            response: PermissionResponse::Questions(vec![
                ChatAnswer {
                    question: String::new(),
                    answers: Vec::new(),
                };
                count
            ]),
        }))
        .validate()
    };
    let one_answer = |count: usize| {
        request(RequestBody::Chat(ChatRequest::Respond {
            session: session_id(),
            generation: 1,
            request_id: "request".into(),
            response: PermissionResponse::Questions(vec![ChatAnswer {
                question: String::new(),
                answers: vec![String::new(); count],
            }]),
        }))
        .validate()
    };
    assert_eq!(one_answer(MAX_CHAT_QUESTION_ENTRIES), Ok(()));
    assert_eq!(
        one_answer(MAX_CHAT_QUESTION_ENTRIES + 1),
        Err(ErrorCode::BadRequest)
    );
    assert_eq!(respond(MAX_CHAT_QUESTION_ENTRIES), Ok(()));
    // Empty answers weigh nothing in bytes, but a peer could still send millions.
    assert_eq!(
        respond(MAX_CHAT_QUESTION_ENTRIES + 1),
        Err(ErrorCode::BadRequest)
    );
}

#[test]
fn chat_commands_are_bounded_typeable_words() {
    use muxy_protocol::{
        ChatCommand, MAX_CHAT_COMMAND_DESCRIPTION, MAX_CHAT_COMMAND_NAME, MAX_CHAT_COMMANDS,
    };

    let with = |commands: Vec<ChatCommand>| {
        let mut snapshot = chat_snapshot(Vec::new(), 1);
        snapshot.capabilities.commands = commands;
        chat_with_snapshot(snapshot, Vec::new()).validate()
    };
    let command = |name: &str, description: usize| ChatCommand {
        name: name.into(),
        description: "d".repeat(description),
        skill: false,
    };
    let longest = "n".repeat(MAX_CHAT_COMMAND_NAME);
    assert_eq!(
        with(vec![
            command("codex:review", MAX_CHAT_COMMAND_DESCRIPTION),
            command(&longest, 0)
        ]),
        Ok(())
    );
    assert_eq!(with(vec![command("compact", 0); MAX_CHAT_COMMANDS]), Ok(()));
    for bad in [
        vec![command("compact", 0); MAX_CHAT_COMMANDS + 1],
        vec![command(&format!("{longest}n"), 0)],
        vec![command("compact", MAX_CHAT_COMMAND_DESCRIPTION + 1)],
        // A name must be one typed word: not empty, no spaces, no control or non-ASCII bytes.
        vec![command("", 0)],
        vec![command("Robinhood:market overview (MCP)", 0)],
        vec![command("caf\u{e9}", 0)],
        vec![command("a\tb", 0)],
    ] {
        assert_eq!(with(bad), Err(ErrorCode::BadRequest));
    }
}
