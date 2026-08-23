use pebble_core::{PebbleError, ProviderType, Result};
use pebble_mail::{GmailProvider, ImapProvider, OutlookProvider};

use crate::state::AppState;

use super::{connect_gmail, connect_imap, connect_outlook};

/// A connected provider, ready for remote message mutations.
///
/// This mirrors the Tauri adapter so lifecycle and pending-op code can stay
/// structurally comparable while Web keeps its own credential transport.
pub(in crate::commands) enum ConnectedProvider {
    Gmail(GmailProvider),
    Outlook(OutlookProvider),
    Imap(ImapProvider),
}

impl ConnectedProvider {
    pub async fn connect(
        state: &AppState,
        account_id: &str,
        provider_type: &ProviderType,
    ) -> Result<Self> {
        match provider_type {
            ProviderType::Gmail => Ok(Self::Gmail(connect_gmail(state, account_id).await?)),
            ProviderType::Outlook => Ok(Self::Outlook(connect_outlook(state, account_id).await?)),
            ProviderType::Imap => Ok(Self::Imap(connect_imap(state, account_id).await?)),
            ProviderType::Pop3 => Err(PebbleError::UnsupportedProvider(
                "POP3 does not support remote message mutations".to_string(),
            )),
        }
    }

    pub async fn disconnect(&self) {
        if let Self::Imap(imap) = self {
            let _ = imap.disconnect().await;
        }
    }
}

pub(in crate::commands) fn parse_imap_uid(remote_id: &str) -> Result<u32> {
    remote_id
        .parse::<u32>()
        .map_err(|_| PebbleError::Internal(format!("Invalid IMAP UID: {remote_id}")))
}
