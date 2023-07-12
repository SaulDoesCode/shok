use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use anyhow::anyhow;
use base64::Engine as _;
use lazy_static::lazy_static;
use salvo::{conn::rustls::{Keycert, RustlsConfig}, http::{*}, prelude::*, rate_limiter::*, logging::Logger, sse::{SseKeepAlive, SseEvent}};
use serde::{Serialize, Deserialize};
use serde_json::json;
use sthash::Hasher;
use std::{io::{Read, Write}, fs::File, path::{Path, PathBuf}, marker::PhantomData, sync::{Arc, mpsc::{channel, Sender, Receiver}}, time::Duration, mem::forget};
use rand::{thread_rng, distributions::Alphanumeric, Rng};
use redb::{Database, ReadableTable, TableDefinition, MultimapTableDefinition, ReadableMultimapTable};
use tantivy::{
    doc,
    collector::TopDocs,
    query::{QueryParser, TermQuery},
    schema::*,
    Index,
    // IndexReader, ReloadPolicy,
    IndexWriter, DateTime
};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use parking_lot::RwLock;

type Astr = Arc<str>;
//type Astrs = Vec<Astr>;
type IsInsert = bool;
type MutEvent = (Astr, IsInsert); // name, value, is_insert

type U8s = Vec<u8>;
type Strings = Vec<String>;

#[derive(Debug)]
enum Message {
    UserId(usize),
    Reply(String)
}

type Users = dashmap::DashMap<usize, mpsc::UnboundedSender<Message>>;

lazy_static!{
    static ref ONLINE_USERS: Users = Users::new();
}

#[handler]
async fn chat_send(req: &mut Request, res: &mut Response) {
    let uid = match session_check(req, None).await {
        Some(id) => id,
        None => {
            uares(res);
            return;
        }
    } as usize;
    if let Ok(b) = req.payload().await {
        if let Ok(mut msg) = std::str::from_utf8(b) {
            if msg.len() > 2048 {
                brq(res, "message too long");
                return;
            }
            if msg.starts_with("@") {
                msg = msg.trim_start_matches("@");
                let mut args = msg.split_whitespace();
                if let Some(moniker) = args.next() {
                    if let Ok(acc) = Account::from_moniker(moniker, &DB) {
                        let mut msg = String::new();
                        while let Some(arg) = args.next() {
                            msg.push_str(arg);
                            msg.push(' ');
                        }
                        str_msg(acc.id, &msg).i(uid as u64);
                    } else {
                        str_auto_msg("No such identity found, failed to parse id as number, tried as a moniker, both failed, cannot send message").i(uid as u64);
                    }
                }
            } else if msg.starts_with("/") {
                // command
                msg = msg.trim_start_matches("/");
                let mut args = msg.split_whitespace();
                if let Some(cmd) = args.next() {
                    match cmd {
                        "help" => {
                            auto_msg(format!("commands: /help, /transfer to:u64 amount:u64 ?when:u64, /broadcast msg:str, /msg id:u64 msg:str")).i(uid as u64);
                        }
                        "whois" => {
                            if let Some(moniker) = args.next() {
                                if let Ok(acc) = Account::from_moniker(moniker, &DB) {
                                    auto_msg(format!("{} is {}", moniker, acc.id)).i(uid as u64);
                                } else {
                                    str_auto_msg("No such identity found").i(uid as u64);
                                }
                            } else {
                                auto_msg(format!("missing name")).i(uid as u64);
                            }
                        }
                        "transfer" => {
                            if let Some(to) = args.next() {
                                if let Ok(acc) = Account::from_moniker(to, &DB) {
                                    if let Some(amount) = args.next() {
                                        if let Ok(amount) = amount.parse::<u64>() {
                                            let when = args.next().map(|s| s.parse::<u64>().ok()).flatten();
                                            auto_msg(format!("transfer {} to {} at {:?}", amount, to, when)).i(uid as u64);
                                            interaction(uid as u64, Interaction::Transfer(acc.id, amount, when));
                                        } else {
                                            auto_msg(format!("failed to parse amount")).i(uid as u64);
                                        }
                                    } else {
                                        auto_msg(format!("missing amount")).i(uid as u64);
                                    }
                                } else {
                                    if let Ok(to) = to.parse::<u64>() {
                                        if let Some(amount) = args.next() {
                                            if let Ok(amount) = amount.parse::<u64>() {
                                                let when = args.next().map(|s| s.parse::<u64>().ok()).flatten();
                                                auto_msg(format!("transfer {} to {} at {:?}", amount, to, when.unwrap_or_else(|| now()))).i(uid as u64);
                                                interaction(uid as u64, Interaction::Transfer(to, amount, when));
                                            } else {
                                                auto_msg(format!("failed to parse amount")).i(uid as u64);
                                            }
                                        } else {
                                            auto_msg(format!("missing amount")).i(uid as u64);
                                        }
                                    } else {
                                        auto_msg(format!("failed to parse to")).i(uid as u64);
                                    }
                                }
                            } else {
                                auto_msg(format!("missing to")).i(uid as u64);
                            }
                        }
                        "balance" | "b" => {
                            // if there is a next arg that can be read as a u64 ammount then if the user is admin add balance
                            if let Some(amount) = args.next() {
                                if let Ok(amount) = amount.parse::<u64>() {
                                    if let Ok(mut acc) = Account::from_id(uid as u64, &DB) {
                                        if acc.id == ADMIN_ID {
                                            auto_msg(format!("adding {} to balance", amount)).i(uid as u64);
                                            acc.balance += amount;
                                            match acc.save(&DB, false) {
                                                Ok(_) => {
                                                    auto_msg(format!("balance: {}", acc.balance)).i(uid as u64);
                                                }
                                                Err(e) => {
                                                    auto_msg(format!("failed to save balance: {}", e)).i(uid as u64);
                                                }
                                            }
                                        } else {
                                            auto_msg(format!("not admin")).i(uid as u64);
                                        }
                                    } else {
                                        auto_msg(format!("failed to get balance")).i(uid as u64);
                                    }
                                } else {
                                    auto_msg(format!("failed to parse amount")).i(uid as u64);
                                }
                            }
                            // check own balance using id to get account
                            if let Ok(acc) = Account::from_id(uid as u64, &DB) {
                                auto_msg(format!("balance: {}", acc.balance)).i(uid as u64);
                            } else {
                                auto_msg(format!("failed to get balance")).i(uid as u64);
                            }
                        }
                        "broadcast" | "bc" => {
                            if let Some(msg) = args.next() {
                                let mut msg = msg.to_string();
                                msg.push(' ');
                                while let Some(arg) = args.next() {
                                    msg.push_str(arg);
                                    msg.push(' ');
                                }
                                auto_msg(format!("broadcasted: {}", msg)).i(uid as u64);
                                Interaction::Broadcast(msg).i(uid as u64);
                            } else {
                                auto_msg(format!("missing message")).i(uid as u64);
                            }
                        }
                        "msg" | "m" => {
                            if let Some(to) = args.next() {
                                if let Ok(to) = to.parse::<u64>() {
                                    // collect all the arguments into a message
                                    let mut msg = String::new();
                                    while let Some(arg) = args.next() {
                                        msg.push_str(arg);
                                        msg.push(' ');
                                    }
                                    if msg == "" {
                                        auto_msg(format!("missing message")).i(uid as u64);
                                    } else {
                                        auto_msg(format!("sending message to {}: {}", to, msg)).i(uid as u64);
                                        Interaction::Message(to, msg.to_string()).i(uid as u64);
                                    }
                                } else {
                                    // handle moniker use instead of id
                                    if let Ok(acc) = Account::from_moniker(to, &DB) {
                                        let mut msg = String::new();
                                        while let Some(arg) = args.next() {
                                            msg.push_str(arg);
                                            msg.push(' ');
                                        }
                                        str_msg(acc.id, &msg).i(uid as u64);
                                    } else {
                                        str_auto_msg("No such identity found, failed to parse id as number, tried as a moniker, both failed, cannot send message").i(uid as u64);
                                    }
                                }
                            } else {
                                auto_msg(format!("missing to")).i(uid as u64);
                            }
                        }
                        _ => {
                            auto_msg(format!("unrecognized command; commands: /help, /transfer, /broadcast, /msg")).i(uid as u64);
                        }
                    }
                } else {
                    auto_msg(format!("unrecognized command; commands: /help, /transfer, /broadcast, /msg")).i(uid as u64);
                }

            } else {
                interaction(uid as u64, Interaction::Broadcast(msg.to_string()));
            }
            jsn(res, json!({"status": "ok"}));
        } else {
            brq(res, "failed to parse message");
        }
    } else {
        brq(res, "failed to read message");
    }
}

#[handler]
async fn user_connected(req: &mut Request, res: &mut Response) {
    let uid = match session_check(req, None).await {
        Some(id) => id,
        None => {
            uares(res);
            return;
        }
    } as usize;

    tracing::info!("chat user came online: {}, req {:?}", uid, req);

    // Use an unbounded channel to handle buffering and flushing of messages
    // to the event source...
    let (tx, rx) = mpsc::unbounded_channel();
    let rx = UnboundedReceiverStream::new(rx);

    tx.send(Message::UserId(uid))
        // rx is right above, so this cannot fail
        .unwrap();

    // Save the sender in our list of connected users.
    ONLINE_USERS.insert(uid, tx);

    // Convert messages into Server-Sent Events and returns resulting stream.
    let stream = rx.map(move |msg| match msg {
        Message::UserId(uid) => Ok::<_, salvo::Error>(SseEvent::default().name("user").text(uid.to_string())),
        Message::Reply(reply) => Ok(SseEvent::default().text(reply))
    });
    SseKeepAlive::new(stream).streaming(res).ok();
}

enum Interaction{
    Transfer(u64, u64, Option<u64>),
    Broadcast(String),
    Message(u64, String),
    AutoMessage(String),
}

impl Interaction{
    pub fn i(self, id: u64) {
        interaction(id, self);
    }
}

fn msg(id: u64, msg: String) -> Interaction {
    Interaction::Message(id, msg)
}

fn str_msg(id: u64, message: &str) -> Interaction {
    msg(id, message.to_string())
}

fn auto_msg(msg: String) -> Interaction {
    Interaction::AutoMessage(msg)
}

fn str_auto_msg(msg: &str) -> Interaction {
    auto_msg(msg.to_string())
}

fn interaction(id: u64, i: Interaction) {
    match i {
        Interaction::Transfer(to, amount, when) => {
            tracing::info!("user {} transfer: {} to {} at {:?}", id, amount, to, when);
            // find the to account and transfer
            if let Ok(mut acc) = Account::from_id(id as u64, &DB) {
                if let Ok(mut recipient_acc) = Account::from_id(to as u64, &DB) {
                    if let Some(when) = when {
                        if when < now() {
                            auto_msg(format!("cannot transfer to the past")).i(id);
                        } else {
                            if let Err(e) = create_transfer_contract(when, id as u64, to as u64, amount) {
                                auto_msg(format!("failed to lodge transfer contract: {}", e)).i(id);
                            } else {
                                auto_msg(format!("transfer contract from {} to {} setup to happen on {}", amount, to, when)).i(id);
                                msg(to, format!("{} will be transfered to You ({}) from {}, at {}", amount, to, id, when)).i(id);                            
                            }
                        }
                    } else if let Err(e) = acc.transfer(&mut recipient_acc, amount, &DB) {
                        auto_msg(format!("failed to transfer: {}", e)).i(id);
                    } else {
                        auto_msg(format!("transfered {} to {}", amount, to)).i(id);
                        msg(to, format!("{} transfered to You ({}) from {}, at {}", amount, to, id, now())).i(id);
                    }
                } else {
                    str_auto_msg("failed to find recipient account").i(id);
                }
            } else {
                str_auto_msg("failed to find account").i(id);
            }
        },
        Interaction::Broadcast(msg) => {
            // tracing::info!("user {} broadcast: {}", id, msg);
            ONLINE_USERS.retain(|i, tx| {
                if id as usize == *i {
                    // don't send to same user, but do retain
                    true
                } else {
                    // If not `is_ok`, the SSE stream is gone, and so don't retain
                    tx.send(Message::Reply(format!("{id}:{msg}"))).is_ok()
                }
            });
        },
        Interaction::Message(uid, msg) => {
            let uid = uid as usize;
            // tracing::info!("user {} message: {}", id, msg);
            if let Some(s) = ONLINE_USERS.get(&uid) {
                if s.send(Message::Reply(format!("{id}:{msg}"))).is_err() {
                    tracing::info!("failed to send message to user {}", id);
                    ONLINE_USERS.remove(&uid);
                }
            }
        },
        Interaction::AutoMessage(msg) => {
            tracing::info!("user {} auto message: {}", id, msg);
            if let Some(s) = ONLINE_USERS.get(&(id as usize)) {
                if s.send(Message::Reply(format!("{id}:{msg}"))).is_err() {
                    tracing::info!("failed to send auto message to user {}", id);
                    ONLINE_USERS.remove(&(id as usize));
                }
            }
        }
    }
}

fn write_bytes_to_path(path: &str, s: &[u8]) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(s)?;
    Ok(())
}

fn read_bytes_from_path(path: &str) -> std::io::Result<U8s> {
    let mut file = File::open(path)?;
    let mut s = vec![];
    file.read_to_end(&mut s)?;
    Ok(s)
}

const ADMIN_PWD_FILE_PATH: &str = "./secrets/ADMIN_PWD.txt";

fn get_or_generate_admin_password() -> (U8s, Hasher) {
    match read_bytes_from_path(ADMIN_PWD_FILE_PATH) {
        Ok(s) => {
            let phsr = sthash::Hasher::new(sthash::Key::from_seed(s.as_slice(), Some(b"shok")), Some(b"zen secure"));
            (s, phsr)
        },
        Err(e) => {
            println!("failed to read admin password from file: {:?} \n generating a new one...", e);
            let s: String = thread_rng()
                .sample_iter(&Alphanumeric)
                .take(32)
                .map(char::from)
                .collect();
            let phsr = sthash::Hasher::new(sthash::Key::from_seed(s.as_bytes(), Some(b"shok")), Some(b"zen secure"));
            // write it to a file
            write_bytes_to_path(ADMIN_PWD_FILE_PATH, s.as_bytes()).expect("failed to write admin password to file");
            (s.as_bytes().to_vec(), phsr)
        }
    }
}

lazy_static!{
    static ref B64: base64::engine::GeneralPurpose = {
        let abc = base64::alphabet::Alphabet::new("+_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789").expect("aplhabet was too much for base64, sorry");
        base64::engine::GeneralPurpose::new(&abc, base64::engine::general_purpose::GeneralPurposeConfig::new().with_encode_padding(false).with_decode_allow_trailing_bits(true))
    };
    static ref PWD: (U8s, Hasher) = get_or_generate_admin_password();
}

fn check_admin_password(pwd: &[u8]) -> bool {
    &PWD.1.hash(pwd) == PWD.0.as_slice()
}

/*
fn zstd_compress(data: &[u8]) -> std::io::Result<U8s> {
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 21)?;
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

fn zstd_decompress(data: &[u8]) -> std::io::Result<U8s> {
    let mut decoder = zstd::stream::Decoder::new(std::io::Cursor::new(data))?;
    let mut buf = Vec::new();
    decoder.read_to_end(&mut buf)?;
    Ok(buf)
}*/

fn zstd_compress(data: &mut Vec<u8>, output: &mut Vec<u8>) -> anyhow::Result<()> {
    let mut encoder = zstd::stream::Encoder::new(output, 6)?;
    encoder.write_all(data)?;
    encoder.finish()?;
    Ok(())
}

fn zstd_decompress(data: &mut Vec<u8>, output: &mut Vec<u8>) -> anyhow::Result<usize> {
    let mut decoder = zstd::stream::Decoder::new(std::io::Cursor::new(data))?;
    let decompressed_size = decoder.read_to_end(output)?;
    Ok(decompressed_size)
}

fn serialize_and_compress<T: Serialize>(payload: T) -> anyhow::Result<U8s> {
    let mut serialized = serde_json::to_vec(&payload)?;
    forget(payload);
    let mut serialized_payload = vec![];
    zstd_compress(&mut serialized, &mut serialized_payload)?;
    Ok(serialized_payload)
}

fn decompress_and_deserialize<'a, T: serde::de::DeserializeOwned>(data: &mut Vec<u8>) -> anyhow::Result<T> {
    let mut serialized = vec![];
    zstd_decompress(data, &mut serialized)?;
    let payload: T = serde_json::from_slice(&serialized)?;
    forget(serialized);
    Ok(payload)
}

fn encrypt<T: Serialize>(payload: T, pwd: &[u8]) -> anyhow::Result<U8s> {
    let mut serialized = serde_json::to_vec(&payload)?;
    let mut serialized_payload = vec![];
    zstd_compress(&mut serialized, &mut serialized_payload)?;
    forget(serialized);
    let cocoon = cocoon::Cocoon::new(pwd);
    match cocoon.encrypt(&mut serialized_payload) {
        Ok(detached) => {
            serialized_payload.extend_from_slice(&detached);
            return Ok(serialized_payload);
        },
        Err(e) => Err(
            anyhow::Error::msg(format!("failed to encrypt payload: {:?}", e))
        )
    }
}

fn decrypt<'a, T: serde::de::DeserializeOwned>(
    whole: &'a [u8],
    pwd: &[u8]
) -> anyhow::Result<T> {
    // split whole into encrypted payload and detached prefix, the last 60 bytes are the detached prefix
    let (encrypted_payload, detached) = whole.split_at(whole.len() - 60);
    let mut decrypted_payload = encrypted_payload.to_vec();
    forget(encrypted_payload);
    let cocoon = cocoon::Cocoon::new(pwd);
    match cocoon.decrypt(decrypted_payload.as_mut_slice(), detached) {
        Ok(()) => {
            let mut dp = vec![];
            zstd_decompress(&mut decrypted_payload, &mut dp)?;
            // println!("decrypted payload: {:?}", String::from_utf8_lossy(&dp));
            let payload: T = serde_json::from_slice(&dp)?;
            Ok(payload)
        },
        Err(e) => Err(
            anyhow::Error::msg(format!("failed to decrypt payload: {:?}", e))
        )
    }
}

const ADMIN_ID: u64 = 1997;
const STATIC_DIR: &'static str = "./static/";
// token, account_id, expiry
const SESSIONS: TableDefinition<&[u8], (u64, u64)> = TableDefinition::new("sessions");

const SESSION_EXPIRIES: TableDefinition<u64, &[u8]> = TableDefinition::new("session_expiries");

fn expire_sessions() -> anyhow::Result<()> {
    let wrtx = DB.begin_write()?;
    {
        let mut t = wrtx.open_table(SESSION_EXPIRIES)?;
        t.drain_filter(..now(), |_, v| {
            let st = wrtx.open_table(SESSIONS);
            let tkh = TOKEN_HASHER.hash(v);
            st.is_ok() && st.unwrap().remove(tkh.as_slice()).is_ok()
        })?;
    }
    wrtx.commit()?;
    Ok(())
}
#[allow(dead_code)]
fn expire_session(token: &str) -> anyhow::Result<()> {
    let wrtx = DB.begin_write()?;
    {
        let mut t = wrtx.open_table(SESSIONS)?;
        let tkh = TOKEN_HASHER.hash(token.as_bytes());
        t.remove(tkh.as_slice())?;
    }
    Ok(())
}
#[allow(dead_code)]
fn register_session_expiry(token: &str, expiry: u64) -> anyhow::Result<()> {
    let wrtx = DB.begin_write()?;
    {
        let mut t = wrtx.open_table(SESSION_EXPIRIES)?;
        let tkh = TOKEN_HASHER.hash(token.as_bytes());
        t.insert(expiry, tkh.as_slice())?;
    }
    Ok(())
}

pub fn expiry_checker() -> std::thread::JoinHandle<()> {
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(Duration::from_secs(30));
            match expire_sessions() {
                Ok(()) => {},
                Err(e) => {
                    println!("failed to expire sessions: {:?}", e);
                }
            }
            match expire_tokens() {
                Ok(()) => {},
                Err(e) => {
                    println!("failed to expire tokens: {:?}", e);
                }
            }
            match expire_resources() {
                Ok(()) => {},
                Err(e) => {
                    println!("failed to expire resources: {:?}", e);
                }
            }
            update_static_dir_paths();
            match run_contracts() {
                Ok(()) => {},
                Err(e) => {
                    println!("failed to run contracts: {:?}", e);
                }
            }
            match run_stored_commands() {
                Ok(()) => {},
                Err(e) => {
                    println!("failed to run stored commands: {:?}", e);
                }
            }
            match run_scoped_variable_exps() {
                Ok(()) => {},
                Err(e) => {
                    println!("failed to run scoped variable exps: {:?}", e);
                }
            }
        }
    })
}

// Accounts             name, since, xp, balance, pwd_hash
const ACCOUNTS: TableDefinition<u64, (&str, u64, u64, u64, &[u8])> = TableDefinition::new("accounts");
const ACCOUNT_MONIKER_LOOKUP: TableDefinition<&str, u64> = TableDefinition::new("account_moniker_lookup");
// Tokens                            perm_schema, account_id, expiry, uses, state
const TOKENS: TableDefinition<&[u8], (u32, u64, u64, u64, Option<&[u8]>)> = TableDefinition::new("tokens");

const TOKEN_EXPIERIES: TableDefinition<u64, &[u8]> = TableDefinition::new("token_expiries");
const RESOURCE_EXPIERIES: TableDefinition<u64, &[u8]> = TableDefinition::new("resource_expiries");

fn expire_resources() -> anyhow::Result<()> {
    let wrtx = DB.begin_write()?;
    {
        let mut t = wrtx.open_table(RESOURCE_EXPIERIES)?;
        t.drain_filter(..now(), |_, v| {
            Resource::delete(v).is_ok()
        })?;
    }
    wrtx.commit()?;
    Ok(())
}

fn register_resource_expiry(resource: &[u8], expiry: u64) -> anyhow::Result<()> {
    let wrtx = DB.begin_write()?;
    {
        let mut t = wrtx.open_table(RESOURCE_EXPIERIES)?;
        t.insert(expiry, resource)?;
    }
    Ok(())
}

fn expire_tokens() -> anyhow::Result<()> {
    let wrtx = DB.begin_write()?;
    {
        let mut t = wrtx.open_table(TOKEN_EXPIERIES)?;
        t.drain_filter(..now(), |_, v| {
            let st = wrtx.open_table(TOKENS);
            st.is_ok() && st.unwrap().remove(v).is_ok()
        })?;
    }
    wrtx.commit()?;
    Ok(())
}

#[allow(dead_code)]
fn register_token_expiry(token: &[u8], expiry: u64) -> anyhow::Result<()> {
    let wrtx = DB.begin_write()?;
    {
        let mut t = wrtx.open_table(TOKEN_EXPIERIES)?;
        t.insert(expiry, token)?;
    }
    wrtx.commit()?;
    Ok(())
}
// const OWNERSHIP_INDEX: MultimapTableDefinition<&str, &[u8]> = MultimapTableDefinition::new("OI");
/*
#[derive(Serialize, Deserialize)]
struct Item{
    shop: u64,
    id: u64,
    created: u64,
    owner: u64,
    unique: bool,
    dupable: bool,
    categories: Strings,
    tags: Strings,
    kind: Option<String>,
    meta_data: Option<U8s>,
    mime_type: Option<String>,
    owners: HashMap<u64, u64>, // account_id, shares
    sellable: bool,
    price: u64,
    coupons: HashMap<String, (u64, bool, Option<U8s>)>, // code, (discount, once, more_data)
    expiry: u64,
    description: Option<String>,
    state: Option<U8s>
}
 */
/*
    Permision Schemas should be determinative of the state deserialization type of token's Option<&[u8]> state field
    0: read
    1: read/write
    2: read/write/execute
    3: read/write/execute/transact

*/

const SCOPED_VARIABLES: TableDefinition<(u64, &str), &[u8]> = TableDefinition::new("SCOPED_VARIABLES");
const SCOPED_VARIABLE_OWNERSHIP_INDEX: MultimapTableDefinition<u64, &str> = MultimapTableDefinition::new("SCOPED_VARIABLE_OWNERSHIP_INDEX");
const SCOPED_VARIABLE_EXPIRIES: TableDefinition<u64, (u64, &str)> = TableDefinition::new("SCOPED_VARIABLE_EXPIRIES");

// tag system for variables with reverse lookup
const SCOPED_VARIABLE_TAGS_MONIKER_LOOKUP: MultimapTableDefinition<(u64, &str), u64> = MultimapTableDefinition::new("SCOPED_VARIABLE_TAGS_MONIKER_LOOKUP");
const SCOPED_VARIABLE_TAGS: MultimapTableDefinition<(u64, &str), u64> = MultimapTableDefinition::new("SCOPED_VARIABLE_TAGS");
// const SCOPED_VARIABLE_TAG_INDEX: MultimapTableDefinition<u64, (u64, &str)> = MultimapTableDefinition::new("SCOPED_VARIABLE_TAG_INDEX");

// const SCOPED_PW_HASH_INDEX: TableDefinition<&[u8], u64> = TableDefinition::new("SCOPED_PW_HASH_INDEX");

struct ScopedVariableStore<T: Serialize + serde::de::DeserializeOwned + Clone> {
    owner: u64,
    s: Option<Sender<MutEvent>>, // name, value, is_insert
    pd: PhantomData<T>,
}

/*

Welcome, tis an experiment at blogging

Handrolled tech stack, and auth lol. oop. 

anyway, it seems to work, not really finished but for a doodle it is functional.. somewhat, now and again.. eh. lemme know. 

meta update
 */

// todo: encryption, serialization, generics for value type [done :D, 30 June, but not tested yet]

#[handler]
async fn scoped_variable_store_api(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    let mut _pm: Option<u32> = None;
    let mut _owner: Option<u64> = None;
    let mut _is_admin = false;
    if session_check(req, Some(ADMIN_ID)).await.is_some() {
        // admin session
        _is_admin = true;
        _owner = Some(ADMIN_ID);
    } else {
        if let Some(tk) = req.query::<String>("tk") {
            if let Ok((perm_schema, owner, _, uses, _)) = validate_token_under_permision_schema(&tk, &[u32::MAX - 5], &DB).await {
                // token session
                _pm = Some(perm_schema);
                _owner = Some(owner);
                if uses == 0 {
                    brq(res, "token's uses are exhausted, it expired, let it go m8");
                    return;
                }
            } else {
                brq(res, "not authorized to use the scoped_variable_api, bad token and not an admin");
                return;
            }
        } else {
            brq(res, "not authorized to use the scoped_variable_api");
            return;
        }
    }

    if _owner.is_none() {
        brq(res, "not authorized to use the scoped_variable_api, no owner");
        return;
    }

    let moniker = match req.param::<String>("moniker") {
        Some(m) => m,
        None => {
            brq(res, "invalid scoped_variable_api request, no moniker provided");
            return;
        }
    };

    match *req.method() {
        Method::GET => {
            if let Ok((svs, _)) = ScopedVariableStore::<serde_json::Value>::open(_owner.unwrap(), false) {
                match svs.get(&moniker) {
                    Ok(v) => {
                        if v.is_none() {
                            brq(res, "variable not found");
                            return;
                        }
                        jsn(res, json!({
                            "ok": true,
                            "value": v.unwrap()
                        }));
                    },
                    Err(e) => {
                        brq(res, &format!("error getting variable: {}", e));
                    }
                }
            } else {
                brq(res, "invalid password");
            }
        },
        Method::POST => {
            let body = match req.parse_json_with_max_size::<serde_json::Value>(20480).await {
                Ok(b) => b,
                Err(e) => {
                    brq(res, &format!("error parsing json body: {}", e));
                    return;
                }
            };

            match ScopedVariableStore::<serde_json::Value>::open(_owner.unwrap(), false) {
                Ok((svs, _)) => {
                    if let Err(e) = svs.set(&moniker, body) {
                        // println!("error setting variable: {:?}", e);
                        brq(res, &format!("error setting variable: {}", e));
                    } else {
                        jsn(res, json!({"ok": true}));
                    }
                    return;
                },
                Err(e) => {
                    brq(res, &format!("error opening scoped variable store: {}", e));
                    return;
                }
            }
        },
        Method::DELETE => if let Ok((svs, _)) = ScopedVariableStore::<serde_json::Value>::open(_owner.unwrap(), false) {
            if let Err(e) = svs.rm(&moniker) {
                brq(res, &format!("error deleting variable: {}", e));
                return;
            } else {
                jsn(res, json!({"ok": true}));
            }
        } else {
            brq(res, "invalid password");
            return;
        },
        _ => {
            res.status_code(StatusCode::METHOD_NOT_ALLOWED);
        }
    }
}


impl<T: Serialize + serde::de::DeserializeOwned + Clone> ScopedVariableStore<T> {
    fn open(owner: u64, with_channel: bool) -> anyhow::Result<(Self, Option<Receiver<MutEvent>>)> {
        let pd = PhantomData::default();
        if with_channel {
            let (s, r) = channel::<MutEvent>();
            return Ok((Self{owner, s: Some(s), pd}, Some(r)));
        }
        Ok((Self{owner, s: None, pd}, None))
    }
    #[allow(dead_code)]
    fn register_expiry(&self, name: &str, exp: u64) -> anyhow::Result<()> {
        let wrtx = DB.begin_write()?;
        {
            let mut t = wrtx.open_table(SCOPED_VARIABLE_EXPIRIES)?;
            t.insert(exp, (self.owner, name))?;
        }
        wrtx.commit()?;
        Ok(())
    }
    fn set(&self, name: &str, value: T) -> anyhow::Result<Option<T>> {
        let mut _od = None;
        let wrtx = DB.begin_write()?;
        {
            let mut t = wrtx.open_table(SCOPED_VARIABLES)?;
            let old_data = t.insert((self.owner, name), serialize_and_compress(value)?.as_slice())?;
            if let Some(ag) = old_data {
                let mut dt = ag.value().to_vec();
                forget(ag);
                _od = Some(decompress_and_deserialize(&mut dt)?);
                forget(dt);
            }
            let mut ot = wrtx.open_multimap_table(SCOPED_VARIABLE_OWNERSHIP_INDEX)?;
            ot.insert(self.owner, name)?;
        }
        wrtx.commit()?;
        if let Some(ref s) = self.s {
            s.send((Arc::from(name), true))?;
        }
        Ok(_od)
    }
    #[allow(dead_code)]
    fn set_many(&self, values: &[(Arc<str>, T)]) -> anyhow::Result<()> {
        let wrtx = DB.begin_write()?;
        {
            let mut t = wrtx.open_table(SCOPED_VARIABLES)?;
            let mut ot = wrtx.open_multimap_table(SCOPED_VARIABLE_OWNERSHIP_INDEX)?;
            for (name, value) in values {
                t.insert((self.owner, name.as_ref()), serialize_and_compress(value)?.as_slice())?;
                ot.insert(self.owner, name.as_ref())?;
            }
        }
        wrtx.commit()?;
        for (name, _value) in values {
            if let Some(ref s) = self.s {
                s.send((name.clone(), true))?;
            }
        }
        Ok(())
    }
    fn get(&self, name: &str) -> anyhow::Result<Option<T>> {
        let mut data = None;
        let rtx = DB.begin_read()?;
        let t = rtx.open_table(SCOPED_VARIABLES)?;
        if let Some(ag) = t.get((self.owner, name))? {
            let mut dt = ag.value().to_vec();
            forget(ag);
            data = Some(decompress_and_deserialize(&mut dt)?);
        }
        match data {
            Some(d) => Ok(Some(d)),
            None => Err(
                anyhow::anyhow!("scoped variable {} not found", name)
            )
        }
    }
    #[allow(dead_code)]
    fn get_many(&self, names: &[&str]) -> anyhow::Result<Vec<Option<T>>> {
        let mut data = Vec::with_capacity(names.len());
        let rtx = DB.begin_read()?;
        let t = rtx.open_table(SCOPED_VARIABLES)?;
        for name in names {
            if let Some(ag) = t.get((self.owner, *name))? {
                let mut dt = ag.value().to_vec();
                forget(ag);
                data.push(decompress_and_deserialize(&mut dt)?);
            } else {
                data.push(None);
            }
        }
        Ok(data)
    }
    fn rm(&self, name: &str) -> anyhow::Result<Option<T>> {
        let mut data = None;
        let wrtx = DB.begin_write()?;
        {
            let mut t = wrtx.open_table(SCOPED_VARIABLES)?;
            let mut ot = wrtx.open_multimap_table(SCOPED_VARIABLE_OWNERSHIP_INDEX)?;
            let old_data = t.remove((self.owner, name))?;
            ot.remove(self.owner, name)?;
            if let Some(ag) = old_data {
                let mut dt = ag.value().to_vec();
                forget(ag);
                data = Some(decompress_and_deserialize(&mut dt)?);
                if let Some(ref s) = self.s {
                    s.send((Arc::from(name), false))?;
                }
            }
            let mut st = wrtx.open_multimap_table(SCOPED_VARIABLE_TAGS)?;
            let mut stl = wrtx.open_multimap_table(SCOPED_VARIABLE_TAGS_MONIKER_LOOKUP)?;
            // get tags
            let mut tags = Vec::new();
            {
                let mut mmv = st.get((self.owner, name))?;
                while let Some(Ok(ag)) = mmv.next() {
                    tags.push(ag.value());
                }
            }
            // remove tags
            for tag in tags {
                st.remove((self.owner, name), tag)?;
                stl.remove((tag, name), self.owner)?;
            }
        }
        wrtx.commit()?;
        Ok(data)
    }
    #[allow(dead_code)]
    fn rm_many(&self, names: &[&str]) -> anyhow::Result<Vec<Option<T>>> {
        let mut data = Vec::with_capacity(names.len());
        let wrtx = DB.begin_write()?;
        {
            let mut t = wrtx.open_table(SCOPED_VARIABLES)?;
            let mut ot = wrtx.open_multimap_table(SCOPED_VARIABLE_OWNERSHIP_INDEX)?;
            for name in names {
                if let Some(ag) = t.remove((self.owner, *name))? {
                    let mut dt = ag.value().to_vec();
                    forget(ag);
                    data.push(Some(decompress_and_deserialize(&mut dt)?));
                    if let Some(ref s) = self.s {
                        s.send((Arc::from(*name), true))?;
                    }
                } else {
                    data.push(None);
                }
                ot.remove(self.owner, name)?;
                let mut st = wrtx.open_multimap_table(SCOPED_VARIABLE_TAGS)?;
                let mut stl = wrtx.open_multimap_table(SCOPED_VARIABLE_TAGS_MONIKER_LOOKUP)?;
                // get tags
                let mut tags = Vec::new();
                {
                    let mut mmv = st.get((self.owner, *name))?;
                    while let Some(Ok(ag)) = mmv.next() {
                        tags.push(ag.value());
                    }
                }
                // remove tags
                for tag in tags {
                    st.remove((self.owner, *name), tag)?;
                    stl.remove((tag, *name), self.owner)?;
                }
            }
        }
        wrtx.commit()?;
        Ok(data)
    }
}

fn run_scoped_variable_exps() -> anyhow::Result<()> {
    let wrtx = DB.begin_write()?;
    {
        let mut exp_t = wrtx.open_table(SCOPED_VARIABLE_EXPIRIES)?;
        exp_t.drain_filter(..now(), |_, (_owner, _moniker)| {
            true
        })?;
    }
    wrtx.commit()?;
    Ok(())
}

//                                      when, from, to, amount
const STAKED_TRANSFERS: TableDefinition<u64, (u64, u64, u64)> = TableDefinition::new("staked-transfers");

fn create_transfer_contract(
    when: u64,
    from: u64,
    to: u64,
    amount: u64
) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut _from_acc = None;
        let mut st = wtx.open_table(STAKED_TRANSFERS)?;
        let mut at = wtx.open_table(ACCOUNTS)?;
        if let Some(ag) = at.get(from)? {
            let (m, since, xp, balance, pwh) = ag.value();
            _from_acc = Some(Account{
                id: from,
                moniker: m.to_string(),
                balance: balance - amount,
                since,
                xp,
                pwd_hash: pwh.to_vec()
            });
        } else {
            return Err(anyhow::Error::msg("from account not found"));
        }

        if _from_acc.is_none() {
            return Err(anyhow::Error::msg("from account not found"));
        }

        if at.get(to)?.is_none() {
            return Err(anyhow::Error::msg("to account not found"));
        }

        let from_acc = _from_acc.unwrap();

        if from_acc.balance < amount {
            return Err(anyhow::Error::msg("insufficient funds"));
        }

        at.insert(from, (
            from_acc.moniker.as_str(),
            from_acc.since,
            from_acc.xp,
            from_acc.balance,
            from_acc.pwd_hash.as_slice()
        ))?;

        // let mut t = at.get(to)?.ok_or_else(|| anyhow::Error::msg("to account not found"))?.value();
        st.insert(when, (from, to, amount))?;
    }
    wtx.commit()?;
    Ok(())
}

fn run_contracts() -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut st = wtx.open_table(STAKED_TRANSFERS)?;
        st.drain_filter(now().., |_when, (from, to, amount)| {
            if let Ok(mut at) = wtx.open_table(ACCOUNTS) {
                let mut _to_acc = None;
                let mut _from_acc = None;
                if let Ok(res) = at.get(to) {
                    if res.is_none() {
                        if let Ok(Some(ag)) = at.get(from) {
                            let (m, since, xp, balance, pwh) = ag.value();
                            _from_acc = Some(Account{
                                id: from,
                                moniker: m.to_string(),
                                balance: balance + amount,
                                since,
                                xp,
                                pwd_hash: pwh.to_vec()
                            });
                        }
                    } else {
                        let ag = res.unwrap();
                        let (m, since, xp, balance, pwh) = ag.value();
                        _to_acc = Some(Account{
                            id: to,
                            moniker: m.to_string(),
                            balance: balance + amount,
                            since,
                            xp,
                            pwd_hash: pwh.to_vec()
                        });
                    }
                }
                if let Some(to_acc) = _to_acc {
                    if at.insert(to, (
                        to_acc.moniker.as_str(),
                        to_acc.since,
                        to_acc.xp,
                        to_acc.balance,
                        to_acc.pwd_hash.as_slice()
                    )).is_ok() {
                        return true;
                    }
                } else if let Some(from_acc) = _from_acc {
                    if at.insert(from, (
                        from_acc.moniker.as_str(),
                        from_acc.since,
                        from_acc.xp,
                        from_acc.balance,
                        from_acc.pwd_hash.as_slice()
                    )).is_ok() {
                        return true;
                    }
                }
            }
            false
        })?;
    }
    wtx.commit()?;
    Ok(())
}

#[derive(Serialize, Deserialize, Debug)]
struct TransferRequest{
    when: u64,
    to: u64,
    amount: u64
}

impl TransferRequest{
    async fn lodge(&self, from: u64) -> anyhow::Result<()> {
        create_transfer_contract(
            self.when,
            from,
            self.to,
            self.amount
        )
    }
}

#[handler]
async fn transference_api(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    let mut _pm: Option<u32> = None;
    let mut _owner: Option<u64> = None;
    let mut _is_admin = false;
    if session_check(req, Some(ADMIN_ID)).await.is_some() {
        // admin session
        _is_admin = true;
    } else {
        if let Some(tk) = req.query::<String>("tk") {
            if let Ok((perm_schema, owner, _, _, _)) = validate_token_under_permision_schema(&tk, &[3], &DB).await {
                // token session
                _pm = Some(perm_schema);
                _owner = Some(owner);
            } else {
                brq(res, "not authorized to use the transference_api");
                return;
            }
        } else {
            brq(res, "not authorized to use the transference_api");
            return;
        }
    }

    if _owner.is_none() {
        brq(res, "not authorized to use the transference_api");
        return;
    }

    match *req.method() {
        Method::POST => if let Ok(tr) = req.parse_json_with_max_size::<TransferRequest>(20480).await {
            match tr.lodge(_owner.unwrap()).await {
                Ok(()) => brq(res, "transfer contract lodged successfully"),
                Err(e) => brq(res, &format!("failed to lodge transfer contract: {}", e))
            }
        } else {
            brq(res, "invalid transfer request");
        },
        _ => {
            brq(res, "invalid method");
        }
    }
}

const PERM_SCHEMAS : MultimapTableDefinition<u32, &str> = MultimapTableDefinition::new("perm_schemas");

#[derive(Serialize, Deserialize)]
struct PermSchema(u32, Strings);

const DEFAULT_PERM_SCHEMAS: &[&str] = &[
    "r",
    "rw",
    "rwx",
    "transact",
    "svs",
    "list-uploads",
    "upload"
];

impl PermSchema {
    fn ensure_basic_defaults(db: &Database) {
        let wrtx = db.begin_write().expect("db write failed");
        {
            let mut t = wrtx.open_multimap_table(PERM_SCHEMAS).expect("db write failed");
            let mut pm = u32::MAX;
            for code in DEFAULT_PERM_SCHEMAS {
                t.insert(pm, code).expect("db default permission schema insert failed");
                pm -= 1;
            }
        }
        wrtx.commit().expect("failed to insert basic default perm schemas");
    }

    fn modify(add: Option<Strings>, rm: Option<Strings>, id: Option<u32>, db: &Database) -> Result<Self, redb::Error> {
        let wrtx = db.begin_write()?;
        let id = match id {
            Some(i) => i,
            None => {
                // random
                let mut rng = thread_rng();
                loop {
                    let i = rng.gen::<u32>();
                    if i < u32::MAX - DEFAULT_PERM_SCHEMAS.len() as u32 {
                        break i;
                    }
                }
            }
        };
        let mut perms = vec![];
        {
            let mut t = wrtx.open_multimap_table(PERM_SCHEMAS)?;
            if let Some(pms) = add {
                for p in pms {
                    t.insert(id, p.as_str())?;
                }
                let mut mmv = t.get(id)?;
                while let Some(p) = mmv.next() {
                    perms.push(p?.value().to_string());
                }
            }
            if let Some(pms) = rm {
                for p in pms {
                    t.remove(id, p.as_str())?;
                }
            }
        }
        wrtx.commit()?;
        Ok(Self(id, perms))
    }
    #[allow(dead_code)]
    fn check_for(pm: u32, db: &Database, perms: &[&str]) -> Result<bool, redb::Error> {
        let rtx = db.begin_read()?;
        let t = rtx.open_multimap_table(PERM_SCHEMAS)?;
        let mut mm = t.get(pm)?;
        let mut all_there = perms.len();
        while let Some(p) = mm.next() {
            if perms.contains(&(p?).value()) { 
                all_there -= 1;
                if all_there == 0 { break; }
            }
        }
        Ok(all_there == 0)
    }
    #[allow(dead_code)]
    fn add_perms(pm: u32, db: &Database, perms: &Strings) -> Result<(), redb::Error> {
        let wrtx = db.begin_write()?;
        {
            let mut t = wrtx.open_multimap_table(PERM_SCHEMAS)?;
            for p in perms.iter() {
                t.insert(pm, p.as_str())?;
            }
        }
        wrtx.commit()?;
        Ok(())
    }
    #[allow(dead_code)]
    fn remove_perms(pm: u32, db: &Database, perms: &Strings) -> Result<(), redb::Error> {
        let wrtx = db.begin_write()?;
        {
            let mut t = wrtx.open_multimap_table(PERM_SCHEMAS)?;
            for p in perms.iter() {
                t.remove(pm, p.as_str())?;
            }
            // TODO: cleanup perm schema if it is empty
        }
        wrtx.commit()?;
        Ok(())
    }
}


async fn make_tokens(
    pm: u32,
    id: u64,
    mut count: u16,
    exp: u64,
    uses: u64,
    data: Option<&[u8]>,
    db: &Database
) -> Result<Strings, redb::Error> {
    let mut tkns = vec![];
    let mut tk: String;
    let wtx = db.begin_write()?;
    {
        let mut t = wtx.open_table(TOKENS)?;
        let mut te_t = wtx.open_table(TOKEN_EXPIERIES)?;
        while count > 0 {
            tk = thread_rng().sample_iter(&Alphanumeric).take(32).map(char::from).collect();
            let token_hash = TOKEN_HASHER.hash(tk.as_bytes());
            t.insert(token_hash.as_slice(), (pm, id, exp, uses, data))?;
            te_t.insert(exp, token_hash.as_slice())?;
            tkns.push(tk.clone());
            count -= 1;
        }
    }
    wtx.commit()?;
    Ok(tkns)
}

async fn remove_tokens(tkns: &[&str], db: &Database) -> Result<(), redb::Error> {
    let wtx = db.begin_write()?;
    {
        let mut t = wtx.open_table(TOKENS)?;
        for tk in tkns.iter() {
            let token_hash = TOKEN_HASHER.hash(tk.as_bytes());
            t.remove(token_hash.as_slice())?;
        }
    }
    wtx.commit()?;
    Ok(())
}

fn read_token(tkh: &[u8], db: &Database) -> Result<(u32, u64, u64, u64, Option<U8s>), redb::Error> {
    let rtx = db.begin_read()?;
    let t = rtx.open_table(TOKENS)?;
    if let Some(ag) = t.get(tkh)? {
        let tk = ag.value();
        return Ok((
            tk.0,
            tk.1,
            tk.2,
            tk.3,
            tk.4.map(|v| v.to_vec())
        ));
    }
    Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "Token not found.")))
}

#[allow(dead_code)]
fn update_token_state_field<F>(tkh: &[u8], db: &Database, closure: F) -> Result<(), redb::Error> where F: Fn(Option<&[u8]>) -> Option<U8s> {
    let wtx = db.begin_write()?;
    let mut was_none = true;
    let mut _tk = None;
    {
        let mut t = wtx.open_table(TOKENS)?;
        if let Some(ag) = t.get(tkh)? {
            let tk = ag.value();
            _tk = Some((
                tk.0,
                tk.1,
                tk.2,
                tk.3,
                closure(tk.4)
            ));
        }
        if let Some(tk) = _tk {
            t.insert(tkh, (
                tk.0,
                tk.1,
                tk.2,
                tk.3,
                match &tk.4 {
                    Some(v) => Some(v.as_slice()),
                    _ => None
                }
            ))?;
            was_none = false;
        }
    }
    if was_none {
        wtx.abort()?;
    } else {
        wtx.commit()?;
    }
    Ok(())
}

async fn validate_token_under_permision_schema(
    tk: &str,
    pms: &[u32],
    db: &Database
) -> Result<(
    u32, // permission
    u64, // id
    u64, // exp
    u64, // uses 
    Option<U8s> // state
), redb::Error> {
    let token_hash = TOKEN_HASHER.hash(tk.as_bytes());
    match read_token(&token_hash, &DB) {
        Ok(d) => {
            let mut adequate_perm = false;
            for pm in pms.iter() {
                if d.0 == *pm {
                    adequate_perm = true;
                    break;
                }
            }
            if !adequate_perm {
                return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "Token does not have the required permissions.")))
            }
            if d.2 < now() {
                remove_tokens(&[tk], &DB).await?;
                return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "Token has expired.")));
            }

            let data = d.4.unwrap_or(vec![]);
            let d_o = match data.len() {
                0 => None,
                // match case for above 20kb
                20480.. => {
                    // return an error saying it is too much
                    remove_tokens(&[tk], &DB).await?;
                    return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, "Token state field data is too large.")));
                },
                _ => Some(data.as_slice())
            };

            if d.3 == 0 {
                remove_tokens(&[tk], &DB).await?;
                return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "Token has been used up.")));
            } else if d.3 != u64::MAX {
                let wtx = db.begin_write()?;
                {
                    let uses = d.3 - 1;
                    let mut t = wtx.open_table(TOKENS)?;
                    if uses == 0 {
                        t.remove(token_hash.as_slice())?;
                    } else {
                        
                        t.insert(token_hash.as_slice(), (d.0, d.1, d.2, uses, d_o))?;
                    }
                }
                wtx.commit()?;
            }
            Ok((d.0, d.1, d.2, d.3, d_o.is_some().then(|| data)))
        },
        Err(e) => Err(e)
    }
}

#[handler]
async fn action_token_handler(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    if let Some(action) = req.param::<&str>("action") {
        if let Some(tk) = req.param::<&str>("tk") {
            if let Ok((pm, id, exp, _, state)) = validate_token_under_permision_schema(tk, &[], &DB).await { // (u32, u64, u64, u64, Option<U8s>)
                match action {
                    "/create-resource" => if req.method() == Method::POST && [u32::MAX - 1].contains(&pm) && state.is_some() {
                        let mut r = Resource::from_blob(&state.unwrap(), Some(id), "application/octet-stream".to_string());
                        r.until = Some(exp);
                        if r.save(false).is_ok() {
                            if r.save(false).is_ok() {
                                jsn(res, json!({
                                    "msg": "Resource created."
                                }));
                            } else {
                                brq(res, "Failed to create resource.");
                            }
                            return;
                        }
                    },
                    _ => {
                        brq(res, "Invalid action.");
                    }
                }
            } else {
                brq(res, "Invalid token.");
            }
        } else {
            brq(res, "Missing token.");
        }
    } else {
        brq(res, "Missing action.");
    }
}

lazy_static! {
    static ref DB: Database = {
        let db = Database::create("./shok.db").expect("Failed to open database.");
        db
    };
    static ref TOKEN_HASHER: sthash::Hasher = {
        let key = PWD.0.as_slice();
        sthash::Hasher::new(sthash::Key::from_seed(key, Some(b"shok-tk")), Some(b"tokens"))
    };
    static ref RESOURCE_HASHER: sthash::Hasher = {
        let key = PWD.0.as_slice();
        sthash::Hasher::new(sthash::Key::from_seed(key, Some(b"resources")), Some(b"insurance"))
    };
    static ref SEARCH: Search = Search::build_150mb().expect("Failed to build search.");
}

#[allow(dead_code)]
struct ScopedScopelessHasher(Vec<sthash::Hasher>);

#[allow(dead_code)]
impl ScopedScopelessHasher {
    fn new(key: &[u8], scopes: Vec<U8s>, extra_scopedness: Option<U8s>) -> Self {
        let mut hashers: Vec<sthash::Hasher> = vec![];
        for s in scopes {
            let h = sthash::Hasher::new(sthash::Key::from_seed(key, Some(&s)), match extra_scopedness {
                Some(ref v) => Some(v.as_slice()),
                None => None
            });
            hashers.push(h);
        }
        Self(hashers)
    }

    fn hash_with_scope(&self, data: &[u8], scope: usize) -> U8s {
        self.0[scope].hash(data)
    }

    fn check(&self, data: &[u8], hash: &[u8]) -> Option<usize> {
        let mut which = 0_usize;
        for h in &self.0 {
            if h.hash(data) == hash {
                return Some(which);
            }
            which += 1;
        }
        None
    }
}


#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();
    //let db = init_surrealdb_connection().await?;
    
    let sp = PathBuf::new().join(STATIC_DIR);
    println!("Static files dir: exists - {:?}, {}", sp.exists(), sp.into_os_string().into_string().unwrap_or("bad path".to_string()));
    
    PermSchema::ensure_basic_defaults(&DB);

    let _expiry_checker = expiry_checker();

    let addr = ("0.0.0.0", 8000);
    let config = load_config();

    let api_limiter = RateLimiter::new(
        FixedGuard::new(),
        MemoryStore::new(),
        RemoteIpIssuer,
        BasicQuota::per_second(160),
    );

    let auth_limiter = RateLimiter::new(
        FixedGuard::new(),
        MemoryStore::new(),
        RemoteIpIssuer,
        BasicQuota::per_second(10),
    );

    let static_files_cache = salvo::cache::Cache::new(
        salvo::cache::MemoryStore::builder().time_to_live(Duration::from_secs(120)).build(),
        salvo::cache::RequestIssuer::default(),
    );

    let router = Router::with_hoop(Logger::new())
        .push(
            Router::with_path("/healthcheck")
                .get(health)
        )
        .push(
            Router::with_path("/auth").hoop(auth_limiter)
                .post(auth_handler)
                .delete(auto_unauth)
        )
        .push(
            Router::with_path("/unauth")
                .get(auto_unauth)
        )
        .push(
            Router::with_path("/api").hoop(api_limiter)
                .push(
                    Router::with_path("/cmd")
                            .post(cmd_request)
                )
                .push(
                    Router::with_path("/moniker-lookup/<id>")
                            .handle(moniker_lookup)
                )
                .push(
                    Router::with_path("/speak")
                            .post(speak)
                )
                .push(
                    Router::with_path("/search")
                    .hoop(Compression::new().enable_gzip(CompressionLevel::Minsize))
                    .hoop(CachingHeaders::new())
                    .handle(search_api)
                )
                .push(
                    Router::with_path("chat")
                        .hoop(Compression::new().enable_gzip(CompressionLevel::Minsize))
                        .get(user_connected)
                        .post(chat_send)
                        //.push(Router::with_path("<id>"))
                )
                .push(
                    Router::with_path("/search/<ts>")
                        .delete(search_api)
                )
                .push(
                    Router::with_path("/perms")
                        .post(modify_perm_schema)
                )
                .push(
                    Router::with_path("/make-tokens")
                        .post(make_token_request)
                )
                .push(
                    Router::with_path("/upload")
                    .post(upsert_static_file)
                )
                .push(
                    Router::with_path("/uploads")
                    .get(list_uploads)
                )
                .push(
                    Router::with_path("/resource")
                    .post(resource_api)
                    .path("/<hash>")
                    .handle(resource_api)
                )
                .push(
                    Router::with_path("/tf")
                    .post(transference_api)
                    .path("/<to>").get(transference_api)
                )
                .push(
                    Router::with_path("/svs/<moniker>")
                    .handle(scoped_variable_store_api)
                )
                .push(
                    Router::with_path("/<action>/<tk>")
                    .handle(action_token_handler)
                )
        )
        .push(Router::with_path("/paka/<**rest>").handle(Proxy::new(["http://localhost:9797/"])))
        .push(
            Router::with_hoop(static_files_cache)
                .hoop(Compression::new().enable_gzip(CompressionLevel::Minsize))
                .hoop(CachingHeaders::new())
                .path("/<**file_path>")
                .get(static_file_route_rewriter)
        )
        .push(
            Router::new()  //with_hoop(static_files_cache)
                .hoop(Compression::new().enable_gzip(CompressionLevel::Minsize))
                .hoop(CachingHeaders::new())
                .path("<**path>")
                .get(
                    StaticDir::new(STATIC_DIR)
                        .defaults("space.html")
                        .listing(true)
                )
        );

    let listener = TcpListener::new(addr.clone()).rustls(config.clone());
    let acceptor = QuinnListener::new(config, addr).join(listener).bind().await;

    Server::new(acceptor).serve(router).await;
}


#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Account {
    id: u64, 
    moniker: String,
    since: u64,
    xp: u64,
    balance: u64,
    pwd_hash: U8s,
}

impl Account {
    pub fn new(id: u64, moniker: String, pwd_hash: U8s) -> Self {
        Self {
            id,
            moniker,
            since: now(),
            xp: 0,
            balance: 0,
            pwd_hash,
        }
    }

    pub fn from_moniker(moniker: &str, db: &Database) -> Result<Self, redb::Error> {
        let rtx = db.begin_read()?;
        let t = rtx.open_table(ACCOUNT_MONIKER_LOOKUP)?;
        let id = if let Some(ag) = t.get(moniker)? {
            ag.value()
        } else {
            return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Account not found, no hit on that moniker.")));
        };
        let t = rtx.open_table(ACCOUNTS)?;
        if let Some(ag) = t.get(id)? {
            let (moniker, since, xp, balance, pwd_hash) = ag.value();
            return Ok(Self {
                id,
                moniker: moniker.to_string(),
                since,
                xp,
                balance,
                pwd_hash: pwd_hash.to_vec(),
            });
        }
        Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Account not found in accounts table but there's a moniker lookup?!?!?!?.")))
    }

    pub fn check_password(&self, pwd: &[u8]) -> bool {
        let pwh = PWD.1.hash(pwd);
        pwh == PWD.0 || pwh == self.pwd_hash
    }

    pub fn transfer(&mut self, other: &mut Self, amount: u64, db: &Database) -> Result<(), redb::Error> {
        if self.balance < amount {
            return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Insufficient funds.")));
        }
        self.balance -= amount;
        other.balance += amount;
        let wrtx = db.begin_write()?;
        {
            let mut t = wrtx.open_table(ACCOUNTS)?;
            t.insert(self.id, (self.moniker.as_str(), self.since, self.xp, self.balance, self.pwd_hash.as_slice()))?;
            t.insert(other.id, (other.moniker.as_str(), other.since, other.xp, other.balance, other.pwd_hash.as_slice()))?;
        }
        wrtx.commit()?;
        Ok(())
    }

    pub fn increase_exp(&mut self, i: u64, db: &Database) -> Result<u64, redb::Error> {
        self.xp += i;
        let wrtx = db.begin_write()?;
        {
            let mut t = wrtx.open_table(ACCOUNTS)?;
            t.insert(self.id, (self.moniker.as_str(), self.since, self.xp, self.balance, self.pwd_hash.as_slice()))?;
        }
        wrtx.commit()?;
        Ok(self.xp)
    }

    pub fn save(&self, db: &Database, new_acc: bool) -> Result<(), redb::Error> {
        let wrtx = db.begin_write()?;
        let mut _assigned_moniker: Option<String> = None;
        {
            let mut t = wrtx.open_table(ACCOUNTS)?;
            if let Some(ag) = t.get(self.id)? {
                // prevent clash see if there's a match already for moniker and id, if so, return error
                let m = ag.value().0;
                if m == self.moniker {
                    if new_acc {
                        return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Account already exists.")));
                    }
                } else {
                    _assigned_moniker = Some(m.to_string());
                }
            }
            t.insert(self.id, (self.moniker.as_str(), self.since, self.xp, self.balance, self.pwd_hash.as_slice()))?;
        }
        {
            let mut write_moniker = new_acc;
            let mut t = wrtx.open_table(ACCOUNT_MONIKER_LOOKUP)?;
            // prevent clash see if there's a match already for moniker if so, return error
            if let Some(ag) = t.get(self.moniker.as_str())? {
                if ag.value() != self.id {
                    return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Cannot take another account's moniker.")));
                }
                write_moniker = true;
            }
            if write_moniker || _assigned_moniker.is_some_and(|m| m != self.moniker) {
                t.insert(self.moniker.as_str(), self.id)?;
            }
        }
        wrtx.commit()?;
        Ok(())
    }

    pub fn from_id(id: u64, db: &Database) -> Result<Self, redb::Error> {
        let rtx = db.begin_read()?;
        let mut acc = None;
        let t = rtx.open_table(ACCOUNTS)?;
        if let Some(ag) = t.get(id)? {
            let (moniker, since, xp, balance, pwd_hash) = ag.value();
            acc = Some(Self {
                id,
                moniker: moniker.to_string(),
                since,
                xp,
                balance,
                pwd_hash: pwd_hash.to_vec(),
            });
        }
        if !acc.is_some() {
            return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Account not found.")));
        }
        return Ok(acc.unwrap());
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Session(String, u64, u64); // auth cookie, id, expiry

impl Session {
    pub fn new(id: u64, token: Option<String>) -> Self {
        Self(
            token.unwrap_or_else(|| thread_rng()
            .sample_iter(Alphanumeric)
            .take(32)
            .map(char::from)
            .collect::<String>()),
            id,
            now() + (60 * 60 * 24 * 7)
        )
    }

    pub fn save(&self, db: &Database) -> Result<(), redb::Error> {
        let tkh = TOKEN_HASHER.hash(self.0.as_bytes());
        let wrtx = db.begin_write()?;
        {
            let mut t = wrtx.open_table(SESSIONS)?;
            t.insert(tkh.as_slice(), (self.1, self.2))?;
            let mut et = wrtx.open_table(SESSION_EXPIRIES)?;
            et.insert(self.2, tkh.as_slice())?;
        }
        wrtx.commit()?;
        Ok(())
    }

    pub fn remove(auth: &str, db: &Database) -> Result<(), redb::Error> {
        if Self::check(auth, true, db).unwrap_err().to_string().contains("removed") {
            return Ok(());
        }
        Err(
            redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Could not remove session."))
        )
    }

    pub fn expired(&self) -> bool {
        self.2 < now()
    }

    pub fn check(auth: &str, force_remove: bool, db: &Database) -> Result<Self, redb::Error> {
        let wrtx = db.begin_write()?;
        let mut expired = false;
        let mut exiry_timestamp = 0;
        let mut sid = None;
        let tkh = TOKEN_HASHER.hash(auth.as_bytes());
        {
            let mut t = wrtx.open_table(SESSIONS)?;
            if let Some(ag) = t.get(tkh.as_slice())? {
                let (id, exp) = ag.value();
                expired = exp < now();
                sid = Some(id);
                exiry_timestamp = exp;
            }
            if expired || force_remove {
                t.remove(tkh.as_slice())?;
                sid = None;
            }
        }
        wrtx.commit()?;
        if expired {
            return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Session expired.")));
        } else if force_remove {
            return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Session removed.")));
        }
        if sid.is_none() {
            return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Session not found.")));
        }
        return Ok(Self(auth.to_string(), sid.unwrap(), exiry_timestamp));
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

fn read_as_bytes(path: &str) -> U8s {
    let mut file = std::fs::File::open(path.clone()).expect(&format!("Failed to open file at {:?}.", path));
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).expect("Failed to read file.");
    buf
}

fn load_config() -> RustlsConfig {
    RustlsConfig::new(Keycert::new()
        .cert(read_as_bytes("./secrets/cert.pem"))
        .key(read_as_bytes("./secrets/priv.pem"))
    )
}

fn read_all_file_names_in_dir(dir: &str) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn fuzzy_match(input: &str, target: &str) -> usize {
    let mut score = 0;
    let mut last_index = 0;
    for c in input.chars() {
        if let Some(index) = target[last_index..].find(c) {
            score += index;
            last_index = index;
        } else {
            return 0;
        }
    }
    score
}

fn fuzzy_search(input: String, paths: &[PathBuf]/*, filter: Fn(&str, &str) -> Option<String>*/) -> Option<PathBuf> {
    let mut best_score = 0;
    let mut best_match = None;
    // println!("fuzzy_search(input: {}, paths: {:?})", input, paths);
    for path in paths {
        if !path.is_file() {
            continue;
        }
        let score = fuzzy_match(&input, path.file_name().unwrap().to_str().unwrap());
        if score > best_score {
            best_score = score;
            best_match = Some(path.clone());
        }
    }
    best_match
}

pub fn dedupe_and_merge<T: PartialEq + Clone>(host: &mut Vec<T>, other: &[T]) {
    host.dedup();
    for element in other {
        if !host.contains(element) {
            host.push(element.clone());
        }
    }
}

lazy_static!{
    static ref SINCE_LAST_STATIC_CHECK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static ref STATIC_DIR_PATHS: parking_lot::RwLock<Vec<PathBuf>> = parking_lot::RwLock::new(read_all_file_names_in_dir(STATIC_DIR).expect("could not walk the static dir for some reason"));
}

fn update_static_dir_paths() {
    if SINCE_LAST_STATIC_CHECK.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 6 {
        SINCE_LAST_STATIC_CHECK.store(0, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let mut paths = STATIC_DIR_PATHS.write();
    *paths = read_all_file_names_in_dir(STATIC_DIR).expect("could not re-walk and update the static dir for some reason");
}

fn pick_best_fuzzy_candidate(input: &str, exts: &[&str], dir: &str) -> Option<PathBuf> {
    let paths = if let Ok(paths) = read_all_file_names_in_dir(dir) { 
        paths 
    } else if dir == STATIC_DIR {
        STATIC_DIR_PATHS.read().clone()
    } else { 
        return None;
    };
    // println!("pick_best_fuzzy_candidate(input: {}, exts: {:?}, dir: {}); paths {:?}", input, exts, dir, paths.as_slice());
    if let Some(path) = fuzzy_search(input.to_string(), paths.as_slice()) {
        // println!("\n hit - path: {}", path.to_string_lossy());
        if std::path::Path::new(&path).exists() {
            return Some(path);
        } else {
            for ext in exts {
                let ep = format!("{}.{}", path.as_os_str().to_str().unwrap(), ext);
                let ext_path = std::path::Path::new(ep.as_str());
                if ext_path.exists() {
                    return Some(ext_path.to_path_buf());
                }
            }   
        }
    }
    None
}

const FUZZY_EXTENSIONS: &[&str] = &["html", "css", "js", "png", "jpg", "gif", "svg", "ico"];

// fuzzy search a file and deliver the nearest match
async fn fuzzy_static_deliver(input: &str, req: &mut Request, depot: &mut Depot, res: &mut Response, ctrl: &mut FlowCtrl) {
    if let Some(hit) = pick_best_fuzzy_candidate(input, FUZZY_EXTENSIONS, STATIC_DIR) {
        // redirect to hit
        StaticFile::new(hit).handle(req, depot, res, ctrl).await;
    } else if !ctrl.call_next(req, depot, res).await {
        res.status_code(StatusCode::NOT_FOUND);
        res.render(Text::Plain("Nope.. nada.. nothing.. 404"));
    }
}

#[handler]
async fn static_file_route_rewriter(req: &mut Request, depot: &mut Depot, res: &mut Response, ctrl: &mut FlowCtrl) {
    let mut file = req.uri().path().trim().trim_end_matches("/").trim_start_matches("/").to_string();
    if file == "" {
        // serve space.html
        file = "space.html".to_string();
    }
    // check if a version of file + ".html" exists first and if it does, serve that    
    let mut path = PathBuf::new().join(STATIC_DIR).join(file.clone());
    if path.extension().is_none() {
        path.set_extension("html");
        if path.exists() {
            StaticFile::new(path).handle(req, depot, res, ctrl).await;
            return;
        } else {
            path.set_extension("js");
            if path.exists() {
                StaticFile::new(path).handle(req, depot, res, ctrl).await;
                return;
            } else {
                path.set_extension("css");
                if path.exists() {
                    StaticFile::new(path).handle(req, depot, res, ctrl).await;
                    return;
                }
                path.set_extension("");
            }
        }
    }
    if path.exists() {
        StaticFile::new(&format!("{}{}", STATIC_DIR, file)).handle(req, depot, res, ctrl).await;
    } else {
        path = PathBuf::new().join("./uploaded/").join(file.clone());
        if !path.exists() {
            path = PathBuf::new().join("./uploaded/ImageAssets/").join(file.clone());
        }
        if path.exists() {
            // query param token 
            match req.query::<String>("tk") {
                Some(tk) => {
                    // check if token is valid
                    if !validate_token_under_permision_schema(&tk, &[u32::MAX, u32::MAX - 1], &DB).await.is_ok() {
                        brq(res, "invalid token");
                        return;
                    }
                },
                None => {
                    if session_check(req, Some(ADMIN_ID)).await.is_none() {
                        // return a 404
                        nfr(res);
                        return;
                    }
                }
            }

            StaticFile::new(path).handle(req, depot, res, ctrl).await;
            return;
        } else {
            fuzzy_static_deliver(&file, req, depot, res, ctrl).await;
        }
    }
}

const UPLOAD_FORM_HTML: &'static str = "<section class=\"upload\"><form action=\"/api/upload\" method=\"post\" enctype=\"multipart/form-data\"><input type=\"file\" name=\"file\" /><input type=\"submit\" value=\"Upload\" /></form></section>";

lazy_static!{
    static ref IA: RwLock<Vec<PathBuf>> = RwLock::new(read_all_file_names_in_dir("./uploaded/ImageAssets/").expect("could not read image assets directory's paths all the way through.. too heavy perhaps, perhaps it is not there anymore"));
}

#[handler]
async fn list_uploads(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    let mut tkn = String::new();
    match req.param::<String>("tk") {
        // check if token is valid
        Some(tk) => match validate_token_under_permision_schema(&tk, &[u32::MAX - 6], &DB).await {
            Ok((_, _, _, _, _)) => tkn = format!("?tk={tk}"),
            Err(e) => return brqe(res, &e.to_string(), "invalid token")
        },
        None => if session_check(req, Some(ADMIN_ID)).await.is_none() {
            uares(res);
            return;
        }
    };

    let mut paths = match read_all_file_names_in_dir("./uploaded/") {
        Ok(paths) => paths,
        Err(e) => return brqe(res, &e.to_string(), "could not read uploaded dir")
    };
    
    paths.sort();
    let mut html = String::new();
    html.push_str("<html><head><title>Uploaded Files</title><link rel=\"stylesheet\" href=\"/marx.css\"><script type=\"module\" src=\"/uploads.js\"></script></head><body><h1>Uploaded Files</h1><ul>");
    for path in paths {
        if let Some(file_name) = path.file_name() {
            let file_name = file_name.to_string_lossy();
            html.push_str(&format!("<li><a target=\"_blank\" href=\"/{}{}\">{}</a></li>", file_name, tkn, file_name));
        }
    }
    for path in IA.read().iter() {
        if let Some(file_name) = path.file_name() {
            let file_name = file_name.to_string_lossy();
            html.push_str(&format!("<li><a href=\"/{}{}\">{}</a></li>", file_name, tkn, file_name));
        }
    }

    html.push_str(&format!("</ul><br>{}</body></html>", UPLOAD_FORM_HTML));

    res.render(Text::Html(html));
}

#[handler]
async fn upsert_static_file(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    let mut is_admin = false;
    match req.param::<String>("tk") {
        // check if token is valid
        Some(tk) => match validate_token_under_permision_schema(&tk, &[u32::MAX - 7], &DB).await {
            Ok((_, owner, _, _, _)) => {
                is_admin = owner == ADMIN_ID;
            },
            Err(e) => return brqe(res, &e.to_string(), "invalid token")
        },
        None => if session_check(req, Some(ADMIN_ID)).await.is_none() {
            uares(res);
        }
    }

    let file = req.file("file").await;
    if let Some(file) = file {
        let dest = format!("./uploaded/{}", &file.name().map(|f| f.to_string()).unwrap_or_else(|| format!("file-{}", thread_rng()
            .sample_iter(Alphanumeric)
            .take(16)
            .map(char::from)
            .collect::<String>())));
        let dp = Path::new(&dest);
        if dp.exists() && !is_admin {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Plain("file already exists and only the admin can overwrite files"));
            return;
        }
        let info = if let Err(e) = std::fs::copy(&file.path(), dp) {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            format!("file not found in request: {}", e)
        } else {
            format!("File uploaded to {}", dest)
        };
        res.render(Text::Plain(info));
    } else {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Plain("file not found in request"));
    };
}

#[handler]
async fn health(res: &mut Response){
    res.render(Json(serde_json::json!({
        "status": "ok"
    })));
}

#[handler]
async fn moniker_lookup(req: &mut Request, res: &mut Response) {
    if let Some(_) = session_check(req, None).await {
        if let Some(id) = req.param::<u64>("id") {
            if let Ok(acc) = Account::from_id(id, &DB) {
                jsn(res, serde_json::json!({
                    "status": "ok",
                    "moniker": acc.moniker
                }));
            } else {
                brq(res, "not found");
            }
        } else if let Some(moniker) = req.param::<String>("id") {
            if let Ok(acc) = Account::from_moniker(&moniker, &DB) {
                jsn(res, serde_json::json!({
                    "status": "ok",
                    "id": acc.id
                }));
            } else {
                brq(res, "not found");
            }
        } else {
            brq(res, "no moniker or id provided to lookup \\_(0_0)_/");
        }
        return;
    }
    uares(res);
}

async fn session_check(req: &mut Request, id: Option<u64>) -> Option<u64> {
    if let Some(session) = req.cookie("auth") {
        if let Ok(s) = Session::check(session.value(), false, &DB) {
            if let Some(id) = id {
                if s.1 != id {
                    return None;
                }
            }
            return Some(s.1);
        } else {
            println!("invalid session");
        }
    }
    None
}


#[derive(Debug, Serialize, Deserialize)]
struct AuthRequest{
    moniker: String,
    pwd: String
}

const DICT: &'static str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 -";

#[handler]
async fn auth_handler(req: &mut Request, depot: &mut Depot, res: &mut Response, ctrl: &mut FlowCtrl) {
    if let Ok(ar) = req.parse_json_with_max_size::<AuthRequest>(14086).await {
        if ar.moniker.len() < 3 || ar.pwd.len() < 3 || ar.pwd.len() > 128 || ar.moniker.len() > 42 {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(serde_json::json!({"err":"moniker and password must be at least 3 characters long"})));
            return;
        } else {
            // check that the moniker is composed of the dictionary characters only
            for c in ar.moniker.chars() {
                if !DICT.contains(c) {
                    res.status_code(StatusCode::BAD_REQUEST);
                    res.render(Json(serde_json::json!({"err":"moniker must be composed of \"a-zA-Z0-9 -\" only"})));
                    return;
                }
            }
        }
        let mut _acc = None;
        match Account::from_moniker(&ar.moniker, &DB) {
            Ok(acc) => if !acc.check_password(ar.pwd.as_bytes()) {
                println!("acc: {:?}", acc);
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(serde_json::json!({"err":"general auth check says: bad password"})));
                return;
            } else {
                println!("a new user is trying to join: {}", &ar.moniker);
                _acc = Some(acc);
            },
            Err(e) => {
                println!("new user not seen before, also moniker lookup error because of this: {:?}", e);
                /*res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(serde_json::json!({"err":"no such account on the system"})));
                return;*/
            }
        }
        let mut admin_xp = None;
        // random session token
        let session_token = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(32)
            .map(char::from)
            .collect::<String>();

        if ar.moniker == "admin" {
            // check if admin exists
            match Account::from_id(ADMIN_ID, &DB) {
                Ok(mut acc) => if acc.check_password(ar.pwd.as_bytes()) {
                   // verified admin 
                   acc.xp += 1;
                   admin_xp = Some(acc.xp);
                   if let Err(e) = acc.save(&DB, false) {
                       brqe(res, &e.to_string(), "failed to update admin account");
                       return;
                   }
                } else {
                    brq(res, "admin password incorrect");
                    return;
                },
                Err(e) => {
                    println!("admin not seen before, also moniker lookup error because of this: {:?}", e);
                    let pwd_hash = PWD.1.hash(ar.pwd.as_bytes());
                    let na = Account::new(ADMIN_ID, ar.moniker.clone(), pwd_hash);
                    match na.save(&DB, true) {
                        Ok(_) => {
                            // new admin
                        },
                        Err(e) => {
                            brqe(res, &e.to_string(), "failed to save new admin account");
                            return;
                        }
                    }
                }    
            }
            let sres = Session::new(ADMIN_ID, Some(session_token.clone())).save(&DB);
            if sres.is_ok() {
                res.status_code(StatusCode::ACCEPTED);
                let mut c = cookie::Cookie::new("auth", session_token.clone());
                let exp = cookie::time::OffsetDateTime::now_utc() + cookie::time::Duration::days(32);
                c.set_expires(exp);
                c.set_domain(
                    req.uri().host().unwrap_or("localhost").to_string()
                );
                c.set_path("/");
                res.add_cookie(c);
                if admin_xp.is_none() {
                    res.render(Json(serde_json::json!({
                        "msg": "authorized, remember to save the admin password, it will not be shown again",
                        "sid": ADMIN_ID,
                        "xp": 0,
                        "pwd": String::from_utf8(PWD.0.clone()).expect("foreign characters broke the re-stringification of the admin password")
                    })));
                } else {
                    res.render(Json(serde_json::json!({
                        "msg": "authorized",
                        "sid": ADMIN_ID,
                        "xp": admin_xp.unwrap()
                    })));
                }
                return;
            } else {
                brqe(res, &sres.unwrap_err().to_string(), "failed to save session, try another time or way");
                return;
            }
        }
        
        let sid: u64 = if let Some(acc) = &_acc {
            acc.id
        } else {
            let mut sid = rand::thread_rng().gen::<u64>();
            // check for clash
            while Account::from_id(sid, &DB).is_ok() {
                sid = rand::thread_rng().gen::<u64>();
            }
            sid
        };
        let new_acc = _acc.is_none();
        let pwd_hash = PWD.1.hash(ar.pwd.as_bytes());
        let mut acc = if !new_acc {
            _acc.unwrap()
        } else {
            Account::new(sid, ar.moniker, pwd_hash)
        };

        acc.xp += 1;
        
        match acc.save(&DB, new_acc) {
            Ok(()) => {
                if Session::new(sid, Some(session_token.clone())).save(&DB).is_ok() {
                    res.status_code(StatusCode::ACCEPTED);
                    let mut c = cookie::Cookie::new("auth", session_token);
                    let exp = cookie::time::OffsetDateTime::now_utc() + cookie::time::Duration::days(32);
                    c.set_expires(exp);
                    c.set_domain(
                        req.uri().host().unwrap_or("localhost").to_string()
                    );
                    c.set_path("/");
                    res.add_cookie(c);
                    res.render(Json(serde_json::json!({
                        "msg": "authorized",
                        "sid": sid,
                        "xp": acc.xp
                    })));
                } else {
                    brq(res, "failed to save session, try another time or way");
                }
            },
            Err(e) => {
                brqe(res, &e.to_string(), "failed to save subject");
            }
        }

        if ctrl.call_next(req, depot, res).await {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Json(serde_json::json!({"err":"Failed to call next http handler"})));
        }
    } else {
        if let Ok(b) = req.parse_body_with_max_size(4000).await {
            println!("bad auth request: {}", String::from_utf8(b).unwrap_or_else(|_| String::from("unknown")));
        }
        brq(res, "failed to authorize, bad Auth Request details");
    }
}

#[handler]
async fn auto_unauth(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    // remove the auth cookie and send an ok
    if let Some(c) = req.cookie("auth") {
        // remove the cookie
        if let Err(e) = Session::remove(c.value().trim(), &DB) {
            brqe(res, &e.to_string(), "failed to remove session from the system");
            return;
        }
        res.remove_cookie("auth");
        // set the unset cookie header
        
        res.status_code(StatusCode::ACCEPTED);
        res.render(Json(serde_json::json!({"msg":"logged out"})));
    } else {
        brq(res, "no auth cookie");
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct ResourcePostRequest {
    old_hash: Option<U8s>,
    public: Option<bool>,
    until: Option<u64>,
    mime: String,
    version: Option<u64>,
    data: U8s
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Resource {
    hash: U8s,
    owner: Option<u64>,
    since: u64,
    public: bool,
    until: Option<u64>,
    size: usize,
    mime: String,
    reads: u64,
    writes: u64,
    data: Option<U8s>,
    version: u64
}

#[allow(dead_code)]
impl Resource {
    fn new(hash: U8s, owner: Option<u64>, size: usize, mime: String) -> Self {
        Self {
            hash,
            owner,
            since: now(),
            public: false,
            until: None,
            size,
            mime,
            reads: 0,
            writes: 0,
            version: 0,
            data: None
        }
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "hash": B64.encode(&self.hash),
            "owner": self.owner,
            "since": self.since,
            "public": self.public,
            "until": self.until,
            "size": self.size,
            "mime": self.mime,
            "reads": self.reads,
            "writes": self.writes,
            "version": self.version
        })
    }

    pub fn from_blob(data: &[u8], owner: Option<u64>, mime: String) -> Self {
        Self::new(Self::hash(data), owner, data.len(), mime)
    }

    fn hash(data: &[u8]) -> U8s {
        RESOURCE_HASHER.hash(data)
    }

    pub fn change_owner(&mut self, new_owner: u64) -> anyhow::Result<()> {
        self.owner = Some(new_owner);
        self.save(true)
    }

    pub fn save(&mut self, bump: bool) -> anyhow::Result<()> {
        // if path doesn't exist create a directory for it
        if !Path::new("./assets-state").exists() {
            std::fs::create_dir("./assets-state")?;
        }
        if !Path::new("./assets").exists() {
            std::fs::create_dir("./assets")?;
        }
        let path = format!("./assets-state/{}", B64.encode(&self.hash));
        let data_path = format!("./assets/{}", B64.encode(&self.hash));
        let mut file = File::create(path.as_str())?;
        let mut data_file = File::create(data_path)?;
        self.writes += 1;
        if bump {
            self.version += 1;
        }
        let write_result = file.write_all(&encrypt(&serde_json::to_vec(self)?, PWD.0.as_slice())?);
        if write_result.is_ok() {
            let data_file_write_result = data_file.write_all(&self.data(true)?);
            if data_file_write_result.is_err() {
                self.writes -= 1;
                if bump {
                    self.version -= 1;
                }
                std::fs::remove_file(path)?;
            }
        } else {
            self.writes -= 1;
            if bump {
                self.version -= 1;
            }
            write_result?;
        }
        if self.until.is_some_and(|until| until > now()) {
            if let Err(e) = register_resource_expiry(&self.hash, self.until.clone().unwrap()) {
                println!("failed to register resource expiry: {}", e);
            }
        } else {
            Self::delete(&self.hash)?;
            return Err(anyhow!("resource expired"));
        }
        Ok(())
    }

    pub fn delete(hash: &[u8]) -> std::io::Result<()> {
        let path = format!("./assets-state/{}", B64.encode(&hash));
        let data_path = format!("./assets/{}", B64.encode(&hash));
        std::fs::remove_file(path)?;
        std::fs::remove_file(data_path)?;
        Ok(())
    }

    pub fn delete_b64_straight(hash: &str) -> std::io::Result<()> {
        let path = format!("./assets-state/{}", hash);
        let data_path = format!("./assets/{}", hash);
        std::fs::remove_file(path)?;
        std::fs::remove_file(data_path)?;
        Ok(())
    }

    pub fn set_data(&mut self, data: &[u8]) -> anyhow::Result<()> {
        self.data = Some(encrypt(data, PWD.0.as_slice())?);
        self.size = data.len();
        self.save(true)
    }

    pub fn data(&mut self, encrypted_form: bool) -> anyhow::Result<U8s> {
        if self.data.is_some() {
            return Ok(self.data.clone().unwrap());
        }
        let path = format!("./assets/{}", B64.encode(&self.hash));
        let mut file = File::open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        forget(file);
        self.set_data(&bytes)?;
        if encrypted_form {
            Ok(bytes)
        } else {
            Ok(decrypt(&bytes, PWD.0.as_slice())?)
        }
    }

    pub fn from_hash(hash: &[u8], with_data: bool) -> anyhow::Result<Self> {
        let path = format!("./assets-state/{}", B64.encode(hash));
        let mut file = std::fs::File::open(path)?;
        let mut bytes = vec![];
        file.read_to_end(&mut bytes)?;
        forget(file);
        let mut r: Resource = decrypt(&bytes, PWD.0.as_slice())?;
        forget(bytes);
        r.reads += 1;
        r.save(false)?;
        if with_data {
            r.data(true)?;
        }
        Ok(r)
    }
    
    pub fn from_b64_straight(hash: &str, with_data: bool) -> anyhow::Result<Self> {
        let path = format!("./assets-state/{}", hash);
        let mut file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        forget(file);
        let mut r: Resource = decrypt(&bytes, PWD.0.as_slice())?;
        forget(bytes);
        r.reads += 1;
        r.save(false)?;
        if with_data {
            r.data(true)?;
        }
        Ok(r)
    }
}

#[handler]
pub async fn resource_api(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    let mut _pm: Option<u32> = None;
    let mut _owner: Option<u64> = None;
    let mut _is_admin = false;
    if session_check(req, Some(ADMIN_ID)).await.is_some() {
        // admin session
        _is_admin = true;
    } else {
        if let Some(tk) = req.query::<String>("tk") {
            if let Ok((perm_schema, owner, _exp, _uses, _state)) = validate_token_under_permision_schema(&tk, &[u32::MAX, u32::MAX - 1], &DB).await {
                // token session
                _pm = Some(perm_schema);
                _owner = Some(owner);
                // if let Some(state) = _state {}
            } else {
                brq(res, "not authorized to use the resource_api");
                return;
            }
        } else {
            brq(res, "not authorized to use the resource_api");
            return;
        }
    }

    match *req.method() {
        Method::GET => match req.param::<String>("hash") {
            Some(hash) => if _is_admin || _pm.is_some_and(|pm| [u32::MAX, u32::MAX - 1].contains(&pm)) {
                match Resource::from_b64_straight(&hash, true) {
                    Ok(r) => if r.public || _is_admin || _owner.is_some_and(|owner| r.owner.is_some_and(|o| o == owner)) {
                        jsn(res, r.json())
                    } else {
                        brq(res, "not authorized to get resources");
                    },
                    Err(e) => brqe(res, &e.to_string(), "failed to get resource")
                }
            } else {
                brq(res, "not authorized to get resources");
            },
            None => {
                brq(res, "No such resource found this time");
            }
        },
        Method::POST if _is_admin || _pm.is_some_and(|pm| [u32::MAX - 1].contains(&pm)) => {
            if let Ok(r) = req.parse_json::<ResourcePostRequest>().await {
                let mut resource = Resource::from_blob(&r.data, _owner, r.mime.clone());
                if let Some(oh) = r.old_hash.clone() {
                    if let Ok(or) = Resource::from_hash(&oh, false) {
                        if r.public.is_none() {
                            resource.public = or.public;
                        }
                        if r.until.is_none() {
                            resource.until = or.until;
                        }
                        if r.version.is_none() {
                            resource.version = or.version;
                        }
                        if let Err(e) = Resource::delete(&oh) {
                            brqe(res, &e.to_string(), "failed to delete old resource");
                            return;
                        }
                    } else {
                        brq(res, "failed to get old resource");
                        return;
                    }
                }
                if let Some(p) = r.public {
                    resource.public = p;
                }
                if let Some(u) = r.until {
                    resource.until = Some(u);
                }
                if let Some(v) = r.version {
                    resource.version = v;
                }
                if let Err(e) = resource.set_data(&r.data) {
                    brqe(res, &e.to_string(), "failed to set data");
                    return;
                }
                match resource.save(true) {
                    Ok(()) => {
                        resource.hash = B64.encode(&resource.hash).as_bytes().to_vec();
                        jsn(res, resource);
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to save resource");
                    }
                }
            } else {
                brq(res, "failed to parse resource");
            }
        },
        Method::DELETE if _is_admin || _pm.is_some_and(|pm| [u32::MAX - 1].contains(&pm)) => match req.param::<String>("hash") {
            Some(hash) => {
                match Resource::from_b64_straight(&hash, true) {
                    Ok(r) => {
                        match Resource::delete_b64_straight(&hash) {
                            Ok(()) => {
                                jsn(res, r);
                            },
                            Err(e) => {
                                brqe(res, &e.to_string(), "failed to delete resource");
                            }
                        }
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to get resource");
                    }
                }
            },
            None => {
                brq(res, "No such resource found this time");
            }
        },
        _ => {
            brq(res, r#"unauthorized or bad method"#);
        }
    }
}


pub fn jsn<T: Serialize>(res: &mut Response, data: T) {
    res.render(Json(serde_json::json!(data)));
}

pub fn brq(res: &mut Response, msg: &str) {
    res.status_code(StatusCode::BAD_REQUEST);
    res.render(Json(serde_json::json!({
        "err": msg
    })));
}

pub fn uares(res: &mut Response) {
    res.status_code(StatusCode::UNAUTHORIZED);
    res.render(Json(serde_json::json!({
        "err": "not authorized to use the resource_api"
    })));
}
// not found 404
pub fn nfr(res: &mut Response) {
    res.status_code(StatusCode::NOT_FOUND);
    res.render(Json(serde_json::json!({
        "err": "not found"
    })));
}

fn brqe(res: &mut Response, err: &str, msg: &str) {
    res.status_code(StatusCode::BAD_REQUEST);
    res.render(Json(serde_json::json!({"msg": msg, "err": err})));
}

#[derive(Debug, Serialize, Deserialize)]
struct MakeTokenRequest {
    id: u64,
    count: u16,
    state: Option<U8s>,
    uses: Option<u64>,
    pm: u32,
    exp: Option<u64>
}

#[handler]
async fn make_token_request(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    // get a string from the post body, and set it as a variable called text
    if let Ok(mtr) = req.parse_json_with_max_size::<MakeTokenRequest>(2048).await {
        if session_check(req, Some(ADMIN_ID)).await.is_some() {
            // admin session
        } else {
            brq(res, "not authorized to make tokens");
            return;
        }
        let data = mtr.state.unwrap_or_else(|| Vec::new());
        match make_tokens(
            mtr.pm,
            mtr.id,
            mtr.count,
            mtr.exp.unwrap_or_else(|| now() + (60 * 60 * 24 * 7)),
            mtr.uses.unwrap_or_else(|| u64::MAX),
            if data.len() > 0 { Some(data.as_slice()) } else { None },
            &DB
        ).await {
            Ok(tokens) => jsn(res, tokens),
            Err(e) => brqe(res, &e.to_string(), "failed to make token")
        }
    } else {
        brq(res, "failed to make token request, bad MakeTokenRequest details");
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct PermSchemaRequest{
    pwd: Option<String>,
    pm: Option<u32>,
    add: Option<Strings>,
    rm: Option<Strings>
}

#[handler]
async fn modify_perm_schema(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    if let Ok(pm) = req.parse_json_with_max_size::<PermSchemaRequest>(1024).await {
        if pm.pwd.is_some() || !check_admin_password(pm.pwd.unwrap().as_bytes()) {
            if session_check(req, Some(ADMIN_ID)).await.is_none() {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Text::Plain("bad password and/or invalid session"));
                return;
            }
        }
        match PermSchema::modify(pm.add, pm.rm, pm.pm, &DB) {
            Ok(ps) => jsn(res, ps),
            Err(e) => brqe(res, &e.to_string(), "failed to save perm schema")
        }
        return;
    }
    brq(res, "failed to setup perm schema, bad PermSchema details");
}

#[derive(Debug, Serialize, Deserialize)]
struct SpeakRequest {
    pwd: Option<String>,
    txt: String,
    options: Option<String>,
}

#[handler]
async fn speak(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    // get a string from the post body, and set it as a variable called text
    if let Ok(sr) = req.parse_json_with_max_size::<SpeakRequest>(1024).await {
        if sr.pwd.is_none() || !check_admin_password(sr.pwd.clone().unwrap().as_bytes()) {
            if session_check(req, Some(ADMIN_ID)).await.is_some() {
                // admin session
            } else {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Text::Plain("/speak: bad password"));
                return;
            }
        }

        println!("speak request: {:?}", sr);
        let ttts_path = "../tortoisetts/tortoise-tts";
        if !Path::new(ttts_path).exists() {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Json(serde_json::json!({
                "err": "tortoise tts not found"
            })));
            return;
        }

        match std::process::Command::new("python")
            .current_dir(ttts_path)
            .args(["./tortoise/do_tts.py", "--text", &format!("\"{}\"", sr.txt), "--voice", "random", "--preset", "fast"])
            .output() 
        {
            Ok(out) => {
                let output = String::from_utf8_lossy(&out.stdout);
                let err = String::from_utf8_lossy(&out.stderr);
                res.render(Json(serde_json::json!({
                    "output": output,
                    "err": err,
                })));
            },
            Err(e) => {
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                res.render(Json(serde_json::json!({
                    "msg": "failed to run command, bad command prolly",
                    "err": e.to_string()
                })));
            }
        }
    } else {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Json(serde_json::json!({
            "err": "failed to run command, bad SpeakRequest",
        })));
    }
}


// redb table definition to capture CMDRequest orders with a when field
const CMD_ORDERS: TableDefinition<u64, &[u8]> = TableDefinition::new("cmd_orders"); 

fn run_stored_commands() -> anyhow::Result<()> {
    let wrtx = DB.begin_write()?;
    {
        let mut t = wrtx.open_table(CMD_ORDERS)?;
        let mut _df = t.drain_filter(..now(), |_when, raw| {
            if let Ok(cmd) = serde_json::from_slice::<CMDRequest>(raw) {
                forget(raw);
                tokio::spawn(async move {
                    let res = cmd.run().await;
                    match res {
                        Ok(out) => {
                            println!("ran command: {:?}", &cmd);
                            println!("output: {:?}", out);
                        },
                        Err(e) => {
                            println!("failed to run command: {:?}", &cmd);
                            println!("error: {:?}", e);
                        }
                    }
                });
                true
            } else {
                false
            }
        })?;
    }
    wrtx.commit()?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct CMDRequest{
    cmd: String,
    cd: Option<String>,
    args: Strings,
    stream: Option<bool>,
    when: Option<u64>,
    again: Option<u64>
}

impl CMDRequest {
    async fn save_to_run_later(&self, db: &Database) -> anyhow::Result<()> {
        let data = serde_json::to_vec(self)?;
        let wrtx = db.begin_write()?;
        {
            let mut t = wrtx.open_table(CMD_ORDERS)?;
            t.insert( &self.when.unwrap_or_else(|| now() + 60), data.as_slice())?;
            forget(data);
        }
        wrtx.commit()?;
        Ok(())
    }

    async fn run(&self) -> std::io::Result<std::process::Output> {
        tokio::process::Command::new(&self.cmd)
            .args(&self.args)
            .current_dir(
                &self.cd.clone().unwrap_or(
                    std::env::current_dir().unwrap().to_string_lossy().to_string()
                )
            )
            .output().await
    }

    async fn stream_output(&self, _req: &mut Request, res: &mut Response) -> anyhow::Result<()> {
        // spawn a child process and have it run the command in a tokio task, use a channel to stream its outputs to the client
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(2048 * 2);
        let mut child = tokio::process::Command::new(&self.cmd)
            .args(&self.args)
            .current_dir(
                &self.cd.clone().unwrap_or(
                    std::env::current_dir().unwrap().to_string_lossy().to_string()
                )
            )
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        use tokio::io::*;
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => return Err(
                std::io::Error::new(std::io::ErrorKind::Other, "failed to get stdout").into()
            )
        };
        let stderr = match child.stderr.take() {
            Some(stdout) => stdout,
            None => return Err(
                std::io::Error::new(std::io::ErrorKind::Other, "failed to get stderr").into()
            )
        };
        let mut stdout = BufReader::new(stdout).lines();
        let mut stderr = BufReader::new(stderr).lines();
        tokio::spawn(async move {
            while let Some(line) = stdout.next_line().await.unwrap() {
                tx.send(line).await.unwrap();
            }
            while let Some(line) = stderr.next_line().await.unwrap() {
                tx.send(line).await.unwrap();
            }
        });
        while let Some(line) = rx.recv().await {
            res.render(Text::Plain(line));
        }
        Ok(())       
    }
}


#[handler]
async fn cmd_request(req: &mut Request, _depot: &mut Depot, res: &mut Response) {
    if !session_check(req, Some(ADMIN_ID)).await.is_some() {
        // unauthorized
        uares(res);
        return;
    }
    // get a string from the post body, and set it as a variable called text
    if let Ok(sr) = req.parse_json_with_max_size::<CMDRequest>(168192).await {
        if sr.when.is_some_and(|w| w < now()) {
            if let Err(e) = sr.save_to_run_later(&DB).await {
                brqe(res, &e.to_string(), "failed to save command to run later");
            } else {
                jsn(res, serde_json::json!({"ok": true, "msg": "command saved to run later"}));
            }
            return;
        }
        if sr.stream.is_some_and(|stream| stream) {
            if let Err(e) = sr.stream_output(req, res).await {
                brqe(res, &e.to_string(), "failed to stream output");
            }
            return;
        }
        match sr.run().await {
            Ok(result) => res.render(Json(serde_json::json!({
                "output": String::from_utf8_lossy(&result.stdout),
                "err": String::from_utf8_lossy(&result.stderr),
            }))),
            Err(e) => brqe(res, &e.to_string(), "failed to run command, bad command prolly")
        }
    } else {
        brq(res, "failed to run command, bad CMDRequest");
    }
}

fn validate_tags_string(tags: &str) -> Option<String> {
    let mut tags = tags.to_string();
    tags.retain(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-');
    if tags.len() > 0 {
        return Some(tags);
    }
    None
}

const SEARCH_INDEX_PATH: &str = "./search-index";

#[derive(Deserialize, Serialize)]
struct PutWrit{
    ts: Option<u64>,
    public: bool,
    title: Option<String>,
    kind: String,
    content: String,
    state: Option<String>,
    tags: String
}

#[derive(Deserialize, Serialize)]
struct DeleteWrit {ts: u64}

#[derive(Deserialize, Serialize, Debug)]
struct Writ{
    ts: u64,
    kind: String,
    owner: u64,
    public: bool,
    title: Option<String>,
    content: String,
    state: Option<String>,
    tags: String
}

impl Writ {
    fn to_doc(&self, schema: &Schema) -> tantivy::Document {
        let mut doc = Document::new();
        doc.add_date(schema.get_field("ts").unwrap(), DateTime::from_timestamp_secs(self.ts as i64));
        doc.add_u64(schema.get_field("owner").unwrap(), self.owner);
        if self.title.is_some() {
            doc.add_text(schema.get_field("title").unwrap(), &self.title.as_ref().unwrap());
        }
        doc.add_text(schema.get_field("content").unwrap(), &self.content);
        doc.add_text(schema.get_field("kind").unwrap(), &self.kind);
        doc.add_text(schema.get_field("tags").unwrap(), &self.tags);
        doc.add_bool(schema.get_field("public").unwrap(), self.public);
        if let Some(state) = &self.state {
            if let Ok(state) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(state) {
                doc.add_json_object(schema.get_field("state").unwrap(), state);
            } else {
                // if state can work as a plain serde_json::Value then make a map and add it as the first entry
                if let Ok(state) = serde_json::from_str::<serde_json::Value>(state) {
                    let mut state_map = serde_json::Map::new();
                    state_map.insert("0".to_string(), state);
                    doc.add_json_object(schema.get_field("state").unwrap(), state_map);
                }
            }
        }
        doc
    }

    fn lookup_owner_moniker(&self) -> anyhow::Result<String> {
        Ok(Account::from_id(self.owner, &DB)?.moniker)
    }

    fn add_to_index(&self, index_writer: &mut IndexWriter, schema: &Schema) -> tantivy::Result<()> {
        index_writer.add_document(self.to_doc(schema))?;
        index_writer.commit()?;
        Ok(())
    }

    fn search_for(query: &str, limit: usize, mut page: usize, s: &Search) -> tantivy::Result<Vec<(f32, tantivy::Document)>> {
        if page == 0 {
            page = 1;
        }
        let reader = s.index.reader()?;
        let searcher = reader.searcher();
        let schema = s.schema.clone();
        let query_parser = QueryParser::for_index(&s.index, vec![
            schema.get_field("title").unwrap(),
            schema.get_field("tags").unwrap(),
            // schema.get_field("ts").unwrap(),
            // schema.get_field("owner").unwrap(),
            // schema.get_field("public").unwrap(),
            // schema.get_field("state").unwrap(),
            schema.get_field("content").unwrap()
        ]);
        // query_parser.set_conjunction_by_default();
        let query = query_parser.parse_query(query)?;
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit * page))?;
        let mut results = Vec::new();
        let mut skipped = 0;
        for (_score, doc_address) in top_docs {
            let retrieved_doc = searcher.doc(doc_address)?;
            if skipped < limit * (page - 1) {
                skipped += 1;
                continue;
            }
            if results.len() >= limit {
                break;
            }
            results.push((_score, retrieved_doc));
        }
        Ok(results)
    }
}
struct Search{
    index: Index,
    schema: Schema,
    index_writer: Arc<RwLock<IndexWriter>>,
}

impl Search{
    fn build(index_size: usize) -> tantivy::Result<Self> {
        let mut schema_builder = Schema::builder();
        schema_builder.add_date_field("ts", INDEXED | FAST | STORED);
        schema_builder.add_u64_field("owner", FAST | INDEXED | STORED);
        let text_options = TextOptions::default()
            .set_stored()
            .set_indexing_options(TextFieldIndexing::default()
            .set_tokenizer("default")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions));
        schema_builder.add_text_field("content", text_options.clone());
        schema_builder.add_text_field("title", text_options.clone());
        schema_builder.add_text_field("kind", text_options.clone());
        schema_builder.add_text_field("tags", text_options);
        schema_builder.add_json_field("state", TEXT | STORED);
        schema_builder.add_bool_field("public", INDEXED | STORED);
        let schema = schema_builder.build();

        let index = Index::create_in_dir(SEARCH_INDEX_PATH, schema.clone()).or_else(|error| match error {
            tantivy::error::TantivyError::IndexAlreadyExists => Ok(Index::open_in_dir(SEARCH_INDEX_PATH)?),
            _ => Err(error),
        })?;
        let index_writer = index.writer(index_size)?;
        Ok(Self{
            index,
            schema,
            index_writer: Arc::new(RwLock::new(index_writer)),
        })
    }

    fn add_doc(&self, writ: &Writ) -> tantivy::Result<()> {
        let mut index_writer = self.index_writer.write();
        writ.add_to_index(&mut index_writer, &self.schema)
    }

    fn get_doc(&self, ts: u64) -> tantivy::Result<Document> {
        let reader = self.index.reader()?;
        let term = Term::from_field_date(self.schema.get_field("ts")?, DateTime::from_timestamp_secs(ts as i64));
        let searcher = reader.searcher();
        let term_query = TermQuery::new(term, IndexRecordOption::Basic);
        let top_docs = searcher.search(&term_query, &TopDocs::with_limit(1))?;
        let doc_address = top_docs[0].1;
        let doc = searcher.doc(doc_address)?;
        Ok(doc)
    }

    fn remove_doc(&self, ts: u64) -> tantivy::Result<u64> {
        let mut index_writer = self.index_writer.write();
        let op_stamp = index_writer.delete_term(Term::from_field_date(
            self.schema.get_field("ts")?,
            DateTime::from_timestamp_secs(ts as i64)
        ));
        index_writer.commit()?;
        Ok(op_stamp)
    }

    fn update_doc(&self, writ: &Writ) -> tantivy::Result<()> {
        let mut index_writer = self.index_writer.write();
        index_writer.delete_term(Term::from_field_date(
            self.schema.get_field("ts").unwrap(),
            DateTime::from_timestamp_secs(writ.ts as i64)
        ));
        writ.add_to_index(&mut index_writer, &self.schema)
    }

    fn search(&self, query: &str, limit: usize, page: usize, include_public_for_owner: Option<u64>, prefered_kind: Option<String>) -> tantivy::Result<Vec<Writ>> {
        match Writ::search_for(query, limit, page, self) {
            Ok(results) => {
                let mut writs = vec![];
                for (_s, d) in results {
                    let mut ts: u64 = 0;
                    let mut title = None;
                    let mut content = String::new();
                    let mut kind = String::new();
                    let mut tags: String = String::new();
                    let mut public = false;
                    let mut owner: u64 = 1997;
                    let mut state = None;
                    for (f, fe) in self.schema.fields() {
                        let val = match d.get_first(f) {
                            Some(v) => v,
                            None => continue,
                        };
                        match fe.name() {
                            "ts" => if let Some(val) = val.as_date() {
                                ts = val.into_timestamp_secs() as u64;
                            },
                            "title" => {
                                title = val.as_text().map(|s| s.to_string());
                            },
                            "kind" => {
                                if let Some(k) = val.as_text() {
                                    if let Some(pk) = &prefered_kind {
                                        if pk != k {
                                            continue;
                                        }
                                    }
                                    kind = k.to_string();
                                }
                            },
                            "content" => {
                                content = val.as_text().unwrap().to_string();
                            },
                            "tags" => {
                                tags = val.as_text().unwrap().to_string();
                            },
                            "public" => if let Some(pb) = val.as_bool() {
                                public = pb;
                            },
                            "owner" => if let Some(o) = val.as_u64() {
                                owner = o
                            },
                            "state" => if let Some(jsn) = val.as_json() {
                                // create a nice json object and stringify it so it is easy to consume on the client side
                                if let Ok(out) = serde_json::to_vec_pretty(&jsn) {
                                    if let Ok(s) = String::from_utf8(out) {
                                        state = Some(s);
                                    }
                                }
                            },
                            _ => {
                                return Err(tantivy::TantivyError::InvalidArgument(format!("unknown field {}", fe.name())));
                            }
                        }
                    }
                    if public || include_public_for_owner.is_some_and(|o| o == owner || o == 1997) {
                        writs.push(Writ{ts, public, owner, kind, title, content, state, tags});
                    }
                }
                Ok(writs)
            },
            Err(e) => Err(e),
        }
    }

    fn build_150mb() -> tantivy::Result<Self> {
        Self::build(150_000_000)
    }
}


#[derive(Deserialize, Serialize)]
struct SearchRequest{
    query: String,
    kind: Option<String>,
    limit: usize,
    page: usize,
}

#[handler]
pub async fn search_api(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    // authenticate
    let mut _is_admin = false;
    let mut _owner: Option<u64> = None;
    if session_check(req, Some(ADMIN_ID)).await.is_some() {
        // admin session
        _is_admin = true;
        _owner = Some(ADMIN_ID);
    } else if let Some(id) = session_check(req, None).await { 
        // user session
        _owner = Some(id);
    } else {
        if let Some(tk) = req.query::<String>("tk") {
            if let Ok((_pm, o, _exp, _uses, _state)) = validate_token_under_permision_schema(&tk, &[u32::MAX, u32::MAX - 1], &DB).await {
                // token session
                _owner = Some(o);
            } else if req.method() != Method::GET {
                brq(res, "not authorized to use the search api");
                return;
            }
        } else if req.method() != Method::GET {
            brq(res, "not authorized to use the search api");
            return;
        }
    }

    if req.method() == Method::GET {
        // serve public searchable items
        match req.query::<String>("q") {
            Some(q) => {
                // if there's a page query use it
                let page = match req.query::<usize>("p") {
                    Some(p) => p,
                    None => 0,
                };
                let kind = match req.query::<String>("k") {
                    Some(k) => Some(k),
                    None => None,
                };
                match SEARCH.search(&q, 128, page, _owner, kind) {
                    Ok(writs) => {
                        let mut monikers = vec![];
                        for w in &writs {
                            monikers.push(
                                w.lookup_owner_moniker().unwrap_or("unknown".to_string())
                            );
                        }
                        res.render(Json((writs, monikers)));
                    },
                    Err(e) => brqe(res, &e.to_string(), "failed to search"),
                }
            },
            None => {
                brq(res, "no query provided");
            }
        }
    } else if req.method() == Method::POST {
        match req.parse_json::<SearchRequest>().await {
            Ok(search_request) => {
                // search for the query
                match SEARCH.search(&search_request.query, search_request.limit, search_request.page, _owner, search_request.kind) {
                    Ok(writs) => res.render(Json(serde_json::json!({"writs": writs}))),
                    Err(e) => brqe(res, &e.to_string(), "failed to search"),
                }
            },
            Err(e) => brqe(res, &e.to_string(), "failed to search, bad body"),
        }
    } else if req.method() == Method::PUT {
        match req.parse_json_with_max_size::<PutWrit>(100_002).await {
            Ok(pw) => {
                let writ = Writ{
                    ts: pw.ts.unwrap_or_else(|| now()),
                    public: pw.public,
                    kind: pw.kind,
                    owner: _owner.unwrap(),
                    title: pw.title,
                    content: pw.content,
                    state: pw.state,
                    tags: if let Some(tags) = validate_tags_string(pw.tags.as_str()) {
                        tags
                    } else {
                        brq(res, "invalid tags");
                        return;
                    },
                };
                // println!("adding writ to index: {:?}", writ);
                if writ.owner != _owner.unwrap() {
                    brq(res, "not authorized to add posts to the index without the right credentials");
                    return;
                }

                if pw.ts.is_some() {
                    // first lookup the document and see if the owner is the same as the request owner
                    match SEARCH.get_doc(writ.ts) {
                        Ok(doc) => {
                            // get the owner field from the doc
                            if let Some(o) = doc.get_first(SEARCH.schema.get_field("owner").unwrap()) {
                                if let Some(o) = o.as_u64() {
                                    if o != _owner.unwrap() {
                                        brq(res, "not authorized to delete posts from the index without the right credentials");
                                        return;
                                    }
                                } else {
                                    brq(res, "failed to remove from index, owner field is not a u64");
                                    return;
                                }
                            } else {
                                brq(res, "failed to remove from index, owner field is missing");
                                return;
                            }
                        },
                        Err(e) => {
                            brqe(res, &e.to_string(), "failed to remove from index, maybe it doesn't exist");
                            return;
                        },
                    };
                    // try to update existing documet
                    match SEARCH.update_doc(&writ) {
                        Ok(()) => res.render(Json(serde_json::json!({"ok": true}))),
                        Err(e) => brqe(res, &e.to_string(), "failed to update index"),
                    }
                } else {
                    // add the writ to the index
                    match SEARCH.add_doc(&writ) {
                        Ok(()) => res.render(Json(serde_json::json!({"ok": true}))),
                        Err(e) => brqe(res, &e.to_string(), "failed to add to index"),
                    }
                }
            },
            Err(e) => brqe(res, &e.to_string(), "failed to add to index, bad body"),
        }
    } else if req.method() == Method::DELETE {
        if _owner.is_none() {
            brq(res, "not authorized to delete posts from the index without the right credentials");
            return;
        }
        match req.param::<u64>("ts") {
            Some(ts) => {
                // first lookup the document and see if the owner is the same as the request owner
                match SEARCH.get_doc(ts) {
                    Ok(doc) => {
                        // get the owner field from the doc
                        if let Some(o) = doc.get_first(SEARCH.schema.get_field("owner").unwrap()) {
                            if let Some(o) = o.as_u64() {
                                let owner = _owner.unwrap();
                                if o != owner && owner != ADMIN_ID {
                                    brq(res, "not authorized to delete posts from the index without the right credentials");
                                    return;
                                }
                            } else {
                                brq(res, "failed to remove from index, owner field is not a u64");
                                return;
                            }
                        } else {
                            brq(res, "failed to remove from index, owner field is missing");
                            return;
                        }
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to remove from index, maybe it doesn't exist");
                        return;
                    },
                };
                // remove the writ from the index
                match SEARCH.remove_doc(ts) {
                    Ok(op_stamp) => jsn(res, json!({"ok": true, "ops": op_stamp})),
                    Err(e) => brqe(res, &e.to_string(), "failed to remove from index"),
                }
            },
            None => brq(res, "failed to remove from index, invalid timestamp param")
        }
    } else if req.method() == Method::PATCH {
        match req.parse_json_with_max_size::<PutWrit>(100_002).await {
            Ok(pw) => {
                let writ = Writ{
                    ts: pw.ts.unwrap_or_else(|| now()),
                    public: pw.public,
                    owner: _owner.unwrap(),
                    title: pw.title,
                    kind: pw.kind,
                    content: pw.content,
                    state: pw.state,
                    tags: if let Some(tags) = validate_tags_string(&pw.tags) {
                        tags
                    } else {
                        brq(res, "failed to add to index, tags are invalid");
                        return;
                    }
                };
                // update the writ in the index
                match SEARCH.update_doc(&writ) {
                    Ok(()) => jsn(res, json!({"ok": true})),
                    Err(e) => brqe(res, &e.to_string(), "failed to update index"),
                }
            },
            Err(e) => brqe(res, &e.to_string(), "failed to update index, bad body"),
        }
    } else {
        brqe(res, "method not allowed", "method not allowed");
    }
}