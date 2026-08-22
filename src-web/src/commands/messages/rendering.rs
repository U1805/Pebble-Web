use pebble_core::{Message, PrivacyMode, RenderedHtml, TrustType};
use pebble_privacy::PrivacyGuard;
use serde_json::Value;

use crate::blocking::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

/// 渲染消息 HTML（隐私模式清洗）。与桌面端一致：信任列表中的发件人可降为 LoadOnce。
pub async fn get_rendered_html(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        message_id: String,
        privacy_mode: PrivacyMode,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_rendered_html args: {e}")))?;
    let store = state.store.clone();
    let rendered = run_blocking(move || {
        let message = store.get_message(&args.message_id)?.ok_or_else(|| {
            pebble_core::PebbleError::Internal(format!("Message not found: {}", args.message_id))
        })?;
        let effective_mode = resolve_privacy_mode(&store, &message, args.privacy_mode)?;
        let guard = PrivacyGuard::new();
        Ok(guard.render_message_html(&message.body_html_raw, &message.body_text, &effective_mode))
    })
    .await?;
    serde_json::to_value(rendered).map_err(ApiError::from_serialize)
}

pub async fn get_message_with_html(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        message_id: String,
        privacy_mode: PrivacyMode,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_message_with_html args: {e}")))?;
    let store = state.store.clone();
    let result = run_blocking(move || {
        let Some(message) = store.get_message(&args.message_id)? else {
            return Ok(None::<(Message, RenderedHtml)>);
        };
        let effective_mode = resolve_privacy_mode(&store, &message, args.privacy_mode)?;
        let guard = PrivacyGuard::new();
        let rendered =
            guard.render_message_html(&message.body_html_raw, &message.body_text, &effective_mode);
        Ok(Some((message, rendered)))
    })
    .await?;
    serde_json::to_value(result).map_err(ApiError::from_serialize)
}

pub async fn is_trusted_sender(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        account_id: String,
        email: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid is_trusted_sender args: {e}")))?;
    let store = state.store.clone();
    let trusted = run_blocking(move || {
        Ok(store
            .is_trusted_sender(&args.account_id, &args.email)?
            .is_some())
    })
    .await?;
    serde_json::to_value(trusted).map_err(ApiError::from_serialize)
}

/// 与桌面端 rendering.rs 相同的隐私模式解析。
fn resolve_privacy_mode(
    store: &pebble_store::Store,
    message: &Message,
    privacy_mode: PrivacyMode,
) -> Result<PrivacyMode, pebble_core::PebbleError> {
    match privacy_mode {
        PrivacyMode::Strict | PrivacyMode::LoadOnce => {
            match store.is_trusted_sender(&message.account_id, &message.from_address)? {
                Some(TrustType::All | TrustType::Images) => Ok(PrivacyMode::LoadOnce),
                None => Ok(privacy_mode),
            }
        }
        PrivacyMode::TrustSender(sender)
            if sender.eq_ignore_ascii_case(message.from_address.trim()) =>
        {
            match store.is_trusted_sender(&message.account_id, &message.from_address)? {
                Some(TrustType::All | TrustType::Images) => Ok(PrivacyMode::LoadOnce),
                None => Ok(PrivacyMode::Strict),
            }
        }
        PrivacyMode::TrustSender(_) => Ok(PrivacyMode::Strict),
        PrivacyMode::Off => Ok(PrivacyMode::Off),
    }
}
