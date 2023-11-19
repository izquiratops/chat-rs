use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::UnboundedReceiverStream;
use uuid::Uuid;
use warp::ws::{Message, WebSocket};
use warp::Filter;

type Users = Arc<RwLock<HashMap<String, mpsc::UnboundedSender<Message>>>>;
type MessageHistory = Arc<RwLock<VecDeque<String>>>;

#[tokio::main]
async fn main() {
    let users = Users::default();
    let with_users = warp::any().map(move || users.clone());

    let msg_history = MessageHistory::default();
    let with_msg_history = warp::any().map(move || msg_history.clone());

    let index = warp::fs::dir("client");

    let chat = warp::path("chat")
        .and(warp::ws())
        .and(with_users)
        .and(with_msg_history)
        .map(|ws: warp::ws::Ws, users, msg_history| {
            ws.on_upgrade(move |socket| user_connected(socket, users, msg_history))
        });

    warp::serve(index.or(chat))
        .run(([127, 0, 0, 1], 3030))
        .await;
}

async fn user_connected(ws: WebSocket, users: Users, msg_history: MessageHistory) {
    let uuid = Uuid::new_v4().simple().to_string();

    let (user_ws_tx, mut user_ws_rx) = ws.split();
    let (user_channel_tx, user_channel_rx) = mpsc::unbounded_channel();

    // Send the user the chat history when a user is connected.
    send_msg_history(&msg_history, &user_channel_tx).await;

    // Use an unbounded channel to set up a message queue for the user.
    tokio::task::spawn(forward_messages(user_channel_rx, user_ws_tx));

    // Save the sender in our list of connected users.
    users.write().await.insert(uuid.clone(), user_channel_tx);

    // Loop that listens for incoming messages from a user WebSocket.
    // It processes each message by calling the `user_message` function with the received message,
    // the `users` collection, and the `chat_history` collection as arguments.
    // If an error occurs during the WebSocket communication, it prints an error message and breaks out of the loop.
    while let Some(result) = user_ws_rx.next().await {
        match result {
            Ok(msg) => user_message(msg, &users, &msg_history).await,
            Err(e) => {
                eprintln!("WebSocket error (uid={}): {}", &uuid, e);
                break;
            }
        }
    }

    user_disconnected(uuid, users).await;
}

async fn send_msg_history(msg_history: &MessageHistory, user_channel_tx: &mpsc::UnboundedSender<Message>) {
    for message in msg_history.read().await.iter() {
        if let Err(_disconnected) = user_channel_tx.send(Message::text(message)) {
            eprintln!("Error sending a history message: {}", _disconnected);
        }
    }
}

async fn forward_messages(user_channel_rx: mpsc::UnboundedReceiver<Message>, mut user_ws_tx: SplitSink<WebSocket, Message>) {
    let mut rx = UnboundedReceiverStream::new(user_channel_rx);
    while let Some(message) = rx.next().await {
        if let Err(_disconnected) = user_ws_tx.send(message).await {
            eprintln!("Error forwarding a message: {}", _disconnected);
        }
    }
}

async fn user_message(msg: Message, users: &Users, msg_history: &MessageHistory) {
    if let Ok(msg_str) = msg.to_str() {
        let msg_text = msg_str.to_string();

        broadcast_message(&users, &msg_text).await;

        save_message_to_history(&msg_history, msg_text).await;
    }
}

async fn save_message_to_history(msg_history: &MessageHistory, msg_text: String) {
    let mut msg_write = msg_history.write().await;
    msg_write.push_back(msg_text);

    let max_msg_history_length = std::env::var("MAX_MSG_HISTORY_LENGTH")
        .expect("MAX_MSG_HISTORY_LENGTH must be set")
        .parse::<usize>()
        .unwrap();

    if msg_write.len() > max_msg_history_length {
        msg_write.pop_front();
    }
}

async fn broadcast_message(users: &Users, msg_text: &str) {
    for (_, user_channel_tx) in users.read().await.iter() {
        if let Err(_disconnected) = user_channel_tx.send(Message::text(msg_text)) {
            // The tx is disconnected, our `user_disconnected` code
            // should be happening in another task, nothing more to
            // do here.
            eprintln!("Couldn't broadcast a message, user may be disconnected: {}", _disconnected);
        }
    }
}

async fn user_disconnected(user_id: String, users: Users) {
    users.write().await.remove(&user_id);
}
