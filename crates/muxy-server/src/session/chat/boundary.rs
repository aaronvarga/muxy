//! Provider-independent transcript changes. Native adapters retain their request IDs and encoders.

use muxy_protocol::{
    ChatCapabilities, ChatItemKind, ChatState, PermissionPrompt, PermissionRemoved,
};

#[derive(Debug)]
pub(crate) enum Change {
    Item {
        turn: String,
        id: String,
        kind: ChatItemKind,
    },
    Delta {
        turn: String,
        id: String,
        text: String,
        thinking: bool,
    },
    End {
        turn: String,
        duration_ms: u64,
        is_error: bool,
    },
    State(ChatState),
    Capabilities(ChatCapabilities),
    Pending(PermissionPrompt),
    Resolved {
        id: String,
        reason: PermissionRemoved,
    },
    Notice(String),
}
