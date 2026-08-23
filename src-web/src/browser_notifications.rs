use serde_json::json;
use tokio::sync::broadcast;

pub const WEB_NOTIFICATION_EVENT: &str = "web:notification";

pub fn emit_browser_notification(
    broadcast: &broadcast::Sender<String>,
    title: &str,
    body: &str,
    account_id: Option<&str>,
    message_id: Option<&str>,
) {
    let _ = broadcast.send(
        json!({
            "type": WEB_NOTIFICATION_EVENT,
            "payload": {
                "title": title,
                "body": body,
                "account_id": account_id,
                "message_id": message_id,
            },
        })
        .to_string(),
    );
}
