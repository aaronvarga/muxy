//! Messages from a newer build: this build skips what it doesn't know and
//! keeps the connection.

use std::error::Error;

use minicbor::Encoder;
use muxy_protocol::wire::{
    Decoder, Encoder as FrameEncoder, HEADER_LEN, Header, MessageKind, WireError, decode, encode,
    legacy_version_unsupported,
};
use muxy_protocol::{
    CONTROL, ChannelId, ErrorCode, ExitReason, Message, MetadataEvent, ProjectId, RequestBody,
    RequestId, SessionId, Topic,
};

/// A frame holding `payload`, as a newer build would write it.
fn frame(kind: u8, channel: ChannelId, payload: &[u8]) -> Result<Vec<u8>, WireError> {
    let mut header = Header::new(payload.len(), channel, MessageKind::Hello)?;
    header.kind = kind;
    let mut bytes = header.to_bytes().to_vec();
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

fn cbor(
    write: impl FnOnce(&mut Encoder<&mut Vec<u8>>) -> Result<(), Box<dyn Error>>,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    write(&mut Encoder::new(&mut bytes))?;
    Ok(bytes)
}

fn decode_one(bytes: &[u8]) -> Result<Option<(ChannelId, Message)>, WireError> {
    let header = Header::from_bytes(
        bytes[..HEADER_LEN]
            .try_into()
            .map_err(|_| WireError::Closed)?,
    )?;
    decode(header, &bytes[HEADER_LEN..])
}

#[test]
fn unknown_kinds_are_skipped_and_the_stream_continues() -> Result<(), Box<dyn Error>> {
    let mut bytes = frame(
        40,
        CONTROL,
        &cbor(|e| {
            e.array(1)?.str("from the future")?;
            Ok(())
        })?,
    )?;
    let ping = Message::Request {
        id: RequestId(1),
        body: RequestBody::Ping,
    };
    FrameEncoder::new(&mut bytes).send(CONTROL, &ping)?;
    assert_eq!(Decoder::new(bytes.as_slice()).next()?, (CONTROL, ping));
    Ok(())
}

#[test]
fn fields_appended_by_a_newer_build_are_skipped() -> Result<(), Box<dyn Error>> {
    let payload = cbor(|e| {
        e.array(3)?
            .encode(Topic::Catalog)?
            .u64(4)?
            .str("newer field")?;
        Ok(())
    })?;
    assert_eq!(
        decode_one(&frame(MessageKind::Changed as u8, CONTROL, &payload)?)?,
        Some((
            CONTROL,
            Message::Changed {
                topic: Topic::Catalog,
                revision: 4
            }
        ))
    );
    Ok(())
}

#[test]
fn unknown_values_of_open_enums_are_kept_by_number() -> Result<(), Box<dyn Error>> {
    let changed = cbor(|e| {
        e.array(2)?.u32(99)?.u64(1)?;
        Ok(())
    })?;
    assert_eq!(
        decode_one(&frame(MessageKind::Changed as u8, CONTROL, &changed)?)?,
        Some((
            CONTROL,
            Message::Changed {
                topic: Topic::Unrecognized(99),
                revision: 1
            }
        ))
    );
    let session = SessionId::new(7).ok_or("zero ID")?;
    let ended = cbor(|e| {
        e.array(2)?.encode(session)?;
        e.array(2)?.u32(8)?.str("a newer exit reason")?;
        Ok(())
    })?;
    assert_eq!(
        decode_one(&frame(MessageKind::SessionEnded as u8, CONTROL, &ended)?)?,
        Some((
            CONTROL,
            Message::SessionEnded {
                session,
                reason: ExitReason::Unrecognized(8)
            }
        ))
    );
    let error = cbor(|e| {
        e.array(1)?.array(2)?.u32(99)?.str("newer error")?;
        Ok(())
    })?;
    assert!(matches!(
        decode_one(&frame(MessageKind::Fatal as u8, CONTROL, &error)?)?,
        Some((_, Message::Fatal(reply))) if reply.code == ErrorCode::Unrecognized(99)
    ));
    Ok(())
}

#[test]
fn requests_for_unknown_methods_are_correlated() -> Result<(), Box<dyn Error>> {
    let payload = cbor(|e| {
        e.array(2)?.encode(RequestId(9))?;
        e.array(2)?.u32(900)?.array(1)?.str("argument")?;
        Ok(())
    })?;
    let bytes = frame(MessageKind::Request as u8, CONTROL, &payload)?;
    assert_eq!(
        decode_one(&bytes)?,
        Some((CONTROL, Message::UnsupportedRequest { id: RequestId(9) }))
    );
    Ok(())
}

#[test]
fn files_actions_and_replies_from_a_newer_build_are_correlated() -> Result<(), Box<dyn Error>> {
    // Files (method 28) holding [project, action] with an action this build doesn't know.
    let request = cbor(|e| {
        e.array(2)?.encode(RequestId(5))?;
        e.array(2)?.u32(28)?.array(1)?;
        e.array(2)?.encode(ProjectId::from_u128(1))?;
        e.array(2)?.u32(99)?.array(1)?.bytes(b"newer")?;
        Ok(())
    })?;
    assert_eq!(
        decode_one(&frame(MessageKind::Request as u8, CONTROL, &request)?)?,
        Some((CONTROL, Message::UnsupportedRequest { id: RequestId(5) }))
    );
    // A files reply (28) of a kind this build doesn't know.
    let reply = cbor(|e| {
        e.array(2)?.encode(RequestId(6))?;
        e.array(2)?.u32(28)?.array(1)?;
        e.array(2)?.u32(99)?.array(1)?.bytes(b"newer")?;
        Ok(())
    })?;
    assert_eq!(
        decode_one(&frame(MessageKind::Reply as u8, CONTROL, &reply)?)?,
        Some((CONTROL, Message::UnreadableReply { id: RequestId(6) }))
    );
    Ok(())
}

#[test]
fn unreadable_reply_bodies_are_correlated() -> Result<(), Box<dyn Error>> {
    let payload = cbor(|e| {
        e.array(2)?.encode(RequestId(4))?;
        e.array(2)?.u32(900)?.array(0)?;
        Ok(())
    })?;
    let bytes = frame(MessageKind::Reply as u8, CONTROL, &payload)?;
    assert_eq!(
        decode_one(&bytes)?,
        Some((CONTROL, Message::UnreadableReply { id: RequestId(4) }))
    );
    Ok(())
}

#[test]
fn malformed_request_bodies_are_still_rejected() -> Result<(), Box<dyn Error>> {
    // A body that claims more items than the payload holds is broken, not newer.
    let payload = [0x82, 0x01, 0x82, 0x18];
    let bytes = frame(MessageKind::Request as u8, CONTROL, &payload)?;
    assert!(matches!(decode_one(&bytes), Err(WireError::Decode(_))));
    Ok(())
}

#[test]
fn events_naming_unknown_variants_are_skipped() -> Result<(), Box<dyn Error>> {
    let payload = cbor(|e| {
        e.array(1)?.array(2)?.u32(99)?.array(0)?;
        Ok(())
    })?;
    let bytes = frame(MessageKind::Metadata as u8, ChannelId(1), &payload)?;
    assert_eq!(decode_one(&bytes)?, None);
    Ok(())
}

#[test]
fn clipboard_write_keeps_its_metadata_variant_number() -> Result<(), Box<dyn Error>> {
    let payload = cbor(|e| {
        e.array(1)?.array(2)?.u32(9)?.array(1)?.str("copied text")?;
        Ok(())
    })?;
    assert_eq!(
        decode_one(&frame(MessageKind::Metadata as u8, ChannelId(1), &payload,)?)?,
        Some((
            ChannelId(1),
            Message::Metadata(MetadataEvent::ClipboardWrite("copied text".into()))
        ))
    );
    Ok(())
}

#[test]
fn builds_before_v2_are_told_to_update_in_their_own_framing() {
    // length 7, version 1, control channel, VersionUnsupported, empty payload.
    assert_eq!(
        legacy_version_unsupported(),
        [7, 0, 0, 0, 1, 0, 0, 0, 0, 0, 5]
    );
}

#[test]
fn mouse_events_naming_newer_buttons_stay_valid() -> Result<(), WireError> {
    use muxy_protocol::{Modifiers, MouseAction, MouseEvent, ScrollDirection};

    let event = Message::Mouse(MouseEvent {
        action: MouseAction::Scroll,
        button: None,
        column: 1,
        row: 1,
        scroll: Some(ScrollDirection::Unrecognized(9)),
        modifiers: Modifiers::default(),
    });
    let mut bytes = Vec::new();
    encode(&event, ChannelId(1), &mut bytes)?;
    let (_, decoded) = Decoder::new(bytes.as_slice()).next()?;
    assert_eq!(decoded, event);
    assert_eq!(decoded.validate(), Ok(()));
    Ok(())
}

#[test]
fn unknown_event_variants_are_ignored_only_when_well_formed() -> Result<(), Box<dyn Error>> {
    let metadata = MessageKind::Metadata as u8;
    // [[99, ...]]: the variant's value is missing.
    let truncated = frame(metadata, ChannelId(1), &[0x81, 0x82, 0x18, 0x63])?;
    assert!(matches!(decode_one(&truncated), Err(WireError::Decode(_))));
    // [[99, []]] followed by a stray byte.
    let trailing = frame(
        metadata,
        ChannelId(1),
        &[0x81, 0x82, 0x18, 0x63, 0x80, 0x00],
    )?;
    assert!(matches!(decode_one(&trailing), Err(WireError::Decode(_))));
    Ok(())
}

#[test]
fn invalid_values_in_known_requests_stay_fatal() -> Result<(), Box<dyn Error>> {
    let request = MessageKind::Request as u8;
    // EndSession (method 6) with session 0, which is never valid.
    let zero_session = cbor(|e| {
        e.array(2)?.encode(RequestId(1))?;
        e.array(2)?.u32(6)?.array(1)?.u64(0)?;
        Ok(())
    })?;
    // Resize (method 9) with 65,536 columns, which overflows u16.
    let overflow = cbor(|e| {
        e.array(2)?.encode(RequestId(2))?;
        e.array(2)?.u32(9)?.array(2)?.u32(1)?;
        e.array(2)?.u64(65_536)?.u16(1)?;
        Ok(())
    })?;
    for payload in [zero_session, overflow] {
        let bytes = frame(request, CONTROL, &payload)?;
        assert!(matches!(decode_one(&bytes), Err(WireError::Decode(_))));
    }
    Ok(())
}

#[test]
fn fork_folder_capability_cannot_alias_upstream_computer_pairing() {
    assert_ne!(
        muxy_protocol::Feature::PROJECT_FOLDERS,
        muxy_protocol::Feature::PAIRED_COMPUTERS
    );
    let unique: std::collections::BTreeSet<_> = muxy_protocol::FEATURES.iter().collect();
    assert_eq!(unique.len(), muxy_protocol::FEATURES.len());
}

#[test]
fn chat_requests_and_replies_from_a_newer_build_are_correlated() -> Result<(), Box<dyn Error>> {
    // Chat (method 1000) holding a chat request this build doesn't know.
    let request = cbor(|e| {
        e.array(2)?.encode(RequestId(7))?;
        e.array(2)?.u32(1000)?.array(1)?;
        e.array(2)?.u32(99)?.array(1)?.str("newer")?;
        Ok(())
    })?;
    assert_eq!(
        decode_one(&frame(MessageKind::Request as u8, CONTROL, &request)?)?,
        Some((CONTROL, Message::UnsupportedRequest { id: RequestId(7) }))
    );
    let reply = cbor(|e| {
        e.array(2)?.encode(RequestId(8))?;
        e.array(2)?.u32(1000)?.array(1)?;
        e.array(2)?.u32(99)?.array(0)?;
        Ok(())
    })?;
    assert_eq!(
        decode_one(&frame(MessageKind::Reply as u8, CONTROL, &reply)?)?,
        Some((CONTROL, Message::UnreadableReply { id: RequestId(8) }))
    );
    Ok(())
}

/// Dropping a whole batch for one newer event would leave a revision gap, and
/// the snapshot after resubscribing would hold the same event again. Skipping
/// the event instead keeps revisions continuous.
#[test]
fn chat_batches_skip_only_events_and_items_a_newer_build_added() -> Result<(), Box<dyn Error>> {
    use muxy_protocol::{
        ChatBatch, ChatCapabilities, ChatEvent, ChatItem, ChatItemKind, ChatSnapshot, ChatState,
    };

    let session = SessionId::new(3).ok_or("zero ID")?;
    let known = ChatItem {
        id: "a".into(),
        turn_id: "t".into(),
        seq: 1,
        kind: ChatItemKind::Notice {
            text: "kept".into(),
        },
    };
    let ordinary = cbor(|e| {
        e.array(1)?
            .array(5)?
            .encode(session)?
            .u64(1)?
            .u64(4)?
            .u64(6)?;
        e.array(2)?;
        e.array(2)?.u32(99)?.array(1)?.str("newer event")?;
        e.encode(ChatEvent::Resync)?;
        Ok(())
    })?;
    assert_eq!(
        decode_one(&frame(MessageKind::Chat as u8, CONTROL, &ordinary)?)?,
        Some((
            CONTROL,
            Message::Chat(ChatBatch {
                session,
                generation: 1,
                from_rev: 4,
                to_rev: 6,
                events: vec![ChatEvent::Resync],
                snapshot: None,
            })
        ))
    );
    let snapshot = cbor(|e| {
        e.array(1)?
            .array(6)?
            .encode(session)?
            .u64(1)?
            .u64(0)?
            .u64(9)?;
        e.array(0)?;
        // [subscription, items, state, capabilities, pending, has_earlier]
        e.array(6)?.u32(2)?;
        e.array(2)?.encode(&known)?;
        e.array(4)?.str("b")?.str("t")?.u64(2)?;
        e.array(2)?.u32(99)?.array(0)?;
        e.encode(ChatState::default())?
            .encode(ChatCapabilities::default())?
            .array(0)?
            .bool(false)?;
        Ok(())
    })?;
    let Some((_, Message::Chat(batch))) =
        decode_one(&frame(MessageKind::Chat as u8, CONTROL, &snapshot)?)?
    else {
        return Err("the snapshot batch was dropped".into());
    };
    assert!(batch.events.is_empty());
    assert_eq!(
        batch.snapshot,
        Some(Box::new(ChatSnapshot {
            subscription: 2,
            items: vec![known],
            state: ChatState::default(),
            capabilities: ChatCapabilities::default(),
            pending: Vec::new(),
            has_earlier: false,
        }))
    );
    Ok(())
}

/// Option previews were added after questions shipped: an older build's option has
/// no preview, and an older build skips the one this build sends.
#[test]
fn question_option_previews_cross_builds_in_both_directions() -> Result<(), Box<dyn Error>> {
    use muxy_protocol::ChatQuestionOption;

    #[derive(Debug, PartialEq, minicbor::Decode)]
    struct OlderOption {
        #[n(0)]
        label: String,
        #[n(1)]
        description: String,
    }

    let older = cbor(|e| {
        e.array(2)?.str("Red")?.str("Pick red")?;
        Ok(())
    })?;
    assert_eq!(
        minicbor::decode::<ChatQuestionOption>(&older)?,
        ChatQuestionOption {
            label: "Red".into(),
            description: "Pick red".into(),
            preview: String::new(),
        }
    );

    let option = ChatQuestionOption {
        label: "Red".into(),
        description: "Pick red".into(),
        preview: "+-----+\n| red |\n+-----+".into(),
    };
    let newer = minicbor::to_vec(&option)?;
    assert_eq!(minicbor::decode::<ChatQuestionOption>(&newer)?, option);
    assert_eq!(
        minicbor::decode::<OlderOption>(&newer)?,
        OlderOption {
            label: "Red".into(),
            description: "Pick red".into(),
        }
    );
    Ok(())
}

/// Slash commands were added to capabilities after chat shipped: an older server's
/// capabilities have none, and an older client skips the ones this build sends.
#[test]
fn chat_commands_cross_builds_in_both_directions() -> Result<(), Box<dyn Error>> {
    use muxy_protocol::{ChatCapabilities, ChatCommand, ChatModel};

    #[derive(Debug, PartialEq, minicbor::Decode)]
    struct OlderCapabilities {
        #[n(0)]
        models: Vec<ChatModel>,
    }

    let model = ChatModel {
        images: None,
        default_effort: None,
        alias: "opus".into(),
        label: "Opus".into(),
        efforts: vec!["high".into()],
    };
    let older = cbor(|e| {
        e.array(1)?.array(1)?.encode(&model)?;
        Ok(())
    })?;
    assert_eq!(
        minicbor::decode::<ChatCapabilities>(&older)?,
        ChatCapabilities {
            models: vec![model.clone()],
            commands: Vec::new(),
        }
    );

    let capabilities = ChatCapabilities {
        models: vec![model.clone()],
        commands: vec![ChatCommand {
            name: "codex:review".into(),
            description: "Review with Codex".into(),
            skill: true,
        }],
    };
    let newer = minicbor::to_vec(&capabilities)?;
    assert_eq!(minicbor::decode::<ChatCapabilities>(&newer)?, capabilities);
    assert_eq!(
        minicbor::decode::<OlderCapabilities>(&newer)?,
        OlderCapabilities {
            models: vec![model],
        }
    );
    Ok(())
}

#[test]
fn provider_state_is_additive_and_legacy_states_keep_claude_defaults() -> Result<(), Box<dyn Error>>
{
    use muxy_protocol::{ChatProvider, ChatState};
    #[derive(minicbor::Decode)]
    struct OlderState {
        #[n(0)]
        busy: bool,
        #[n(1)]
        quiescent: bool,
        #[n(2)]
        model: Option<String>,
    }
    let state = ChatState {
        provider: Some(ChatProvider::Codex),
        shared_engine: true,
        model: Some("native-model".into()),
        quiescent: true,
        ..ChatState::default()
    };
    let encoded = minicbor::to_vec(&state)?;
    let older: OlderState = minicbor::decode(&encoded)?;
    assert!(!older.busy && older.quiescent);
    assert_eq!(older.model.as_deref(), Some("native-model"));
    let legacy = cbor(|encoder| {
        encoder
            .array(6)?
            .bool(false)?
            .bool(true)?
            .null()?
            .null()?
            .u8(0)?
            .null()?;
        Ok(())
    })?;
    let restored: ChatState = minicbor::decode(&legacy)?;
    assert_eq!(restored.provider, None);
    assert!(!restored.shared_engine && restored.target.is_none());
    Ok(())
}
