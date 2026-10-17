/// Where the shell's link to the bridge service stands. The bridge banner
/// picks its sentence from this state, never from [`LinkStatus::detail`], which
/// carries error text and is only ever shown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LinkState {
    /// Starting up, or the first attempt hasn't finished.
    #[default]
    Connecting,
    Connected,
    /// The connection was lost or couldn't be made; the client keeps trying.
    Retrying,
    /// The client couldn't be started at all; nothing will retry.
    Failed,
    /// The client finished (its sender went away); nothing will retry.
    Stopped,
}

impl LinkState {
    /// The word `BridgeFeed.linkState` carries to QML.
    pub fn token(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Retrying => "retrying",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}

/// The link's [`LinkState`] and the message that goes with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkStatus {
    pub state: LinkState,
    /// A message for the user to read, not to parse: it can carry error text.
    pub detail: String,
}

impl LinkStatus {
    pub(super) fn connecting() -> Self {
        Self {
            state: LinkState::Connecting,
            detail: "Connecting to bridge service…".to_string(),
        }
    }
}
