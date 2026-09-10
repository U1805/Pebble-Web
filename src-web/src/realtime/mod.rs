use axum::{
    extract::{
        ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use tokio::sync::broadcast;

use crate::auth;
use crate::state::AppStateRef;

/// WebSocket 事件通道（计划书 §21/§22）。
///
/// 协议（与参考仓库 B 一致）：连接后首条消息为 JWT token 裸文本；
/// 校验通过回 `{"type":"authenticated"}`，失败回 error 并关闭（码 4001）。
/// 此后服务器通过 broadcast 向所有订阅连接推送事件 JSON，
/// 事件 type 与桌面端 Tauri event 名保持一致（mail:sync-progress 等），
/// 供前端调用层 events.ts 统一订阅。
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppStateRef>) -> Response {
    let jwt_secret = state.config.jwt_secret.clone();
    let rx = state.ws_broadcast.subscribe();
    ws.on_upgrade(move |socket| handle_socket(socket, rx, jwt_secret))
}

async fn handle_socket(
    mut socket: WebSocket,
    mut rx: broadcast::Receiver<String>,
    jwt_secret: String,
) {
    let authenticated =
        match tokio::time::timeout(std::time::Duration::from_secs(10), socket.recv()).await {
            Ok(Some(Ok(Message::Text(token)))) => {
                auth::validate_token(token.as_str(), &jwt_secret).is_ok()
            }
            _ => false,
        };

    if !authenticated {
        let _ = socket
            .send(Message::Text(Utf8Bytes::from(
                r#"{"type":"error","message":"unauthorized"}"#.to_string(),
            )))
            .await;
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code: 4001,
                reason: Utf8Bytes::from("unauthorized"),
            })))
            .await;
        return;
    }

    let _ = socket
        .send(Message::Text(Utf8Bytes::from(
            r#"{"type":"authenticated"}"#.to_string(),
        )))
        .await;

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(text) => {
                        if socket.send(Message::Text(Utf8Bytes::from(text))).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}
