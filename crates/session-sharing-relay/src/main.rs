use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    str::FromStr,
    sync::Arc,
};

use anyhow::Context;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use session_sharing_protocol::{
    common::{
        AddGuestsResponse, AgentPromptFailureReason, CommandExecutionFailureReason,
        CommandExecutionRequestId, ControlActionFailureReason, ControlActionRequestId,
        FailedToAddGuestsReason, FailedToRemoveGuestReason, FailedToUpdatePendingUserRoleReason,
        FailedToUpdateTeamAccessLevelReason, InputOperationId, InputUpdateFailureReason,
        LinkAccessLevelUpdateResponse, OrderedTerminalEvent, OrderedTerminalEventType,
        ParticipantId, ParticipantInfo, ParticipantList, ParticipantPresenceUpdate, PresenceUpdate,
        ProfileData, RemoveGuestResponse, Role, RoleRequestId, RoleRequestResponse, Scrollback,
        Selection, SessionId, SessionSecret, TeamAccessLevelUpdateResponse,
        UniversalDeveloperInputContext, UniversalDeveloperInputContextUpdate,
        UpdatePendingUserRoleResponse, UserID, WindowSize, WriteToPtyFailureReason,
        WriteToPtyRequestId,
    },
    sharer::{
        self, ReconnectToken, SessionEndedReason as SharerSessionEndedReason, SessionSourceType,
    },
    viewer::{self, FailedToJoinReason, SessionEndedReason as ViewerSessionEndedReason},
};
use tokio::{net::TcpListener, sync::mpsc};
use tower_http::trace::TraceLayer;
use tracing::{debug, info, warn};

type Relay = Arc<Mutex<RelayState>>;
type SharerTx = mpsc::UnboundedSender<sharer::DownstreamMessage>;
type ViewerTx = mpsc::UnboundedSender<viewer::DownstreamMessage>;

#[derive(Parser, Debug)]
#[command(about = "Self-hosted Warp session sharing relay")]
struct Args {
    #[arg(
        long,
        env = "WARP_SESSION_SHARING_RELAY_HOST",
        default_value = "0.0.0.0"
    )]
    host: String,

    #[arg(long, env = "WARP_SESSION_SHARING_RELAY_PORT", default_value_t = 8788)]
    port: u16,
}

#[derive(Default)]
struct RelayState {
    sessions: HashMap<SessionId, Session>,
}

struct Session {
    secret: SessionSecret,
    reconnect_token: ReconnectToken,
    sharer_id: ParticipantId,
    sharer_info: ParticipantInfo,
    sharer_tx: Option<SharerTx>,
    scrollback: Scrollback,
    active_prompt: session_sharing_protocol::common::ActivePrompt,
    window_size: WindowSize,
    init_block_id: session_sharing_protocol::common::BlockId,
    universal_developer_input_context: Option<UniversalDeveloperInputContext>,
    source_type: SessionSourceType,
    event_log: BTreeMap<usize, OrderedTerminalEvent>,
    latest_processed_event_no: Option<usize>,
    viewers: HashMap<ParticipantId, ViewerState>,
    link_access_role: Option<Role>,
    pending_write_to_pty_requests: HashMap<String, ParticipantId>,
}

#[derive(Clone)]
struct ViewerState {
    info: ParticipantInfo,
    role: Role,
    is_present: bool,
    tx: Option<ViewerTx>,
}

#[derive(Serialize)]
struct Health {
    ok: bool,
}

#[derive(Deserialize)]
struct JoinQuery {
    pwd: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "session_sharing_relay=info,tower_http=info".into()),
        )
        .init();

    let args = Args::parse();
    let relay = Relay::default();
    let app = app(relay);
    let addr: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .with_context(|| format!("invalid listen address {}:{}", args.host, args.port))?;

    let listener = TcpListener::bind(addr).await?;
    info!(%addr, "session sharing relay listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn app(relay: Relay) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/sessions/create", get(create_session))
        .route("/sessions/join/{session_id}", get(join_session))
        .route("/sessions/{session_id}/resume", get(resume_session))
        .layer(TraceLayer::new_for_http())
        .with_state(relay)
}

async fn shutdown_signal() {
    if let Err(err) = tokio::signal::ctrl_c().await {
        warn!(%err, "failed to listen for shutdown signal");
    }
}

async fn health() -> Json<Health> {
    Json(Health { ok: true })
}

async fn create_session(ws: WebSocketUpgrade, State(relay): State<Relay>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| create_socket(socket, relay))
}

async fn join_session(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    Query(query): Query<JoinQuery>,
    State(relay): State<Relay>,
) -> impl IntoResponse {
    let Ok(session_id) = SessionId::from_str(&session_id) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let secret = query
        .pwd
        .as_deref()
        .and_then(|pwd| SessionSecret::from_str(pwd).ok());
    ws.on_upgrade(move |socket| join_socket(socket, relay, session_id, secret))
        .into_response()
}

async fn resume_session(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    State(relay): State<Relay>,
) -> impl IntoResponse {
    let Ok(session_id) = SessionId::from_str(&session_id) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    ws.on_upgrade(move |socket| resume_socket(socket, relay, session_id))
        .into_response()
}

async fn create_socket(socket: WebSocket, relay: Relay) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<sharer::DownstreamMessage>();
    let send_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            match message.to_json() {
                Ok(serialized) => {
                    if sink.send(Message::Text(serialized.into())).await.is_err() {
                        break;
                    }
                }
                Err(err) => warn!(%err, "failed to serialize sharer downstream message"),
            }
        }
    });

    let Some(sharer::UpstreamMessage::Initialize(init)) = next_sharer_message(&mut stream).await
    else {
        send_task.abort();
        return;
    };

    let session_id = SessionId::new();
    let secret = SessionSecret::new();
    let reconnect_token = ReconnectToken::new();
    let sharer_id = ParticipantId::new();
    let sharer_uid = participant_uid(&init.user_id);
    let sharer_info = participant_info(
        sharer_id.clone(),
        sharer_uid.clone(),
        "Sharer".to_string(),
        init.input_replica_id.clone(),
        init.selection.clone(),
    );

    {
        let mut relay = relay.lock();
        relay.sessions.insert(
            session_id,
            Session {
                secret: secret.clone(),
                reconnect_token: reconnect_token.clone(),
                sharer_id: sharer_id.clone(),
                sharer_info,
                sharer_tx: Some(tx.clone()),
                scrollback: init.scrollback,
                active_prompt: init.active_prompt,
                window_size: init.window_size,
                init_block_id: init.init_block_id,
                universal_developer_input_context: init.universal_developer_input_context,
                source_type: init.source_type,
                event_log: BTreeMap::new(),
                latest_processed_event_no: None,
                viewers: HashMap::new(),
                link_access_role: Some(Role::Reader),
                pending_write_to_pty_requests: HashMap::new(),
            },
        );
    }

    let _ = tx.send(sharer::DownstreamMessage::SessionInitialized {
        session_id,
        session_secret: secret,
        reconnect_token,
        sharer_id,
        sharer_firebase_uid: sharer_uid,
    });

    info!(%session_id, "created shared session");
    handle_sharer_messages(stream, relay, session_id).await;
    send_task.abort();
}

async fn resume_socket(socket: WebSocket, relay: Relay, session_id: SessionId) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<sharer::DownstreamMessage>();
    let send_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            match message.to_json() {
                Ok(serialized) => {
                    if sink.send(Message::Text(serialized.into())).await.is_err() {
                        break;
                    }
                }
                Err(err) => warn!(%err, "failed to serialize sharer downstream message"),
            }
        }
    });

    let Some(sharer::UpstreamMessage::Reconnect(payload)) = next_sharer_message(&mut stream).await
    else {
        send_task.abort();
        return;
    };

    let reconnect_result = {
        let mut relay = relay.lock();
        let Some(session) = relay.sessions.get_mut(&session_id) else {
            let _ = tx.send(sharer::DownstreamMessage::FailedToReconnect {
                reason: sharer::ReconnectionFailedReason::SessionNotFound,
            });
            send_task.abort();
            return;
        };

        if payload.session_secret != session.secret {
            let _ = tx.send(sharer::DownstreamMessage::FailedToReconnect {
                reason: sharer::ReconnectionFailedReason::WrongPassword,
            });
            send_task.abort();
            return;
        }
        if payload.reconnect_token != session.reconnect_token {
            let _ = tx.send(sharer::DownstreamMessage::FailedToReconnect {
                reason: sharer::ReconnectionFailedReason::WrongReconnectionToken,
            });
            send_task.abort();
            return;
        }

        session.sharer_info.selection = payload.selection;
        session.sharer_tx = Some(tx.clone());
        session.participant_list()
    };

    let latest_processed_event_no = relay
        .lock()
        .sessions
        .get(&session_id)
        .and_then(|session| session.latest_processed_event_no);
    let _ = tx.send(sharer::DownstreamMessage::SessionReconnected {
        last_received_event_no: latest_processed_event_no,
        participant_list: reconnect_result,
    });

    info!(%session_id, "sharer reconnected");
    handle_sharer_messages(stream, relay, session_id).await;
    send_task.abort();
}

async fn join_socket(
    socket: WebSocket,
    relay: Relay,
    session_id: SessionId,
    secret: Option<SessionSecret>,
) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<viewer::DownstreamMessage>();
    let send_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            match message.to_json() {
                Ok(serialized) => {
                    if sink.send(Message::Text(serialized.into())).await.is_err() {
                        break;
                    }
                }
                Err(err) => warn!(%err, "failed to serialize viewer downstream message"),
            }
        }
    });

    let Some(viewer::UpstreamMessage::Initialize(init)) = next_viewer_message(&mut stream).await
    else {
        send_task.abort();
        return;
    };

    let join = {
        let mut relay = relay.lock();
        let Some(session) = relay.sessions.get_mut(&session_id) else {
            let _ = tx.send(viewer::DownstreamMessage::FailedToJoin {
                reason: FailedToJoinReason::SessionNotFound,
            });
            send_task.abort();
            return;
        };

        if secret.as_ref() != Some(&session.secret) {
            let _ = tx.send(viewer::DownstreamMessage::FailedToJoin {
                reason: FailedToJoinReason::WrongPassword,
            });
            send_task.abort();
            return;
        }

        let Some(default_role) = session.link_access_role else {
            let _ = tx.send(viewer::DownstreamMessage::FailedToJoin {
                reason: FailedToJoinReason::SessionNotAccessible,
            });
            send_task.abort();
            return;
        };

        session.join_viewer(init, tx.clone(), default_role)
    };

    let _ = tx.send(join.initial_message);
    for event in join.replay_events {
        let _ = tx.send(viewer::DownstreamMessage::OrderedTerminalEvent(event));
    }
    broadcast_participant_list(&relay, session_id);

    info!(%session_id, viewer_id = %join.viewer_id, "viewer joined shared session");
    handle_viewer_messages(stream, relay.clone(), session_id, join.viewer_id.clone()).await;

    {
        let mut relay = relay.lock();
        if let Some(session) = relay.sessions.get_mut(&session_id) {
            if let Some(viewer) = session.viewers.get_mut(&join.viewer_id) {
                viewer.tx = None;
                viewer.is_present = false;
            }
        }
    }
    broadcast_participant_list(&relay, session_id);
    send_task.abort();
}

async fn handle_sharer_messages(
    mut stream: futures_util::stream::SplitStream<WebSocket>,
    relay: Relay,
    session_id: SessionId,
) {
    while let Some(message) = next_sharer_message(&mut stream).await {
        match message {
            sharer::UpstreamMessage::Initialize(_) | sharer::UpstreamMessage::Reconnect(_) => {
                warn!(%session_id, "unexpected sharer initialize/reconnect after session start");
            }
            sharer::UpstreamMessage::Ping { data } => {
                send_to_sharer(&relay, session_id, sharer::DownstreamMessage::Pong { data });
            }
            sharer::UpstreamMessage::EndSession { reason } => {
                end_session(&relay, session_id, reason);
                break;
            }
            sharer::UpstreamMessage::UpdateActivePrompt(update) => {
                let viewers = {
                    let mut relay = relay.lock();
                    let Some(session) = relay.sessions.get_mut(&session_id) else {
                        break;
                    };
                    session.active_prompt = update.active_prompt.clone();
                    session.viewer_txs()
                };
                for tx in viewers {
                    let _ = tx.send(viewer::DownstreamMessage::ActivePromptUpdated(
                        update.clone(),
                    ));
                }
            }
            sharer::UpstreamMessage::UpdateUniversalDeveloperInputContext(update) => {
                let viewers = {
                    let mut relay = relay.lock();
                    let Some(session) = relay.sessions.get_mut(&session_id) else {
                        break;
                    };
                    apply_context_update(session, update.clone());
                    session.viewer_txs()
                };
                for tx in viewers {
                    let _ = tx.send(
                        viewer::DownstreamMessage::UniversalDeveloperInputContextUpdated(
                            update.clone(),
                        ),
                    );
                }
            }
            sharer::UpstreamMessage::OrderedTerminalEvent(event) => {
                let (viewers, ack_no) = {
                    let mut relay = relay.lock();
                    let Some(session) = relay.sessions.get_mut(&session_id) else {
                        break;
                    };
                    if let OrderedTerminalEventType::Resize { window_size } = &event.event_type {
                        session.window_size = *window_size;
                    }
                    session.latest_processed_event_no = Some(event.event_no);
                    session.event_log.insert(event.event_no, event.clone());
                    (session.viewer_txs(), event.event_no)
                };
                for tx in viewers {
                    let _ = tx.send(viewer::DownstreamMessage::OrderedTerminalEvent(
                        event.clone(),
                    ));
                }
                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::EventsProcessedAck {
                        latest_processed_event_no: ack_no,
                    },
                );
            }
            sharer::UpstreamMessage::UpdateSelection(update) => {
                let presence_update = {
                    let mut relay = relay.lock();
                    let Some(session) = relay.sessions.get_mut(&session_id) else {
                        break;
                    };
                    session.sharer_info.selection = update.selection.clone();
                    ParticipantPresenceUpdate {
                        participant_id: session.sharer_id.clone(),
                        update: PresenceUpdate::Selection(update.selection),
                    }
                };
                broadcast_presence(&relay, session_id, presence_update);
            }
            sharer::UpstreamMessage::UpdateRole {
                participant_id,
                role,
            } => update_participant_role(&relay, session_id, participant_id, role),
            sharer::UpstreamMessage::UpdateUserRole { .. } => {
                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::UpdatePendingUserRoleResponse(
                        UpdatePendingUserRoleResponse::Error(
                            FailedToUpdatePendingUserRoleReason::Invalid,
                        ),
                    ),
                );
            }
            sharer::UpstreamMessage::UpdatePendingUserRole { .. } => {
                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::UpdatePendingUserRoleResponse(
                        UpdatePendingUserRoleResponse::Error(
                            FailedToUpdatePendingUserRoleReason::Invalid,
                        ),
                    ),
                );
            }
            sharer::UpstreamMessage::RespondToRoleRequest {
                participant_id,
                request_id: _,
                response,
            } => {
                if let RoleRequestResponse::Approved { new_role } = &response {
                    update_participant_role(&relay, session_id, participant_id.clone(), *new_role);
                }
                let tx = relay
                    .lock()
                    .sessions
                    .get(&session_id)
                    .and_then(|session| session.viewers.get(&participant_id))
                    .and_then(|viewer| viewer.tx.clone());
                if let Some(tx) = tx {
                    let _ = tx.send(viewer::DownstreamMessage::RoleRequestResponse(response));
                }
            }
            sharer::UpstreamMessage::UpdateAllRolesToReader { reason } => {
                let participant_ids = {
                    let mut relay = relay.lock();
                    let Some(session) = relay.sessions.get_mut(&session_id) else {
                        break;
                    };
                    let ids = session.viewers.keys().cloned().collect::<Vec<_>>();
                    for viewer in session.viewers.values_mut() {
                        viewer.role = Role::Reader;
                    }
                    ids
                };
                for participant_id in participant_ids {
                    broadcast_role_change(
                        &relay,
                        session_id,
                        participant_id,
                        Role::Reader,
                        reason.into(),
                    );
                }
            }
            sharer::UpstreamMessage::UpdateInput(update) => {
                let viewers = relay
                    .lock()
                    .sessions
                    .get(&session_id)
                    .map(Session::viewer_txs)
                    .unwrap_or_default();
                for tx in viewers {
                    let _ = tx.send(viewer::DownstreamMessage::InputUpdated(update.clone()));
                }
            }
            sharer::UpstreamMessage::RejectInputUpdate { id, reason } => {
                let tx = viewer_tx_for_input(&relay, session_id, &id);
                if let Some(tx) = tx {
                    let _ = tx.send(viewer::DownstreamMessage::InputUpdateRejected {
                        id: id.clone(),
                        reason,
                    });
                }
                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::InputUpdateRejectedAck { id },
                );
            }
            sharer::UpstreamMessage::RejectCommandExecutionRequest {
                id,
                participant_id,
                reason,
            } => {
                if let Some(tx) = viewer_tx(&relay, session_id, &participant_id) {
                    let _ = tx.send(viewer::DownstreamMessage::CommandExecutionRequestFailed {
                        id,
                        reason,
                    });
                }
            }
            sharer::UpstreamMessage::RejectWriteToPtyRequest { id, reason } => {
                let participant_id = {
                    let mut relay = relay.lock();
                    relay.sessions.get_mut(&session_id).and_then(|session| {
                        session
                            .pending_write_to_pty_requests
                            .remove(&write_id_key(&id))
                    })
                };
                if let Some(participant_id) = participant_id {
                    if let Some(tx) = viewer_tx(&relay, session_id, &participant_id) {
                        let _ =
                            tx.send(viewer::DownstreamMessage::WriteToPtyRequestFailed { reason });
                    }
                }
            }
            sharer::UpstreamMessage::RejectAgentPromptRequest {
                participant_id,
                reason,
                ..
            } => {
                if let Some(tx) = viewer_tx(&relay, session_id, &participant_id) {
                    let _ = tx.send(viewer::DownstreamMessage::AgentPromptRequestFailed { reason });
                }
            }
            sharer::UpstreamMessage::RejectControlActionRequest {
                participant_id,
                reason,
                ..
            } => {
                if let Some(tx) = viewer_tx(&relay, session_id, &participant_id) {
                    let _ =
                        tx.send(viewer::DownstreamMessage::ControlActionRequestFailed { reason });
                }
            }
            sharer::UpstreamMessage::UpdateLinkAccessLevel { role } => {
                let viewers = {
                    let mut relay = relay.lock();
                    let Some(session) = relay.sessions.get_mut(&session_id) else {
                        break;
                    };
                    session.link_access_role = role;
                    session.viewer_txs()
                };
                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::LinkAccessLevelUpdateResponse(
                        LinkAccessLevelUpdateResponse::Ok { role },
                    ),
                );
                for tx in viewers {
                    let _ = tx.send(viewer::DownstreamMessage::LinkAccessLevelUpdated { role });
                    let _ = tx.send(viewer::DownstreamMessage::LinkAccessLevelUpdateResponse(
                        LinkAccessLevelUpdateResponse::Ok { role },
                    ));
                }
            }
            sharer::UpstreamMessage::UpdateTeamAccessLevel { .. } => {
                reject_sharer_team_access(&relay, session_id);
            }
            sharer::UpstreamMessage::AddGuests { .. } => reject_sharer_guests(&relay, session_id),
            sharer::UpstreamMessage::RemoveGuest { .. }
            | sharer::UpstreamMessage::RemovePendingGuest { .. } => {
                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::RemoveGuestResponse(RemoveGuestResponse::Error(
                        FailedToRemoveGuestReason::Invalid,
                    )),
                );
            }
        }
    }

    let mut relay = relay.lock();
    if let Some(session) = relay.sessions.get_mut(&session_id) {
        session.sharer_tx = None;
    }
}

async fn handle_viewer_messages(
    mut stream: futures_util::stream::SplitStream<WebSocket>,
    relay: Relay,
    session_id: SessionId,
    viewer_id: ParticipantId,
) {
    while let Some(message) = next_viewer_message(&mut stream).await {
        match message {
            viewer::UpstreamMessage::Initialize(_) => {
                warn!(%session_id, %viewer_id, "unexpected viewer initialize after join");
            }
            viewer::UpstreamMessage::Ping { data } => {
                if let Some(tx) = viewer_tx(&relay, session_id, &viewer_id) {
                    let _ = tx.send(viewer::DownstreamMessage::Pong { data });
                }
            }
            viewer::UpstreamMessage::UpdateSelection(update) => {
                let presence_update = {
                    let mut relay = relay.lock();
                    let Some(session) = relay.sessions.get_mut(&session_id) else {
                        break;
                    };
                    let Some(viewer) = session.viewers.get_mut(&viewer_id) else {
                        break;
                    };
                    viewer.info.selection = update.selection.clone();
                    ParticipantPresenceUpdate {
                        participant_id: viewer_id.clone(),
                        update: PresenceUpdate::Selection(update.selection),
                    }
                };
                broadcast_presence(&relay, session_id, presence_update);
            }
            viewer::UpstreamMessage::RequestRole(role) => {
                let request_id = RoleRequestId::new();
                if let Some(tx) = viewer_tx(&relay, session_id, &viewer_id) {
                    let _ = tx.send(viewer::DownstreamMessage::RoleRequestInFlight(
                        request_id.clone(),
                    ));
                }
                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::RoleRequested {
                        participant_id: viewer_id.clone(),
                        request_id,
                        role,
                    },
                );
            }
            viewer::UpstreamMessage::CancelRoleRequest(request_id) => {
                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::RoleRequestCancelled {
                        participant_id: viewer_id.clone(),
                        request_id,
                    },
                );
            }
            viewer::UpstreamMessage::UpdateInput(update) => {
                if !viewer_can_execute(&relay, session_id, &viewer_id) {
                    if let Some(tx) = viewer_tx(&relay, session_id, &viewer_id) {
                        let _ = tx.send(viewer::DownstreamMessage::InputUpdateRejected {
                            id: update.id.clone(),
                            reason: InputUpdateFailureReason::InsufficientPermissions,
                        });
                    }
                    continue;
                }

                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::InputUpdated(update.clone()),
                );
                let viewers = relay
                    .lock()
                    .sessions
                    .get(&session_id)
                    .map(Session::viewer_txs)
                    .unwrap_or_default();
                for tx in viewers {
                    let _ = tx.send(viewer::DownstreamMessage::InputUpdated(update.clone()));
                }
            }
            viewer::UpstreamMessage::ExecuteCommand { buffer_id, command } => {
                let id = CommandExecutionRequestId::new();
                if !viewer_can_execute(&relay, session_id, &viewer_id) {
                    if let Some(tx) = viewer_tx(&relay, session_id, &viewer_id) {
                        let _ = tx.send(viewer::DownstreamMessage::CommandExecutionRequestFailed {
                            id,
                            reason: CommandExecutionFailureReason::InsufficientPermissions,
                        });
                    }
                    continue;
                }

                if let Some(tx) = viewer_tx(&relay, session_id, &viewer_id) {
                    let _ = tx.send(viewer::DownstreamMessage::CommandExecutionRequestInFlight(
                        id.clone(),
                    ));
                }
                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::CommandExecutionRequested {
                        id,
                        participant_id: viewer_id.clone(),
                        buffer_id,
                        command,
                    },
                );
            }
            viewer::UpstreamMessage::WriteToPty { request_id, bytes } => {
                if !viewer_can_execute(&relay, session_id, &viewer_id) {
                    if let Some(tx) = viewer_tx(&relay, session_id, &viewer_id) {
                        let _ = tx.send(viewer::DownstreamMessage::WriteToPtyRequestFailed {
                            reason: WriteToPtyFailureReason::InsufficientPermissions,
                        });
                    }
                    continue;
                }
                {
                    let mut relay = relay.lock();
                    if let Some(session) = relay.sessions.get_mut(&session_id) {
                        session
                            .pending_write_to_pty_requests
                            .insert(write_id_key(&request_id), viewer_id.clone());
                    }
                }
                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::WriteToPtyRequested {
                        id: request_id,
                        bytes,
                    },
                );
            }
            viewer::UpstreamMessage::SendAgentPrompt(request) => {
                if !viewer_can_execute(&relay, session_id, &viewer_id) {
                    if let Some(tx) = viewer_tx(&relay, session_id, &viewer_id) {
                        let _ = tx.send(viewer::DownstreamMessage::AgentPromptRequestFailed {
                            reason: AgentPromptFailureReason::InsufficientPermissions,
                        });
                    }
                    continue;
                }
                if let Some(tx) = viewer_tx(&relay, session_id, &viewer_id) {
                    let _ = tx.send(viewer::DownstreamMessage::AgentPromptRequestInFlight(
                        request.id.clone(),
                    ));
                }
                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::AgentPromptRequested {
                        id: request.id.clone(),
                        participant_id: viewer_id.clone(),
                        request,
                    },
                );
            }
            viewer::UpstreamMessage::UpdateUniversalDeveloperInputContext(update) => {
                if !viewer_can_execute(&relay, session_id, &viewer_id) {
                    continue;
                }
                let (sharer_tx, viewers) = {
                    let mut relay = relay.lock();
                    let Some(session) = relay.sessions.get_mut(&session_id) else {
                        break;
                    };
                    apply_context_update(session, update.clone());
                    (session.sharer_tx.clone(), session.viewer_txs())
                };
                if let Some(tx) = sharer_tx {
                    let _ = tx.send(
                        sharer::DownstreamMessage::UniversalDeveloperInputContextUpdated(
                            update.clone(),
                        ),
                    );
                }
                for tx in viewers {
                    let _ = tx.send(
                        viewer::DownstreamMessage::UniversalDeveloperInputContextUpdated(
                            update.clone(),
                        ),
                    );
                }
            }
            viewer::UpstreamMessage::SendControlAction(action) => {
                let request_id = ControlActionRequestId::new();
                if !viewer_can_execute(&relay, session_id, &viewer_id) {
                    if let Some(tx) = viewer_tx(&relay, session_id, &viewer_id) {
                        let _ = tx.send(viewer::DownstreamMessage::ControlActionRequestFailed {
                            reason: ControlActionFailureReason::InsufficientPermissions,
                        });
                    }
                    continue;
                }
                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::ControlActionRequested {
                        participant_id: viewer_id.clone(),
                        request_id,
                        action,
                    },
                );
            }
            viewer::UpstreamMessage::Reauthenticated { user_id } => {
                let mut relay = relay.lock();
                if let Some(session) = relay.sessions.get_mut(&session_id) {
                    if let Some(viewer) = session.viewers.get_mut(&viewer_id) {
                        viewer.info.profile_data.firebase_uid = participant_uid(&user_id);
                    }
                }
            }
            viewer::UpstreamMessage::UpdateLinkAccessLevel { role } => {
                let viewers = {
                    let mut relay = relay.lock();
                    let Some(session) = relay.sessions.get_mut(&session_id) else {
                        break;
                    };
                    session.link_access_role = role;
                    session.viewer_txs()
                };
                if let Some(tx) = viewer_tx(&relay, session_id, &viewer_id) {
                    let _ = tx.send(viewer::DownstreamMessage::LinkAccessLevelUpdateResponse(
                        LinkAccessLevelUpdateResponse::Ok { role },
                    ));
                }
                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::LinkAccessLevelUpdateResponse(
                        LinkAccessLevelUpdateResponse::Ok { role },
                    ),
                );
                for tx in viewers {
                    let _ = tx.send(viewer::DownstreamMessage::LinkAccessLevelUpdated { role });
                }
            }
            viewer::UpstreamMessage::UpdateTeamAccessLevel { .. } => {
                reject_viewer_team_access(&relay, session_id, &viewer_id);
            }
            viewer::UpstreamMessage::AddGuests { .. } => {
                reject_viewer_guests(&relay, session_id, &viewer_id)
            }
            viewer::UpstreamMessage::RemoveGuest { .. }
            | viewer::UpstreamMessage::RemovePendingGuest { .. } => {
                if let Some(tx) = viewer_tx(&relay, session_id, &viewer_id) {
                    let _ = tx.send(viewer::DownstreamMessage::RemoveGuestResponse(
                        RemoveGuestResponse::Error(FailedToRemoveGuestReason::Invalid),
                    ));
                }
            }
            viewer::UpstreamMessage::UpdateUserRole { .. }
            | viewer::UpstreamMessage::UpdatePendingUserRole { .. } => {
                if let Some(tx) = viewer_tx(&relay, session_id, &viewer_id) {
                    let _ = tx.send(viewer::DownstreamMessage::UpdatePendingUserRoleResponse(
                        UpdatePendingUserRoleResponse::Error(
                            FailedToUpdatePendingUserRoleReason::Invalid,
                        ),
                    ));
                }
            }
            viewer::UpstreamMessage::ReportTerminalSize { window_size } => {
                send_to_sharer(
                    &relay,
                    session_id,
                    sharer::DownstreamMessage::ViewerTerminalSizeReported {
                        participant_id: viewer_id.clone(),
                        window_size,
                    },
                );
            }
        }
    }
}

struct JoinResult {
    viewer_id: ParticipantId,
    initial_message: viewer::DownstreamMessage,
    replay_events: Vec<OrderedTerminalEvent>,
}

impl Session {
    fn join_viewer(
        &mut self,
        init: viewer::InitPayload,
        tx: ViewerTx,
        default_role: Role,
    ) -> JoinResult {
        let is_rejoin = init.viewer_id.is_some()
            && init
                .viewer_id
                .as_ref()
                .is_some_and(|id| self.viewers.contains_key(id));
        let viewer_id = init.viewer_id.unwrap_or_else(ParticipantId::new);
        let user_uid = participant_uid(&init.user_id);
        let display_name = if is_rejoin {
            self.viewers
                .get(&viewer_id)
                .map(|viewer| viewer.info.profile_data.display_name.clone())
                .unwrap_or_else(|| "Viewer".to_string())
        } else {
            format!("Viewer {}", self.viewers.len() + 1)
        };

        let input_replica_id =
            session_sharing_protocol::common::InputReplicaId::from(format!("viewer-{viewer_id}"));
        let role = self
            .viewers
            .get(&viewer_id)
            .map(|viewer| viewer.role)
            .unwrap_or(default_role);
        let info = self
            .viewers
            .get(&viewer_id)
            .map(|viewer| viewer.info.clone())
            .unwrap_or_else(|| {
                participant_info(
                    viewer_id.clone(),
                    user_uid,
                    display_name,
                    input_replica_id.clone(),
                    Selection::None,
                )
            });
        self.viewers.insert(
            viewer_id.clone(),
            ViewerState {
                info,
                role,
                is_present: true,
                tx: Some(tx),
            },
        );

        let participant_list = Box::new(self.participant_list());
        let replay_events = self
            .event_log
            .range(init.last_received_event_no.map_or(0, |last| last + 1)..)
            .map(|(_, event)| event.clone())
            .collect::<Vec<_>>();

        let initial_message = if is_rejoin || init.last_received_event_no.is_some() {
            viewer::DownstreamMessage::RejoinedSuccessfully { participant_list }
        } else {
            viewer::DownstreamMessage::JoinedSuccessfully {
                scrollback: Box::new(self.scrollback.clone()),
                active_prompt: self.active_prompt.clone(),
                latest_event_no: self.event_log.keys().next_back().copied(),
                window_size: self.window_size,
                participant_list,
                viewer_id: viewer_id.clone(),
                viewer_firebase_uid: self
                    .viewers
                    .get(&viewer_id)
                    .map(|viewer| viewer.info.profile_data.firebase_uid.clone())
                    .unwrap_or_default(),
                init_block_id: self.init_block_id.clone(),
                input_replica_id,
                universal_developer_input_context: self.universal_developer_input_context.clone(),
                source_type: (&self.source_type).into(),
                detailed_source_type: self.source_type.clone(),
            }
        };

        JoinResult {
            viewer_id,
            initial_message,
            replay_events,
        }
    }

    fn participant_list(&self) -> ParticipantList {
        let mut list = ParticipantList {
            sharer: session_sharing_protocol::common::Sharer {
                info: self.sharer_info.clone(),
            },
            ..Default::default()
        };

        for viewer in self.viewers.values() {
            list.viewers.push(session_sharing_protocol::common::Viewer {
                info: viewer.info.clone(),
                role: viewer.role,
                is_present: viewer.is_present,
            });
            if viewer.is_present {
                list.present_viewers
                    .push(session_sharing_protocol::common::PresentViewer {
                        info: viewer.info.clone(),
                        max_acl: viewer.role,
                    });
            } else {
                list.absent_viewers
                    .push(session_sharing_protocol::common::AbsentViewer {
                        info: viewer.info.clone(),
                    });
            }
        }

        list
    }

    fn viewer_txs(&self) -> Vec<ViewerTx> {
        self.viewers
            .values()
            .filter(|viewer| viewer.is_present)
            .filter_map(|viewer| viewer.tx.clone())
            .collect()
    }
}

fn participant_uid(user_id: &UserID) -> String {
    format!("anonymous:{}", user_id.anonymous_id)
}

fn participant_info(
    id: ParticipantId,
    firebase_uid: String,
    display_name: String,
    input_replica_id: session_sharing_protocol::common::InputReplicaId,
    selection: Selection,
) -> ParticipantInfo {
    ParticipantInfo {
        id,
        profile_data: ProfileData {
            firebase_uid,
            display_name,
            photo_url: None,
            email: None,
            input_replica_id,
        },
        selection,
    }
}

fn apply_context_update(session: &mut Session, update: UniversalDeveloperInputContextUpdate) {
    let current = session
        .universal_developer_input_context
        .take()
        .unwrap_or_default();
    session.universal_developer_input_context = Some(update.merge_into(current));
}

async fn next_sharer_message(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
) -> Option<sharer::UpstreamMessage> {
    while let Some(message) = stream.next().await {
        match message {
            Ok(Message::Text(text)) => match sharer::UpstreamMessage::from_json(&text) {
                Ok(message) => return Some(message),
                Err(err) => warn!(%err, "invalid sharer websocket message"),
            },
            Ok(Message::Close(_)) => return None,
            Ok(_) => {}
            Err(err) => {
                debug!(%err, "sharer websocket closed with error");
                return None;
            }
        }
    }
    None
}

async fn next_viewer_message(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
) -> Option<viewer::UpstreamMessage> {
    while let Some(message) = stream.next().await {
        match message {
            Ok(Message::Text(text)) => match viewer::UpstreamMessage::from_json(&text) {
                Ok(message) => return Some(message),
                Err(err) => warn!(%err, "invalid viewer websocket message"),
            },
            Ok(Message::Close(_)) => return None,
            Ok(_) => {}
            Err(err) => {
                debug!(%err, "viewer websocket closed with error");
                return None;
            }
        }
    }
    None
}

fn send_to_sharer(relay: &Relay, session_id: SessionId, message: sharer::DownstreamMessage) {
    let tx = relay
        .lock()
        .sessions
        .get(&session_id)
        .and_then(|session| session.sharer_tx.clone());
    if let Some(tx) = tx {
        let _ = tx.send(message);
    }
}

fn viewer_tx(
    relay: &Relay,
    session_id: SessionId,
    participant_id: &ParticipantId,
) -> Option<ViewerTx> {
    relay
        .lock()
        .sessions
        .get(&session_id)
        .and_then(|session| session.viewers.get(participant_id))
        .and_then(|viewer| viewer.tx.clone())
}

fn viewer_tx_for_input(
    relay: &Relay,
    session_id: SessionId,
    id: &InputOperationId,
) -> Option<ViewerTx> {
    viewer_tx(relay, session_id, &id.participant_id)
}

fn viewer_can_execute(
    relay: &Relay,
    session_id: SessionId,
    participant_id: &ParticipantId,
) -> bool {
    relay
        .lock()
        .sessions
        .get(&session_id)
        .and_then(|session| session.viewers.get(participant_id))
        .is_some_and(|viewer| viewer.role.can_execute())
}

fn broadcast_participant_list(relay: &Relay, session_id: SessionId) {
    let (participant_list, sharer_tx, viewers) = {
        let relay = relay.lock();
        let Some(session) = relay.sessions.get(&session_id) else {
            return;
        };
        (
            session.participant_list(),
            session.sharer_tx.clone(),
            session.viewer_txs(),
        )
    };

    if let Some(tx) = sharer_tx {
        let _ = tx.send(sharer::DownstreamMessage::ParticipantListUpdated(
            participant_list.clone(),
        ));
    }
    for tx in viewers {
        let _ = tx.send(viewer::DownstreamMessage::ParticipantListUpdated(
            participant_list.clone(),
        ));
    }
}

fn broadcast_presence(relay: &Relay, session_id: SessionId, update: ParticipantPresenceUpdate) {
    let (sharer_tx, viewers) = {
        let relay = relay.lock();
        let Some(session) = relay.sessions.get(&session_id) else {
            return;
        };
        (session.sharer_tx.clone(), session.viewer_txs())
    };
    if let Some(tx) = sharer_tx {
        let _ = tx.send(sharer::DownstreamMessage::ParticipantPresenceUpdated(
            update.clone(),
        ));
    }
    for tx in viewers {
        let _ = tx.send(viewer::DownstreamMessage::ParticipantPresenceUpdated(
            update.clone(),
        ));
    }
}

fn update_participant_role(
    relay: &Relay,
    session_id: SessionId,
    participant_id: ParticipantId,
    role: Role,
) {
    {
        let mut relay = relay.lock();
        let Some(session) = relay.sessions.get_mut(&session_id) else {
            return;
        };
        let Some(viewer) = session.viewers.get_mut(&participant_id) else {
            return;
        };
        viewer.role = role;
    }
    broadcast_role_change(
        relay,
        session_id,
        participant_id,
        role,
        viewer::RoleUpdatedReason::UpdatedBySharer,
    );
    broadcast_participant_list(relay, session_id);
}

fn broadcast_role_change(
    relay: &Relay,
    session_id: SessionId,
    participant_id: ParticipantId,
    role: Role,
    reason: viewer::RoleUpdatedReason,
) {
    let (sharer_tx, viewers) = {
        let relay = relay.lock();
        let Some(session) = relay.sessions.get(&session_id) else {
            return;
        };
        (session.sharer_tx.clone(), session.viewer_txs())
    };
    if let Some(tx) = sharer_tx {
        let _ = tx.send(sharer::DownstreamMessage::ParticipantRoleChanged {
            participant_id: participant_id.clone(),
            role,
        });
    }
    for tx in viewers {
        let _ = tx.send(viewer::DownstreamMessage::ParticipantRoleChanged {
            participant_id: participant_id.clone(),
            reason,
            role,
        });
    }
}

fn end_session(relay: &Relay, session_id: SessionId, reason: SharerSessionEndedReason) {
    let viewers = relay
        .lock()
        .sessions
        .remove(&session_id)
        .map(|session| session.viewer_txs())
        .unwrap_or_default();
    for tx in viewers {
        let _ = tx.send(viewer::DownstreamMessage::SessionEnded {
            reason: match reason {
                SharerSessionEndedReason::EndedBySharer => ViewerSessionEndedReason::EndedBySharer,
                SharerSessionEndedReason::InactivityLimitReached => {
                    ViewerSessionEndedReason::InactivityLimitReached
                }
                SharerSessionEndedReason::ExceededSizeLimit => {
                    ViewerSessionEndedReason::ExceededSizeLimit
                }
            },
        });
    }
    info!(%session_id, "shared session ended");
}

fn reject_sharer_team_access(relay: &Relay, session_id: SessionId) {
    send_to_sharer(
        relay,
        session_id,
        sharer::DownstreamMessage::TeamAccessLevelUpdateResponse(
            TeamAccessLevelUpdateResponse::Error(FailedToUpdateTeamAccessLevelReason::Invalid),
        ),
    );
}

fn reject_viewer_team_access(relay: &Relay, session_id: SessionId, viewer_id: &ParticipantId) {
    if let Some(tx) = viewer_tx(relay, session_id, viewer_id) {
        let _ = tx.send(viewer::DownstreamMessage::TeamAccessLevelUpdateResponse(
            TeamAccessLevelUpdateResponse::Error(FailedToUpdateTeamAccessLevelReason::Invalid),
        ));
    }
}

fn reject_sharer_guests(relay: &Relay, session_id: SessionId) {
    send_to_sharer(
        relay,
        session_id,
        sharer::DownstreamMessage::AddGuestsResponse(AddGuestsResponse::Error(
            FailedToAddGuestsReason::Invalid,
        )),
    );
}

fn reject_viewer_guests(relay: &Relay, session_id: SessionId, viewer_id: &ParticipantId) {
    if let Some(tx) = viewer_tx(relay, session_id, viewer_id) {
        let _ = tx.send(viewer::DownstreamMessage::AddGuestsResponse(
            AddGuestsResponse::Error(FailedToAddGuestsReason::Invalid),
        ));
    }
}

fn write_id_key(id: &WriteToPtyRequestId) -> String {
    format!("{}:{}", id.participant_id, id.op_no.as_usize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_sharing_protocol::{
        common::{ActivePrompt, BlockId, InputReplicaId},
        viewer::InitPayload as ViewerInitPayload,
    };

    fn test_session() -> Session {
        let sharer_id = ParticipantId::new();
        let input_replica_id = InputReplicaId::from("sharer".to_string());
        let sharer_info = participant_info(
            sharer_id.clone(),
            "anonymous:sharer".to_string(),
            "Sharer".to_string(),
            input_replica_id.clone(),
            Selection::None,
        );
        Session {
            secret: SessionSecret::new(),
            reconnect_token: ReconnectToken::new(),
            sharer_id,
            sharer_info,
            sharer_tx: None,
            scrollback: Scrollback {
                blocks: Vec::new(),
                is_alt_screen_active: false,
            },
            active_prompt: ActivePrompt::PS1,
            window_size: WindowSize {
                num_rows: 24,
                num_cols: 80,
            },
            init_block_id: BlockId::from("block-1".to_string()),
            universal_developer_input_context: None,
            source_type: SessionSourceType::User,
            event_log: BTreeMap::new(),
            latest_processed_event_no: None,
            viewers: HashMap::new(),
            link_access_role: Some(Role::Reader),
            pending_write_to_pty_requests: HashMap::new(),
        }
    }

    fn viewer_init(last_received_event_no: Option<usize>) -> ViewerInitPayload {
        ViewerInitPayload {
            viewer_id: None,
            user_id: UserID::default(),
            last_received_event_no,
            latest_block_id: None,
            telemetry_context: None,
            feature_support: Default::default(),
        }
    }

    #[test]
    fn late_join_replays_ordered_events() {
        let mut session = test_session();
        session.event_log.insert(
            0,
            OrderedTerminalEvent {
                event_no: 0,
                event_type: OrderedTerminalEventType::PtyBytesRead {
                    bytes: b"hello".to_vec(),
                },
            },
        );
        session.event_log.insert(
            1,
            OrderedTerminalEvent {
                event_no: 1,
                event_type: OrderedTerminalEventType::PtyBytesRead {
                    bytes: b"world".to_vec(),
                },
            },
        );

        let (tx, _rx) = mpsc::unbounded_channel();
        let joined = session.join_viewer(viewer_init(None), tx, Role::Reader);
        assert_eq!(joined.replay_events.len(), 2);
        assert!(matches!(
            joined.initial_message,
            viewer::DownstreamMessage::JoinedSuccessfully {
                latest_event_no: Some(1),
                ..
            }
        ));
    }

    #[test]
    fn viewer_reconnect_only_replays_missed_events() {
        let mut session = test_session();
        for event_no in 0..3 {
            session.event_log.insert(
                event_no,
                OrderedTerminalEvent {
                    event_no,
                    event_type: OrderedTerminalEventType::PtyBytesRead { bytes: vec![0] },
                },
            );
        }

        let (tx, _rx) = mpsc::unbounded_channel();
        let joined = session.join_viewer(viewer_init(Some(1)), tx, Role::Reader);
        assert_eq!(
            joined
                .replay_events
                .iter()
                .map(|event| event.event_no)
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert!(matches!(
            joined.initial_message,
            viewer::DownstreamMessage::RejoinedSuccessfully { .. }
        ));
    }

    #[test]
    fn participant_list_excludes_cloud_acl_state() {
        let mut session = test_session();
        let (tx, _rx) = mpsc::unbounded_channel();
        let _ = session.join_viewer(viewer_init(None), tx, Role::Reader);

        let participant_list = session.participant_list();
        assert_eq!(participant_list.guests.len(), 0);
        assert_eq!(participant_list.pending_guests.len(), 0);
        assert_eq!(participant_list.present_viewers.len(), 1);
    }
}
