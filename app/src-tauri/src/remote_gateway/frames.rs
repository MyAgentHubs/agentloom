use super::*;
use zeroize::Zeroizing;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TokenAckAction {
    Ignored,
    Consumed,
    Rejected,
    PairReady(PairReadyFrame),
    /// The rotated `token.put` received an ack, so its response may now be sent.
    RefreshOk(RefreshOkFrame),
    /// The relay rejected a rotated `token.put` carrying a pending refresh response.
    /// `handle_frame` immediately sends `token.refresh.fail{reason:"put_rejected"}` without
    /// `close` and without counting an invalid attempt. It also sets
    /// `RegistryState::resync_required`; the connection loop records a disconnect intent instead
    /// of resending `token.sync` in place, then disconnects after the current read timeout or the
    /// two-second hard deadline so the normal reconnect path can synchronize the new DB generation.
    RefreshDropped {
        request_id: String,
        subject: String,
    },
}

pub(crate) struct InputSendFrame {
    pub session: String,
    pub command_id: String,
    pub text: String,
}

pub(crate) struct InputAnswerFrame {
    pub session: String,
    pub command_id: String,
    pub decision_id: String,
    pub option: String,
}

pub(crate) struct ControlStopFrame {
    pub session: String,
    pub command_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AckOutcome {
    Ok,
    Queued,
    Failed,
}

pub(crate) type InputSendHandler = Box<dyn Fn(InputSendFrame) -> Option<AckOutcome> + Send + Sync>;
pub(crate) type InputAnswerHandler =
    Box<dyn Fn(InputAnswerFrame) -> Option<AckOutcome> + Send + Sync>;
pub(crate) type ControlStopHandler = Box<dyn Fn(ControlStopFrame) -> AckOutcome + Send + Sync>;
pub(crate) type ControlReplayHandler = Box<dyn Fn(&str, &str) -> bool + Send + Sync>;

/// Provisional wire names. The relay and mobile implementations do not pin these fields yet;
/// keep them aligned with the existing `ct`/`n` convention until the protocol is finalized.
pub(crate) struct PairHelloFrame {
    pub room: String,
    pub remote_pub: [u8; 32],
    pub token_ct: String,
    pub token_n: String,
    pub origin_connection_id: String,
}

pub(crate) struct PairAcceptFrame {
    pub room: String,
    pub device_id: String,
    pub k_room_ct: String,
    pub k_room_n: String,
    pub tokens_ct: String,
    pub tokens_n: String,
    pub k_room: Zeroizing<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PairReadyFrame {
    pub room: String,
    pub device_id: String,
    pub ct: String,
    pub n: String,
}

#[derive(Debug)]
pub(crate) enum PairDoneAction {
    Rejected,
    Accepted {
        newly_paired_device_id: Option<String>,
    },
    Ready(PairReadyFrame),
}

#[derive(Clone, Default)]
pub(crate) struct PairDoneFrame {
    pub room: String,
    pub device_id: String,
    pub confirm_ct: Option<String>,
    pub confirm_n: Option<String>,
    pub origin_connection_id: String,
}
