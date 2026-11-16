use minicbor::{Decode, Encode};
use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, Encode, Decode,
)]
#[serde(transparent)]
#[cbor(transparent)]
pub struct Version(#[n(0)] pub u16);

/// Builds before V2. Only recognised so they can be told to update.
pub const V1: Version = Version(1);
/// Fields and variants carry numbers, so additive changes keep the version.
pub const V2: Version = Version(2);
/// Bumped only by a breaking change.
pub const CURRENT: Version = V2;
/// During beta a build speaks only its current version.
pub const SUPPORTED: &[Version] = &[CURRENT];

/// Something a server can do that a client checks before offering it.
///
/// Add one only when a client must change its UI for older servers. Numbers
/// are permanent.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, Encode, Decode,
)]
#[serde(transparent)]
#[cbor(transparent)]
pub struct Feature(#[n(0)] pub u16);

impl Feature {
    /// Server-side key encoding, paste framing, focus reports and ordered screen clearing.
    pub const TERMINAL_INPUT: Self = Self(1);
    /// New sessions start with the environment their request names.
    pub const SESSION_ENVIRONMENT: Self = Self(2);
    /// Another computer can pair as `DeviceKind::Computer`, with desktop rights.
    pub const PAIRED_COMPUTERS: Self = Self(3);
    /// A session can move to another project with `MoveSession`.
    pub const SESSION_MOVE: Self = Self(4);
    /// Paired phones can register for push notifications, and the server
    /// has a switch for them.
    pub const PUSH_NOTIFICATIONS: Self = Self(5);
    /// Fork capability: projects carry extra folders, and file requests can name one.
    /// The high range avoids upstream capability IDs, including computer pairing at 3.
    pub const PROJECT_FOLDERS: Self = Self(0x8001);
    /// Fork capability: folder groups with saved terminal launchers.
    pub const FOLDER_GROUPS: Self = Self(0x8002);
    /// Fork capability: project icons can be up to `MAX_ICON_TEXT` characters.
    pub const TEXT_ICONS: Self = Self(0x8003);
    /// Fork capability: change a top-level project's starting folder.
    pub const PROJECT_DIRECTORY: Self = Self(0x800b);
    /// Fork capability: change the built-in Home project's starting folder.
    pub const HOME_DIRECTORY: Self = Self(0x8004);
    /// Fork capability: a session running Claude Code can switch to chat, with
    /// `RequestBody::Chat` and `Message::Chat`.
    pub const AGENT_CHAT: Self = Self(0x8005);
    /// Fork chat feature a client lists in `ChatRequest::Subscribe`: it can
    /// answer `PermissionKind::Questions`. Tool approvals need no feature.
    pub const CHAT_QUESTIONS: Self = Self(0x8006);
    /// Fork chat feature a client lists in `ChatRequest::Subscribe`: it can
    /// answer `PermissionKind::Plan`.
    pub const CHAT_PLAN: Self = Self(0x8007);
    pub const CHAT_PROVIDERS: Self = Self(0x8008);
    pub const CHAT_OPERATIONS: Self = Self(0x8009);
    pub const CHAT_NATIVE_APPROVALS: Self = Self(0x800a);
}

/// Features this build's server supports.
pub const FEATURES: &[Feature] = &[
    Feature::TERMINAL_INPUT,
    Feature::SESSION_ENVIRONMENT,
    Feature::PAIRED_COMPUTERS,
    Feature::SESSION_MOVE,
    Feature::PUSH_NOTIFICATIONS,
    Feature::PROJECT_FOLDERS,
    Feature::FOLDER_GROUPS,
    Feature::TEXT_ICONS,
    Feature::HOME_DIRECTORY,
    Feature::PROJECT_DIRECTORY,
    #[cfg(target_os = "macos")]
    Feature::AGENT_CHAT,
    #[cfg(target_os = "macos")]
    Feature::CHAT_PROVIDERS,
    #[cfg(target_os = "macos")]
    Feature::CHAT_OPERATIONS,
];
