use std::sync::{Arc, Mutex};

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
    routing::get,
    Router,
};

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpListener, sync::broadcast};

#[derive(Debug)]
struct AppState {
    clients_count: Mutex<usize>,
    clients_position: Mutex<Vec<ClientPosition>>,
    tx: broadcast::Sender<Message>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "lowercase")]
struct CustomMessaage {
    message: Option<String>,
    message_type: Option<String>,
    sender_id: Option<usize>,
    my_id: Option<usize>,
    position: Option<Position>,
    client_count: Option<usize>,
    clients_position: Option<Vec<ClientPosition>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "lowercase")]
struct Position {
    x: i32,
    y: i32,
}

impl Position {
    fn set_position(&mut self, x: i32, y: i32) {
        self.x = x;
        self.y = y;
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "lowercase")]
struct ClientPosition {
    client_id: usize,
    poistion: Position,
}

impl CustomMessaage {
    fn set_sender_id(&mut self, id: usize) {
        self.sender_id = Some(id)
    }

    fn set_position(&mut self, x: i32, y: i32) {
        self.position = Some(Position { x, y });
    }
}

#[tokio::main]
async fn main() {
    let (tx, _) = broadcast::channel(100);

    let state = Arc::new(AppState {
        clients_count: Mutex::new(0),
        clients_position: Mutex::new(Vec::new()),
        tx,
    });

    let app = Router::new()
        .route("/ws", get(websocket_handler))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:3000").await.unwrap();

    axum::serve(listener, app).await.unwrap()
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |ws| handle_game(ws, state))
}

async fn handle_game(stream: WebSocket, state: Arc<AppState>) {
    // By splitting we can send and receive at the same time.
    let (mut sender, mut receiver) = stream.split();

    let my_id: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));

    let mut rx: broadcast::Receiver<Message> = {
        let mut count = state.clients_count.lock().unwrap();
        *count += 1;

        let mut my_id = my_id.lock().unwrap();
        *my_id = *count;

        let mut clients = state.clients_position.lock().unwrap();

        let client_position = ClientPosition {
            client_id: *my_id,
            poistion: Position { x: 0, y: 0 },
        };
        clients.push(client_position);

        state.tx.subscribe()
    };

    let message = CustomMessaage {
        message: Some(format!(
            "Welcome! There is {} players. Your id is {}.",
            state.clients_count.lock().unwrap(),
            my_id.lock().unwrap(),
        )),
        message_type: Some("server".into()),
        sender_id: None,
        position: None,
        my_id: Some(*my_id.lock().unwrap()),
        client_count: Some(*state.clients_count.lock().unwrap()),
        clients_position: Some(state.clients_position.lock().unwrap().to_vec()),
    };

    let serialized = serde_json::to_string(&message).unwrap();
    let _ = sender.send(Message::Text(serialized)).await;

    let message = CustomMessaage {
        message: Some(format!(
            "Player {} joined",
            state.clients_count.lock().unwrap(),
        )),
        message_type: Some("server".into()),
        sender_id: None,
        position: None,
        my_id: Some(*my_id.lock().unwrap()),
        client_count: Some(*state.clients_count.lock().unwrap()),
        clients_position: Some(state.clients_position.lock().unwrap().to_vec()),
    };

    let serialized = serde_json::to_string(&message).unwrap();

    let _ = state.tx.send(Message::Text(serialized));
    let my_id_clone: Arc<Mutex<usize>> = my_id.clone();

    // This task will receive watch messages and forward it to this connected client.

    let mut send_task = tokio::spawn(async move {
        while let Ok(message) = rx.recv().await {
            let m: Message = message.clone();
            let deserialized: CustomMessaage =
                serde_json::from_str(&message.into_text().unwrap()).unwrap();

            if deserialized.message_type == Some("server_error".into()) {
                if deserialized.sender_id.unwrap() == *my_id_clone.lock().unwrap()
                    && sender.send(m).await.is_err()
                {
                    break;
                }
            } else {
                if sender.send(m).await.is_err() {
                    break;
                }
            }
        }
    });

    let cloned_state = state.clone();
    let my_id_clone = my_id.clone();

    // This task will receive messages from this client.
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(Message::Text(text))) = receiver.next().await {
            println!("sent {}", text);

            let deserialized: Result<CustomMessaage, serde_json::Error> =
                serde_json::from_str(&text);

            match deserialized {
                Ok(mut result) => {
                    println!("got {:?}", result);
                    result.set_sender_id(*my_id_clone.lock().unwrap());

                    result.set_position(
                        result.position.clone().unwrap().x,
                        result.position.clone().unwrap().y,
                    );

                    let mut clients_position = cloned_state.clients_position.lock().unwrap();

                    let position = clients_position
                        .iter()
                        .map(|e| e.client_id)
                        .collect::<Vec<_>>()
                        .iter()
                        .position(|e| *e == *my_id.lock().unwrap())
                        .unwrap();

                    clients_position[position].poistion.set_position(
                        result.position.clone().unwrap().x,
                        result.position.clone().unwrap().y,
                    );

                    cloned_state
                        .tx
                        .send(Message::Text(serde_json::to_string(&result).unwrap()))
                        .unwrap();
                }
                Err(err) => {
                    let err_message = CustomMessaage {
                        message: Some(err.to_string()),
                        message_type: Some("server_error".into()),
                        sender_id: Some(*my_id_clone.lock().unwrap()),
                        position: None,
                        my_id: Some(*my_id.lock().unwrap()),
                        client_count: None,
                        clients_position: None,
                    };
                    cloned_state
                        .tx
                        .send(Message::Text(serde_json::to_string(&err_message).unwrap()))
                        .unwrap();
                }
            }
        }
    });

    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    };

    state.tx.send("One Player Disconnected!".into()).unwrap();
    let mut count = state.clients_count.lock().unwrap();
    *count -= 1;
}
